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
import { adoptOneToOnePeer, type OneToOnePeer } from "./adopt-one-to-one";

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
      typeof (a as AcceptedContact).groupIdB64 === "string")
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
): AcceptedContact | undefined {
  for (const b64 of memberSigPubkeysB64) {
    if (b64 === ownSigPubkeyB64) continue;
    const contact = accepted.find((c) => c.signaturePubkeyB64 === b64);
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
  save(account, { ...current, accepted: next });
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
    },
  };
  save(account, { ...current, pins: nextPins });
  return "set";
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
