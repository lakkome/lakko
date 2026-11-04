// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * IndexedDB-backed encrypted store for plaintext chat
 * messages (Phase 2a3f1-web).
 *
 * Mirror of:
 *
 *   * `apps/ios/Lakko/Chat/MessageLogStore.swift`
 *   * `apps/android/.../chat/MessageLogStore.kt`
 *
 * Persists the per-account `{ groupIdHex → (cursor, messages) }`
 * map the UI currently keeps in component-local state. Before
 * this layer the timeline vanished on every page reload —
 * MLS state survived via the snapshot store but its forward-
 * secret chain keys could not re-decrypt the already-consumed
 * wire bytes.
 *
 * ## Schema
 *
 * One row per `accountIdB64`:
 *
 * ```text
 *   db   "lakko-message-log"  v1
 *   ├── store "logs"  keyPath: "accountId" (string)
 *         row {
 *           accountId:  <b64-uuidv7>,
 *           ciphertext: Uint8Array   // [12-byte IV | AES-GCM ct+tag]
 *         }
 * ```
 *
 * Plaintext is the JSON-encoded [`MessageLogPayload`]. Payload
 * `version` 2 adds `PersistedMessage.authorKeyB64`; a v1 row is
 * still read (there is no migration and none is needed — the absent
 * field fails the author gate closed exactly as a reloaded row
 * always did), so the transcript survives rather than being wiped
 * for a field whose absence is already safe.
 * Wrapped under the device root key (the non-extractable
 * AES-256-GCM `CryptoKey` from 1.16c3c2) — same posture as
 * the MLS snapshot blob.
 */

import type { DeviceCipher } from "../auth/device-cipher";
import { accountStoresWritable } from "../auth/account-store-gate";
import { publishMessageLogChanged } from "./message-log-events";
import type { PersistedMembership } from "./membership-rows";

const DB_NAME = "lakko-message-log";
const DB_VERSION = 1;
const STORE = "logs";
const IV_LEN = 12;

// ----- JSON payload (mirror of iOS / Android shapes) -------------

/**
 * Payload schema version this client WRITES.
 *
 * v2 adds [`PersistedMessage.authorKeyB64`]. A v1 payload is still
 * read: its rows simply carry no author identity, which fails the
 * ADR-0023 edit/delete gate closed — the same thing every reloaded
 * row did before the field existed. There is no migration and none
 * is owed; discarding the transcript would cost history to fix
 * nothing.
 *
 * v3 adds [`PersistedMessage.edited`] / [`deleted`] / [`reactions`],
 * which is what makes an edit or a delete outlive the tab. Before
 * it, the op was folded into a session-scoped overlay in `Chat.tsx`
 * and nowhere else: a reload dropped the overlay, the journal cursor
 * had long since passed the op rows, and MLS forward secrecy means
 * they can never be decrypted again — so a deleted message came back
 * and stayed back, for good. An older payload reads with all three
 * absent, which is exactly the row as it was written.
 *
 * v4 adds [`PersistedMessage.reply`] (ADR-0052). A row without it reads as
 * a plain message, which is exactly what it was.
 *
 * v5 adds [`PersistedMessage.forward`] (ADR-0052 decision 2). A row without
 * it reads as a message that was not forwarded, which is exactly what it was.
 *
 * [`PersistedMessage.unsupportedVersion`] (ADR-0061 section 6) is optional
 * without a version bump: a row without it is an ordinary message, and an
 * older build reading a row with it shows the empty text it stores.
 */
export const MESSAGE_LOG_VERSION = 5;

export interface PersistedAttachment {
  /** Base64-encoded 32-byte SHA-256 blob id. */
  readonly blobIdB64: string;
  /** Base64-encoded 32-byte CEK. */
  readonly keyB64: string;
  /** Base64-encoded 12-byte AEAD nonce. */
  readonly nonceB64: string;
  readonly mime: string | null;
  /** Stored as a string because JS can't round-trip `u64` >2^53
   *  through `JSON.parse`. Always present (no `null`); empty
   *  attachments aren't a thing. */
  readonly sizeStr: string;
  readonly name: string | null;
  /**
   * ADR-0047 media metadata, copied from the received envelope. Every field
   * is optional so a row written before they existed reads unchanged (a
   * file chip); `persisted-attachment.ts` re-checks each bound on read.
   */
  readonly kind?: string | null;
  readonly durationMs?: number | null;
  /** Base64url of at most 128 waveform bytes. */
  readonly waveformB64?: string | null;
  readonly width?: number | null;
  readonly height?: number | null;
  readonly thumb?: PersistedThumb | null;
  /**
   * UX spec §8 — "played" is LOCAL state: this device has started playing
   * the message. Never sent, never part of a history bundle. Absent means
   * unplayed.
   */
  readonly played?: boolean;
}

/** ADR-0047 — a persisted poster-frame reference. */
export interface PersistedThumb {
  readonly blobIdB64: string;
  readonly keyB64: string;
  readonly nonceB64: string;
  readonly mime: string | null;
  readonly sizeStr: string;
  readonly width: number | null;
  readonly height: number | null;
}

export interface PersistedMessage {
  /** Stable across snapshots. Stored as a string for the same
   *  reason as `sizeStr`. */
  readonly messageId: string;
  readonly direction: "outgoing" | "incoming" | "system";
  readonly text: string;
  readonly attachments: readonly PersistedAttachment[];
  readonly createdAt: string;
  /**
   * ADR-0023 — the author's IDENTITY: their Ed25519 signature public
   * key, base64, resolved from the MLS sender leaf at receipt (see
   * `author-identity.ts`). `null` when the client could not resolve
   * it, and **absent entirely on a v1 row written before this field
   * existed** — both read back as "unknown author", which fails the
   * edit/delete gate closed. That is the same posture every reloaded
   * message had before this field existed; it never misattributes.
   *
   * Persisting it is what makes edit / delete survive a reload at
   * all: the leaf could not be persisted, because a leaf stored now
   * names a different member after the next commit. A signature key
   * does not move, so there is nothing left to defer. No secret is
   * added to the blob — the key is public, and already sits in the
   * MLS snapshot store beside it.
   */
  readonly authorKeyB64?: string | null;
  /**
   * ADR-0023 — this message was edited. `text` above is the EDITED
   * text, not the original, which matches iOS and Android and means
   * a reader that knows nothing of this flag still shows the right
   * words; the flag only adds the "(edited)" marker.
   *
   * Absent on a row written before v3, which reads as `false`.
   */
  readonly edited?: boolean;
  /**
   * ADR-0082 section 5 — the DS `messageId` of the latest edit or delete op
   * applied to this message, stringified like `messageId`. What a cached
   * translation or transcript is keyed on, so an edit (even back to the
   * same words) invalidates it. Absent when no such op carried an id.
   */
  readonly lastOpId?: string;
  /**
   * ADR-0023 — this message was deleted; the bubble is the tombstone.
   *
   * Such a row carries an EMPTY `text` and no `attachments`, which
   * is a deliberate divergence from iOS and Android: they keep both.
   * Nothing renders either one once the flag is set, so keeping them
   * would write the plaintext of a message the user deleted - and
   * the content keys of its attachments - to disk permanently, in
   * exchange for nothing. The natives should follow.
   */
  readonly deleted?: boolean;
  /**
   * ADR-0023 — the folded reactions, as `(reactor, emoji)` pairs.
   * The same flat shape iOS and Android store. Sorted on write so a
   * re-persist of unchanged state produces identical bytes.
   */
  readonly reactions?: readonly PersistedReaction[];
  /**
   * ADR-0052 — this message replies to another. The snapshot is stored as
   * received / as sent and never re-derived from the target: the bubble
   * draws it the same whether or not the original is still held here.
   * Absent on a plain message and on a row written before v4.
   */
  readonly reply?: PersistedReply;
  /**
   * ADR-0052 decision 2 — this message was forwarded. The name and the origin
   * are stored as received / as sent: the name is the forwarder's label and
   * is never looked up, the origin is what a tap on the line jumps to.
   * Absent on a message that was not forwarded and on a row written before v5.
   */
  readonly forward?: PersistedForward;
  /**
   * ADR-0061 section 6 — the message came from a newer Lakko in a wire
   * version this build cannot read; the value is that version. `text` is
   * empty and there are no attachments; the bubble and the chat list preview
   * draw `chat.message.unsupported`, in the language current when drawn.
   */
  readonly unsupportedVersion?: number;
  /**
   * `docs/ux/second-device.md` section 4 — on a system (commit) row only: the
   * person keys a received batch added to and removed from a group, recorded
   * on the batch's last commit row. Names are never stored; the row resolves
   * them when drawn. Optional without a version bump: a row without it (every
   * row written before this field) is a commit row that is not drawn.
   */
  readonly membership?: PersistedMembership;
}

/** ADR-0052 decision 2 — a persisted forward attribution. */
export interface PersistedForward {
  readonly author?: string;
  readonly origin?: {
    /** The origin conversation's group id, standard base64. */
    readonly groupIdB64: string;
    /** The original's DS `messageId`, stringified like `messageId` above. */
    readonly messageId: string;
  };
}

/** ADR-0052 — a persisted reply reference. */
export interface PersistedReply {
  /** The target's DS `messageId`, stringified like `messageId` above. */
  readonly target: string;
  readonly author?: string;
  readonly excerpt?: string;
}

/** ADR-0023 — one reaction, keyed on the reactor's IDENTITY. */
export interface PersistedReaction {
  /**
   * The reactor's Ed25519 signature key, base64, resolved from the
   * MLS sender leaf at receipt. Never a leaf index: OpenMLS hands a
   * vacated leaf to the next member added, so a leaf-keyed reaction
   * lets a newcomer toggle a departed member's reaction off and
   * merge their own into that member's count (C-68).
   *
   * May be the own-reaction sentinel, for a reaction this device
   * made before it could resolve its own key.
   */
  readonly reactorKeyB64: string;
  readonly emoji: string;
  /**
   * The journal position (DS message id, decimal) of the add that gave it,
   * so a later op re-folds against the same order on every device. Absent on
   * a row written before the fold (UX reactions-and-emoji section 10): read
   * as 0.
   */
  readonly at?: string;
}

export interface PersistedGroup {
  /** DS journal cursor (stringified i64). */
  readonly cursor: string;
  readonly messages: readonly PersistedMessage[];
  /**
   * The newest DS id this device's chat view has displayed (stringified). The
   * chat list counts incoming rows above it — and above the cross-device read
   * cursor, when that has arrived — as unread. Absent for a chat never opened
   * on this device, whose every incoming row is unread. See
   * `background-log.ts`.
   */
  readonly lastViewedMessageId?: string;
  /**
   * ADR-0063 section 5 - incoming messages above `lastViewedMessageId` the
   * feed kept on screen long enough to count as seen (stringified ids). Folded
   * into `lastViewedMessageId` once contiguous, so it only ever holds the
   * gaps. See `feed/feed-seen.ts`.
   */
  readonly feedSeen?: readonly string[];
  /**
   * docs/ux/second-device.md section 2, "History is not news": the highest DS
   * id that came with a pairing history bundle (stringified), the higher of
   * every bundle filed. A row at or below it is never unread on this device,
   * whatever the read positions say. Local: it moves no read position of the
   * account and sends no read receipt, and it is not `lastViewedMessageId`,
   * which says what the person saw. See `background-log.ts`.
   */
  readonly syncedThroughMessageId?: string;
}

/** Wire shape of the encrypted blob, post-decrypt + JSON-parse. */
export interface MessageLogPayload {
  readonly version: number;
  readonly groups: { readonly [groupIdHex: string]: PersistedGroup };
  /**
   * ADR-0073 R.7 case 1 — messages composed for a one-to-one whose successor
   * did not exist yet (`held-send.ts`), kept here so a reload does not lose
   * them: this is the one store that already holds message plaintext under
   * the device key. Written only through {@link setHeldInMessageLog}; every
   * other writer leaves it alone and {@link updateMessageLog} carries it
   * across their writes. Absent = none held.
   */
  readonly held?: readonly PersistedHeldSend[];
}

/** One held message, as stored. Never sent anywhere in this form. */
export interface PersistedHeldSend {
  /** Hex of the conversation id (the lineage root) it was composed for. */
  readonly conversationHex: string;
  /** When it was first held, UNIX milliseconds. */
  readonly heldAtMs: number;
  readonly text: string;
  readonly attachments: readonly PersistedAttachment[];
  readonly reply?: PersistedReply;
  /** The group (standard base64) the reply's target id belongs to: the
   *  group it was composed in. */
  readonly replyGroupIdB64?: string;
  readonly forward?: PersistedForward;
}

/**
 * What a write of the log keeps of the held list: the writer's own, when it
 * names one (`held` present, even empty); what is on disk otherwise. Pure.
 */
export function carryHeld(
  current: MessageLogPayload | null,
  next: MessageLogPayload,
): MessageLogPayload {
  if (Object.prototype.hasOwnProperty.call(next, "held")) {
    if (next.held !== undefined && next.held.length > 0) return next;
    const { held: _empty, ...rest } = next;
    return rest;
  }
  const held = current?.held;
  return held === undefined || held.length === 0 ? next : { ...next, held };
}

// ----- IDB plumbing ----------------------------------------------

interface PersistedRow {
  accountId: string;
  ciphertext: Uint8Array;
}

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") {
      reject(new Error("IndexedDB unavailable in this environment"));
      return;
    }
    const req = indexedDB.open(DB_NAME, DB_VERSION);
    req.onupgradeneeded = (): void => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STORE)) {
        db.createObjectStore(STORE, { keyPath: "accountId" });
      }
    };
    req.onsuccess = (): void => resolve(req.result);
    req.onerror = (): void =>
      reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

function awaitTx<T>(tx: IDBTransaction, value: T): Promise<T> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = (): void => resolve(value);
    tx.onabort = (): void => reject(tx.error ?? new Error("idb tx aborted"));
    tx.onerror = (): void => reject(tx.error ?? new Error("idb tx errored"));
  });
}

async function encryptBlob(
  deviceKey: DeviceCipher,
  plaintext: Uint8Array,
): Promise<Uint8Array> {
  const iv = crypto.getRandomValues(new Uint8Array(IV_LEN));
  const ct = new Uint8Array(
    await deviceKey.encrypt(iv, plaintext),
  );
  const out = new Uint8Array(IV_LEN + ct.length);
  out.set(iv, 0);
  out.set(ct, IV_LEN);
  return out;
}

async function decryptBlob(
  deviceKey: DeviceCipher,
  blob: Uint8Array,
): Promise<Uint8Array> {
  if (blob.length < IV_LEN + 16) {
    throw new Error("encrypted message log is too short to be valid");
  }
  const iv = blob.slice(0, IV_LEN);
  const ct = blob.slice(IV_LEN);
  const pt = new Uint8Array(
    await deviceKey.decrypt(iv, ct),
  );
  return pt;
}

// ----- Public CRUD ----------------------------------------------

/**
 * Persist (or replace) the message log for an account.
 *
 * Best-effort: an IDB / WebCrypto failure surfaces as a
 * rejected promise; callers (the chat surface) log + swallow.
 * The in-memory state stays good and the next mutation
 * retries the write.
 */
export async function saveMessageLog(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  payload: MessageLogPayload,
): Promise<void> {
  // A wiped account takes no late write (ADR-0077), and its device key may
  // already be gone (the desktop host releases it at the wipe), so ask
  // before the key is used, and again before the write lands.
  if (!accountStoresWritable(accountIdB64)) return;
  const plaintext = new TextEncoder().encode(
    JSON.stringify(withoutBlockedSlots(accountIdB64, payload)),
  );
  const ciphertext = await encryptBlob(deviceKey, plaintext);
  const row: PersistedRow = { accountId: accountIdB64, ciphertext };
  const db = await openDb();
  try {
    if (!accountStoresWritable(accountIdB64)) return;
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).put(row);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
  // G-8: notify any unread badge (in this or another tab/route) that the log
  // changed, so it can reload instead of waiting for its next mount.
  publishMessageLogChanged(accountIdB64);
}

/**
 * Read + decrypt the message log for `accountIdB64`. Returns
 * `null` when:
 *
 *   * no row on disk for this account,
 *   * AES-GCM auth failure (device key rotated since the row
 *     was written, blob tampered, …),
 *   * JSON parse failure (corrupt blob — caller starts with
 *     an empty timeline and re-bootstraps from the DS cursor).
 */
export async function loadMessageLog(
  accountIdB64: string,
  deviceKey: DeviceCipher,
): Promise<MessageLogPayload | null> {
  const db = await openDb();
  let row: PersistedRow | undefined;
  try {
    const tx = db.transaction(STORE, "readonly");
    const req = tx.objectStore(STORE).get(accountIdB64);
    row = await new Promise<PersistedRow | undefined>((resolve, reject) => {
      req.onsuccess = (): void => resolve(req.result as PersistedRow | undefined);
      req.onerror = (): void => reject(req.error ?? new Error("idb get failed"));
    });
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
  if (row === undefined) return null;
  let plaintext: Uint8Array;
  try {
    plaintext = await decryptBlob(deviceKey, row.ciphertext);
  } catch {
    return null;
  }
  try {
    return JSON.parse(new TextDecoder().decode(plaintext)) as MessageLogPayload;
  } catch {
    return null;
  }
}

/**
 * ADR-0087 section 9 — groups whose history is gone from this device, in this
 * page. A chat view that was open keeps its rows in memory and writes them on
 * its way out, and a poll that was in flight files what it decrypted: either
 * would put a wiped conversation back. Every write goes through
 * {@link saveMessageLog}, so that is where the slot is left out. Lifted when
 * the person is added again ({@link allowGroupInMessageLog}), whose new rows
 * belong here. In memory: after a reload no such writer exists.
 */
const blockedSlots = new Set<string>();

function blockKey(accountIdB64: string, groupIdHex: string): string {
  return `${accountIdB64}|${groupIdHex}`;
}

/** Refuse this group's rows from now on (the end of a membership). */
export function blockGroupInMessageLog(accountIdB64: string, groupIdHex: string): void {
  blockedSlots.add(blockKey(accountIdB64, groupIdHex));
}

/** Take this group's rows again (a Welcome brought the group back). */
export function allowGroupInMessageLog(accountIdB64: string, groupIdHex: string): void {
  blockedSlots.delete(blockKey(accountIdB64, groupIdHex));
}

/** The payload as it may be written: no blocked slot. Pure over the block
 *  set; the same object when nothing is blocked. */
export function withoutBlockedSlots(
  accountIdB64: string,
  payload: MessageLogPayload,
): MessageLogPayload {
  if (blockedSlots.size === 0) return payload;
  const blocked = Object.keys(payload.groups).filter((hex) =>
    blockedSlots.has(blockKey(accountIdB64, hex)),
  );
  if (blocked.length === 0) return payload;
  const groups: { [k: string]: PersistedGroup } = {};
  for (const [k, v] of Object.entries(payload.groups)) {
    if (!blocked.includes(k)) groups[k] = v;
  }
  return carryHeld(payload, { version: payload.version, groups });
}

/** Per-account tail of the in-page write chain — see {@link updateMessageLog}. */
const writeChains = new Map<string, Promise<unknown>>();

/**
 * Read-modify-write the message log for `accountIdB64`, serialized against
 * every other `updateMessageLog` call for the same account in this page.
 *
 * The log is ONE blob per account holding every group, and two paths now write
 * it: the open chat view and the background receive for the chats that are not
 * open. Unserialized, a write for one group that read the blob before another
 * path's write for a different group landed would put the older blob back and
 * erase that group's new rows — rows MLS forward secrecy can never decrypt
 * again. `mutate` receives what is on disk (or `null`) and returns the payload
 * to write, or `null` to write nothing.
 *
 * Not serialized across tabs: two tabs on one account each hold their own MLS
 * provider and already diverge, which is a larger problem than this lock.
 */
export function updateMessageLog(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  mutate: (current: MessageLogPayload | null) => MessageLogPayload | null,
): Promise<void> {
  const prev = writeChains.get(accountIdB64) ?? Promise.resolve();
  const run = prev.then(async () => {
    // Nothing to read or write for a wiped account (ADR-0077).
    if (!accountStoresWritable(accountIdB64)) return;
    const current = await loadMessageLog(accountIdB64, deviceKey);
    const next = mutate(current);
    // The held sends ride every writer's rewrite of the payload untouched.
    if (next !== null) await saveMessageLog(accountIdB64, deviceKey, carryHeld(current, next));
  });
  const tail = run.catch(() => undefined);
  writeChains.set(accountIdB64, tail);
  void tail.then(() => {
    if (writeChains.get(accountIdB64) === tail) writeChains.delete(accountIdB64);
  });
  return run;
}

/**
 * Erase the message log for `accountIdB64`. No-op when
 * absent. Called from `forgetDevice`. The device root key
 * itself lives in a different IDB store and gets wiped by
 * the auth layer; this call drops only the per-account
 * message-log row.
 */
export async function deleteMessageLog(accountIdB64: string): Promise<void> {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(accountIdB64);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}

/**
 * ADR-0045 UX §6.2.4 — the pure half of a per-group history delete.
 *
 * No client had one before: {@link deleteMessageLog} and its `mls-store`
 * sibling are whole-ACCOUNT operations. But the log is a per-account blob
 * keyed `{ groupIdHex -> PersistedGroup }` internally, so dropping one group's
 * entry is a read-modify-write on a store that already exists rather than a
 * new one.
 *
 * Kept separate from the IO so it is testable: `vitest` runs in plain `node`
 * with no IndexedDB, so {@link deleteGroupFromMessageLog} cannot be, and this
 * is where the rule that could actually be wrong lives.
 *
 * Returns `null` when the group was **absent**, so the caller can skip the
 * re-encrypt + write entirely. That is not just an optimisation: a rewrite
 * publishes a `message-log-changed` event to every tab, and firing one for a
 * no-op would have unread badges reload for nothing.
 *
 * Never mutates its input.
 */
export function withoutGroup(
  payload: MessageLogPayload,
  groupIdHex: string,
): MessageLogPayload | null {
  if (!Object.prototype.hasOwnProperty.call(payload.groups, groupIdHex)) {
    return null;
  }
  const groups: { [k: string]: PersistedGroup } = {};
  for (const [k, v] of Object.entries(payload.groups)) {
    if (k !== groupIdHex) groups[k] = v;
  }
  return carryHeld(payload, { version: payload.version, groups });
}

/**
 * Replace the held sends kept in the log (ADR-0073 R.7 case 1). Serialized
 * with every other write of the log; creates the payload when none exists.
 */
export function setHeldInMessageLog(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  held: readonly PersistedHeldSend[],
): Promise<void> {
  return updateMessageLog(accountIdB64, deviceKey, (current) => {
    const before = current?.held ?? [];
    if (before.length === 0 && held.length === 0) return null;
    return {
      version: current?.version ?? MESSAGE_LOG_VERSION,
      groups: current?.groups ?? {},
      held,
    };
  });
}

/**
 * ADR-0045 UX §6.2.4 — drop one group's transcript from this device.
 *
 * Resolves `false` when there was nothing to drop (no row, an undecryptable
 * row, or no entry for this group), so the caller can tell "deleted" from
 * "there was nothing there" without a second read.
 *
 * The MLS group goes separately, through `Group.deleteLocally`
 * (`chat/membership-wipe.ts`).
 */
export async function deleteGroupFromMessageLog(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  groupIdHex: string,
): Promise<boolean> {
  // Through the write chain: a row filed for another group while this ran
  // must not be lost to a read of the blob from before it.
  let dropped = false;
  await updateMessageLog(accountIdB64, deviceKey, (current) => {
    const next = current === null ? null : withoutGroup(current, groupIdHex);
    dropped = next !== null;
    return next;
  });
  return dropped;
}
