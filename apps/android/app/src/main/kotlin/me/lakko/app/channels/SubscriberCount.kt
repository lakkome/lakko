// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

/**
 * What the subscriber count and list count (`docs/ux/group-and-channel-privacy.md`
 * section 15.1): one person is one subscriber, and the publishing account's
 * own reader key is not a subscriber. Mirror of web `channels/subscriber-count.ts`.
 *
 * The device that holds a channel's PublisherGroup registers the account's
 * one subscriber key at the channels service so the account's other devices
 * can read ([ChannelAccountReaderKey]). The service counts that row like any
 * other, and the owner's list showed it as "From an invitation" with
 * `Remove` (live walk 2026-11-24: two people subscribed, the owner read
 * "Subscribers (3)"). The service cannot tell the row apart, so the
 * publishing viewer leaves its own account's keys out. Pure.
 */
public object SubscriberCount {
    /** The count to show: the service's less the publishers' reader rows
     *  known to be registered there. Never below 0. */
    public fun shown(serviceCount: Long, publisherReaderRows: Int): Long =
        (serviceCount - publisherReaderRows.coerceAtLeast(0)).coerceAtLeast(0L)

    /** How many publisher reader rows the count leaves out: this account's
     *  registered key and the other publishers' announced keys, each once. */
    public fun publisherReaderRows(ownRegisteredHex: String?, otherKeys: List<String>): Int {
        val all = otherKeys.map { it.lowercase() }.toMutableSet()
        if (!ownRegisteredHex.isNullOrEmpty()) all += ownRegisteredHex.lowercase()
        return all.size
    }

    /** The count after [removed] subscriber keys were removed here. */
    public fun afterRemoval(count: Long, removed: Int): Long = (count - removed).coerceAtLeast(0L)

    /**
     * The keys the publishing viewer leaves out of its subscriber list: its
     * account's reader key, as registered from this device and as the
     * account's register holds it, this device's own subscriber key and the
     * other publishers' announced reader keys. Lower case, no duplicates, no
     * blanks. A viewer that does not publish leaves nothing out.
     */
    public fun ownPublisherKeys(publishes: Boolean, keys: List<String?>): Set<String> =
        if (!publishes) emptySet() else keys.mapNotNull { it?.lowercase()?.takeIf(String::isNotEmpty) }.toSet()
}
