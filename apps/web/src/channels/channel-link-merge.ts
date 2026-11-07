// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 phase B — one list of a channel's invite links out of this
 * device's records and the account's link register (UX 14.2: links sync
 * between one account's devices). Pure.
 */

/** One link as the Invite link screen lists it. */
export interface ChannelLinkView {
  /** Lower-case hex of the 6-byte ledger id: one link, on every device. */
  readonly linkIdHex: string;
  readonly url: string;
  readonly label: string;
  readonly expiryMs: number;
  readonly maxUses: number;
  readonly uses: number;
  readonly revoked: boolean;
  /** Made by this account (on this device or another of its devices). */
  readonly mine: boolean;
  readonly mintedAtMs: number;
}

/**
 * The union of the links this device minted (`local`) and the links the
 * register holds for the channel (`registered`), one entry per ledger id,
 * newest first. A link revoked on either side reads as revoked (revocation is
 * final); a use count is the larger one known.
 */
export function mergeChannelLinks(
  local: readonly ChannelLinkView[],
  registered: readonly ChannelLinkView[],
): ChannelLinkView[] {
  const byId = new Map<string, ChannelLinkView>();
  for (const l of local) byId.set(l.linkIdHex.toLowerCase(), l);
  for (const r of registered) {
    const id = r.linkIdHex.toLowerCase();
    const have = byId.get(id);
    if (have === undefined) {
      byId.set(id, r);
      continue;
    }
    byId.set(id, {
      ...have,
      revoked: have.revoked || r.revoked,
      uses: Math.max(have.uses, r.uses),
      label: have.label.length > 0 ? have.label : r.label,
    });
  }
  return [...byId.values()].sort((a, b) => b.mintedAtMs - a.mintedAtMs);
}
