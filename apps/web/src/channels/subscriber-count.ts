// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the subscriber count and list count (`docs/ux/group-and-channel-privacy.md`
 * section 15.1): one person is one subscriber, and the publishing account's
 * own reader key is not a subscriber.
 *
 * The device that holds a channel's PublisherGroup registers the account's
 * one subscriber key at the channels service so the account's other devices
 * can read (`account-reader-key.ts`). The service counts that row like any
 * other, and the owner's list showed it as "From an invitation" with
 * `Remove` (live walk 2026-11-24: two people subscribed, the owner read
 * "Subscribers (3)"). The service cannot tell the row apart, so the
 * publishing viewer leaves its own account's keys out. Pure.
 */

/** The count to show: the service's count less the publishers' reader keys
 *  known to be registered there (this account's, once this device
 *  registered it, and every other publishing account's it announced on the
 *  PublisherGroup, `publisher-reader-keys.ts`). Never below 0. */
export function shownSubscriberCount(serviceCount: number, publisherReaderRows: number): number {
  return Math.max(0, serviceCount - Math.max(0, publisherReaderRows));
}

/** How many publisher reader rows the count leaves out: this account's
 *  registered key, and the other publishers' announced keys, each once. */
export function publisherReaderRows(ownRegisteredHex: string | null | undefined, otherKeys: readonly string[]): number {
  const all = new Set(otherKeys.map((k) => k.toLowerCase()));
  if (typeof ownRegisteredHex === "string" && ownRegisteredHex.length > 0) all.add(ownRegisteredHex.toLowerCase());
  return all.size;
}

/** The count after `removed` subscriber keys were removed on this screen. */
export function countAfterRemoval(count: number | null, removed: number): number | null {
  return count === null ? null : Math.max(0, count - removed);
}

/**
 * The keys the publishing viewer leaves out of its subscriber list: its
 * account's reader key, as registered from this device and as the account's
 * register holds it, this device's own subscriber key, and the other
 * publishers' reader keys. Lower case, no
 * duplicates, no empties. A viewer that does not publish leaves nothing out
 * (its own key is its subscription).
 */
export function ownPublisherKeys(publishes: boolean, keys: readonly (string | null | undefined)[]): readonly string[] {
  if (!publishes) return [];
  const out = new Set<string>();
  for (const k of keys) {
    if (typeof k === "string" && k.length > 0) out.add(k.toLowerCase());
  }
  return [...out];
}
