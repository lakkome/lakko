// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The per-channel "last notified post" cursor of
 * `docs/ux/channel-notifications.md` section 5: kept on this device only,
 * never synced, so a post is never notified twice and a refetch of a
 * channel's history announces nothing.
 *
 * One `localStorage` row per account, `{ channelIdHex: cursor }`. The key
 * names the account, so the account store gate (`auth/account-store-gate.ts`)
 * refuses a late write after Forget device, and the wipe removes the row
 * (`auth/wipe-account.ts`, step `channel-notified`).
 */

const PREFIX = "lakko.channels.notified.v1.";
const CHANNEL_RE = /^[0-9a-f]{32}$/;

/** The row's key for an account. */
export function channelNotifiedKey(accountIdB64: string): string {
  return `${PREFIX}${accountIdB64}`;
}

function storage(): Storage | undefined {
  try {
    return globalThis.localStorage ?? undefined;
  } catch {
    return undefined;
  }
}

/** Every channel's stored cursor; empty when there is none or it is
 *  unreadable. */
export function readChannelNotified(accountIdB64: string, store: Storage | undefined = storage()): Record<string, number> {
  try {
    const raw = store?.getItem(channelNotifiedKey(accountIdB64));
    if (raw === null || raw === undefined) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (typeof parsed !== "object" || parsed === null) return {};
    const out: Record<string, number> = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (CHANNEL_RE.test(k) && typeof v === "number" && Number.isSafeInteger(v) && v >= 0) out[k] = v;
    }
    return out;
  } catch {
    return {};
  }
}

/** Store one channel's cursor; never moves it back. Channels not in `keep`
 *  (no longer held) are dropped from the row. */
export function writeChannelNotified(
  accountIdB64: string,
  channelIdHex: string,
  cursor: number,
  keep: ReadonlySet<string>,
  store: Storage | undefined = storage(),
): void {
  if (!CHANNEL_RE.test(channelIdHex) || !Number.isSafeInteger(cursor) || cursor < 0) return;
  const current = readChannelNotified(accountIdB64, store);
  const next: Record<string, number> = {};
  for (const [k, v] of Object.entries(current)) if (keep.has(k)) next[k] = v;
  next[channelIdHex] = Math.max(current[channelIdHex] ?? 0, cursor);
  try {
    store?.setItem(channelNotifiedKey(accountIdB64), JSON.stringify(next));
  } catch {
    // Storage blocked or the account is sealed: nothing is kept, so the next
    // read decides from the read marker alone.
  }
}

/** Remove the account's row. Called by the Forget-device wipe. */
export function clearChannelNotified(accountIdB64: string, store: Storage | undefined = storage()): void {
  try {
    store?.removeItem(channelNotifiedKey(accountIdB64));
  } catch {
    // best-effort
  }
}
