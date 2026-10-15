// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0078 - the `link` reach grant a `/c/` call link carries names the
 * owner's mailbox through its mailbox binding, and the core does not check it
 * against anything (only `/i/` capabilities are checked in the core). The
 * joiner checks it here, against the owner's ACCOUNT key: the account the
 * capability's one-time KeyPackage is bound to (ADR-0050), or - for an
 * unbound KeyPackage - the owner key the capability is signed with.
 *
 * A capability whose grant fails is refused as damaged (fail closed): the
 * grant would otherwise point a cold-start write at a mailbox the owner
 * cannot show is theirs.
 */

/** The part of the wasm surface the check needs, structurally. */
export interface LinkGrantWasm {
  keyPackageAccountKey(keyPackage: Uint8Array): Uint8Array | undefined;
}

/** The part of a `CallInviteCapability` the check reads. */
export interface LinkGrantCapability {
  readonly oneTimeKeyPackage: Uint8Array;
  readonly ownerSignaturePubkey: Uint8Array;
  readonly linkGrant: { mailboxPkVerified(ownerAccountPk: Uint8Array): Uint8Array; free(): void };
}

/** The owner account key the link grant must verify against. */
export function callLinkOwnerAccountKey(wasm: LinkGrantWasm, cap: LinkGrantCapability): Uint8Array {
  let bound: Uint8Array | undefined;
  try {
    bound = wasm.keyPackageAccountKey(cap.oneTimeKeyPackage);
  } catch {
    bound = undefined;
  }
  return bound ?? cap.ownerSignaturePubkey;
}

/** Whether the `/c/` link grant's mailbox binding verifies against the
 *  owner's account key. Never throws; logs without identifiers. */
export function callLinkGrantVerifies(wasm: LinkGrantWasm, cap: LinkGrantCapability): boolean {
  let grant: LinkGrantCapability["linkGrant"];
  try {
    grant = cap.linkGrant;
  } catch {
    return false;
  }
  try {
    grant.mailboxPkVerified(callLinkOwnerAccountKey(wasm, cap));
    return true;
  } catch {
    // eslint-disable-next-line no-console
    console.warn("ADR-0078: a call link was refused; its link grant's mailbox binding did not verify");
    return false;
  } finally {
    try {
      grant.free();
    } catch {
      /* already freed */
    }
  }
}
