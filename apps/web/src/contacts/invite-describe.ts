// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Words for an invite link's expiry and remaining uses, straight off the
 * wire sentinels the server enforces: `expiry_ms == 0` never expires;
 * `max_uses == 0` is single-use and `0xFFFFFFFF` unlimited. Pure; tested in
 * `invite-describe.test.ts`. The words are catalog messages
 * (`invite.links.*`, ADR-0061).
 */
import { t } from "../l10n";
import { displayLocale } from "../l10n/display-locale";

/** The `max_uses` sentinel for an unlimited link. */
export const INVITE_USES_UNLIMITED = 0xffff_ffff;

/** "Expires in 3d" / "Expires in 2h" / "Expires in 5m" / "Never expires" /
 *  "Expired". */
export function describeInviteExpiry(expiryMs: number, nowMs: number): string {
  if (expiryMs === 0) return t("invite.links.never_expires");
  if (expiryMs <= nowMs) return t("invite.links.expired");
  // Rounded to the nearest unit, not down: a 7-day link made an hour ago
  // reads "in 7d", not "in 6d". The same rule on iOS and Android.
  const deltaMs = expiryMs - nowMs;
  const minutes = Math.max(1, Math.ceil(deltaMs / 60_000));
  if (minutes < 60) return t("invite.links.expires_minutes", { count: minutes });
  const hours = Math.round(deltaMs / 3_600_000);
  if (hours < 24) return t("invite.links.expires_hours", { count: hours });
  return t("invite.links.expires_days", { count: Math.round(deltaMs / 86_400_000) });
}

/** Uses left, or `null` for an unlimited link. Never below zero. */
export function inviteUsesLeft(usedCount: number, maxUses: number): number | null {
  if (maxUses === INVITE_USES_UNLIMITED) return null;
  const cap = maxUses === 0 ? 1 : maxUses;
  return Math.max(0, cap - usedCount);
}

/** "1 use left" / "3 uses left" / "Used up" / "Unlimited uses". */
export function describeInviteUses(usedCount: number, maxUses: number): string {
  const left = inviteUsesLeft(usedCount, maxUses);
  if (left === null) return t("invite.links.unlimited_uses");
  if (left === 0) return t("invite.links.used_up");
  return t("invite.links.uses_left", { count: left });
}

/** The limit a link was minted with: "single-use" / "up to 10 uses" /
 *  "unlimited". Since ADR-0078 section 10 the owner learns no use count
 *  from the ledger, so the list states the limit, not what is left. */
export function describeInviteLimit(maxUses: number): string {
  if (maxUses === INVITE_USES_UNLIMITED) return t("invite.uses.unlimited");
  if (maxUses === 0 || maxUses === 1) return t("invite.uses.single");
  return t("invite.uses.up_to", { count: maxUses });
}

/** The uses fact of a link in the account's own list, which knows the use
 *  count (ADR-0079 register): the limit until someone used the link, then
 *  what is left of it. A single-use link leaves the list once used and an
 *  unlimited one has nothing to count down, so both state their limit. */
export function describeInviteUsage(usedCount: number, maxUses: number): string {
  const single = maxUses === 0 || maxUses === 1;
  if (single || maxUses === INVITE_USES_UNLIMITED || usedCount <= 0) return describeInviteLimit(maxUses);
  return describeInviteUses(usedCount, maxUses);
}

/** "Created 16 Sep, 14:32" style date and time (formatted for `locale`, by
 *  default the interface language), or an empty string for an unparseable
 *  timestamp. The time is there so two links made the same day can be told
 *  apart. */
export function describeInviteCreated(createdAt: string, locale?: string): string {
  const ms = Date.parse(createdAt);
  if (Number.isNaN(ms)) return "";
  const date = new Date(ms).toLocaleString(locale ?? displayLocale(), {
    day: "numeric",
    month: "short",
    hour: "numeric",
    minute: "2-digit",
  });
  return t("invite.links.created", { date });
}
