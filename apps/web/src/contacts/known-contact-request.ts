// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.7 case 1 / UX 16.6 - a contact request (or an `/i/` link) from
 * an account that is ALREADY a contact.
 *
 * A person who reset their keys lost their contacts and may find the other
 * person again before that person's device noticed anything. The device
 * never shows a request for someone who is already a contact: it runs the
 * R.2 check on the sender's account id and then answers the request itself
 * (the conversation's successor is the answer) or drops it. Nothing is
 * shown either way, and on a refusal nothing is sent to the keys that sent
 * the request.
 *
 * Pure: `contacts/MailboxContext.tsx` gathers the facts and acts.
 */

/** How the identity check of the sender's account ended. */
export type KnownRequestCheck =
  /** The gate passed: the published key is what this device now trusts (the
   *  pin, a proven successor, or an attested reset just accepted). */
  | "current"
  /** The gate refused the published key (`SupersessionRefused`). */
  | "refused"
  /** No verdict yet: the directory, the attest key or the key log could not
   *  be reached, or the account publishes nothing. */
  | "inconclusive";

export type KnownRequestVerdict =
  /** The sender is not a contact: the ordinary request row. */
  | "show"
  /** A contact, and the request is signed by the key this device trusts for
   *  the account: answered silently (ack; the conversation continues by
   *  itself). No request row. */
  | "answer"
  /** A contact, and the request is NOT from the key this device trusts for
   *  the account (the check refused, or it is some other key): nothing is
   *  shown and nothing is sent. */
  | "drop"
  /** A contact, and there is no verdict yet: the request waits, unshown,
   *  for the next check. */
  | "wait";

/**
 * | sender is a contact | check          | request key is the trusted key | verdict  |
 * |---------------------|----------------|--------------------------------|----------|
 * | no                  | -              | -                              | `show`   |
 * | yes                 | `refused`      | -                              | `drop`   |
 * | yes                 | `inconclusive` | -                              | `wait`   |
 * | yes                 | `current`      | yes                            | `answer` |
 * | yes                 | `current`      | no                             | `drop`   |
 *
 * `trustedKeyB64` is the pin AFTER the check ran; `null` (no pin) cannot
 * happen for a contact with a known account id once the check passed, and
 * reads as "not the trusted key".
 */
export function knownContactRequestVerdict(input: {
  readonly senderIsContact: boolean;
  readonly check: KnownRequestCheck;
  readonly requestKeyB64: string;
  readonly trustedKeyB64: string | null;
}): KnownRequestVerdict {
  if (!input.senderIsContact) return "show";
  if (input.check === "refused") return "drop";
  if (input.check === "inconclusive") return "wait";
  return input.trustedKeyB64 === input.requestKeyB64 ? "answer" : "drop";
}

/** The slice of a contact row this reads. */
export interface ContactAccountRow {
  readonly signaturePubkeyB64: string;
  readonly accountIdHex?: string;
}

/**
 * The contact row that makes `accountIdHex` an ESTABLISHED contact, or
 * `undefined`.
 *
 * A contact row's account id can come from the peer's OWN profile envelope
 * (`OneToOneContactAdoption.tsx` completes a row from it), and a member can
 * write any account id there. Such a row establishes nothing: a member who
 * claimed another account's id would otherwise make that account's later,
 * genuine request vanish (answered silently, never listed). So a row counts
 * only when its key is one this device verified for the account itself.
 * The lookup is by the ACCOUNT, never by the key (the key is exactly what a
 * reset changes):
 *
 * | a row names the account | the pin names that row's key | a verified binding names that row's key | result    |
 * |-------------------------|------------------------------|-----------------------------------------|-----------|
 * | no                      | -                            | -                                       | none      |
 * | yes                     | yes                          | any                                     | that row  |
 * | yes                     | no                           | yes                                     | that row  |
 * | yes                     | no                           | no                                      | none      |
 *
 * "The pin names the key" means the pinned key or one of its PROVEN
 * predecessors (ADR-0074 section 3.5: after a proven rotation the row keeps
 * the key the person's leaves are bound to). A key a reset retired is not
 * among them. Every row naming the account is considered, so a claimed row
 * cannot hide the real one, and a pin written for the account's REAL key
 * (by this check or any earlier fetch) never certifies a claimed row: the
 * comparison is with the row's own key.
 */
export function establishedContactOfAccount<T extends ContactAccountRow>(
  accepted: readonly T[],
  accountIdHex: string | null | undefined,
  verified: { readonly pinKeysB64: readonly string[]; readonly bindingKeyB64: string | null },
): T | undefined {
  if (accountIdHex === null || accountIdHex === undefined || accountIdHex.length === 0) return undefined;
  const wanted = accountIdHex.toLowerCase();
  return accepted.find(
    (c) =>
      c.accountIdHex !== undefined &&
      c.accountIdHex.toLowerCase() === wanted &&
      c.signaturePubkeyB64.length > 0 &&
      (verified.pinKeysB64.includes(c.signaturePubkeyB64) ||
        c.signaturePubkeyB64 === verified.bindingKeyB64),
  );
}

/**
 * The contact row under the key that signed a request, or `undefined`.
 *
 * A request reaches this code with its signature verified, so the sender
 * holds that key, and a row under the same key is that person whatever the
 * row says about their account: a row with no account id, or one no pin or
 * binding vouches for yet, is still a contact and must be answered. Treating
 * it as a stranger's request drops it silently (the inbox keeps no request
 * from a contact's key) and leaves the sender waiting for good. A member who
 * claimed someone else's account id gains nothing: the comparison is with the
 * key that signed.
 */
export function contactSignedBy<T extends ContactAccountRow>(
  accepted: readonly T[],
  requestKeyB64: string,
): T | undefined {
  if (requestKeyB64.length === 0) return undefined;
  return accepted.find((c) => c.signaturePubkeyB64 === requestKeyB64);
}

/** The keys a pin names for an account: the pinned key and its proven
 *  predecessors; empty when the account is not pinned. */
export function pinKeysOf(
  keys: { readonly currentKeyB64: string; readonly provenPredecessorsB64: readonly string[] } | undefined,
): string[] {
  return keys === undefined ? [] : [keys.currentKeyB64, ...keys.provenPredecessorsB64];
}
