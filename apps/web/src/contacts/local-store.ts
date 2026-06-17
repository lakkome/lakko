// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * On-device persistence for the contacts UI (1.16d3d).
 *
 * Two pieces survive a page reload:
 *
 *   1. **Mailbox cursor** (`number`): the highest DS message id
 *      we have already processed. Lets the next poll skip the
 *      backlog we already showed the user.
 *
 *   2. **Accepted contacts** (`AcceptedContact[]`): the local
 *      "address book". Holds the public keys we'll need to send
 *      messages back to each contact, plus a user-typed display
 *      label. Bytes-only; no secrets.
 *
 * Both live in `localStorage` for now. The values are not
 * secret — public keys are public by definition, and the cursor
 * is just a row id. Phase 1.16f will move them to an encrypted
 * IDB store once `lakko-storage`'s wasm-targeted backend lands.
 *
 * **Pending requests are NOT persisted.** They live in the
 * Solid component's signal during the current page session and
 * get re-fetched on every refresh — the source of truth is the
 * DS mailbox, replaying the cursor from `0` reproduces the same
 * list. Persisting decoded `intro` plaintext to localStorage
 * would be a regression — those bytes are out of the WebCrypto
 * sandbox.
 *
 * ## G-4 (ADR-0016): per-account account-pubkey PIN store
 *
 * The third persisted piece (schema v2) is the **account-pubkey
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
 * fetch-path verify in `contacts/client.ts`.
 */

/** Current schema version. v1 (`lakko.contacts.v1`) carried only
 *  `cursor` + `accepted`; v2 adds the account-pubkey `pins` map
 *  (G-4, ADR-0016) and migrates the v1 row additively on first
 *  read so existing users keep their contacts. */
const STORAGE_KEY = "lakko.contacts.v2";
/** Legacy v1 key, read once during migration then left in place
 *  (harmless; we never write it again). */
const LEGACY_STORAGE_KEY_V1 = "lakko.contacts.v1";

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
  /** User-chosen display label. Defaults to a fingerprint hex
   *  prefix on accept; the user can edit later. */
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

interface PersistedV2 {
  v: 2;
  /** Highest DS message id already processed. */
  cursor: number;
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

function emptyRecord(): PersistedV2 {
  return { v: 2, cursor: 0, accepted: [], pins: {} };
}

/** Read + validate the v1 legacy row, returning its `cursor` +
 *  `accepted` (or `null` if absent / malformed). The v1 schema
 *  had no `pins`, so the migration seeds an empty map. */
function loadLegacyV1(): { cursor: number; accepted: AcceptedContact[] } | null {
  const raw = localStorage.getItem(LEGACY_STORAGE_KEY_V1);
  if (raw === null) return null;
  try {
    const parsed = JSON.parse(raw) as {
      v?: unknown;
      cursor?: unknown;
      accepted?: unknown;
    };
    if (
      parsed.v === 1 &&
      typeof parsed.cursor === "number" &&
      Array.isArray(parsed.accepted)
    ) {
      return {
        cursor: parsed.cursor,
        accepted: parsed.accepted.filter(isAcceptedContact),
      };
    }
  } catch {
    // Fall through — malformed legacy row, drop it.
  }
  return null;
}

function load(): PersistedV2 {
  if (typeof localStorage === "undefined") {
    return emptyRecord();
  }
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw === null) {
    // No v2 row yet. Migrate the v1 row additively if present so
    // existing users don't lose their contacts on the schema bump.
    const legacy = loadLegacyV1();
    if (legacy !== null) {
      return { v: 2, cursor: legacy.cursor, accepted: legacy.accepted, pins: {} };
    }
    return emptyRecord();
  }
  try {
    const parsed = JSON.parse(raw) as Partial<PersistedV2>;
    if (
      parsed.v === 2 &&
      typeof parsed.cursor === "number" &&
      Array.isArray(parsed.accepted)
    ) {
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
        v: 2,
        cursor: parsed.cursor,
        accepted: parsed.accepted.filter(isAcceptedContact),
        pins,
      };
    }
  } catch {
    // Fall through to the empty default.
  }
  return emptyRecord();
}

function save(record: PersistedV2): void {
  if (typeof localStorage === "undefined") return;
  localStorage.setItem(STORAGE_KEY, JSON.stringify(record));
}

/** Read the persisted cursor. Defaults to `0` when there's no
 *  record yet (first-ever poll). */
export function loadCursor(): number {
  return load().cursor;
}

/** Update the cursor (and rewrite the accepted list along with
 *  it — the schema is one row). */
export function saveCursor(cursor: number): void {
  const current = load();
  save({ ...current, cursor });
}

/** Read the accepted-contacts list. */
export function loadAccepted(): AcceptedContact[] {
  return load().accepted;
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

/** Append (or replace by `signaturePubkeyB64`) an accepted
 *  contact and persist. Returns the new list. */
export function upsertAccepted(contact: AcceptedContact): AcceptedContact[] {
  const current = load();
  const existing = current.accepted.findIndex(
    (a) => a.signaturePubkeyB64 === contact.signaturePubkeyB64,
  );
  const next =
    existing === -1
      ? [...current.accepted, contact]
      : current.accepted.map((a, i) => (i === existing ? contact : a));
  save({ ...current, accepted: next });
  return next;
}

/** Remove an accepted contact by its signature pubkey. No-op
 *  when there's no match. */
export function removeAccepted(signaturePubkeyB64: string): AcceptedContact[] {
  const current = load();
  const next = current.accepted.filter(
    (a) => a.signaturePubkeyB64 !== signaturePubkeyB64,
  );
  save({ ...current, accepted: next });
  return next;
}

/** Wipe everything. Called from the AppShell's "Forget device"
 *  flow in a follow-up sprint. Today the AuthContext doesn't
 *  call this — leaving it here so 1.16d3d's persistence isn't
 *  surprise-survived after a deliberate device wipe. Drops both
 *  the v2 row and the legacy v1 row (G-4) so a wipe truly clears
 *  the device. */
export function clearAll(): void {
  if (typeof localStorage === "undefined") return;
  localStorage.removeItem(STORAGE_KEY);
  localStorage.removeItem(LEGACY_STORAGE_KEY_V1);
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
export function readPin(accountIdB64: string): AccountPubkeyPin | undefined {
  return load().pins[accountIdB64];
}

/** Outcome of a [`setPin`] call. `"set"` covers both a first-time
 *  TOFU-pin and a monotonic forward re-pin; `"rejected-backward"`
 *  signals the marker would have moved backwards (ADR-0016 rule
 *  #4) so nothing was written. */
export type SetPinResult = "set" | "rejected-backward";

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
): SetPinResult {
  const current = load();
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
  save({ ...current, pins: nextPins });
  return "set";
}

/** Remove the pin for `accountIdB64`. No-op when absent. Exposed
 *  for the "forget contact" / re-verify-from-scratch flows. */
export function removePin(accountIdB64: string): void {
  const current = load();
  if (current.pins[accountIdB64] === undefined) return;
  const nextPins = { ...current.pins };
  delete nextPins[accountIdB64];
  save({ ...current, pins: nextPins });
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
): boolean {
  const current = load();
  const existing = current.pins[accountIdB64];
  if (existing === undefined) return false;
  if (existing.logStatus === logStatus) return true; // idempotent, no write
  const nextPins: Record<string, AccountPubkeyPin> = {
    ...current.pins,
    [accountIdB64]: { ...existing, logStatus },
  };
  save({ ...current, pins: nextPins });
  return true;
}

/** Read the recorded log-verify status for an account, or `undefined`
 *  when no pin / no status. Exposed for the UI signal. */
export function readLogStatus(
  accountIdB64: string,
): LogVerifyStatus | undefined {
  return load().pins[accountIdB64]?.logStatus;
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
): boolean {
  const current = load();
  const existing = current.pins[accountIdB64];
  if (existing === undefined) return false;
  const currentValue = existing.isReset;
  // Normalise: `null` means "clear" (no marker), a boolean means "set".
  if (isReset === null) {
    if (currentValue === undefined) return true; // already clear, no write
    const cleared: AccountPubkeyPin = { ...existing };
    delete cleared.isReset;
    save({
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
  save({ ...current, pins: nextPins });
  return true;
}

/** Read the recorded `is_reset` marker (KT-log S4) for an account, or
 *  `undefined` when no pin / no recorded marker. Exposed for the UI. */
export function readLogReset(accountIdB64: string): boolean | undefined {
  return load().pins[accountIdB64]?.isReset;
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
): boolean {
  const current = load();
  const existing = current.pins[accountIdB64];
  if (existing === undefined) return false;
  const currentValue = existing.pinnedLogVersion;
  // Normalise: `null` means "clear" (no version), a number means "set".
  if (version === null) {
    if (currentValue === undefined) return true; // already clear, no write
    const cleared: AccountPubkeyPin = { ...existing };
    delete cleared.pinnedLogVersion;
    save({
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
  save({ ...current, pins: nextPins });
  return true;
}

/** Read the recorded chain-confirmed log version (G-30 #2) for an
 *  account, or `undefined` when no pin / no recorded version. The next
 *  rotation's chain proof is checked against this as the downgrade
 *  floor (default `0` when absent). */
export function readLogVersion(accountIdB64: string): number | undefined {
  return load().pins[accountIdB64]?.pinnedLogVersion;
}

/** Render a short human-readable hex prefix for a public key.
 *  Used as the default `displayName` on accept. */
export function fingerprintShort(pubkey: Uint8Array): string {
  let out = "";
  for (let i = 0; i < Math.min(pubkey.length, 4); i++) {
    out += (pubkey[i] ?? 0).toString(16).padStart(2, "0");
  }
  return out;
}
