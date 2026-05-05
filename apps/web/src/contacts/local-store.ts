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
 */

const STORAGE_KEY = "lakko.contacts.v1";

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

interface PersistedV1 {
  v: 1;
  /** Highest DS message id already processed. */
  cursor: number;
  accepted: AcceptedContact[];
}

function load(): PersistedV1 {
  if (typeof localStorage === "undefined") {
    return { v: 1, cursor: 0, accepted: [] };
  }
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw === null) {
    return { v: 1, cursor: 0, accepted: [] };
  }
  try {
    const parsed = JSON.parse(raw) as Partial<PersistedV1>;
    if (
      parsed.v === 1 &&
      typeof parsed.cursor === "number" &&
      Array.isArray(parsed.accepted)
    ) {
      return {
        v: 1,
        cursor: parsed.cursor,
        // Filter to entries that look structurally well-formed.
        // A schema-bumped `v: 2` row would land here on
        // downgrade and we'd rather drop it than mis-render.
        accepted: parsed.accepted.filter(
          (a): a is AcceptedContact =>
            typeof a === "object" &&
            a !== null &&
            typeof (a as AcceptedContact).signaturePubkeyB64 === "string" &&
            typeof (a as AcceptedContact).displayName === "string" &&
            typeof (a as AcceptedContact).acceptedAtMs === "number",
        ),
      };
    }
  } catch {
    // Fall through to the empty default.
  }
  return { v: 1, cursor: 0, accepted: [] };
}

function save(record: PersistedV1): void {
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
 *  surprise-survived after a deliberate device wipe. */
export function clearAll(): void {
  if (typeof localStorage === "undefined") return;
  localStorage.removeItem(STORAGE_KEY);
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
