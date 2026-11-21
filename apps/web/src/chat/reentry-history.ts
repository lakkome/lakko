// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 2 section 7 - the messages sent while a device was out of a
 * conversation reach it from the device that brought it back.
 *
 * ## The carrier (one for all three request routes; same on iOS and Android)
 *
 * Security review 2026-11-21: the history no longer rides an exporter secret
 * of the group (every member after the commit held it, and any of them could
 * post a bundle the returning device took as the re-admitter's). Instead:
 *
 * - the returning device mints a gap key per request (`generateGapKey`),
 *   keeps the secret durably with the request record (sealed under the
 *   device key, `reentry-store.ts`) and puts the public key in the request;
 * - the re-admitter starts the gap's rows at `gapHistoryStart(claim,
 *   leafFirstSeen, leafLastWrite)` (nothing when that is `undefined`), seals
 *   them to the gap key and signs them with its own leaf key
 *   (`sealGapHistoryStructured`), and posts to
 *   `gapHistoryRendezvous(gapKey)` once the replacement landed;
 * - the returning device opens a row with its gap secret
 *   (`openGapHistoryStructured`) and keeps the bundle only when the joined
 *   group accepts its signer (`Group.gapHistorySignerAccepted`: its own
 *   account, the owner or an admin, the other person of a one-to-one).
 *   Otherwise the bundle is dropped and the gap line stays.
 *
 * ## When the replacement lands
 *
 * The re-admitter also re-sends the group state a newcomer receives on growth
 * (name, photo, pins, profile, push routing secret, knock key): a
 * replacement changes no member set, so no member sees growth, and the
 * returning device starts from an empty state at the new epoch.
 */

import { base64Decode, base64Encode } from "../auth/account-id";
import type { PersistedMessage } from "./message-log-store";
import {
  deleteGapLook,
  gapLookFor,
  loadOwedGapSends,
  saveGapLook,
  saveOwedGapSends,
  type ReentryRouteName,
  type SealedGapSecret,
  type StoredGapLook,
  type StoredOwedGapSend,
} from "./reentry-store";
import { DEVICE_CIPHER_IV_LEN, type DeviceCipher } from "../auth/device-cipher";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

/** Told when the gap's messages of a conversation were filed into the log,
 *  so an open chat shows them at once. */
export type GapHistoryListener = (groupIdB64: string, rows: readonly PersistedMessage[]) => void;

const listeners = new Set<GapHistoryListener>();

/** Listen for filed gap messages. Returns the unsubscribe. */
export function onGapHistoryFiled(listener: GapHistoryListener): () => void {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

/** The gap's messages of `groupIdB64` were filed. Never throws. */
export function emitGapHistoryFiled(groupIdB64: string, rows: readonly PersistedMessage[]): void {
  for (const l of [...listeners]) {
    try {
      l(groupIdB64, rows);
    } catch {
      /* a listener never fails the filing */
    }
  }
}

/** An owed send is forgotten 30 days after it was noted, the DS retention
 *  the returning device keeps looking for (chosen, not measured; live walk
 *  2026-11-19: it was a day, in memory). */
export const GAP_OWED_TTL_MS = 30 * 24 * 60 * 60_000;

/** The device that came back looks at most once a minute per group (chosen,
 *  not measured). */
export const GAP_HISTORY_LOOK_INTERVAL_MS = 60_000;

/** It keeps looking for 30 days after it came back, the DS retention; then
 *  the gap line stays (chosen, not measured). */
export const GAP_HISTORY_LOOK_RETENTION_MS = 30 * 24 * 60 * 60_000;

/** Seal a 32-byte gap secret under the device key for storage. Throws when
 *  it could not be sealed. */
export async function sealGapSecret(secret: Uint8Array, deviceKey: DeviceCipher): Promise<SealedGapSecret> {
  const iv = globalThis.crypto.getRandomValues(new Uint8Array(DEVICE_CIPHER_IV_LEN));
  const sealed = new Uint8Array(await deviceKey.encrypt(iv, secret));
  return { ivB64: base64Encode(iv), sealedSecretB64: base64Encode(sealed) };
}

/** One owed send of the re-admitter. */
interface Owed {
  readonly groupIdB64: string;
  readonly leafKeyB64: string;
  readonly gapKey: Uint8Array;
  readonly epochAfter: bigint;
  readonly gapFromMs: number;
  readonly notedAtMs: number;
}

function keyOf(o: { readonly groupIdB64: string; readonly leafKeyB64: string }): string {
  return `${o.groupIdB64}:${o.leafKeyB64}`;
}

function toOwed(row: StoredOwedGapSend): Owed | null {
  try {
    const gapKey = base64Decode(row.gapKeyB64);
    if (gapKey.length !== 32) return null;
    return {
      groupIdB64: row.groupIdB64,
      leafKeyB64: row.leafKeyB64,
      gapKey,
      epochAfter: BigInt(row.epochAfter),
      gapFromMs: row.gapFromMs,
      notedAtMs: row.notedAtMs,
    };
  } catch {
    return null;
  }
}

/** Tests: nothing is held in memory any more (the owed sends are durable,
 *  `reentry-store.ts`); kept so a test's teardown reads the same. */
export function resetGapHistoryMemory(): void {
  looking.clear();
}

/**
 * The re-admitter proposed a replacement; send once it lands. Durable
 * (live walk 2026-11-19: a hidden re-admitter's tab polled late and the
 * in-memory entry was the only record).
 */
export function noteGapHistoryOwed(input: {
  readonly groupIdB64: string;
  readonly leafKey: Uint8Array;
  /** The request's 32-byte public gap key. */
  readonly gapKey: Uint8Array;
  readonly epochAfter: bigint;
  /** The request's claim of where the gap starts. */
  readonly gapFromMs: number;
  readonly nowMs: number;
}): void {
  if (input.gapKey.length !== 32) return;
  const row: StoredOwedGapSend = {
    groupIdB64: input.groupIdB64,
    leafKeyB64: base64Encode(input.leafKey),
    gapKeyB64: base64Encode(input.gapKey),
    epochAfter: input.epochAfter.toString(),
    gapFromMs: input.gapFromMs,
    notedAtMs: input.nowMs,
  };
  const all = loadOwedGapSends().filter((o) => keyOf(o) !== keyOf(row));
  if (!saveOwedGapSends([...all, row])) {
    // eslint-disable-next-line no-console
    console.warn("chat re-entry: owed missed messages not recorded");
  }
}

/** Whether a send is owed for the group. */
export function gapHistoryOwed(groupIdB64: string): boolean {
  return loadOwedGapSends().some((o) => o.groupIdB64 === groupIdB64);
}

/** The groups a send is owed for, with the epoch each waits for. */
export function owedGapHistoryGroups(): { readonly groupIdB64: string; readonly epochAfter: bigint }[] {
  const out: { groupIdB64: string; epochAfter: bigint }[] = [];
  for (const row of loadOwedGapSends()) {
    const o = toOwed(row);
    if (o !== null) out.push({ groupIdB64: o.groupIdB64, epochAfter: o.epochAfter });
  }
  return out;
}

/** What the send needs from the app. */
export interface GapSendPorts {
  now(): number;
  /** This device's identity in the conversation (its leaf signs). */
  identity(): WasmIdentity | null;
  /** The replacement landed: re-send the growth-time group state. */
  landed(group: WasmGroup): void;
  /** When this device first saw the leaf in the group's roster. */
  leafFirstSeenMs(groupIdB64: string, leafKeyB64: string): number | undefined;
  /** The last message this device decrypted from the leaf. */
  leafLastWriteMs(groupIdB64: string, leafKeyB64: string): number | undefined;
  /** The core's `gapHistoryStart`: `null` when nothing may be sent. */
  gapStart(claimedMs: number, leafFirstSeenMs: number | undefined, leafLastWriteMs: number | undefined): number | null;
  /** Seal the gap's rows from `fromMs` on to `gapKey`, signed by
   *  `readmitter` (`null`: nothing held). */
  build(group: WasmGroup, readmitter: WasmIdentity, gapKey: Uint8Array, fromMs: number): Promise<Uint8Array | null>;
  /** The core's `gapHistoryRendezvous(gapKey)`. */
  rendezvous(gapKey: Uint8Array): Uint8Array;
  publish(rendezvous: Uint8Array, sealed: Uint8Array): Promise<void>;
}

/** What happened to one owed send. */
export type GapSendOutcome = "sent" | "waiting" | "dropped" | "no-start";

/**
 * A poll of `group` applied: for each owed send of the group, send the bundle
 * once the replacement landed (the group stands at or past the epoch it
 * created; nothing depends on the exact epoch any more), drop it when stale.
 * The rows start at the core's clamp of the request's claim; when the core
 * names no start, nothing is sent. Never throws.
 */
export async function afterGroupPollForGapHistory(group: WasmGroup, ports: GapSendPorts): Promise<GapSendOutcome[]> {
  let groupIdB64: string;
  let epoch: bigint;
  try {
    groupIdB64 = base64Encode(group.groupId);
    epoch = group.epoch;
  } catch {
    return [];
  }
  const out: GapSendOutcome[] = [];
  let landedSignalled = false;
  // The group's rows are claimed (removed from the store) before the first
  // wait, so two poll paths never send one bundle twice.
  const all = loadOwedGapSends();
  const mine = all.filter((o) => o.groupIdB64 === groupIdB64);
  if (mine.length === 0) return out;
  const keep = all.filter((o) => o.groupIdB64 !== groupIdB64);
  const due: Owed[] = [];
  for (const row of mine) {
    const entry = toOwed(row);
    if (entry === null || ports.now() - entry.notedAtMs > GAP_OWED_TTL_MS) {
      out.push("dropped");
      continue;
    }
    if (epoch < entry.epochAfter) {
      keep.push(row);
      out.push("waiting");
      continue;
    }
    due.push(entry);
  }
  saveOwedGapSends(keep);
  for (const entry of due) {
    if (!landedSignalled) {
      landedSignalled = true;
      try {
        ports.landed(group);
      } catch {
        /* the growth re-sends are best-effort */
      }
    }
    const identity = ports.identity();
    if (identity === null) {
      out.push("dropped");
      continue;
    }
    try {
      const fromMs = ports.gapStart(
        entry.gapFromMs,
        ports.leafFirstSeenMs(groupIdB64, entry.leafKeyB64),
        ports.leafLastWriteMs(groupIdB64, entry.leafKeyB64),
      );
      if (fromMs === null) {
        // eslint-disable-next-line no-console
        console.info("chat re-entry: missed messages not sent: no start");
        out.push("no-start");
        continue;
      }
      const sealed = await ports.build(group, identity, entry.gapKey, fromMs);
      if (sealed === null) {
        out.push("dropped");
        continue;
      }
      await ports.publish(ports.rendezvous(entry.gapKey), sealed);
      // eslint-disable-next-line no-console
      console.info("chat re-entry: missed messages sent");
      out.push("sent");
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`chat re-entry: missed messages not sent: ${err instanceof Error ? err.name : "error"}`);
      out.push("dropped");
    }
  }
  return out;
}

// ---------- the device that came back keeps looking (live walk 2026-11-19) --

/**
 * Whether a gap look is due at `nowMs`: dropped once the retention passed
 * since the device came back, held for {@link GAP_HISTORY_LOOK_INTERVAL_MS}
 * after the last look. A clock that moved back counts as due.
 */
export function gapLookStep(
  look: { readonly gapToMs: number; readonly lastLookMs: number },
  nowMs: number,
): "look" | "wait" | "drop" {
  if (nowMs - look.gapToMs > GAP_HISTORY_LOOK_RETENTION_MS) return "drop";
  if (look.lastLookMs > 0 && nowMs >= look.lastLookMs && nowMs - look.lastLookMs < GAP_HISTORY_LOOK_INTERVAL_MS) {
    return "wait";
  }
  return "look";
}

/** What one filed bundle showed (`applyGapHistoryBundle`). */
export interface FiledGapHistory {
  readonly applied: number;
  readonly complete: boolean;
  readonly earliestMs: number | undefined;
  readonly rows: readonly PersistedMessage[];
}

/** What a look needs from the app. */
export interface GapLookPorts {
  now(): number;
  /** The account's device key, or `null`. */
  deviceKey(): Promise<DeviceCipher | null>;
  /** The rendezvous a gap secret's history is posted to
   *  (`gapHistoryRendezvous(gapPublicKey(secret))`). */
  rendezvousOf(secret: Uint8Array): Uint8Array;
  /** One page of the rendezvous: the rows after row id `after`, at most
   *  `limit`, oldest first, and whether more follow; `null` when the read
   *  failed. */
  readPage(rendezvous: Uint8Array, after: number, limit: number): Promise<GapLookPage | null>;
  /** Open and file one row; `null` when it is not the bundle, does not open,
   *  or its signer is not one the joined group accepts (the bundle is then
   *  dropped). `route` is the request's. */
  apply(secret: Uint8Array, sealed: Uint8Array, groupIdB64: string, route: ReentryRouteName | undefined): Promise<FiledGapHistory | null>;
  /** The bundle was filed: narrow the gap line ending at `gapToMs`, show the rows. */
  received(groupIdB64: string, gapToMs: number, filed: FiledGapHistory): void;
}

/** One page of a gap rendezvous (`GapLookPorts.readPage`). */
export interface GapLookPage {
  readonly rows: readonly { readonly id: number; readonly payload: Uint8Array }[];
  readonly hasMore: boolean;
}

/** Rows asked for per page of a gap rendezvous (the DS's own default). */
export const GAP_LOOK_PAGE_SIZE = 100;
/** Pages read per rendezvous per look: 1000 rows. Chosen, not measured. Anyone
 *  who saw the request can write rows there, so the look reads past them
 *  instead of stopping at the first page. */
export const GAP_LOOK_PAGE_CAP = 10;

/**
 * Every row of one gap rendezvous, oldest first: page after page from row 0,
 * at most {@link GAP_LOOK_PAGE_CAP} pages. `null` when the first page could
 * not be read; a later page that fails ends the read with what was read.
 */
export async function readGapRendezvous(
  ports: Pick<GapLookPorts, "readPage">,
  rendezvous: Uint8Array,
): Promise<Uint8Array[] | null> {
  const rows: Uint8Array[] = [];
  let after = 0;
  for (let page = 0; page < GAP_LOOK_PAGE_CAP; page += 1) {
    const read = await ports.readPage(rendezvous, after, GAP_LOOK_PAGE_SIZE).catch(() => null);
    if (read === null) return page === 0 ? null : rows;
    for (const row of read.rows) rows.push(row.payload);
    const last = read.rows[read.rows.length - 1];
    if (!read.hasMore || last === undefined || last.id <= after) break;
    after = last.id;
  }
  return rows;
}

/** Groups whose look is running now. */
const looking = new Set<string>();

/**
 * The device came back: keep looking for the gap's messages (durable). The
 * gap secrets are the ones the request record holds, already sealed under
 * the device key. Returns `false` when nothing could be stored.
 */
export function recordGapLook(input: {
  readonly groupIdB64: string;
  readonly secrets: readonly SealedGapSecret[];
  readonly gapFromMs: number;
  readonly gapToMs: number;
  readonly route?: ReentryRouteName;
}): boolean {
  if (input.secrets.length === 0) return false;
  const look: StoredGapLook = {
    secrets: [...input.secrets],
    gapFromMs: input.gapFromMs,
    gapToMs: input.gapToMs,
    lastLookMs: 0,
    ...(input.route === undefined ? {} : { route: input.route }),
  };
  return saveGapLook(input.groupIdB64, look);
}

/**
 * One look for the gap's messages of `groupIdB64`, if one is due: a read of
 * every page of each request's rendezvous (up to {@link GAP_LOOK_PAGE_CAP});
 * rows are tried newest first, and the first row that opens and whose
 * signer the group accepts is filed and the look ends. A failed read, no
 * row, or no row that is kept keeps the look for the next one. Never throws.
 */
export async function lookForGapHistory(
  groupIdB64: string,
  ports: GapLookPorts,
): Promise<"absent" | "busy" | "wait" | "dropped" | "none" | "received"> {
  if (looking.has(groupIdB64)) return "busy";
  const look = gapLookFor(groupIdB64);
  if (look === undefined) return "absent";
  const nowMs = ports.now();
  const step = gapLookStep(look, nowMs);
  if (step === "drop") {
    deleteGapLook(groupIdB64);
    // eslint-disable-next-line no-console
    console.info("chat re-entry: gap look dropped: retention passed");
    return "dropped";
  }
  if (step === "wait") return "wait";
  looking.add(groupIdB64);
  try {
    // The throttle holds even when this look fails half-way.
    saveGapLook(groupIdB64, { ...look, lastLookMs: nowMs });
    const deviceKey = await ports.deviceKey();
    if (deviceKey === null) return "none";
    const secrets: Uint8Array[] = [];
    for (const sealed of look.secrets) {
      try {
        secrets.push(new Uint8Array(await deviceKey.decrypt(base64Decode(sealed.ivB64), base64Decode(sealed.sealedSecretB64))));
      } catch {
        /* sealed under a key this device no longer holds: it never opens */
      }
    }
    if (secrets.length === 0) {
      deleteGapLook(groupIdB64);
      return "dropped";
    }
    for (const secret of [...secrets].reverse()) {
      let rendezvous: Uint8Array;
      try {
        rendezvous = ports.rendezvousOf(secret);
      } catch {
        continue;
      }
      const rows = await readGapRendezvous(ports, rendezvous);
      if (rows === null || rows.length === 0) continue;
      for (const row of [...rows].reverse()) {
        const filed = await ports.apply(secret, row, groupIdB64, look.route).catch(() => null);
        if (filed === null) continue;
        deleteGapLook(groupIdB64);
        ports.received(groupIdB64, look.gapToMs, filed);
        // eslint-disable-next-line no-console
        console.info(`chat re-entry: missed messages received: rows=${filed.applied}`);
        return "received";
      }
    }
    return "none";
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(`chat re-entry: gap look failed: ${err instanceof Error ? err.name : "error"}`);
    return "none";
  } finally {
    looking.delete(groupIdB64);
  }
}

/** Installed by the re-entry runtime: a look for `groupIdB64` now, if due. */
let lookRequester: ((groupIdB64: string) => void) | null = null;

/** Install (or, with `null`, remove) the look requester. */
export function installGapLookRequester(next: ((groupIdB64: string) => void) | null): void {
  lookRequester = next;
}

/** The chat was opened: look for its gap's messages, if a look is due. */
export function requestGapHistoryLook(groupIdB64: string): void {
  try {
    lookRequester?.(groupIdB64);
  } catch {
    /* a look never fails the caller */
  }
}
