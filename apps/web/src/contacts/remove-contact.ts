// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Remove contact, the one way it runs (the Contacts card, and since
 * ADR-0099 the block that performs it too, UX `reporting-and-moderation.md`
 * section 1.2).
 *
 * Three steps, each the one already in place: the row leaves the contact
 * list (kept as removed, so a one-to-one that remains offers `Add to
 * contacts`), the `contact` reach key is rotated so the key handed to them
 * stops working at once (ADR-0060 section 1), and they leave the chosen list
 * of "Who can find me" (adding them back brings them in switched off). What
 * they offered or relayed for contacts of contacts is forgotten too, so this
 * device stops relaying their record (ADR-0060 section 4).
 */

import { base64Decode } from "../auth/account-id";
import { journalHex } from "../ds/access";
import { onContactRemoved } from "../reach/reach-service";
import { dropIntroductionsFrom } from "../reach/scoped-intake";
import { personKeysHex } from "./address-resolver";
import { forgetChosenContact } from "../settings/visibility-actions";
import { loadAccepted, removeAccepted, type AcceptedContact } from "./local-store";

/** Remove the contact whose account key is `keyB64`. Returns the list as it
 *  now stands. A person who is not on the list still has the reach key
 *  rotated: a block must stop their way to the inbox whatever the list held. */
export function removeContactByKey(keyB64: string): AcceptedContact[] {
  const contact = loadAccepted().find((c) => c.signaturePubkeyB64 === keyB64);
  const next = removeAccepted(keyB64);
  let key: Uint8Array | null = null;
  try {
    key = base64Decode(keyB64);
  } catch {
    key = null;
  }
  if (key !== null && key.length === 32) {
    void onContactRemoved(key);
    // ADR-0060 section 4 - what they offered and relayed is forgotten, under
    // every key they have had, so their record stops being relayed.
    void dropIntroductionsFrom(personKeysHex(journalHex(key)));
  }
  const accountIdHex = contact?.accountIdHex;
  if (accountIdHex !== undefined) {
    void forgetChosenContact(accountIdHex).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("[privacy] a removed contact stayed in the chosen list", err);
    });
  }
  return next;
}
