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
}

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
    typeof (a as AcceptedContact).acceptedAtMs === "number"
  );
}

/** Narrowing guard for one persisted `AccountPubkeyPin`. */
function isAccountPubkeyPin(p: unknown): p is AccountPubkeyPin {
  return (
    typeof p === "object" &&
    p !== null &&
    typeof (p as AccountPubkeyPin).pinnedSignaturePubkeyB64 === "string" &&
    typeof (p as AccountPubkeyPin).rotationMarker === "number"
  );
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
  const nextPins: Record<string, AccountPubkeyPin> = {
    ...current.pins,
    [accountIdB64]: { pinnedSignaturePubkeyB64, rotationMarker },
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

/** Render a short human-readable hex prefix for a public key.
 *  Used as the default `displayName` on accept. */
export function fingerprintShort(pubkey: Uint8Array): string {
  let out = "";
  for (let i = 0; i < Math.min(pubkey.length, 4); i++) {
    out += (pubkey[i] ?? 0).toString(16).padStart(2, "0");
  }
  return out;
}
