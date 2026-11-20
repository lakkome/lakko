// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 section 2 — the account id the on-screen directory check asks
 * about for a person key. A contact's own record first (ADR-0021, trusted);
 * for a group member who is not a contact, the id their own profile v2
 * claims (ADR-0048); then a pending contact's; then the id kept with the
 * person's status record. `null` when none is known: the person is not
 * asked about.
 */

/** Where an account id for a person key can come from. */
export interface PersonAccountIdSources {
  /** A contact record's account id, standard base64, or `null`. */
  readonly contactIdB64: (keyB64: string) => string | null;
  /** The id the person's received profile claims, lower-case hex. */
  readonly profileIdHex: (keyB64: string) => string | null;
  /** A pending contact's account id, lower-case hex. */
  readonly pendingIdHex: (keyB64: string) => string | null;
  /** The id kept with the person's status record, lower-case hex. */
  readonly recordIdHex: (keyB64: string) => string | null;
  /** Standard base64 decode; throws on bad input. */
  readonly decodeB64: (b64: string) => Uint8Array;
}

const HEX_ID = /^[0-9a-f]{32}$/;

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** The 16-byte account id this device holds for `keyB64`, as lower-case hex,
 *  or `null`. A malformed value from one source falls through to the next. */
export function personAccountIdHex(keyB64: string, sources: PersonAccountIdSources): string | null {
  const contact = sources.contactIdB64(keyB64);
  if (contact !== null) {
    try {
      const bytes = sources.decodeB64(contact);
      if (bytes.length === 16) return hexOf(bytes);
    } catch {
      // fall through
    }
  }
  for (const next of [sources.profileIdHex, sources.pendingIdHex, sources.recordIdHex]) {
    const hex = next(keyB64);
    if (hex !== null && HEX_ID.test(hex)) return hex;
  }
  return null;
}
