// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/who-can-find-me.md` sections 4.1 and 8, ADR-0060 section 4 ("Only
 * the contacts I choose"): the account ids the setting names, as bytes, and
 * whether a change of the setting narrowed who can find the person through
 * their contacts.
 *
 * Who is allowed is `allowsContact`, one copy in Rust, read here and never
 * restated. No Solid and no storage; the rule arrives as an argument, so a test
 * hands it the real wasm.
 */
import type { VisibilitySetting } from "../../wasm/lakko_ffi_wasm";

/** The length of an account id. */
const ACCOUNT_ID_LEN = 16;

/**
 * What `allowsContact` is asked about a contact with no known account id. The
 * chosen list holds real ids only, so the answer is the setting's answer for
 * "any contact": yes under All and Everyone, no under Only the ones I choose.
 */
export const NO_ACCOUNT_ID = new Uint8Array(ACCOUNT_ID_LEN);

/** An account id in hex as bytes; `null` when it is not one. */
export function accountIdBytes(hex: string): Uint8Array | null {
  if (!/^[0-9a-fA-F]{32}$/.test(hex)) return null;
  const out = new Uint8Array(ACCOUNT_ID_LEN);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** Account ids as the setting's rules take a list of them: concatenated. An
 *  entry that is not an account id is left out. */
export function concatAccountIds(hexes: readonly string[]): Uint8Array {
  const ids = hexes.map(accountIdBytes).filter((id): id is Uint8Array => id !== null);
  const out = new Uint8Array(ids.length * ACCOUNT_ID_LEN);
  ids.forEach((id, i) => out.set(id, i * ACCOUNT_ID_LEN));
  return out;
}

/** The account ids of a concatenated list, as lower-case hex. */
export function accountIdsHexOf(concatenated: Uint8Array): string[] {
  const out: string[] = [];
  for (let i = 0; i + ACCOUNT_ID_LEN <= concatenated.length; i += ACCOUNT_ID_LEN) {
    let hex = "";
    for (const b of concatenated.subarray(i, i + ACCOUNT_ID_LEN)) hex += b.toString(16).padStart(2, "0");
    out.push(hex);
  }
  return out;
}

/** The rules {@link contactsNarrowed} asks. */
export interface ContactRules {
  allowsContact(setting: VisibilitySetting, accountId: Uint8Array): boolean;
  allowsContactsOfContacts(setting: VisibilitySetting): boolean;
}

/**
 * Whether going from `before` to `after` took someone out of Contacts of my
 * contacts while it stays on: a contact switched off, All replaced by Only the
 * ones I choose, or leaving Everyone for a chosen list. The `fof` key then
 * moves at once, so the old one stops opening the mailbox. With the part off
 * no `fof` key is registered, and widening needs no new key at all.
 */
export function contactsNarrowed(
  rules: ContactRules,
  before: VisibilitySetting,
  after: VisibilitySetting,
): boolean {
  if (!rules.allowsContactsOfContacts(after)) return false;
  if (rules.allowsContact(before, NO_ACCOUNT_ID) && !rules.allowsContact(after, NO_ACCOUNT_ID)) return true;
  return accountIdsHexOf(before.chosenContacts).some((hex) => {
    const id = accountIdBytes(hex);
    return id !== null && rules.allowsContact(before, id) && !rules.allowsContact(after, id);
  });
}
