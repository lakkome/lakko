// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Every publishing account's reader key, known to the channel's other
/// publishers (`docs/ux/group-and-channel-privacy.md` section 15.1; live
/// walk 2026-11-24, "Night walk": web owner, iOS publisher). Mirror of web
/// `channels/publisher-reader-keys.ts`.
///
/// The device that holds the PublisherGroup registers its account's reader
/// key at the channels service as a subscriber row
/// (``ChannelAccountReaderKey``). The owner saw a co-publisher's row as
/// "From an invitation" with `Remove` and counted it, and the publisher saw
/// the owner's the same way. Each publishing device now announces its
/// account's reader key on the PublisherGroup
/// (`lakko-contacts::publisher_reader_key`) once registered and again when
/// the group grows; every publishing device keeps the newest per current
/// publisher account (sender resolved at receipt, C-68) and leaves those
/// keys out of the subscriber list and count.
@MainActor
enum PublisherReaderKeys {
    /// The receiver's rule, pure: kept only for this channel, from a sender
    /// that is a current publisher account.
    nonisolated static func verdict(
        channelId: Data, envelopeChannelId: Data, readerKey: Data, sender: Data?, publisherAccounts: Set<Data>
    ) -> (account: String, readerHex: String)? {
        guard envelopeChannelId == channelId, let sender, publisherAccounts.contains(sender),
              readerKey.count == 32 else { return nil }
        return (sender.base64EncodedString(), ChannelsComposer.bytesToHex(readerKey).lowercased())
    }

    /// The stored keys whose account still publishes, pure.
    nonisolated static func pruned(_ stored: [String: String]?, publisherAccounts: Set<Data>) -> [String: String] {
        let accounts = Set(publisherAccounts.map { $0.base64EncodedString() })
        return (stored ?? [:]).filter { accounts.contains($0.key) }
    }

    /// When to announce, pure: once registered, again when the key or the
    /// number of PublisherGroup leaves changed since the last announcement.
    nonisolated static func sendDue(registeredHex: String?, leaves: Int, lastSent: String?) -> String? {
        guard let registeredHex, !registeredHex.isEmpty else { return nil }
        let sig = "\(registeredHex.lowercased())|\(leaves)"
        return sig == lastSent ? nil : sig
    }

    /// The other publishers' reader keys of `channelId` this device holds.
    static func otherKeys(channelId: String, state: ChannelsUiState) -> [String] {
        Array(Set((state.extras(channelId).publisherReaderKeys ?? [:]).values.map { $0.lowercased() }))
    }

    /// Announce this account's reader key when due. Best effort.
    static func sendIfDue(
        channelId: String, state: ChannelsUiState, identity: Identity, group: PublisherGroup, ds: DsClient
    ) async {
        let extras = state.extras(channelId)
        let leaves = (try? group.members())?.count ?? 0
        guard leaves > 0,
              let due = sendDue(
                  registeredHex: extras.readerKeyRegisteredHex, leaves: leaves, lastSent: extras.publisherReaderKeySent),
              let hex = extras.readerKeyRegisteredHex, let key = Data(channelHex: hex),
              let channelBytes = ChannelsClient.hexToData(channelId),
              let envelope = try? composePublisherReaderKeyEnvelope(channelId: channelBytes, readerKey: key)
        else { return }
        if await ChannelDeviceAdmission.publish(envelope, identity: identity, group: group, ds: ds) {
            state.updateExtras(channelId) { $0.publisherReaderKeySent = due }
        }
    }

    /// Keep the reader keys a journal poll read from current publishers, and
    /// drop those of accounts that no longer publish.
    static func receive(
        _ applications: [CoPublisher.JournalApplication], channelId: String, group: PublisherGroup,
        state: ChannelsUiState
    ) {
        guard let channelBytes = ChannelsClient.hexToData(channelId),
              let members = try? group.members() else { return }
        let accounts = Set(members.map { $0.accountKey.isEmpty ? $0.signaturePublicKey : $0.accountKey })
        var stored = pruned(state.extras(channelId).publisherReaderKeys, publisherAccounts: accounts)
        for app in applications {
            guard app.data.count == Int(publisherReaderKeyEnvelopeLen()),
                  let decoded = try? decodePublisherReaderKeyEnvelope(plaintext: app.data),
                  let kept = verdict(
                      channelId: channelBytes, envelopeChannelId: decoded.channelId, readerKey: decoded.readerKey,
                      sender: app.senderAccount, publisherAccounts: accounts) else { continue }
            stored[kept.account] = kept.readerHex
        }
        guard stored != (state.extras(channelId).publisherReaderKeys ?? [:]) else { return }
        state.updateExtras(channelId) { $0.publisherReaderKeys = stored.isEmpty ? nil : stored }
    }
}
