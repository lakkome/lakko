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
   *  discovery). ADR-0021: the SFU group-call roster carries a
   *  per-call tag derived from this account_id; re-deriving the tag
   *  over this field resolves a roster entry to this contact's
   *  `displayName`. Optional — pre-ADR-0021 contacts lack it and
   *  fall back to the SFU pseudonym until re-resolved. */
  accountIdHex?: string;
  /** base64(group_id) of the 1×1 chat created with this contact at
   *  accept time (ADR-0021 follow-up). Links the chat back to this
   *  contact so the in-call roster + chat label resolve to the
   *  user-chosen `displayName`. Optional — set only on the accept side
   *  (where the contact + the group are created together); a chat
   *  joined from a Welcome has no contact link and falls back to the
   *  group label. */
  groupIdB64?: string;
  /** ADR-0065 phase 2 (UX 16.2 / 16.8): the account keys this contact held
   *  BEFORE a reset this device has since verified, base64, oldest first.
   *  The row is re-keyed to the new key on confirmation (so the person keeps
   *  their rename, folders, pins and mute); the old keys stay here so the
   *  closed one-to-one and the old leaves in shared groups still name the
   *  person. Never sent anywhere. */
  previousSignaturePubkeysB64?: string[];
}

/**
 * ADR-0065 phase 2 (UX 16.1 / 16.8) — what this device recorded when a
 * contact's published account key changed and the KT log confirmed the
 * change as an owner-initiated RESET. Present on the pin ⇒ the contact's
 * `keyState` is `reset-unverified`; absent ⇒ `normal`. Cleared by the
 * person's own confirmation ({@link confirmKeyReset}) and nothing else.
 */
export interface KeyResetRecord {
  /** The pin BEFORE the reset: the key the contact's old devices still hold
   *  in every shared group. The eviction (UX 16.3) targets leaves that
   *  resolve to this key. */
  previousKeyB64: string;
  /** The published key the log confirmed as a reset. The safety-number
   *  section computes over THIS key (never `previousKeyB64`); confirming
   *  re-pins the account to it. */
  pendingKeyB64: string;
  /** When this device noticed (ms since epoch). */
  noticedAtMs: number;
  /** The directory's `key_generation` for `pendingKeyB64` (ADR-0065: 1 =
   *  never reset). The monotone marker for the re-pin: a later reset of
   *  the same account carries a strictly greater generation. */
  keyGeneration: number;
}

/** The contact's key state as the UI reads it (UX 16.8). */
export type ContactKeyState = "normal" | "reset-unverified";

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
  /** ADR-0065 phase 2: present while the contact is `reset-unverified`
   *  (UX 16.1). The pin itself is NOT advanced until the person confirms
   *  (invariant #6: no blind re-pin). */
  keyReset?: KeyResetRecord;
  /** ADR-0065 phase 2: the `key_generation` this device last CONFIRMED
   *  (UX 16.2), so a stale observation of an already-verified reset is
   *  never re-flagged and a later reset is never refused as backwards. */
  confirmedKeyGeneration?: number;
  /** ADR-0065 phase 2: every account key this device pinned for the
   *  account before a confirmed reset, base64, oldest first. What the
   *  eviction bookkeeping needs after the UI has forgotten `previousKey`
   *  (UX 16.2): the old leaves in a group still resolve to one of these. */
  previousKeysB64?: string[];
  /** ADR-0066 section 3.5: every key this account had before a PROVEN
   *  rotation (a re-pin authorised by a supersession proof), base64, oldest
   *  first. The conversation leaves that exist stay bound to these, so they
   *  name the same person (`predecessor.ts`); nothing is evicted and nothing
   *  is shown. Emptied by a confirmed reset: a reset makes every
   *  predecessor, proven or not, a superseded key. */
  provenPredecessorsB64?: string[];
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

/** Narrowing guard for the ADR-0065 phase-2 reset record on a pin. */
function isKeyResetRecord(r: unknown): r is KeyResetRecord {
  return (
    typeof r === "object" &&
    r !== null &&
    typeof (r as KeyResetRecord).previousKeyB64 === "string" &&
    typeof (r as KeyResetRecord).pendingKeyB64 === "string" &&
    typeof (r as KeyResetRecord).noticedAtMs === "number" &&
    Number.isFinite((r as KeyResetRecord).noticedAtMs) &&
    typeof (r as KeyResetRecord).keyGeneration === "number" &&
    Number.isFinite((r as KeyResetRecord).keyGeneration) &&
    (r as KeyResetRecord).keyGeneration >= 0
  );
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
  // ADR-0065 phase 2: a junk reset record is dropped, which reads as
  // `normal` — the safe direction, since the record only ever BLOCKS
  // actions and the next identity check re-records a real reset.
  const kr = (p as AccountPubkeyPin).keyReset;
  if (kr !== undefined && !isKeyResetRecord(kr)) {
    delete (p as { keyReset?: unknown }).keyReset;
  }
  const cg = (p as AccountPubkeyPin).confirmedKeyGeneration;
  if (cg !== undefined && (typeof cg !== "number" || !Number.isFinite(cg) || cg < 0)) {
    delete (p as { confirmedKeyGeneration?: unknown }).confirmedKeyGeneration;
  }
  const pk = (p as AccountPubkeyPin).previousKeysB64;
  if (pk !== undefined && !isStringArray(pk)) {
    delete (p as { previousKeysB64?: unknown }).previousKeysB64;
  }
  // ADR-0066: a junk predecessor list is dropped, which reads as "no proven
  // predecessor" (the fail-closed direction).
  const pp = (p as AccountPubkeyPin).provenPredecessorsB64;
  if (pp !== undefined && !isStringArray(pp)) {
    delete (p as { provenPredecessorsB64?: unknown }).provenPredecessorsB64;
  }
  return true;
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
      return {
        v: 3,
        accepted: parsed.accepted.filter(isAcceptedContact),
        pins,
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
  /** ADR-0065 phase 2: a retired or pending key still names the person
   *  ({@link loadKeyAliases}); a closed one-to-one with the old keys is
   *  still the contact's chat. */
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

/** Append (or replace by `signaturePubkeyB64`) an accepted
 *  contact in `owner`'s list (default: the bound account) and persist.
 *  Returns the new list; with no account nothing is written and the result
 *  is empty. */
export function upsertAccepted(contact: AcceptedContact, owner?: string): AcceptedContact[] {
  const account = ownerOf(owner);
  if (account === null) return [];
  const current = load(account);
  const existing = current.accepted.findIndex(
    (a) => a.signaturePubkeyB64 === contact.signaturePubkeyB64,
  );
  const next =
    existing === -1
      ? [...current.accepted, contact]
      : current.accepted.map((a, i) => (i === existing ? contact : a));
  save(account, { ...current, accepted: next });
  notifyAcceptedChanged("edited");
  return next;
}

/** A person this account has a one-to-one conversation with IS a contact
 *  (`adopt-one-to-one.ts`): apply the rule for one peer against the
 *  persisted list, write only when it changes something, and tell the
 *  listeners. Returns whether a write happened. */
export function adoptOneToOne(peer: OneToOnePeer, owner?: string): boolean {
  const account = ownerOf(owner);
  if (account === null) return false;
  const current = load(account);
  const next = adoptOneToOnePeer(current.accepted, peer);
  if (next === null) return false;
  save(account, { ...current, accepted: next });
  notifyAcceptedChanged("edited");
  return true;
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
  // ADR-0065 phase 2 (UX 16.1): removing the contact clears the reset state
  // with it. The pin itself stays — it is this device's trust decision, and
  // a re-added contact whose key is still unverified is flagged again.
  let pins = current.pins;
  const removed = current.accepted.find((a) => a.signaturePubkeyB64 === signaturePubkeyB64);
  const accountIdB64 = accountIdB64OfHex(removed?.accountIdHex);
  if (accountIdB64 !== null && pins[accountIdB64]?.keyReset !== undefined) {
    const cleared: AccountPubkeyPin = { ...(pins[accountIdB64] as AccountPubkeyPin) };
    delete cleared.keyReset;
    pins = { ...pins, [accountIdB64]: cleared };
  }
  save(account, { ...current, accepted: next, pins });
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
  /** ADR-0066 section 3.5: `proven` is passed ONLY by the fail-closed gate,
   *  after a supersession proof by the pinned key verified for exactly this
   *  new key. The replaced key then becomes a proven predecessor. Any other
   *  re-pin records none. */
  options: { readonly proven?: boolean } = {},
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
  // ADR-0065 phase 2: the confirmed generation travels with the account.
  // A re-pin to a DIFFERENT key retires the old one into `previousKeysB64`
  // (the old leaves in shared groups still resolve to it) and ends any
  // reset record: whatever authorised this re-pin (a proof, or the person's
  // own confirmation through `confirmKeyReset`) has superseded it.
  const carriedGeneration = existing?.confirmedKeyGeneration;
  const keyChanged =
    existing !== undefined && existing.pinnedSignaturePubkeyB64 !== pinnedSignaturePubkeyB64;
  const previousKeys = [...(existing?.previousKeysB64 ?? [])];
  if (keyChanged && existing !== undefined && !previousKeys.includes(existing.pinnedSignaturePubkeyB64)) {
    previousKeys.push(existing.pinnedSignaturePubkeyB64);
  }
  const carriedReset = keyChanged ? undefined : existing?.keyReset;
  // ADR-0066: the proven chain. A proven re-pin appends the replaced key; an
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
      ...(carriedGeneration !== undefined ? { confirmedKeyGeneration: carriedGeneration } : {}),
      ...(carriedReset !== undefined ? { keyReset: carriedReset } : {}),
      ...(previousKeys.length > 0 ? { previousKeysB64: previousKeys } : {}),
      ...(provenPredecessors.length > 0 ? { provenPredecessorsB64: provenPredecessors } : {}),
    },
  };
  save(account, { ...current, pins: nextPins });
  // A proven re-pin changes which keys name the contact: the views that
  // resolve names read the aliases again.
  if (keyChanged) notifyAcceptedChanged("edited");
  return "set";
}

// ----------------------------------------------------------------------------
// ADR-0065 phase 2 (UX 16.1 / 16.2 / 16.8): the flagged contact
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

/** Outcome of {@link recordKeyReset}. */
export type RecordKeyResetResult =
  /** Written: the contact is now `reset-unverified`. */
  | "recorded"
  /** The same pending key was already recorded: no write. */
  | "already"
  /** The generation is not newer than one this device already confirmed:
   *  a stale observation of a reset the person has verified. No write. */
  | "stale"
  /** The published key IS the pin: nothing was reset. No write. */
  | "not-a-change"
  /** No pin for the account (a stranger, never pinned): nothing to flag. */
  | "no-pin"
  | "no-account";

/**
 * Record that the published key for `accountIdB64` changed to
 * `pendingKeyB64` and the KT log confirmed the change as a reset (UX 16.1).
 *
 * The pin is NOT advanced (invariant #6, ADR-0016's fail-closed gate stays
 * closed); the record sits beside it until {@link confirmKeyReset}. The
 * `previousKey` is the pin at the FIRST observation: a second reset noticed
 * before the first is verified replaces `pendingKey` and keeps the original
 * `previousKey`, because that is still the key the old leaves hold.
 */
export function recordKeyReset(
  accountIdB64: string,
  pendingKeyB64: string,
  keyGeneration: number,
  noticedAtMs: number,
  owner?: string,
): RecordKeyResetResult {
  const account = ownerOf(owner);
  if (account === null) return "no-account";
  const current = load(account);
  const existing = current.pins[accountIdB64];
  if (existing === undefined) return "no-pin";
  if (existing.pinnedSignaturePubkeyB64 === pendingKeyB64) return "not-a-change";
  if (keyGeneration <= (existing.confirmedKeyGeneration ?? 0)) return "stale";
  if (existing.keyReset !== undefined && existing.keyReset.pendingKeyB64 === pendingKeyB64) {
    return "already";
  }
  const keyReset: KeyResetRecord = {
    previousKeyB64: existing.keyReset?.previousKeyB64 ?? existing.pinnedSignaturePubkeyB64,
    pendingKeyB64,
    noticedAtMs,
    keyGeneration,
  };
  save(account, {
    ...current,
    pins: { ...current.pins, [accountIdB64]: { ...existing, keyReset } },
  });
  notifyAcceptedChanged("edited");
  return "recorded";
}

/** The reset record for an account, or `undefined` when the contact is
 *  `normal` (or unknown). */
export function readKeyReset(accountIdB64: string, owner?: string): KeyResetRecord | undefined {
  return load(ownerOf(owner)).pins[accountIdB64]?.keyReset;
}

/** The contact's key state (UX 16.8). An unknown account is `normal`. */
export function readKeyState(accountIdB64: string, owner?: string): ContactKeyState {
  return readKeyReset(accountIdB64, owner) === undefined ? "normal" : "reset-unverified";
}

/** The reset record of the contact ROW `contact`, or `undefined`. */
export function keyResetOfContact(
  contact: AcceptedContact,
  owner?: string,
): KeyResetRecord | undefined {
  const accountIdB64 = accountIdB64OfHex(contact.accountIdHex);
  return accountIdB64 === null ? undefined : readKeyReset(accountIdB64, owner);
}

/** Outcome of {@link confirmKeyReset}. */
export type ConfirmKeyResetResult = "confirmed" | "not-flagged" | "no-account";

/**
 * The person compared the new safety number and confirmed it (UX 16.2).
 *
 * Re-pins the account to `pendingKey` with a marker that is never backwards
 * (the larger of the held marker and `nowSecs`; equal is accepted by
 * {@link setPin}), records the confirmed generation, retires the old key
 * into `previousKeysB64` (the eviction bookkeeping still needs it), clears
 * the record, and re-keys the contact ROW to the new key so the rename, the
 * folders, the pins and the mute stay with the person. The row's one-to-one
 * link is dropped: that chat is closed (UX 16.4) and Message must create
 * the new one.
 */
export function confirmKeyReset(
  accountIdB64: string,
  nowSecs: number,
  owner?: string,
): ConfirmKeyResetResult {
  const account = ownerOf(owner);
  if (account === null) return "no-account";
  const current = load(account);
  const existing = current.pins[accountIdB64];
  const reset = existing?.keyReset;
  if (existing === undefined || reset === undefined) return "not-flagged";
  const marker = Math.max(existing.rotationMarker, Math.floor(nowSecs));
  const previousKeys = [...(existing.previousKeysB64 ?? [])];
  for (const k of [existing.pinnedSignaturePubkeyB64, reset.previousKeyB64]) {
    if (!previousKeys.includes(k)) previousKeys.push(k);
  }
  const pin: AccountPubkeyPin = {
    ...existing,
    pinnedSignaturePubkeyB64: reset.pendingKeyB64,
    rotationMarker: marker,
    confirmedKeyGeneration: reset.keyGeneration,
    previousKeysB64: previousKeys,
  };
  delete pin.keyReset;
  // ADR-0066 section 3.5: a reset makes every predecessor, proven or not, a
  // superseded key.
  delete pin.provenPredecessorsB64;
  const rowIndex = current.accepted.findIndex(
    (c) => accountIdB64OfHex(c.accountIdHex) === accountIdB64,
  );
  let accepted = current.accepted;
  if (rowIndex !== -1) {
    const row = current.accepted[rowIndex] as AcceptedContact;
    const previousRowKeys = [...(row.previousSignaturePubkeysB64 ?? [])];
    if (row.signaturePubkeyB64 !== reset.pendingKeyB64 && !previousRowKeys.includes(row.signaturePubkeyB64)) {
      previousRowKeys.push(row.signaturePubkeyB64);
    }
    const next: AcceptedContact = {
      ...row,
      signaturePubkeyB64: reset.pendingKeyB64,
      previousSignaturePubkeysB64: previousRowKeys,
    };
    delete next.groupIdB64;
    // The x25519 key belonged to the old identity; the new one is fetched
    // with the next identity bundle.
    delete next.x25519PubkeyB64;
    accepted = current.accepted.map((c, i) => (i === rowIndex ? next : c));
  }
  save(account, { ...current, accepted, pins: { ...current.pins, [accountIdB64]: pin } });
  notifyAcceptedChanged("edited");
  return "confirmed";
}

/**
 * Every key that names a contact OTHER than the row's own: a flagged
 * contact's `pendingKey` (so a message from the new keys names the person,
 * UX 16.1) and every retired key (so the closed one-to-one and the old
 * leaves still name them). Maps alias → the row's `signaturePubkeyB64`.
 */
export function loadKeyAliases(owner?: string): Map<string, string> {
  const record = load(ownerOf(owner));
  const out = new Map<string, string>();
  for (const c of record.accepted) {
    for (const old of c.previousSignaturePubkeysB64 ?? []) {
      if (old !== c.signaturePubkeyB64) out.set(old, c.signaturePubkeyB64);
    }
    const accountIdB64 = accountIdB64OfHex(c.accountIdHex);
    if (accountIdB64 === null) continue;
    const pin = record.pins[accountIdB64];
    if (pin === undefined) continue;
    if (pin.keyReset !== undefined && pin.keyReset.pendingKeyB64 !== c.signaturePubkeyB64) {
      out.set(pin.keyReset.pendingKeyB64, c.signaturePubkeyB64);
    }
    for (const old of pin.previousKeysB64 ?? []) {
      if (old !== c.signaturePubkeyB64 && !out.has(old)) out.set(old, c.signaturePubkeyB64);
    }
    // ADR-0066 section 3.5: after a PROVEN rotation the row keeps the key the
    // person's leaves are bound to, and the newly pinned key names the same
    // row. Only for a proven chain: a pin that differs from the row for any
    // other reason is not an alias.
    if (
      pin.keyReset === undefined &&
      pin.pinnedSignaturePubkeyB64 !== c.signaturePubkeyB64 &&
      (pin.provenPredecessorsB64 ?? []).includes(c.signaturePubkeyB64) &&
      !out.has(pin.pinnedSignaturePubkeyB64)
    ) {
      out.set(pin.pinnedSignaturePubkeyB64, c.signaturePubkeyB64);
    }
  }
  return out;
}

/** ADR-0066 section 3.5: the keys of a pinned account as `predecessor.ts`
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
 * re-check a person's address (ADR-0066 section 3.5a rule 6); a claimed
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

/** Every flagged contact of `owner` (default: the bound account) with its
 *  record — what the eviction arm and the picker read. */
export function loadFlaggedContacts(
  owner?: string,
): Array<{ contact: AcceptedContact; accountIdB64: string; reset: KeyResetRecord }> {
  const record = load(ownerOf(owner));
  const out: Array<{ contact: AcceptedContact; accountIdB64: string; reset: KeyResetRecord }> = [];
  for (const contact of record.accepted) {
    const accountIdB64 = accountIdB64OfHex(contact.accountIdHex);
    if (accountIdB64 === null) continue;
    const reset = record.pins[accountIdB64]?.keyReset;
    if (reset !== undefined) out.push({ contact, accountIdB64, reset });
  }
  return out;
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
