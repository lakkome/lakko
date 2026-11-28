// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * On-device persistence for the contacts UI (1.16d3d).
 *
 * The local "address book" of the SIGNED-IN ACCOUNT: the accepted contacts
 * (the public keys we need to reach each one, plus the viewer's local rename)
 * and the per-contact account-pubkey pins (below). Bytes-only; no secrets.
 *
 * ## One row per account (2026-09-30)
 *
 * The row is keyed by the signed-in account id,
 * `lakko.contacts.v3.<accountIdB64>`, like the inbox
 * (`inbox-store.ts`) and the sent-requests list. It used to be one
 * device-wide row, so a second account signed in on the same browser listed,
 * messaged and searched the first account's contacts as its own (seen live:
 * account Eve, on a browser where Cara had been, treated Cara's contact Dan
 * as hers).
 *
 * Which account a call means: the caller may pass the account id as the
 * trailing `owner` argument; otherwise the account bound by
 * {@link bindContactsAccount} is used. `AuthContext` binds the account the
 * moment a session lands and unbinds it on sign-out. With no account at all,
 * a read is empty and a write is a no-op: there is no shared fallback row.
 * Log out keeps the row for the next sign-in of the same account; Forget
 * device drops it ({@link clearAccountContacts}).
 *
 * The DS mailbox cursor that used to share this row lives in the per-account
 * inbox store now.
 *
 * The values are not secret (public keys are public by definition), so they
 * live in `localStorage`.
 *
 * ## G-4 (ADR-0016): per-account account-pubkey PIN store
 *
 * The second persisted piece is the **account-pubkey
 * pin** map. A peer's discovery identity bundle carries the
 * peer's AccountIdentity Ed25519 `signature_pubkey`; that key
 * **rotates** on recover-from-words. ADR-0016 correctness rule
 * #3 requires the pin be keyed on the stable **account_id**, not
 * on the (mutable) pubkey — keying on the pubkey forks the
 * contact silently when the key changes, defeating the whole
 * supersession-proof bridge.
 *
 * Each pin records:
 *   * `pinnedSignaturePubkeyB64` — the currently-trusted
 *     AccountIdentity pubkey for this account (mutable; advanced
 *     only via a verified supersession proof on the fetch path).
 *   * `rotationMarker` — a **monotonic** marker (ADR-0016 rule
 *     #4): the `expires_at` unix-seconds of the last-accepted
 *     proof, default `0` for a TOFU-pin. A re-pin that would move
 *     the marker backwards is refused, blunting within-window
 *     rollback.
 *
 * The pin bytes are NOT secret (a pubkey is public), but the
 * *trust* decision they encode is security-critical — see the
 * fetch-path verify in `contacts/client.ts`. A pin is the VIEWING account's
 * trust decision, so it lives in that account's row too.
 */
import { base64Encode } from "../auth/account-id";
import { adoptOneToOnePeer, type OneToOnePeer } from "./adopt-one-to-one";
import { afterProvenRotation, afterReset, type AccountKeys } from "./predecessor";

/** Prefix of the per-account row; the signed-in account id follows. */
const KEY_PREFIX = "lakko.contacts.v3.";
/** The retired device-wide rows. Never read; removed by Forget device so a
 *  wipe leaves nothing of them behind. */
const RETIRED_DEVICE_WIDE_KEYS = ["lakko.contacts.v2", "lakko.contacts.v1"] as const;

/** The account a call without an explicit `owner` reads and writes. */
let boundAccountIdB64: string | null = null;

/** Why the accepted list a listener sees may have changed. */
export type AcceptedChange =
  /** An accept, a rename, a removal or a wipe of the bound account's list. */
  | "edited"
  /** A different account (or none) is bound now: re-read, nothing was
   *  written. */
  | "account-changed";

/** A contact the user has explicitly accepted. */
export interface AcceptedContact {
  /** Sender's Ed25519 identity public key, base64-encoded —
   *  the same value `ContactRequest.fromPubkey` carries. */
  signaturePubkeyB64: string;
  /** Sender's X25519 HPKE pubkey, base64-encoded. Stored so we
   *  can later compose a reply envelope without round-tripping
   *  the discovery directory each time. Optional in v1 because
   *  the receive path doesn't always have it (it lives in the
   *  identity bundle, not in the ContactRequest itself). */
  x25519PubkeyB64?: string;
  /** The viewer's LOCAL RENAME of this contact (ADR-0048 §5, precedence
   *  1). Empty string = no rename, which is the value on accept: the name
   *  shown then comes from the contact's own profile, their verified
   *  username, or the key fingerprint, via `profile/resolver.ts`. Never sent
   *  anywhere. */
  displayName: string;
  /** When the user accepted the request (ms since epoch). */
  acceptedAtMs: number;
  /** The peer's 16-byte account_id as lower-case unseparated hex —
   *  captured at accept (the contact flow already learns it via
   *  discovery). Founding a one-to-one and the backup read it. (It once
   *  resolved the ADR-0021 SFU peer tag; ADR-0078 section 2 removed
   *  that.) Optional — older contacts lack it. */
  accountIdHex?: string;
  /** base64(group_id) of the 1×1 chat created with this contact at
   *  accept time (ADR-0021 follow-up). Links the chat back to this
   *  contact so the in-call roster + chat label resolve to the
   *  user-chosen `displayName`. Optional — set only on the accept side
   *  (where the contact + the group are created together); a chat
   *  joined from a Welcome has no contact link and falls back to the
   *  group label. */
  groupIdB64?: string;
  /** ADR-0073 R.2: the account keys this contact held BEFORE a reset this
   *  device accepted, base64, oldest first. The row is re-keyed to the new
   *  key at the re-pin (so the person keeps their rename, folders, pins,
   *  mute and chat link); the old keys stay here as DISPLAY ALIASES for
   *  stored history only (C-68). A previous key is never accepted for new
   *  traffic, a new pin, a role or a send. Never sent anywhere. */
  previousSignaturePubkeysB64?: string[];
}

/**
 * A pinned AccountIdentity pubkey for one peer, keyed on the
 * peer's stable `account_id` (G-4, ADR-0016 rule #3). The map
 * value, not the key, holds the (mutable) pubkey.
 */
export interface AccountPubkeyPin {
  /** The currently-trusted AccountIdentity Ed25519 pubkey for
   *  this account, base64-encoded. Advanced only via a verified
   *  supersession proof on the fetch path. */
  pinnedSignaturePubkeyB64: string;
  /** Monotonic rotation marker (ADR-0016 rule #4): the
   *  `expires_at` unix-seconds of the last-accepted proof, or
   *  `0` for a plain TOFU-pin. A re-pin that would lower this is
   *  refused. */
  rotationMarker: number;
  /** KT-log S3c: the last transparency-log inclusion verdict
   *  observed for this account on the identity-fetch path. The
   *  log check is the equivocation-detection layer ON TOP of the
   *  supersession/pin gate (ADR-0016); this records the latest
   *  best-effort outcome so the UI can surface it. Absent on a pin
   *  written before S3c (or by a path that did not run the check). */
  logStatus?: LogVerifyStatus;
  /** KT-log S4: the `is_reset` marker the transparency log's
   *  `/latest` entry carried for the CURRENTLY-PINNED key, when a
   *  latest entry was read (`confirmed` / `mismatch`). `true` means
   *  the log records an owner-initiated identity RESET (ADR-0017
   *  lost-everything fresh-start); `false` means a non-reset entry;
   *  absent when no latest entry was read (or the pin predates S4).
   *  Stored next to `logStatus` so the UI can surface the
   *  reset-vs-compromise distinction. */
  isReset?: boolean;
  /** G-30 #2 (ADR-0019): the transparency-log VERSION at which the
   *  currently-pinned key was chain-confirmed (the `/latest` entry's
   *  per-account-key `version`). Written ONLY on a `confirmed`
   *  rotation-chain verdict (`verifyRotationChain`). The next
   *  rotation's chain proof must reference a STRICTLY-NEWER logged
   *  version, so persisting it gives the chain verifier its
   *  downgrade-protection floor. Absent on a pin that has never been
   *  chain-confirmed (a TOFU pin, or one written before G-30 #2). */
  pinnedLogVersion?: number;
  /** ADR-0073 R.2: the `key_generation` this device last ACCEPTED for the
   *  account. The baseline is the bundle's generation at the first pin; an
   *  accepted reset raises it. A reset attestation is accepted only at a
   *  generation strictly above this, so an old one can never be replayed. */
  acceptedKeyGeneration?: number;
  /** ADR-0073 R.2: every account key this device pinned for the account
   *  before a key change, base64, oldest first. Display aliases for stored
   *  history and the lookup of old leaves in a group; never trusted for
   *  anything new. */
  previousKeysB64?: string[];
  /** ADR-0073 R.5 / R.6: the keys an ACCEPTED RESET retired (the pin it
   *  replaced and that pin's proven predecessors), base64. Rows and claims
   *  from leaves bound to these are dropped; a proven predecessor of the
   *  CURRENT key is never in this list. */
  supersededKeysB64?: string[];
  /** ADR-0073 R.7 case 1: the `signed_at` (ms) of the last accepted reset's
   *  attestation: the moment after which this account's messages in the old
   *  one-to-one reached only removed devices. */
  resetSignedAtMs?: number;
  /** ADR-0074 section 3.5: every key this account had before a PROVEN
   *  rotation (a re-pin authorised by a supersession proof), base64, oldest
   *  first. The conversation leaves that exist stay bound to these, so they
   *  name the same person (`predecessor.ts`); nothing is evicted and nothing
   *  is shown. Emptied by an accepted reset: a reset makes every
   *  predecessor, proven or not, a superseded key. */
  provenPredecessorsB64?: string[];
  /** ADR-0101 section 10 (the peer's check 1): the highest seal
   *  announcement `seq` this device has seen for the account, as a decimal
   *  string (a u64). Never lowered. A reset whose previous announcement is
   *  below it is not sealed. */
  sealSeqFloor?: string;
  /** ADR-0101 section 11 (UX 16.11): the account's keys changed by a reset
   *  this device could NOT confirm (an unsealed reset), and the person has
   *  not decided about the new account yet. The pin still names the old
   *  key: nothing was re-pinned, moved or returned. Cleared when the person
   *  accepts a request from the new key ({@link adoptStartedOver}) or the
   *  reset later proves sealed ({@link acceptReset}). */
  startedOver?: StartedOverRecord;
  /** Every account key this account held before an unsealed reset, base64
   *  (the pin it left behind and that pin's proven predecessors). A
   *  one-to-one whose other person is bound to one of these is closed
   *  (`started-over.ts`). Kept after the person accepted the new account,
   *  so the old conversation stays closed. Never trusted for anything. */
  startedOverKeysB64?: string[];
}

/** An unsealed reset this device noticed (ADR-0101 section 11). */
export interface StartedOverRecord {
  /** The key the directory publishes for the account now. */
  newKeyB64: string;
  /** The attested generation of the reset. */
  keyGeneration: number;
  /** The attestation's `signed_at`, unix milliseconds. */
  signedAtMs: number;
  /** When this device noticed, unix milliseconds. */
  noticedAtMs: number;
}

/** Mirror of `transparency/verify.ts::LogVerifyStatus` — duplicated
 *  here (rather than imported) to keep the persistence layer free of a
 *  dependency on the transparency module + its wasm loader. Kept in
 *  lock-step with that union; a value the guard does not recognise is
 *  dropped on read. */
export type LogVerifyStatus =
  | "confirmed"
  | "absent"
  | "rollback"
  | "mismatch"
  | "unreachable"
  | "off";

/** The set of valid `LogVerifyStatus` strings, for the read-path
 *  narrowing guard. */
const LOG_VERIFY_STATUSES: ReadonlySet<string> = new Set([
  "confirmed",
  "absent",
  "rollback",
  "mismatch",
  "unreachable",
  "off",
]);

interface PersistedV3 {
  v: 3;
  accepted: AcceptedContact[];
  /** G-4 (ADR-0016): `account_id` (base64) → account-pubkey pin.
   *  A plain object, not a `Map`, so it survives `JSON.stringify`
   *  round-trips like the rest of the record. */
  pins: Record<string, AccountPubkeyPin>;
  /** Account keys (base64) the person removed from their contacts
   *  (`docs/ux/conversation-info.md`: removal is local and leaves the
   *  conversation). The one-to-one adoption rule skips them; an explicit add
   *  (Accept, an invite link, starting a chat, a rename) takes a key out
   *  again. Absent on rows written before it existed. */
  removed?: string[];
}

/** Narrowing guard for one persisted `AcceptedContact` — shared
 *  by the v2 and v1-migration read paths. */
function isAcceptedContact(a: unknown): a is AcceptedContact {
  return (
    typeof a === "object" &&
    a !== null &&
    typeof (a as AcceptedContact).signaturePubkeyB64 === "string" &&
    typeof (a as AcceptedContact).displayName === "string" &&
    typeof (a as AcceptedContact).acceptedAtMs === "number" &&
    // Optional ADR-0021 fields — validated when present, tolerated when
    // absent (pre-ADR-0021 contacts).
    ((a as AcceptedContact).accountIdHex === undefined ||
      typeof (a as AcceptedContact).accountIdHex === "string") &&
    ((a as AcceptedContact).groupIdB64 === undefined ||
      typeof (a as AcceptedContact).groupIdB64 === "string") &&
    ((a as AcceptedContact).previousSignaturePubkeysB64 === undefined ||
      isStringArray((a as AcceptedContact).previousSignaturePubkeysB64))
  );
}

function isStringArray(v: unknown): v is string[] {
  return Array.isArray(v) && v.every((s) => typeof s === "string");
}

/** Narrowing guard for one persisted `AccountPubkeyPin`. The optional
 *  `logStatus` (KT-log S3c) is validated when present and dropped when
 *  it is an unrecognised string — a forward-compat / tamper guard. */
function isAccountPubkeyPin(p: unknown): p is AccountPubkeyPin {
  if (
    typeof p !== "object" ||
    p === null ||
    typeof (p as AccountPubkeyPin).pinnedSignaturePubkeyB64 !== "string" ||
    typeof (p as AccountPubkeyPin).rotationMarker !== "number"
  ) {
    return false;
  }
  const ls = (p as AccountPubkeyPin).logStatus;
  if (ls !== undefined && (typeof ls !== "string" || !LOG_VERIFY_STATUSES.has(ls))) {
    // A present-but-junk logStatus is dropped (the structural fields
    // are still valid, so we keep the pin itself).
    delete (p as { logStatus?: unknown }).logStatus;
  }
  // KT-log S4: validate the optional `isReset` marker. A present-but-
  // non-boolean value is dropped (forward-compat / tamper guard); the
  // pin itself stays valid.
  const ir = (p as AccountPubkeyPin).isReset;
  if (ir !== undefined && typeof ir !== "boolean") {
    delete (p as { isReset?: unknown }).isReset;
  }
  // G-30 #2 (ADR-0019): validate the optional `pinnedLogVersion`. A
  // present-but-junk value (non-number, non-finite, or negative) is
  // dropped (forward-compat / tamper guard) while the pin itself stays
  // valid — a missing chain-version floor degrades safely to 0.
  const lv = (p as AccountPubkeyPin).pinnedLogVersion;
  if (
    lv !== undefined &&
    (typeof lv !== "number" || !Number.isFinite(lv) || lv < 0)
  ) {
    delete (p as { pinnedLogVersion?: unknown }).pinnedLogVersion;
  }
  const cg = (p as AccountPubkeyPin).acceptedKeyGeneration;
  if (cg !== undefined && (typeof cg !== "number" || !Number.isFinite(cg) || cg < 0)) {
    delete (p as { acceptedKeyGeneration?: unknown }).acceptedKeyGeneration;
  }
  const pk = (p as AccountPubkeyPin).previousKeysB64;
  if (pk !== undefined && !isStringArray(pk)) {
    delete (p as { previousKeysB64?: unknown }).previousKeysB64;
  }
  const sk = (p as AccountPubkeyPin).supersededKeysB64;
  if (sk !== undefined && !isStringArray(sk)) {
    delete (p as { supersededKeysB64?: unknown }).supersededKeysB64;
  }
  const rs = (p as AccountPubkeyPin).resetSignedAtMs;
  if (rs !== undefined && (typeof rs !== "number" || !Number.isFinite(rs))) {
    delete (p as { resetSignedAtMs?: unknown }).resetSignedAtMs;
  }
  // ADR-0074: a junk predecessor list is dropped, which reads as "no proven
  // predecessor" (the fail-closed direction).
  const pp = (p as AccountPubkeyPin).provenPredecessorsB64;
  if (pp !== undefined && !isStringArray(pp)) {
    delete (p as { provenPredecessorsB64?: unknown }).provenPredecessorsB64;
  }
  // ADR-0101: a junk floor reads as 0 (then only the held key's signature
  // decides); a junk started-over record or key list is dropped.
  const sf = (p as AccountPubkeyPin).sealSeqFloor;
  if (sf !== undefined && (typeof sf !== "string" || !/^[0-9]{1,20}$/.test(sf))) {
    delete (p as { sealSeqFloor?: unknown }).sealSeqFloor;
  }
  const so = (p as AccountPubkeyPin).startedOver as unknown;
  if (so !== undefined && !isStartedOverRecord(so)) {
    delete (p as { startedOver?: unknown }).startedOver;
  }
  const sk2 = (p as AccountPubkeyPin).startedOverKeysB64;
  if (sk2 !== undefined && !isStringArray(sk2)) {
    delete (p as { startedOverKeysB64?: unknown }).startedOverKeysB64;
  }
  return true;
}

function isStartedOverRecord(v: unknown): v is StartedOverRecord {
  if (typeof v !== "object" || v === null) return false;
  const r = v as StartedOverRecord;
  return (
    typeof r.newKeyB64 === "string" &&
    r.newKeyB64.length > 0 &&
    typeof r.keyGeneration === "number" &&
    Number.isFinite(r.keyGeneration) &&
    typeof r.signedAtMs === "number" &&
    Number.isFinite(r.signedAtMs) &&
    typeof r.noticedAtMs === "number" &&
    Number.isFinite(r.noticedAtMs)
  );
}

function emptyRecord(): PersistedV3 {
  return { v: 3, accepted: [], pins: {} };
}

/**
 * Bind the signed-in account whose contacts the owner-less calls read and
 * write; `null` unbinds (signed out). Listeners are told, so a view showing
 * the list re-reads it for the new account.
 */
export function bindContactsAccount(accountIdB64: string | null): void {
  const next = accountIdB64 === null || accountIdB64.length === 0 ? null : accountIdB64;
  if (next === boundAccountIdB64) return;
  boundAccountIdB64 = next;
  notifyAcceptedChanged("account-changed");
}

/** The account the owner-less calls use, or `null` when none is bound. */
export function boundContactsAccount(): string | null {
  return boundAccountIdB64;
}

function ownerOf(owner: string | undefined): string | null {
  if (owner !== undefined) return owner.length === 0 ? null : owner;
  return boundAccountIdB64;
}

function storageKey(owner: string): string {
  return KEY_PREFIX + owner;
}

function load(owner: string | null): PersistedV3 {
  if (owner === null || typeof localStorage === "undefined") {
    return emptyRecord();
  }
  let raw: string | null;
  try {
    raw = localStorage.getItem(storageKey(owner));
  } catch {
    return emptyRecord();
  }
  if (raw === null) return emptyRecord();
  try {
    const parsed = JSON.parse(raw) as Partial<PersistedV3>;
    if (parsed.v === 3 && Array.isArray(parsed.accepted)) {
      // Filter `pins` to structurally well-formed entries; a junk
      // value is dropped rather than mis-trusted.
      const pins: Record<string, AccountPubkeyPin> = {};
      const rawPins = parsed.pins;
      if (typeof rawPins === "object" && rawPins !== null) {
        for (const [accountIdB64, pin] of Object.entries(rawPins)) {
          if (isAccountPubkeyPin(pin)) {
            pins[accountIdB64] = pin;
          }
        }
      }
      const removed = Array.isArray(parsed.removed)
        ? parsed.removed.filter((k): k is string => typeof k === "string")
        : [];
      return {
        v: 3,
        accepted: parsed.accepted.filter(isAcceptedContact),
        pins,
        removed,
      };
    }
  } catch {
    // Fall through to the empty default.
  }
  return emptyRecord();
}

/** Write one account's row. A call with no account writes nothing: there is
 *  no shared row to fall back to. Returns whether the row was written. */
function save(owner: string | null, record: PersistedV3): boolean {
  if (owner === null || typeof localStorage === "undefined") return false;
  localStorage.setItem(storageKey(owner), JSON.stringify(record));
  return true;
}

/** Read the accepted-contacts list of `owner` (default: the bound
 *  account). Empty with no account. */
export function loadAccepted(owner?: string): AcceptedContact[] {
  return load(ownerOf(owner)).accepted;
}

/**
 * Resolve the accepted contact behind a 1×1 chat by matching the chat
 * group's non-self member against the accepted list on the Ed25519
 * signature pubkey (ADR-0021). The caller passes the group members'
 * base64 signature pubkeys (`Group.members()[].signaturePublicKey`) and its
 * own base64 signature pubkey; the first member that is NOT self and IS a
 * known contact wins.
 *
 * This is the UNIVERSAL resolver: it works for every 1×1 chat regardless of
 * how the group was created (outbound `startGroupWithUsername`, an accepted
 * request, or a Welcome-joined group with no stored link), because the peer
 * identity is read live from the MLS group rather than captured at creation.
 * Returns `undefined` when no non-self member is a known contact.
 */
export function peerContactByMemberPubkeys(
  memberSigPubkeysB64: readonly string[],
  ownSigPubkeyB64: string | null,
  accepted: readonly AcceptedContact[],
  /** ADR-0073 R.2: a retired key still names the person
   *  ({@link loadKeyAliases}) in a conversation's older groups. */
  aliases: ReadonlyMap<string, string> = new Map(),
): AcceptedContact | undefined {
  for (const b64 of memberSigPubkeysB64) {
    if (b64 === ownSigPubkeyB64) continue;
    const contact = contactByAnyKey(b64, accepted, aliases);
    if (contact !== undefined) return contact;
  }
  return undefined;
}

const acceptedListeners = new Set<(change: AcceptedChange) => void>();

/** ADR-0048 — subscribe to changes of the accepted list (an accept, a rename,
 *  a removal, or another account being bound), so a view resolving names
 *  re-reads the local renames. Returns the unsubscribe. Same-page only. */
export function subscribeAcceptedChanged(
  listener: (change: AcceptedChange) => void,
): () => void {
  acceptedListeners.add(listener);
  return () => acceptedListeners.delete(listener);
}

function notifyAcceptedChanged(change: AcceptedChange): void {
  for (const l of acceptedListeners) {
    try {
      l(change);
    } catch {
      // A failing listener must not break the write that triggered it.
    }
  }
}

/** ADR-0048 — the local renames of `owner` (default: the bound account),
 *  keyed by base64 signature key. Contacts with no rename are absent. */
export function loadRenames(owner?: string): Map<string, string> {
  const out = new Map<string, string>();
  for (const c of loadAccepted(owner)) {
    const name = c.displayName.trim();
    if (name.length > 0) out.set(c.signaturePubkeyB64, name);
  }
  return out;
}

/** Whether two account keys are one person across proven rotations: both on
 *  one pin, as its current key or a proven predecessor (ADR-0074 section 3.7).
 *  Contact rows are matched by this, so a person is one row whichever of their
 *  keys an event names. */
function provenSamePerson(state: {
  readonly pins: Readonly<Record<string, AccountPubkeyPin>>;
}): (a: string, b: string) => boolean {
  const chains = Object.values(state.pins).map(
    (pin) => new Set([pin.pinnedSignaturePubkeyB64, ...(pin.provenPredecessorsB64 ?? [])]),
  );
  return (a, b) => a === b || chains.some((keys) => keys.has(a) && keys.has(b));
}

/** The row `incoming` makes of a person who already has `held`. Under one key
 *  it replaces the row, as it always did; under another key of theirs the row
 *  keeps what it knew and the key that no longer names it stays an alias. */
function mergedRow(held: AcceptedContact, incoming: AcceptedContact): AcceptedContact {
  if (held.signaturePubkeyB64 === incoming.signaturePubkeyB64) return incoming;
  const aliases = [
    ...(held.previousSignaturePubkeysB64 ?? []),
    ...(incoming.previousSignaturePubkeysB64 ?? []),
    held.signaturePubkeyB64,
  ];
  return {
    ...held,
    ...incoming,
    displayName: incoming.displayName.length > 0 ? incoming.displayName : held.displayName,
    previousSignaturePubkeysB64: [...new Set(aliases)],
  };
}

/** Append (or replace by `signaturePubkeyB64`) an accepted
 *  contact in `owner`'s list (default: the bound account) and persist.
 *  Returns the new list; with no account nothing is written and the result
 *  is empty. */
export function upsertAccepted(contact: AcceptedContact, owner?: string): AcceptedContact[] {
  const account = ownerOf(owner);
  if (account === null) return [];
  const current = load(account);
  const onePerson = provenSamePerson(current);
  const existing = current.accepted.findIndex((a) =>
    onePerson(a.signaturePubkeyB64, contact.signaturePubkeyB64),
  );
  const next =
    existing === -1
      ? [...current.accepted, contact]
      : current.accepted.map((a, i) => (i === existing ? mergedRow(a, contact) : a));
  // An explicit add is the person's own act: a removal before it no longer
  // keeps the adoption rule away.
  const removed = (current.removed ?? []).filter((k) => !onePerson(k, contact.signaturePubkeyB64));
  save(account, { ...current, accepted: next, removed });
  notifyAcceptedChanged("edited");
  return next;
}

/** A person this account has a one-to-one conversation with IS a contact
 *  (`adopt-one-to-one.ts`): apply the rule for one peer against the
 *  persisted list, write only when it changes something, and tell the
 *  listeners. Returns whether a write happened. */
export function adoptOneToOne(
  peer: OneToOnePeer,
  owner?: string,
  /** Whether a row's recorded chat id is still that person's one-to-one on
   *  this device; a link that is not is replaced (`adopt-one-to-one.ts`). */
  linkHolds?: (groupIdB64: string, row: AcceptedContact) => boolean,
): boolean {
  const account = ownerOf(owner);
  if (account === null) return false;
  const current = load(account);
  // A person removed from the list stays removed although the conversation
  // remains (seen on test.lakko.me 2026-10-24: a removed contact came back
  // on the next page load).
  const onePerson = provenSamePerson(current);
  if ((current.removed ?? []).some((k) => onePerson(k, peer.signatureKey))) return false;
  const next = adoptOneToOnePeer(current.accepted, peer, onePerson, linkHolds);
  if (next === null) return false;
  save(account, { ...current, accepted: next });
  notifyAcceptedChanged("edited");
  return true;
}

/** Put a person the viewer removed back on the contact list, with their
 *  one-to-one (`docs/ux/conversation-info.md` section 2.1, amended
 *  2026-10-24): the removal is forgotten and the one-to-one rule applied.
 *  Local; sends nothing. Returns whether a write happened. */
export function restoreRemovedContact(peer: OneToOnePeer, owner?: string): boolean {
  const account = ownerOf(owner);
  if (account === null) return false;
  const current = load(account);
  const onePerson = provenSamePerson(current);
  const wasRemoved = (current.removed ?? []).some((k) => onePerson(k, peer.signatureKey));
  const removed = (current.removed ?? []).filter((k) => !onePerson(k, peer.signatureKey));
  const next = adoptOneToOnePeer(current.accepted, peer, onePerson);
  if (!wasRemoved && next === null) return false;
  save(account, { ...current, accepted: next ?? current.accepted, removed });
  notifyAcceptedChanged("edited");
  return true;
}

/** Whether the viewer removed this person from their contact list. */
export function isRemovedContact(signaturePubkeyB64: string, owner?: string): boolean {
  const account = ownerOf(owner);
  if (account === null) return false;
  return (load(account).removed ?? []).includes(signaturePubkeyB64);
}

/** Remove an accepted contact by its signature pubkey. No-op
 *  when there's no match or no account. */
export function removeAccepted(signaturePubkeyB64: string, owner?: string): AcceptedContact[] {
  const account = ownerOf(owner);
  if (account === null) return [];
  const current = load(account);
  const next = current.accepted.filter(
    (a) => a.signaturePubkeyB64 !== signaturePubkeyB64,
  );
  const removed = current.removed ?? [];
  save(account, {
    ...current,
    accepted: next,
    removed: removed.includes(signaturePubkeyB64) ? removed : [...removed, signaturePubkeyB64],
  });
  notifyAcceptedChanged("edited");
  return next;
}

/** Forget device: drop `owner`'s row (default: the bound account) and the
 *  retired device-wide rows. Another account's row on this browser is left
 *  alone — it is that account's, and only its own Forget device drops it. */
export function clearAccountContacts(owner?: string): void {
  if (typeof localStorage === "undefined") return;
  const account = ownerOf(owner);
  if (account !== null) localStorage.removeItem(storageKey(account));
  for (const key of RETIRED_DEVICE_WIDE_KEYS) localStorage.removeItem(key);
  notifyAcceptedChanged("edited");
}

// ----------------------------------------------------------------------------
// G-4 (ADR-0016): account-pubkey PIN store
// ----------------------------------------------------------------------------

/**
 * Read the pinned AccountIdentity pubkey for a peer, keyed on the
 * peer's stable `account_id` (base64, standard alphabet).
 *
 * Returns `undefined` when no pin exists yet — the caller treats
 * that as the TOFU (trust-on-first-use) case and pins the
 * freshly-observed key. A present pin is what the caller compares
 * the fetched `signature_pubkey` against; a mismatch demands a
 * verified supersession proof (ADR-0016 rule #2, fail-closed).
 */
export function readPin(accountIdB64: string, owner?: string): AccountPubkeyPin | undefined {
  return load(ownerOf(owner)).pins[accountIdB64];
}

/** Outcome of a [`setPin`] call. `"set"` covers both a first-time
 *  TOFU-pin and a monotonic forward re-pin; `"rejected-backward"`
 *  signals the marker would have moved backwards (ADR-0016 rule
 *  #4) so nothing was written; `"no-account"` means no signed-in
 *  account was given or bound, so nothing was written either. */
export type SetPinResult = "set" | "rejected-backward" | "no-account";

/**
 * Set or update the account-pubkey pin for `accountIdB64`,
 * enforcing the monotonic-marker invariant (ADR-0016 rule #4).
 *
 * Semantics:
 *   * No existing pin → store `{ pubkey, marker }` (TOFU or first
 *     verified pin). Returns `"set"`.
 *   * Existing pin and `marker >= existing.rotationMarker` →
 *     overwrite the pubkey + advance the marker. Returns `"set"`.
 *     (A `==` marker permits re-pinning the *same* successor — an
 *     idempotent re-observation — without forking.)
 *   * Existing pin and `marker < existing.rotationMarker` →
 *     **refuse**, leave the pin untouched, return
 *     `"rejected-backward"`. This blunts a within-window rollback
 *     to an older successor.
 *
 * The pubkey is intentionally **mutable** for a given
 * `account_id`: a changed key is a *detected mutation*, not a new
 * contact (ADR-0016 rule #3). The trust gate that authorises the
 * mutation lives on the fetch path, not here.
 */
export function setPin(
  accountIdB64: string,
  pinnedSignaturePubkeyB64: string,
  rotationMarker: number,
  owner?: string,
  /** ADR-0074 section 3.5: `proven` is passed ONLY by the fail-closed gate,
   *  after a supersession proof by the pinned key verified for exactly this
   *  new key. The replaced key then becomes a proven predecessor. Any other
   *  re-pin records none. */
  options: {
    readonly proven?: boolean;
    /** ADR-0073 R.2: the fetched bundle's `key_generation`. Recorded as the
     *  accepted-generation baseline when the pin holds none; never lowers
     *  or replaces a held one (only {@link acceptReset} raises it). */
    readonly keyGeneration?: number;
    /** ADR-0101 section 10: the generation a PROVEN rotation moved the
     *  account to (the bundle's, confirmed by the attestation beside the
     *  proof). Raises the accepted generation when it is above the held
     *  one, so the next reset's seal is checked against the generation the
     *  account is really at. Only read together with `proven`. */
    readonly provenGeneration?: number;
  } = {},
): SetPinResult {
  const account = ownerOf(owner);
  if (account === null) return "no-account";
  const current = load(account);
  const existing = current.pins[accountIdB64];
  if (existing !== undefined && rotationMarker < existing.rotationMarker) {
    return "rejected-backward";
  }
  // Preserve any recorded `logStatus` (KT-log S3c) + `isReset` (KT-log
  // S4) across a re-pin — they are orthogonal, separately-written
  // fields, and a key rotation shouldn't silently drop the last log
  // verdict / reset marker. The fetch path re-records both right after
  // a re-pin, so a stale carried value is replaced immediately; we
  // carry it so a re-pin that does NOT re-run the log check (or one
  // whose check is inconclusive) doesn't blank a previously-known value.
  const carriedLogStatus = existing?.logStatus;
  const carriedIsReset = existing?.isReset;
  // G-30 #2 (ADR-0019): carry the chain-confirmed log version across a
  // re-pin too. It is a separately-written field (only `recordLogVersion`
  // touches it, on a `confirmed` chain verdict), so a re-pin must not
  // silently drop the downgrade floor; the fetch path re-records it right
  // after a confirmed rotation, so a stale carried value is replaced.
  const carriedLogVersion = existing?.pinnedLogVersion;
  // ADR-0073 R.2: the accepted generation travels with the account; the
  // first observation of a bundle's generation is its baseline. A re-pin to
  // a DIFFERENT key retires the old one into `previousKeysB64` (the old
  // leaves in shared groups still resolve to it).
  let carriedGeneration =
    existing?.acceptedKeyGeneration ??
    (options.keyGeneration !== undefined && Number.isSafeInteger(options.keyGeneration)
      ? options.keyGeneration
      : undefined);
  if (
    options.proven === true &&
    options.provenGeneration !== undefined &&
    Number.isSafeInteger(options.provenGeneration) &&
    options.provenGeneration > (carriedGeneration ?? 0)
  ) {
    carriedGeneration = options.provenGeneration;
  }
  const keyChanged =
    existing !== undefined && existing.pinnedSignaturePubkeyB64 !== pinnedSignaturePubkeyB64;
  const previousKeys = [...(existing?.previousKeysB64 ?? [])];
  if (keyChanged && existing !== undefined && !previousKeys.includes(existing.pinnedSignaturePubkeyB64)) {
    previousKeys.push(existing.pinnedSignaturePubkeyB64);
  }
  const carriedSuperseded = existing?.supersededKeysB64;
  const carriedResetSignedAt = existing?.resetSignedAtMs;
  // ADR-0074: the proven chain. A proven re-pin appends the replaced key; an
  // unchanged key carries the chain; a change that is NOT proven ends it.
  let provenPredecessors: string[] = [];
  if (existing !== undefined) {
    const held: AccountKeys = {
      currentKeyB64: existing.pinnedSignaturePubkeyB64,
      provenPredecessorsB64: existing.provenPredecessorsB64 ?? [],
    };
    const next = !keyChanged
      ? held
      : options.proven === true
        ? afterProvenRotation(held, pinnedSignaturePubkeyB64)
        : afterReset(held, pinnedSignaturePubkeyB64);
    provenPredecessors = [...next.provenPredecessorsB64];
  }
  const nextPins: Record<string, AccountPubkeyPin> = {
    ...current.pins,
    [accountIdB64]: {
      pinnedSignaturePubkeyB64,
      rotationMarker,
      ...(carriedLogStatus !== undefined ? { logStatus: carriedLogStatus } : {}),
      ...(carriedIsReset !== undefined ? { isReset: carriedIsReset } : {}),
      ...(carriedLogVersion !== undefined
        ? { pinnedLogVersion: carriedLogVersion }
        : {}),
      ...(carriedGeneration !== undefined ? { acceptedKeyGeneration: carriedGeneration } : {}),
      ...(previousKeys.length > 0 ? { previousKeysB64: previousKeys } : {}),
      ...(carriedSuperseded !== undefined && carriedSuperseded.length > 0
        ? { supersededKeysB64: carriedSuperseded }
        : {}),
      ...(carriedResetSignedAt !== undefined ? { resetSignedAtMs: carriedResetSignedAt } : {}),
      ...(provenPredecessors.length > 0 ? { provenPredecessorsB64: provenPredecessors } : {}),
      // ADR-0101: the seal floor and the started-over facts travel with the
      // account across every re-pin.
      ...(existing?.sealSeqFloor !== undefined ? { sealSeqFloor: existing.sealSeqFloor } : {}),
      ...(existing?.startedOver !== undefined ? { startedOver: existing.startedOver } : {}),
      ...(existing?.startedOverKeysB64 !== undefined && existing.startedOverKeysB64.length > 0
        ? { startedOverKeysB64: existing.startedOverKeysB64 }
        : {}),
    },
  };
  save(account, { ...current, pins: nextPins });
  // A proven re-pin changes which keys name the contact: the views that
  // resolve names read the aliases again.
  if (keyChanged) notifyAcceptedChanged("edited");
  return "set";
}

// ----------------------------------------------------------------------------
// ADR-0073 R.2: the silent re-pin of an accepted reset
// ----------------------------------------------------------------------------

/** The standard-base64 form of a contact row's `accountIdHex`, or `null`
 *  when the row carries none / a malformed one. The pin map is keyed on
 *  this; the contact row on the signature key — this is the bridge. */
export function accountIdB64OfHex(hex: string | undefined): string | null {
  if (hex === undefined || hex.length !== 32 || !/^[0-9a-f]+$/i.test(hex)) return null;
  const out = new Uint8Array(16);
  for (let i = 0; i < 16; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return base64Encode(out);
}

/** Outcome of {@link acceptReset}. */
export type AcceptResetResult =
  /** Re-pinned to the new key at the new generation. */
  | "accepted"
  /** The new key is already the pin at this generation or above: no write. */
  | "already"
  /** The generation is not above the accepted one: nothing written. The
   *  verdict (`reset-verdict.ts`) refuses this before it reaches here; the
   *  store repeats the check so a stale caller cannot lower it. */
  | "stale"
  | "no-account";

/** What an accepted reset carries into the store. */
export interface AcceptedReset {
  readonly accountIdB64: string;
  /** The key this device held for the account (its pin; for a group member
   *  it never pinned, the roster's account key for that account id). */
  readonly heldKeyB64: string;
  readonly newKeyB64: string;
  /** The new identity's X25519 key from the same bundle, when known. */
  readonly newX25519B64?: string;
  readonly keyGeneration: number;
  readonly signedAtMs: number;
  readonly nowMs: number;
  /** Keys proven as predecessors of `heldKeyB64` where no pin holds them (a
   *  verified binding's conversation key): retired with it. */
  readonly heldPredecessorsB64?: readonly string[];
}

/**
 * ADR-0073 R.2 "The re-pin". The verdict was `accepted`; this is the write.
 *
 * `pin[account] = new key`, marker `max(held marker, now in seconds)`,
 * accepted generation `g`. The held key (and its proven predecessors: a
 * reset makes every predecessor a superseded key) is appended to the
 * previous keys, which are display aliases for stored history only. The
 * contact ROW is re-keyed to the new key keeping its rename, account id and
 * chat link. No state is left on the contact: nothing reads as "reset".
 */
export function acceptReset(reset: AcceptedReset, owner?: string): AcceptResetResult {
  const account = ownerOf(owner);
  if (account === null) return "no-account";
  const current = load(account);
  const existing = current.pins[reset.accountIdB64];
  const acceptedGeneration = existing?.acceptedKeyGeneration ?? 0;
  if (existing !== undefined && existing.pinnedSignaturePubkeyB64 === reset.newKeyB64) {
    if (acceptedGeneration >= reset.keyGeneration) return "already";
  } else if (reset.keyGeneration <= acceptedGeneration) {
    return "stale";
  }
  const heldMarker = existing?.rotationMarker ?? 0;
  const marker = Math.max(heldMarker, Math.floor(reset.nowMs / 1000));
  const retired: string[] = [];
  const retire = (k: string): void => {
    if (k !== reset.newKeyB64 && !retired.includes(k)) retired.push(k);
  };
  if (existing !== undefined) {
    for (const k of existing.provenPredecessorsB64 ?? []) retire(k);
    retire(existing.pinnedSignaturePubkeyB64);
  }
  retire(reset.heldKeyB64);
  for (const k of reset.heldPredecessorsB64 ?? []) retire(k);
  const previousKeys = [...(existing?.previousKeysB64 ?? [])];
  const superseded = [...(existing?.supersededKeysB64 ?? [])];
  for (const k of retired) {
    if (!previousKeys.includes(k)) previousKeys.push(k);
    if (!superseded.includes(k)) superseded.push(k);
  }
  const pin: AccountPubkeyPin = {
    ...(existing ?? { rotationMarker: 0 }),
    pinnedSignaturePubkeyB64: reset.newKeyB64,
    rotationMarker: marker,
    acceptedKeyGeneration: reset.keyGeneration,
    previousKeysB64: previousKeys,
    supersededKeysB64: superseded,
    resetSignedAtMs: reset.signedAtMs,
  };
  // The log verdict and chain floor described the old key.
  delete pin.logStatus;
  delete pin.isReset;
  delete pin.pinnedLogVersion;
  delete pin.provenPredecessorsB64;
  // ADR-0101: the reset proved sealed after all (a seal the directory had
  // not served before): it is the same person, so nothing of theirs is
  // "started over" any more; the old conversation moves like any other.
  delete pin.startedOver;
  const stillStartedOver = (existing?.startedOverKeysB64 ?? []).filter((k) => !retired.includes(k));
  if (stillStartedOver.length > 0) pin.startedOverKeysB64 = stillStartedOver;
  else delete pin.startedOverKeysB64;
  const accepted = rowsAfterSealedReset(current.accepted, {
    accountIdB64: reset.accountIdB64,
    newKeyB64: reset.newKeyB64,
    retiredKeysB64: retired,
    leftBehindKeysB64: stillStartedOver,
    ...(reset.newX25519B64 !== undefined ? { newX25519B64: reset.newX25519B64 } : {}),
  });
  save(account, { ...current, accepted, pins: { ...current.pins, [reset.accountIdB64]: pin } });
  notifyAcceptedChanged("edited");
  return "accepted";
}

/**
 * The contact rows after a SEALED reset this device accepted (ADR-0073 R.2,
 * ADR-0101 section 10): one person, one row (walk of 2026-11-28, where a
 * sealed reset left the same person twice in Contacts).
 *
 * The row that names the person is re-keyed to the new key; its old key
 * becomes a display alias. Every OTHER row that names the same person by key
 * (a key the reset retired, or the new key itself: a request from the new
 * identity accepted before the reset was noticed, a row the pairing bundle
 * carried) folds into it. Which row is kept:
 *
 *   1. the first row under a retired key (the person's current row);
 *   2. else the first row under the new key;
 *   3. else the first row that names the account by id alone.
 *
 * Never touched, never folded: a row under a key this account LEFT BEHIND by
 * an earlier unsealed reset (UX 16.11: that is the started-over account, a
 * different contact), and a row that names the account by id alone when a
 * row by key exists (an id can come from a peer's own profile; such a row
 * may be someone else). Pure.
 */
export function rowsAfterSealedReset(
  rows: readonly AcceptedContact[],
  reset: {
    readonly accountIdB64: string;
    readonly newKeyB64: string;
    readonly retiredKeysB64: readonly string[];
    readonly leftBehindKeysB64: readonly string[];
    readonly newX25519B64?: string;
  },
): AcceptedContact[] {
  const leftBehind = (c: AcceptedContact): boolean => reset.leftBehindKeysB64.includes(c.signaturePubkeyB64);
  const byRetired = (c: AcceptedContact): boolean =>
    !leftBehind(c) && reset.retiredKeysB64.includes(c.signaturePubkeyB64);
  const byNew = (c: AcceptedContact): boolean => c.signaturePubkeyB64 === reset.newKeyB64;
  const byId = (c: AcceptedContact): boolean =>
    !leftBehind(c) && accountIdB64OfHex(c.accountIdHex) === reset.accountIdB64;
  let primaryIndex = rows.findIndex(byRetired);
  if (primaryIndex === -1) primaryIndex = rows.findIndex(byNew);
  if (primaryIndex === -1) primaryIndex = rows.findIndex(byId);
  if (primaryIndex === -1) return [...rows];
  const primary = rows[primaryIndex] as AcceptedContact;
  const folded = rows.filter((c, i) => i !== primaryIndex && (byRetired(c) || byNew(c)));
  const aliases: string[] = [];
  const alias = (k: string): void => {
    if (k.length > 0 && k !== reset.newKeyB64 && !aliases.includes(k)) aliases.push(k);
  };
  for (const k of primary.previousSignaturePubkeysB64 ?? []) alias(k);
  alias(primary.signaturePubkeyB64);
  for (const c of folded) {
    for (const k of c.previousSignaturePubkeysB64 ?? []) alias(k);
    alias(c.signaturePubkeyB64);
  }
  const rename =
    primary.displayName.length > 0
      ? primary.displayName
      : (folded.find((c) => c.displayName.length > 0)?.displayName ?? "");
  const next: AcceptedContact = {
    ...primary,
    signaturePubkeyB64: reset.newKeyB64,
    displayName: rename,
    previousSignaturePubkeysB64: aliases,
  };
  if (next.accountIdHex === undefined) {
    const withId = folded.find((c) => c.accountIdHex !== undefined);
    if (withId?.accountIdHex !== undefined) next.accountIdHex = withId.accountIdHex;
  }
  if (next.groupIdB64 === undefined) {
    const withChat = folded.find((c) => c.groupIdB64 !== undefined);
    if (withChat?.groupIdB64 !== undefined) next.groupIdB64 = withChat.groupIdB64;
  }
  // The x25519 key belonged to the old identity (a row already under the
  // new key keeps its own).
  if (reset.newX25519B64 !== undefined) next.x25519PubkeyB64 = reset.newX25519B64;
  else if (primary.signaturePubkeyB64 !== reset.newKeyB64) delete next.x25519PubkeyB64;
  const drop = new Set(folded);
  return rows.filter((c) => !drop.has(c)).map((c) => (c === primary ? next : c));
}

/** ADR-0073 R.2: the generation this device last accepted for an account
 *  (0 when it never recorded one). */
export function acceptedGenerationOf(accountIdB64: string, owner?: string): number {
  return readPin(accountIdB64, owner)?.acceptedKeyGeneration ?? 0;
}

// ----------------------------------------------------------------------------
// ADR-0101 section 10: the seal announcement floor
// ----------------------------------------------------------------------------

function parseSeq(value: string | undefined): bigint {
  if (value === undefined || !/^[0-9]{1,20}$/.test(value)) return 0n;
  try {
    return BigInt(value);
  } catch {
    return 0n;
  }
}

/** The highest seal announcement `seq` this device has seen for the account
 *  (0 when none, or when the account is not pinned). */
export function sealSeqFloorOf(accountIdB64: string, owner?: string): bigint {
  return parseSeq(readPin(accountIdB64, owner)?.sealSeqFloor);
}

/**
 * Raise the account's seal announcement floor to `seq`. Never lowers it;
 * a no-op when the account is not pinned (the floor travels with the pin).
 * Returns whether it was raised.
 */
export function raiseSealSeqFloor(accountIdB64: string, seq: bigint, owner?: string): boolean {
  const account = ownerOf(owner);
  if (account === null || seq < 0n) return false;
  const current = load(account);
  const existing = current.pins[accountIdB64];
  if (existing === undefined) return false;
  if (seq <= parseSeq(existing.sealSeqFloor)) return false;
  const pin: AccountPubkeyPin = { ...existing, sealSeqFloor: seq.toString() };
  save(account, { ...current, pins: { ...current.pins, [accountIdB64]: pin } });
  return true;
}

// ----------------------------------------------------------------------------
// ADR-0101 section 11 (UX 16.11): an unsealed reset is a new account
// ----------------------------------------------------------------------------

/** What an unsealed reset carries into the store. */
export interface UnsealedReset {
  readonly accountIdB64: string;
  /** The key this device held for the account (its pin). */
  readonly heldKeyB64: string;
  /** Keys proven as predecessors of the held key where no pin holds them
   *  (a verified binding's conversation key). */
  readonly heldPredecessorsB64?: readonly string[];
  readonly newKeyB64: string;
  readonly keyGeneration: number;
  readonly signedAtMs: number;
  readonly nowMs: number;
}

/** Outcome of {@link recordStartedOver}. */
export type RecordStartedOverResult =
  /** Recorded now (or the record's new key was updated). */
  | "recorded"
  /** The same reset was recorded already: no write. */
  | "already"
  /** The account is not pinned under the held key: nothing written. */
  | "no-pin"
  | "no-account";

/**
 * ADR-0101 section 11. The verdict was `unsealed`; this is the only write.
 *
 * The pin is NOT moved: the old key stays the key this device trusts for
 * the old contact, no key is superseded, no reset time is recorded, so
 * nothing of ADR-0073 R.5 to R.7 or ADR-0101 sections 4 to 9 can run for
 * this account (they all read {@link loadSupersededKeys} and
 * {@link acceptedResetOfKey}). What is kept is the fact itself: the keys the
 * account left behind (which close the old one-to-one) and the key the new
 * account publishes (which marks its contact request).
 */
export function recordStartedOver(reset: UnsealedReset, owner?: string): RecordStartedOverResult {
  const account = ownerOf(owner);
  if (account === null) return "no-account";
  const current = load(account);
  const existing = current.pins[reset.accountIdB64];
  if (existing === undefined || existing.pinnedSignaturePubkeyB64 !== reset.heldKeyB64) return "no-pin";
  if (reset.newKeyB64 === reset.heldKeyB64) return "no-pin";
  const left = [...(existing.startedOverKeysB64 ?? [])];
  const leave = (k: string): void => {
    if (k.length > 0 && k !== reset.newKeyB64 && !left.includes(k)) left.push(k);
  };
  for (const k of existing.provenPredecessorsB64 ?? []) leave(k);
  leave(existing.pinnedSignaturePubkeyB64);
  for (const k of reset.heldPredecessorsB64 ?? []) leave(k);
  const held = existing.startedOver;
  const sameRecord =
    held !== undefined &&
    held.newKeyB64 === reset.newKeyB64 &&
    held.keyGeneration === reset.keyGeneration &&
    left.length === (existing.startedOverKeysB64 ?? []).length;
  if (sameRecord) return "already";
  const pin: AccountPubkeyPin = {
    ...existing,
    startedOver: {
      newKeyB64: reset.newKeyB64,
      keyGeneration: reset.keyGeneration,
      signedAtMs: reset.signedAtMs,
      noticedAtMs: held?.noticedAtMs ?? reset.nowMs,
    },
    startedOverKeysB64: left,
  };
  save(account, { ...current, pins: { ...current.pins, [reset.accountIdB64]: pin } });
  notifyAcceptedChanged("edited");
  return "recorded";
}

/** The pending unsealed reset of an account, or `undefined`. */
export function startedOverOf(accountIdB64: string, owner?: string): StartedOverRecord | undefined {
  return readPin(accountIdB64, owner)?.startedOver;
}

/**
 * The new account behind a started-over record rotated its key BY PROOF
 * before the person decided: the record follows it (the caller verified the
 * supersession proof under the record's key). Returns whether it changed.
 */
export function moveStartedOverKey(
  accountIdB64: string,
  fromKeyB64: string,
  toKeyB64: string,
  owner?: string,
): boolean {
  const account = ownerOf(owner);
  if (account === null) return false;
  const current = load(account);
  const existing = current.pins[accountIdB64];
  const record = existing?.startedOver;
  if (existing === undefined || record === undefined) return false;
  if (record.newKeyB64 !== fromKeyB64 || toKeyB64 === fromKeyB64) return false;
  if (toKeyB64 === existing.pinnedSignaturePubkeyB64) return false;
  const pin: AccountPubkeyPin = { ...existing, startedOver: { ...record, newKeyB64: toKeyB64 } };
  save(account, { ...current, pins: { ...current.pins, [accountIdB64]: pin } });
  notifyAcceptedChanged("edited");
  return true;
}

/**
 * Every key an account left behind by an unsealed reset, across all
 * accounts, mapped to the account id it belonged to. A one-to-one whose
 * other person is bound to one of these is closed (UX 16.11).
 */
export function loadStartedOverKeys(owner?: string): Map<string, string> {
  const record = load(ownerOf(owner));
  const out = new Map<string, string>();
  for (const [accountIdB64, pin] of Object.entries(record.pins)) {
    for (const k of pin.startedOverKeysB64 ?? []) out.set(k, accountIdB64);
  }
  return out;
}

/**
 * Alignment of 2026-11-27, item F: the pairing state bundle carried a key a
 * contact left behind by an unsealed reset (another device of this account
 * noticed it). It joins the account's left-behind keys, so the old
 * one-to-one is closed here too. Never the pinned key's new account record:
 * the bundle carries no new key, only what is closed. Returns whether it
 * was added (a pin is needed; none, or a key already listed, adds nothing).
 */
export function addCarriedStartedOverKey(accountIdB64: string, oldKeyB64: string, owner?: string): boolean {
  const account = ownerOf(owner);
  if (account === null || oldKeyB64.length === 0) return false;
  const current = load(account);
  const existing = current.pins[accountIdB64];
  if (existing === undefined) return false;
  const left = existing.startedOverKeysB64 ?? [];
  if (left.includes(oldKeyB64)) return false;
  const pin: AccountPubkeyPin = { ...existing, startedOverKeysB64: [...left, oldKeyB64] };
  save(account, { ...current, pins: { ...current.pins, [accountIdB64]: pin } });
  notifyAcceptedChanged("edited");
  return true;
}

/** Outcome of {@link adoptStartedOver}. */
export type AdoptStartedOverResult =
  /** The pin now names the new account's key. */
  | "adopted"
  /** No pending record names `newKeyB64` for the account: nothing written. */
  | "none"
  | "no-account";

/**
 * UX 16.11: the person ACCEPTED a contact request from the new account of
 * someone who started over. The pin moves to the new key because the person
 * decided, not because the device confirmed anything.
 *
 * Unlike {@link acceptReset} this retires nothing: no key becomes
 * superseded and no reset time is recorded (those of an earlier, sealed
 * reset of the old account are dropped too), so no conversation moves, no
 * leaf is evicted and nothing is returned. The old keys stay display
 * aliases (the old conversation keeps the person's name) and stay in
 * `startedOverKeysB64` (the old conversation stays closed).
 *
 * The contact ROWS are not touched (walk of 2026-11-28, UX 16.11: the new
 * keys are a new account under the same username). The old row keeps its
 * key, its rename and its link to the closed one-to-one, and reads "Started
 * over with a new account" in Contacts; the accept writes a NEW row for the
 * new key ({@link upsertAccepted} matches by key and proven chain, and the
 * old key is in neither after this). Until 2026-11-28 the old row was
 * re-keyed to the new key, which merged two accounts into one contact.
 */
export function adoptStartedOver(
  accountIdB64: string,
  newKeyB64: string,
  nowMs: number,
  owner?: string,
): AdoptStartedOverResult {
  const account = ownerOf(owner);
  if (account === null) return "no-account";
  const current = load(account);
  const existing = current.pins[accountIdB64];
  const record = existing?.startedOver;
  if (existing === undefined || record === undefined || record.newKeyB64 !== newKeyB64) return "none";
  const oldKeys = [...(existing.provenPredecessorsB64 ?? []), existing.pinnedSignaturePubkeyB64];
  const previousKeys = [...(existing.previousKeysB64 ?? [])];
  for (const k of oldKeys) if (k !== newKeyB64 && !previousKeys.includes(k)) previousKeys.push(k);
  const pin: AccountPubkeyPin = {
    ...existing,
    pinnedSignaturePubkeyB64: newKeyB64,
    rotationMarker: Math.max(existing.rotationMarker, Math.floor(nowMs / 1000)),
    acceptedKeyGeneration: Math.max(existing.acceptedKeyGeneration ?? 0, record.keyGeneration),
    previousKeysB64: previousKeys,
  };
  // What described the old key does not describe the new account.
  delete pin.startedOver;
  delete pin.logStatus;
  delete pin.isReset;
  delete pin.pinnedLogVersion;
  delete pin.provenPredecessorsB64;
  delete pin.sealSeqFloor;
  // An EARLIER reset of the old account that this device accepted (sealed)
  // must not make the new key read as "the key an accepted reset produced"
  // ({@link acceptedResetOfKey}): the continuity key, the history answer and
  // the channel returns all hang on that, and none of them is owed to an
  // account this device could not confirm. The keys that reset retired are
  // earlier selves of the same person: conversations still bound to them
  // are closed like the rest, never moved to the new account.
  delete pin.resetSignedAtMs;
  const leftBehind = [...(existing.startedOverKeysB64 ?? [])];
  for (const k of [...oldKeys, ...(existing.supersededKeysB64 ?? [])]) {
    if (k !== newKeyB64 && !leftBehind.includes(k)) leftBehind.push(k);
  }
  delete pin.supersededKeysB64;
  if (leftBehind.length > 0) pin.startedOverKeysB64 = leftBehind;
  save(account, { ...current, pins: { ...current.pins, [accountIdB64]: pin } });
  notifyAcceptedChanged("edited");
  return "adopted";
}

/**
 * ADR-0073 R.4: the account and generation behind `currentKeyB64` when it is
 * the key an ACCEPTED RESET re-pinned this device to, with the attestation's
 * `signed_at`: what a Welcome to the new identity derives the continuity
 * reach key from. `null` for a key no accepted reset produced (a first pin,
 * a proven rotation): such a person is reached with the grants they issued.
 */
export function acceptedResetOfKey(
  currentKeyB64: string,
  owner?: string,
): { accountIdB64: string; keyGeneration: number; signedAtMs: number } | null {
  const record = load(ownerOf(owner));
  for (const [accountIdB64, pin] of Object.entries(record.pins)) {
    if (pin.pinnedSignaturePubkeyB64 !== currentKeyB64) continue;
    if (
      pin.resetSignedAtMs === undefined ||
      pin.acceptedKeyGeneration === undefined ||
      (pin.supersededKeysB64 ?? []).length === 0
    ) {
      return null;
    }
    return {
      accountIdB64,
      keyGeneration: pin.acceptedKeyGeneration,
      signedAtMs: pin.resetSignedAtMs,
    };
  }
  return null;
}

/** ADR-0073 R.5 / R.6: every key an accepted reset retired, across all
 *  accounts, mapped to the account id it belonged to. A row or a claim from
 *  a leaf bound to one of these is dropped. */
export function loadSupersededKeys(owner?: string): Map<string, string> {
  const record = load(ownerOf(owner));
  const out = new Map<string, string>();
  for (const [accountIdB64, pin] of Object.entries(record.pins)) {
    for (const k of pin.supersededKeysB64 ?? []) out.set(k, accountIdB64);
  }
  return out;
}

/**
 * Every key that names a contact OTHER than the row's own: every retired
 * key (so stored history and the old leaves still name the person) and a
 * proven successor. Maps alias → the row's `signaturePubkeyB64`.
 */
export function loadKeyAliases(owner?: string): Map<string, string> {
  const record = load(ownerOf(owner));
  const out = new Map<string, string>();
  // UX 16.11 (walk of 2026-11-28): a key that is itself a contact row's is
  // that row, never an alias of another. After an unsealed reset the account
  // that started over keeps its own row under its old key, beside the new
  // account's row; the pin's previous keys must not point it at the new one.
  const rowKeys = new Set(record.accepted.map((c) => c.signaturePubkeyB64));
  for (const c of record.accepted) {
    for (const old of c.previousSignaturePubkeysB64 ?? []) {
      if (old !== c.signaturePubkeyB64) out.set(old, c.signaturePubkeyB64);
    }
    const accountIdB64 = accountIdB64OfHex(c.accountIdHex);
    if (accountIdB64 === null) continue;
    const pin = record.pins[accountIdB64];
    if (pin === undefined) continue;
    for (const old of pin.previousKeysB64 ?? []) {
      if (old !== c.signaturePubkeyB64 && !rowKeys.has(old) && !out.has(old)) out.set(old, c.signaturePubkeyB64);
    }
    // ADR-0074 section 3.5: after a PROVEN rotation the row keeps the key the
    // person's leaves are bound to, and the newly pinned key names the same
    // row. Only for a proven chain: a pin that differs from the row for any
    // other reason is not an alias.
    if (
      pin.pinnedSignaturePubkeyB64 !== c.signaturePubkeyB64 &&
      (pin.provenPredecessorsB64 ?? []).includes(c.signaturePubkeyB64) &&
      !out.has(pin.pinnedSignaturePubkeyB64)
    ) {
      out.set(pin.pinnedSignaturePubkeyB64, c.signaturePubkeyB64);
    }
  }
  return out;
}

/**
 * The proven predecessors a pin holds after another device of the SAME
 * account handed over its own (ADR-0074, the pairing state bundle): the
 * carried chain when this device holds none or only keys the carried chain
 * names too; this device's own chain otherwise. The current key is never a
 * predecessor. Pure.
 */
export function mergeCarriedProvenPredecessors(
  currentKeyB64: string,
  heldB64: readonly string[],
  carriedB64: readonly string[],
): string[] {
  const carried = carriedB64.filter((k, i) => k !== currentKeyB64 && carriedB64.indexOf(k) === i);
  if (heldB64.every((k) => carried.includes(k))) return carried;
  return [...heldB64];
}

/**
 * Record a contact's proven predecessors as carried in a pairing state
 * bundle. The second writer of the proven-predecessor store beside the
 * fail-closed gate of ADR-0016: the evidence is the sending device's own
 * gate, and the bundle is sealed under the account's DeviceGroup exporter,
 * the same trust the carried pin itself arrives with.
 *
 * Written only when this device pins the account under exactly the key the
 * chain was proven for. Returns whether anything changed.
 */
export function recordCarriedProvenPredecessors(
  accountIdB64: string,
  pinnedSignaturePubkeyB64: string,
  carriedB64: readonly string[],
  owner?: string,
): boolean {
  const account = ownerOf(owner);
  if (account === null || carriedB64.length === 0) return false;
  const current = load(account);
  const existing = current.pins[accountIdB64];
  if (existing === undefined) return false;
  if (existing.pinnedSignaturePubkeyB64 !== pinnedSignaturePubkeyB64) return false;
  const held = existing.provenPredecessorsB64 ?? [];
  const next = mergeCarriedProvenPredecessors(pinnedSignaturePubkeyB64, held, carriedB64);
  if (next.length === held.length && next.every((k, i) => k === held[i])) return false;
  const pin: AccountPubkeyPin = { ...existing };
  if (next.length > 0) pin.provenPredecessorsB64 = next;
  else delete pin.provenPredecessorsB64;
  save(account, { ...current, pins: { ...current.pins, [accountIdB64]: pin } });
  notifyAcceptedChanged("edited");
  return true;
}

/**
 * ADR-0074 section 3.7: record the key a person's conversations are bound
 * to, from a two-way conversation link that verified under the pinned key.
 * The third writer of the proven-predecessor store beside the fail-closed
 * gate and the pairing bundle; the evidence is both keys' signatures.
 *
 * It goes FIRST in the chain: it is the oldest key the account has. It is
 * also an alias of the person (`previousKeysB64`, and the contact row's old
 * keys), so a leaf bound to it names them, for a contact who never pinned it.
 *
 * Written only when the account is pinned under exactly `pinnedKeyB64`, and
 * never for a key an accepted reset retired: old keys do not come back.
 * Returns whether anything changed.
 */
export function recordConversationKey(
  accountIdB64: string,
  pinnedKeyB64: string,
  conversationKeyB64: string,
  owner?: string,
): boolean {
  const account = ownerOf(owner);
  if (account === null || conversationKeyB64.length === 0 || conversationKeyB64 === pinnedKeyB64) {
    return false;
  }
  const current = load(account);
  const existing = current.pins[accountIdB64];
  if (existing === undefined || existing.pinnedSignaturePubkeyB64 !== pinnedKeyB64) return false;
  if ((existing.supersededKeysB64 ?? []).includes(conversationKeyB64)) return false;
  const held = existing.provenPredecessorsB64 ?? [];
  if (held.includes(conversationKeyB64)) return false;
  const previousKeys = existing.previousKeysB64 ?? [];
  const pin: AccountPubkeyPin = {
    ...existing,
    provenPredecessorsB64: [conversationKeyB64, ...held],
    previousKeysB64: previousKeys.includes(conversationKeyB64)
      ? [...previousKeys]
      : [...previousKeys, conversationKeyB64],
  };
  const accepted = current.accepted.map((c) => {
    const namesAccount =
      accountIdB64OfHex(c.accountIdHex) === accountIdB64 || c.signaturePubkeyB64 === pinnedKeyB64;
    if (!namesAccount || c.signaturePubkeyB64 === conversationKeyB64) return c;
    const rowKeys = c.previousSignaturePubkeysB64 ?? [];
    return rowKeys.includes(conversationKeyB64)
      ? c
      : { ...c, previousSignaturePubkeysB64: [...rowKeys, conversationKeyB64] };
  });
  save(account, { ...current, accepted, pins: { ...current.pins, [accountIdB64]: pin } });
  notifyAcceptedChanged("edited");
  return true;
}

/** ADR-0074 section 3.5: the keys of a pinned account as `predecessor.ts`
 *  reads them, or `undefined` when the account is not pinned. */
export function accountKeysOf(accountIdB64: string, owner?: string): AccountKeys | undefined {
  const pin = readPin(accountIdB64, owner);
  if (pin === undefined) return undefined;
  return {
    currentKeyB64: pin.pinnedSignaturePubkeyB64,
    provenPredecessorsB64: pin.provenPredecessorsB64 ?? [],
  };
}

/**
 * The account id (standard base64) this device knows the key `keyB64` under:
 * a pin whose current key or proven predecessor it is, else an accepted
 * contact's row. `null` for a key no trusted local record names. Used to
 * re-check a person's address (ADR-0074 section 3.5a rule 6); a claimed
 * account id (a profile's) is deliberately not consulted.
 */
export function accountIdB64OfKey(keyB64: string, owner?: string): string | null {
  const record = load(ownerOf(owner));
  for (const [accountIdB64, pin] of Object.entries(record.pins)) {
    if (
      pin.pinnedSignaturePubkeyB64 === keyB64 ||
      (pin.provenPredecessorsB64 ?? []).includes(keyB64)
    ) {
      return accountIdB64;
    }
  }
  for (const contact of record.accepted) {
    if (
      contact.signaturePubkeyB64 === keyB64 ||
      (contact.previousSignaturePubkeysB64 ?? []).includes(keyB64)
    ) {
      return accountIdB64OfHex(contact.accountIdHex);
    }
  }
  return null;
}

/** Every pinned account's keys (the predecessor table's input). */
export function loadAccountKeys(owner?: string): AccountKeys[] {
  const record = load(ownerOf(owner));
  return Object.values(record.pins).map((pin) => ({
    currentKeyB64: pin.pinnedSignaturePubkeyB64,
    provenPredecessorsB64: pin.provenPredecessorsB64 ?? [],
  }));
}

/** The contact ROW a key names, through its own key or an alias
 *  ({@link loadKeyAliases}); `undefined` for a stranger. */
export function contactByAnyKey(
  keyB64: string,
  accepted: readonly AcceptedContact[],
  aliases: ReadonlyMap<string, string>,
): AcceptedContact | undefined {
  const rowKey = aliases.get(keyB64) ?? keyB64;
  return accepted.find((c) => c.signaturePubkeyB64 === rowKey);
}

/** Remove the pin for `accountIdB64`. No-op when absent. Exposed
 *  for the "forget contact" / re-verify-from-scratch flows. */
export function removePin(accountIdB64: string, owner?: string): void {
  const account = ownerOf(owner);
  const current = load(account);
  if (current.pins[accountIdB64] === undefined) return;
  const nextPins = { ...current.pins };
  delete nextPins[accountIdB64];
  save(account, { ...current, pins: nextPins });
}

/**
 * Record the latest transparency-log inclusion verdict (KT-log S3c)
 * for an account, keyed on the same stable `account_id` as the pin.
 *
 * Writes onto the existing pin record so the verdict travels with the
 * pinned key. **No-op when no pin exists** — the log check is always
 * run right after the supersession gate (which TOFU-pins on first
 * sight), so by the time a verdict is recorded a pin is present; if
 * for any reason it is not, we do not fabricate a pin entry with no
 * pubkey. Returns whether a write happened (a present pin).
 */
export function recordLogStatus(
  accountIdB64: string,
  logStatus: LogVerifyStatus,
  owner?: string,
): boolean {
  const account = ownerOf(owner);
  const current = load(account);
  const existing = current.pins[accountIdB64];
  if (existing === undefined) return false;
  if (existing.logStatus === logStatus) return true; // idempotent, no write
  const nextPins: Record<string, AccountPubkeyPin> = {
    ...current.pins,
    [accountIdB64]: { ...existing, logStatus },
  };
  save(account, { ...current, pins: nextPins });
  return true;
}

/** Read the recorded log-verify status for an account, or `undefined`
 *  when no pin / no status. Exposed for the UI signal. */
export function readLogStatus(
  accountIdB64: string,
  owner?: string,
): LogVerifyStatus | undefined {
  return load(ownerOf(owner)).pins[accountIdB64]?.logStatus;
}

/**
 * Record the transparency log's `is_reset` marker (KT-log S4) for an
 * account, keyed on the same stable `account_id` as the pin.
 *
 * Mirrors `recordLogStatus`: writes onto the existing pin record so the
 * marker travels with the pinned key, and is a **no-op when no pin
 * exists** (the log check runs right after the supersession gate, which
 * TOFU-pins on first sight, so a pin is present by the time a marker is
 * recorded). A `null` argument (no latest entry was read — `absent` /
 * `unreachable` / `off` / `rollback`) CLEARS any stale marker so the UI
 * never surfaces a reset claim that the current log state no longer
 * supports. Returns whether a write happened (a present pin).
 */
export function recordLogReset(
  accountIdB64: string,
  isReset: boolean | null,
  owner?: string,
): boolean {
  const account = ownerOf(owner);
  const current = load(account);
  const existing = current.pins[accountIdB64];
  if (existing === undefined) return false;
  const currentValue = existing.isReset;
  // Normalise: `null` means "clear" (no marker), a boolean means "set".
  if (isReset === null) {
    if (currentValue === undefined) return true; // already clear, no write
    const cleared: AccountPubkeyPin = { ...existing };
    delete cleared.isReset;
    save(account, {
      ...current,
      pins: { ...current.pins, [accountIdB64]: cleared },
    });
    return true;
  }
  if (currentValue === isReset) return true; // idempotent, no write
  const nextPins: Record<string, AccountPubkeyPin> = {
    ...current.pins,
    [accountIdB64]: { ...existing, isReset },
  };
  save(account, { ...current, pins: nextPins });
  return true;
}

/** Read the recorded `is_reset` marker (KT-log S4) for an account, or
 *  `undefined` when no pin / no recorded marker. Exposed for the UI. */
export function readLogReset(accountIdB64: string, owner?: string): boolean | undefined {
  return load(ownerOf(owner)).pins[accountIdB64]?.isReset;
}

/**
 * Record the chain-confirmed transparency-log VERSION (G-30 #2,
 * ADR-0019) for an account, keyed on the same stable `account_id` as
 * the pin.
 *
 * Mirrors `recordLogReset`: writes onto the existing pin record so the
 * version travels with the pinned key, and is a **no-op when no pin
 * exists** (the supersession gate TOFU-pins on first sight, so a pin is
 * present by the time a chain verdict is recorded). A `null` argument
 * CLEARS any stored version. The caller persists it ONLY on a
 * `confirmed` rotation-chain verdict — a transient `unreachable` /
 * `mismatch` / `absent` outcome must NOT wipe a previously-confirmed
 * version, so the caller does not pass `null` on those (see
 * `checkTransparencyLog`). Idempotent on an unchanged value (no write).
 * Returns whether a write happened (a present pin).
 */
export function recordLogVersion(
  accountIdB64: string,
  version: number | null,
  owner?: string,
): boolean {
  const account = ownerOf(owner);
  const current = load(account);
  const existing = current.pins[accountIdB64];
  if (existing === undefined) return false;
  const currentValue = existing.pinnedLogVersion;
  // Normalise: `null` means "clear" (no version), a number means "set".
  if (version === null) {
    if (currentValue === undefined) return true; // already clear, no write
    const cleared: AccountPubkeyPin = { ...existing };
    delete cleared.pinnedLogVersion;
    save(account, {
      ...current,
      pins: { ...current.pins, [accountIdB64]: cleared },
    });
    return true;
  }
  if (currentValue === version) return true; // idempotent, no write
  const nextPins: Record<string, AccountPubkeyPin> = {
    ...current.pins,
    [accountIdB64]: { ...existing, pinnedLogVersion: version },
  };
  save(account, { ...current, pins: nextPins });
  return true;
}

/** Read the recorded chain-confirmed log version (G-30 #2) for an
 *  account, or `undefined` when no pin / no recorded version. The next
 *  rotation's chain proof is checked against this as the downgrade
 *  floor (default `0` when absent). */
export function readLogVersion(accountIdB64: string, owner?: string): number | undefined {
  return load(ownerOf(owner)).pins[accountIdB64]?.pinnedLogVersion;
}

/** Render a short hex prefix for a public key (8 hex chars, unseparated).
 *  NOT a display label: a person is labelled through
 *  `profile/resolver.ts`, whose fingerprint fallback is `a1b2 c3d4` with no
 *  `@` (ADR-0048). */
export function fingerprintShort(pubkey: Uint8Array): string {
  let out = "";
  for (let i = 0; i < Math.min(pubkey.length, 4); i++) {
    out += (pubkey[i] ?? 0).toString(16).padStart(2, "0");
  }
  return out;
}
