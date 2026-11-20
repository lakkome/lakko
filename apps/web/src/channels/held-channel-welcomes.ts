// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 — channel Welcomes that came too early (walk follow-up of
 * 2026-11-14).
 *
 * An owner device that retires this device's leaf of a channel's
 * PublisherGroup and admits it again sends the new Welcome to the account
 * mailbox. The mailbox poll can deliver that Welcome before the channel's
 * journal poll has applied the commit that removed the leaf: the held group
 * is then still ACTIVE and unsuspected, and the replace rule refuses to
 * replace an active group (a replayed old Welcome must never do that). The
 * mailbox cursor moves past the row, and the owner device does not send the
 * Welcome again, so the device would wait for ever for a Welcome it already
 * had.
 *
 * Such a Welcome is held here instead: per account, keyed by channel, and
 * tried again on every publisher pass, where the normal replace rule decides
 * once the held group is inactive (or suspected). The bounds are CHOSEN, not
 * measured:
 *
 * - at most {@link HELD_CHANNEL_WELCOMES_PER_CHANNEL} per channel, newest
 *   kept;
 * - dropped after {@link HELD_CHANNEL_WELCOME_MAX_AGE_MS} (the removal it
 *   answered never came here); a clock that moved backwards keeps it;
 * - dropped once the channel was joined through any Welcome, once the
 *   channel is no longer the account's to publish in, and once the replace
 *   rule ran on it and did not join.
 *
 * A Welcome's epoch cannot be read without joining it (its GroupInfo is
 * sealed), so "the device publishes again at an epoch at or above the
 * Welcome's" is not built (as on iOS); the age bound and the clearing on a
 * join stand in for it. The same bounds as iOS
 * (`ChannelDeviceAdmission.swift`), so the clients agree.
 *
 * Stored in `localStorage` (a Welcome is already ciphertext to this device's
 * KeyPackage; nothing here is a key). Reads and writes never throw.
 */

/** Chosen: one re-admission plus a retry or two. */
export const HELD_CHANNEL_WELCOMES_PER_CHANNEL = 3;
/** Chosen: a week of the held group staying active. */
export const HELD_CHANNEL_WELCOME_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000;

export interface HeldChannelWelcome {
  readonly channelIdHex: string;
  readonly welcomeB64: string;
  readonly heldAtMs: number;
}

/** The list with `entry` added: an identical Welcome is not held twice (its
 *  first time stays), and a channel keeps only its newest few. Pure. */
export function withHeldChannelWelcome(
  list: readonly HeldChannelWelcome[],
  entry: HeldChannelWelcome,
  perChannel: number = HELD_CHANNEL_WELCOMES_PER_CHANNEL,
): HeldChannelWelcome[] {
  if (list.some((e) => e.channelIdHex === entry.channelIdHex && e.welcomeB64 === entry.welcomeB64)) return [...list];
  const same = [...list.filter((e) => e.channelIdHex === entry.channelIdHex), entry].sort(
    (a, b) => b.heldAtMs - a.heldAtMs,
  );
  const kept = same.slice(0, Math.max(0, perChannel));
  return [...list.filter((e) => e.channelIdHex !== entry.channelIdHex), ...kept];
}

/** Whether a held Welcome has outlived the bound. A clock that moved
 *  backwards keeps it. Pure. */
export function heldChannelWelcomeExpired(
  entry: HeldChannelWelcome,
  nowMs: number,
  maxAgeMs: number = HELD_CHANNEL_WELCOME_MAX_AGE_MS,
): boolean {
  return nowMs - entry.heldAtMs > maxAgeMs;
}

/**
 * What a publisher pass does with a held Welcome, pure:
 * - `drop`: the channel is not the account's to publish in any more, or the
 *   Welcome outlived its bound (as on iOS, whatever the group's state);
 * - `try`: the held group is inactive or suspected, or no group is held:
 *   the normal replace rule decides now (and the entry goes either way);
 * - `keep`: the held group is still active and unsuspected.
 */
export function heldChannelWelcomeAction(input: {
  readonly channelStillOurs: boolean;
  readonly heldGroup: "none" | "active" | "inactive";
  readonly suspected: boolean;
  readonly expired: boolean;
}): "drop" | "try" | "keep" {
  if (!input.channelStillOurs || input.expired) return "drop";
  if (input.heldGroup !== "active" || input.suspected) return "try";
  return "keep";
}

function storageKey(accountIdB64: string): string {
  return `lakko.channels.held-welcomes.v1.${accountIdB64}`;
}

export function loadHeldChannelWelcomes(accountIdB64: string): HeldChannelWelcome[] {
  try {
    const raw = localStorage.getItem(storageKey(accountIdB64));
    if (raw === null) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const out: HeldChannelWelcome[] = [];
    for (const v of parsed) {
      if (typeof v !== "object" || v === null) continue;
      const e = v as Partial<HeldChannelWelcome>;
      if (typeof e.channelIdHex !== "string" || typeof e.welcomeB64 !== "string" || typeof e.heldAtMs !== "number") continue;
      out.push({ channelIdHex: e.channelIdHex, welcomeB64: e.welcomeB64, heldAtMs: e.heldAtMs });
    }
    return out;
  } catch {
    return [];
  }
}

/** `false` when the write failed (the caller keeps going: the Welcome is
 *  then only as durable as this page). */
export function saveHeldChannelWelcomes(accountIdB64: string, list: readonly HeldChannelWelcome[]): boolean {
  try {
    if (list.length === 0) localStorage.removeItem(storageKey(accountIdB64));
    else localStorage.setItem(storageKey(accountIdB64), JSON.stringify(list));
    return true;
  } catch {
    return false;
  }
}
