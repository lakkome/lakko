// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The label of one pending external-join request in the owner's members panel.
 *
 * A request is keyed on the joiner's DEVICE leaf key (ADR-0050: every device
 * is its own leaf), while contacts, profiles and renames are keyed on the
 * ACCOUNT key. Labelling by the leaf key therefore showed a contact as
 * "Member". The core resolves the account key behind the request from the
 * joiner's verified device binding (`Group.pendingExternalJoinAccountKey`);
 * the label is resolved through that key first, and falls back to the leaf
 * key when the core has none or the account key names nobody this device
 * knows, which keeps the old label for a non-contact.
 *
 * Only the LABEL changes: approve and reject still go by the leaf key.
 *
 * Pure: the group, the base64 encoder and the name resolver are handed in.
 */
import type { ResolvedName } from "../profile/resolver";
import { memberListLabelOf } from "../profile/member-label";

/** The part of the wasm `Group` this reads. */
export interface PendingJoinGroup {
  pendingExternalJoinAccountKey(joinerSignatureKey: Uint8Array): Uint8Array | undefined;
}

/** The list label for the pending request whose leaf key is `leafKey`. */
export function pendingJoinLabel(
  group: PendingJoinGroup,
  leafKey: Uint8Array,
  encodeB64: (bytes: Uint8Array) => string,
  resolve: (keyB64: string) => ResolvedName,
): string {
  let accountKey: Uint8Array | undefined;
  try {
    accountKey = group.pendingExternalJoinAccountKey(leafKey);
  } catch {
    accountKey = undefined;
  }
  if (accountKey !== undefined && accountKey.length === 32) {
    const byAccount = resolve(encodeB64(accountKey));
    if (byAccount.source !== "fingerprint") return memberListLabelOf(byAccount);
  }
  return memberListLabelOf(resolve(encodeB64(leafKey)));
}
