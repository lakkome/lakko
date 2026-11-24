// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// What the subscriber count and list count
/// (`docs/ux/group-and-channel-privacy.md` section 15.1): one person is one
/// subscriber, and the publishing account's own reader key is not a
/// subscriber. Mirror of web `channels/subscriber-count.ts`.
///
/// The device that holds a channel's PublisherGroup registers the account's
/// one subscriber key at the channels service so the account's other devices
/// can read (``ChannelAccountReaderKey``). The service counts that row like
/// any other, and the owner's list showed it as "From an invitation" with
/// `Remove` (live walk 2026-11-24: two people subscribed, the owner read
/// "Subscribers (3)"). The service cannot tell the row apart, so the
/// publishing viewer leaves its own account's keys out. Pure.
enum SubscriberCount {
    /// The count to show: the service's less the publishers' reader rows
    /// known to be registered there. Never below 0.
    static func shown(serviceCount: UInt64, publisherReaderRows: Int) -> Int {
        let raw = Int(clamping: serviceCount)
        return max(0, raw - max(0, publisherReaderRows))
    }

    /// How many publisher reader rows the count leaves out: this account's
    /// registered key and the other publishers' announced keys, each once.
    static func publisherReaderRows(ownRegisteredHex: String?, otherKeys: [String]) -> Int {
        var all = Set(otherKeys.map { $0.lowercased() })
        if let own = ownRegisteredHex, !own.isEmpty { all.insert(own.lowercased()) }
        return all.count
    }

    /// The count after `removed` subscriber keys were removed here.
    static func afterRemoval(_ count: Int, removed: Int) -> Int { max(0, count - removed) }

    /// The keys the publishing viewer leaves out of its subscriber list: its
    /// account's reader key, as registered from this device and as the
    /// account's register holds it, this device's own subscriber key, and the
    /// other publishers' announced reader keys.
    /// Lower case, no duplicates, no blanks. A viewer that does not publish
    /// leaves nothing out.
    static func ownPublisherKeys(publishes: Bool, keys: [String?]) -> Set<String> {
        guard publishes else { return [] }
        return Set(keys.compactMap { $0?.lowercased() }.filter { !$0.isEmpty })
    }
}

extension ChannelsUiState {
    /// Record the count the channels service answered for `channelId`, less
    /// the publishers' reader rows (this account's when this device
    /// registered it, the others' as announced; section 15.1).
    func setServiceSubscriberCount(channelId: String, serviceCount: UInt64) {
        setSubscriberCount(
            channelId: channelId,
            count: SubscriberCount.shown(
                serviceCount: serviceCount,
                publisherReaderRows: SubscriberCount.publisherReaderRows(
                    ownRegisteredHex: extras(channelId).readerKeyRegisteredHex,
                    otherKeys: Array((extras(channelId).publisherReaderKeys ?? [:]).values))))
    }
}
