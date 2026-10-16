// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Words for an invite link's expiry and remaining uses, straight off the
 * wire sentinels the server enforces: `expiry_ms == 0` never expires;
 * `max_uses == 0` is single-use and `0xFFFFFFFF` unlimited. Pure; tested in
 * `invite-describe.test.ts`. The words are catalog messages
 * (`invite.links.*`, ADR-0061).
 */
import { currentLanguage, t } from "../l10n";

/** The `max_uses` sentinel for an unlimited link. */
export const INVITE_USES_UNLIMITED = 0xffff_ffff;

/** "Expires in 3d" / "Expires in 2h" / "Expires in 5m" / "Never expires" /
 *  "Expired". */
export function describeInviteExpiry(expiryMs: number, nowMs: number): string {
  if (expiryMs === 0) return t("invite.links.never_expires");
  if (expiryMs <= nowMs) return t("invite.links.expired");
  const deltaMs = expiryMs - nowMs;
  const days = Math.floor(deltaMs / 86_400_000);
  if (days >= 1) return t("invite.links.expires_days", { count: days });
  const hours = Math.floor(deltaMs / 3_600_000);
  if (hours >= 1) return t("invite.links.expires_hours", { count: hours });
  const minutes = Math.max(1, Math.floor(deltaMs / 60_000));
  return t("invite.links.expires_minutes", { count: minutes });
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

/** "Created 16 Sep" style date (formatted for `locale`, by default the
 *  interface language), or an empty string for an unparseable timestamp. */
export function describeInviteCreated(createdAt: string, locale?: string): string {
  const ms = Date.parse(createdAt);
  if (Number.isNaN(ms)) return "";
  const date = new Date(ms).toLocaleDateString(locale ?? currentLanguage(), {
    day: "numeric",
    month: "short",
  });
  return t("invite.links.created", { date });
}
