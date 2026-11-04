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
import { memberListLabelOf, memberNameOf } from "../profile/member-label";
import { knockClaimedName } from "../ds/knock-seeds";

/** The part of the wasm `Group` this reads. */
export interface PendingJoinGroup {
  pendingExternalJoinAccountKey(joinerSignatureKey: Uint8Array): Uint8Array | undefined;
}

/**
 * ADR-0084 - the key a pending joiner's photo is looked up under: the
 * ACCOUNT key the request's device binding names, else the leaf key. The
 * same choice `pendingJoinLabel` makes for the name.
 */
export function pendingJoinPersonKey(
  group: PendingJoinGroup,
  leafKey: Uint8Array,
  encodeB64: (bytes: Uint8Array) => string,
): string {
  let accountKey: Uint8Array | undefined;
  try {
    accountKey = group.pendingExternalJoinAccountKey(leafKey);
  } catch {
    accountKey = undefined;
  }
  return encodeB64(accountKey !== undefined && accountKey.length === 32 ? accountKey : leafKey);
}

/** The resolved name of the pending request whose leaf key is `leafKey`:
 *  through the account key the device binding names when it names somebody
 *  this device knows, else through the leaf key. */
function resolvePendingJoin(
  group: PendingJoinGroup,
  leafKey: Uint8Array,
  encodeB64: (bytes: Uint8Array) => string,
  resolve: (keyB64: string) => ResolvedName,
): ResolvedName {
  let accountKey: Uint8Array | undefined;
  try {
    accountKey = group.pendingExternalJoinAccountKey(leafKey);
  } catch {
    accountKey = undefined;
  }
  if (accountKey !== undefined && accountKey.length === 32) {
    const byAccount = resolve(encodeB64(accountKey));
    if (byAccount.source !== "fingerprint") return byAccount;
  }
  const byLeaf = resolve(encodeB64(leafKey));
  // ADR-0087 UX section 8 — the name a request carries, unverified, when the
  // device knows nothing better.
  const claimed = byLeaf.source === "fingerprint" ? knockClaimedName(encodeB64(leafKey)) : null;
  return claimed === null ? byLeaf : { ...byLeaf, name: claimed, listLabel: claimed, source: "profile" };
}

/** The list label for the pending request whose leaf key is `leafKey`
 *  (`Name · @username` when there is one). */
export function pendingJoinLabel(
  group: PendingJoinGroup,
  leafKey: Uint8Array,
  encodeB64: (bytes: Uint8Array) => string,
  resolve: (keyB64: string) => ResolvedName,
): string {
  return memberListLabelOf(resolvePendingJoin(group, leafKey, encodeB64, resolve));
}

/** The name alone, for the request's avatar initials: never the list label,
 *  whose ` · @username` would become an initial (seen live 2026-10-29). */
export function pendingJoinName(
  group: PendingJoinGroup,
  leafKey: Uint8Array,
  encodeB64: (bytes: Uint8Array) => string,
  resolve: (keyB64: string) => ResolvedName,
): string {
  return memberNameOf(resolvePendingJoin(group, leafKey, encodeB64, resolve));
}
