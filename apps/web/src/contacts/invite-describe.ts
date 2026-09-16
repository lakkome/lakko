// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Words for an invite link's expiry and remaining uses, straight off the
 * wire sentinels the server enforces: `expiry_ms == 0` never expires;
 * `max_uses == 0` is single-use and `0xFFFFFFFF` unlimited. Pure; tested in
 * `invite-describe.test.ts`.
 */

/** The `max_uses` sentinel for an unlimited link. */
export const INVITE_USES_UNLIMITED = 0xffff_ffff;

/** "Expires in 3d" / "Expires in 2h" / "Expires in 5m" / "Never expires" /
 *  "Expired". */
export function describeInviteExpiry(expiryMs: number, nowMs: number): string {
  if (expiryMs === 0) return "Never expires";
  if (expiryMs <= nowMs) return "Expired";
  const deltaMs = expiryMs - nowMs;
  const days = Math.floor(deltaMs / 86_400_000);
  if (days >= 1) return `Expires in ${days}d`;
  const hours = Math.floor(deltaMs / 3_600_000);
  if (hours >= 1) return `Expires in ${hours}h`;
  const minutes = Math.max(1, Math.floor(deltaMs / 60_000));
  return `Expires in ${minutes}m`;
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
  if (left === null) return "Unlimited uses";
  if (left === 0) return "Used up";
  return `${left} ${left === 1 ? "use" : "uses"} left`;
}

/** "Created 16 Sep" style date, or an empty string for an unparseable
 *  timestamp. */
export function describeInviteCreated(createdAt: string, locale?: string): string {
  const t = Date.parse(createdAt);
  if (Number.isNaN(t)) return "";
  return `Created ${new Date(t).toLocaleDateString(locale, { day: "numeric", month: "short" })}`;
}
