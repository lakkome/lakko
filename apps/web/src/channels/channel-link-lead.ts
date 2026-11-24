// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The first line of a channel's link page (`docs/ux/group-and-channel-privacy.md`
 * section 7.2): "Anna invites you to", the channel's name following on its
 * own line. When the link carries no inviter name (one minted before the
 * name was carried, or a name the link could not hold), "You are invited
 * to". A publisher invitation has its own line (section 15.2) and none of
 * these. Pure.
 */
import { t } from "../l10n";

export function channelLinkLead(opened: { readonly publisherInvite: boolean; readonly inviterName?: string }): string | null {
  if (opened.publisherInvite) return null;
  const name = opened.inviterName?.trim() ?? "";
  return name.length > 0 ? t("invite.group.lead_named", { name }) : t("invite.group.lead_anonymous");
}
