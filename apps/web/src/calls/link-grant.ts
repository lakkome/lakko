// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0078 - the `link` reach grant a `/c/` call link carries names the
 * owner's mailbox through its mailbox binding. The check lives in the core:
 * `CallInviteCapability.verifiedLinkGrantMailboxPk()` verifies the one-time
 * KeyPackage, the KeyPackage's account binding (ADR-0050) and the grant's
 * mailbox binding against the owner's account key, and returns the owner's
 * verified mailbox key, or throws.
 *
 * A capability whose check throws is refused as damaged (fail closed): the
 * grant would otherwise point a cold-start write at a mailbox the owner
 * cannot show is theirs. A write under the grant seals to and addresses the
 * returned key, never a key derived from anything else.
 */

/** The part of a `CallInviteCapability` the check reads. */
export interface LinkGrantCapability {
  verifiedLinkGrantMailboxPk(): Uint8Array;
}

/** The owner's verified mailbox key of a `/c/` link, or `null` when the
 *  core refuses the link grant. Never throws; logs without identifiers. */
export function callLinkGrantMailboxPk(cap: LinkGrantCapability): Uint8Array | null {
  try {
    return cap.verifiedLinkGrantMailboxPk();
  } catch {
    // eslint-disable-next-line no-console
    console.warn("ADR-0078: a call link was refused; its link grant's mailbox binding did not verify");
    return null;
  }
}
