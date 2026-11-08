// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The channel work a subscriber's device does without the channel open,
/// run from the foreground channel sync (``ChannelPostSync``):
///
/// - ADR-0087 section 7.3 / UX 9.5 — a private channel this device reads is
///   checked for its removal (an envelope read answering `410
///   subscriber-removed`), at most every ``removalCheckIntervalMs``; a removed
///   channel ends here and on the account's other devices, with the banner.
/// - Section 13.1 — reaction adds that found no token leave with the next
///   refill, whether or not the channel is open.
@MainActor
enum ChannelBackgroundChecks {

    /// Between two removal checks of one channel (chosen, not measured): one
    /// envelope read per private channel per this long.
    static let removalCheckIntervalMs: UInt64 = 10 * 60_000

    private static var lastRemovalCheck: [String: UInt64] = [:]

    static func reset() { lastRemovalCheck = [:] }

    static func run(state: ChannelsUiState, client: ChannelsClient, accountIdentitySeed: Data?) async {
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        for channel in state.channels where state.keysByChannel[channel.id] == nil {
            guard let keys = state.subscriberKeysByChannel[channel.id],
                  state.extras(channel.id).isPublic == false || state.extras(channel.id).waitingForAdmission,
                  now >= (lastRemovalCheck[channel.id] ?? 0) &+ removalCheckIntervalMs else { continue }
            lastRemovalCheck[channel.id] = now
            do {
                _ = try await client.fetchEnvelope(
                    channelIdHex: channel.id, subscriberPubkeyHex: ChannelsComposer.bytesToHex(keys.publicKey))
            } catch let error as ChannelsApiError where error.code == "subscriber-removed" {
                // UX 9.5 — the banner says it, the row and its posts go.
                let name = channel.title
                ChannelMemberCards.forgetUploads(channelId: channel.id)
                state.clearSubscriberKeys(channel.id)
                state.removeChannel(channel.id)
                state.onRemovedHere?(channel.id, name)
            } catch {}
        }
        guard let seed = accountIdentitySeed,
              let writer = ChannelLineage.identity(accountIdB64: AcceptedContactsStore.boundAccountIdB64, currentSeed: seed)
        else { return }
        let pendingChannels = Set(PendingChannelReactionStore.load().map(\.channelIdHex))
        for channelId in pendingChannels {
            // The composer's seed for the one epoch it serves only.
            let seeds = state.readSeeds(channelId: channelId)
            await flushReactions(channelId: channelId, seeds: seeds, client: client, writer: writer)
        }
    }

    /// Send the kept adds of one channel; stops at the first that still finds
    /// no token (the rest wait for the next refill).
    static func flushReactions(
        channelId: String, seeds: [UInt32: Data], client: ChannelsClient, writer: Identity
    ) async {
        guard let id = ChannelsClient.hexToData(channelId) else { return }
        for add in PendingChannelReactionStore.load().filter({ $0.channelIdHex == channelId }) {
            guard let seed = seeds[add.epoch],
                  let key = try? ChannelReactionKey.derive(epochSeed: seed, channelId: id, epochId: add.epoch),
                  let label = try? key.label(postCursor: add.cursor, emoji: add.emoji),
                  let write = try? ChannelReactionWriteKey(
                    accountIdentity: writer, channelId: id, postCursor: add.cursor, label: label)
            else { continue }
            do {
                try await client.addReaction(
                    channelIdHex: channelId, postCursor: add.cursor, writeKey: write, label: label)
                PendingChannelReactionStore.remove(channelId: channelId, cursor: add.cursor, emoji: add.emoji)
            } catch let error as ChannelsApiError where error.status == 404 {
                // The post is gone: nothing to add to.
                PendingChannelReactionStore.remove(channelId: channelId, cursor: add.cursor, emoji: add.emoji)
            } catch {
                return
            }
        }
    }
}
