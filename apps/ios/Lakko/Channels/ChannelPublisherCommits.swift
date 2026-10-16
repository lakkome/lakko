// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// G-50 — how a channel PublisherGroup commit (a co-publisher approve, a
/// rotation) is made durable and how its effects leave the device, over the
/// live clients. The record, the pure rules and the runner are in
/// ``PublisherCommitEntry`` / ``PublisherCommitRunner``.
///
/// Every step reopens the group from the channel provider
/// (``ChannelPublisherIdentity``) instead of holding a handle across waits:
/// after a failed record the provider is dropped and re-restored from disk,
/// and a handle kept from before would still carry the epoch that was rolled
/// back.
@MainActor
public enum ChannelPublisherCommits {

    /// What a commit's steps run against.
    public struct Context {
        /// 32-char hex channel id.
        public let channelIdHex: String
        /// The account's CURRENT phrase seed (the screens' seed); the
        /// channel provider is built from the lineage seed it implies.
        public let currentSeed: Data
        /// The account the work was started for.
        public let accountIdB64: String
        public let channelsClient: ChannelsClient
        public let ds: DsClient

        public init(
            channelIdHex: String, currentSeed: Data, accountIdB64: String,
            channelsClient: ChannelsClient = ChannelsClient(), ds: DsClient = DsClient()
        ) {
            self.channelIdHex = channelIdHex
            self.currentSeed = currentSeed
            self.accountIdB64 = accountIdB64
            self.channelsClient = channelsClient
            self.ds = ds
        }

        /// The identity channels-service requests are signed with: the
        /// current seed's, which an ADR-0074 rotation handed the channels to.
        func ownerIdentity() throws -> Identity {
            try Identity.fromSeed(seed: currentSeed)
        }

        /// The channel provider identity and the channel's PublisherGroup,
        /// opened afresh (load only: a commit's group exists).
        @MainActor
        func open() throws -> (identity: Identity, group: PublisherGroup) {
            let identity = try ChannelPublisherIdentity.shared.identity(currentSeed: currentSeed)
            guard let group = try CoPublisher.loadPublisherGroup(
                identity: identity, channelIdHex: channelIdHex) else {
                throw CoPublisherError.badChannelId(channelIdHex)
            }
            return (identity, group)
        }
    }

    /// Record `entry` for a commit just created in the channel provider:
    /// the entry FIRST, then the N+1 provider snapshot, both fail-closed. On
    /// a failure the entry is removed, the in-memory provider (which holds
    /// the unrecorded N+1) is dropped so the next open restores the durable
    /// epoch N, and the error is rethrown: nothing runs.
    public static func record(_ entry: PublisherCommitEntry, accountIdB64: String) throws {
        do {
            try PublisherCommitStore.shared.put(accountIdB64: accountIdB64, entry)
        } catch {
            ChannelPublisherIdentity.shared.unbind()
            throw error
        }
        do {
            try ChannelPublisherIdentity.shared.persistOrThrow()
        } catch {
            PublisherCommitStore.shared.remove(accountIdB64: accountIdB64, id: entry.id)
            ChannelPublisherIdentity.shared.unbind()
            throw error
        }
    }

    /// The ports a recorded commit runs through. `state`, when given, gets
    /// the N+1 content seed and the new server epoch once the reseal landed.
    public static func ports(
        _ context: Context, state: ChannelsUiState?
    ) -> PublisherCommitPorts {
        let account = context.accountIdB64
        return PublisherCommitPorts(
            registerEpoch: {
                let (identity, group) = try context.open()
                let next = try group.dsAccessSigner(identity: identity)
                do {
                    try await CoPublisher.registerEpoch(
                        ds: context.ds, identity: identity, publisherGroup: group, next: next)
                } catch DsStatementError.conflict {
                    // This device's own earlier registration of the epoch.
                }
            },
            publishCommit: { commit in
                let (identity, group) = try context.open()
                _ = try await context.ds.publishGroup(
                    groupId: try group.groupId(), payload: commit,
                    auth: DsAuth(signer: try group.dsAccessSigner(identity: identity)))
            },
            deliverWelcome: { welcome, joinerKey in
                try await CoPublisher.deliverWelcome(ds: context.ds, welcome: welcome, joinerKey: joinerKey)
            },
            serverEpoch: {
                try await context.channelsClient.fetchEpoch(channelIdHex: context.channelIdHex)
            },
            reseal: { targetEpoch in
                let (identity, group) = try context.open()
                try await ChannelReseal.resealToSubscribers(
                    publisherGroup: group, identity: identity,
                    ownerIdentity: try context.ownerIdentity(),
                    channelIdHex: context.channelIdHex, targetEpoch: targetEpoch,
                    client: context.channelsClient)
                state?.setEpoch(channelId: context.channelIdHex, epoch: targetEpoch)
            },
            appoint: { joinerKey in
                let (_, group) = try context.open()
                guard let key = await CoPublisherKeyBook.shared.key(
                    groupId: try group.groupId(), joinerKey: joinerKey) else {
                    NSLog("ADR-0059: no co-publisher binding read for the approved joiner; not appointed")
                    return
                }
                try await context.channelsClient.setCoPublisherAppointment(
                    appoint: true, channelIdHex: context.channelIdHex,
                    accountIdentity: try context.ownerIdentity(), copublisherKey: key)
            },
            save: { entry in try PublisherCommitStore.shared.put(accountIdB64: account, entry) },
            finish: { entry in PublisherCommitStore.shared.remove(accountIdB64: account, id: entry.id) }
        )
    }

    /// Write the group's CURRENT content seed back to `state` (the seed every
    /// later post seals under). Best effort: a replay writes it again.
    public static func refreshContentSeed(_ context: Context, state: ChannelsUiState?) {
        guard let state, let old = state.keysByChannel[context.channelIdHex],
              let opened = try? context.open(),
              let seed = try? opened.group.channelSeed(identity: opened.identity) else { return }
        guard seed != old.seed else { return }
        state.setKeys(
            channelId: context.channelIdHex,
            keys: ChannelPublisherKeys(seed: seed, signingKeySeed: old.signingKeySeed))
    }

    /// Resume every recorded commit of the account whose steps had not all
    /// landed (a restart, a failed step). An entry whose group did not reach
    /// its target epoch is dropped (``PublisherCommitEntry/replayVerdict(_:restoredEpoch:)``).
    /// Errors are logged; the entry stays for the next run.
    public static func replayAll(
        accountIdB64: String, currentSeed: Data, state: ChannelsUiState?,
        channelsClient: ChannelsClient = ChannelsClient(), ds: DsClient = DsClient()
    ) async {
        for entry in PublisherCommitStore.shared.entries(accountIdB64: accountIdB64) {
            // Without the account's channel provider nothing is judged: an
            // entry is dropped only against a provider that did restore.
            guard accountIdB64 == AcceptedContactsStore.boundAccountIdB64,
                  let identity = try? ChannelPublisherIdentity.shared.identity(currentSeed: currentSeed)
            else { return }
            let context = Context(
                channelIdHex: entry.channelIdHex, currentSeed: currentSeed,
                accountIdB64: accountIdB64, channelsClient: channelsClient, ds: ds)
            let loaded = (try? CoPublisher.loadPublisherGroup(
                identity: identity, channelIdHex: entry.channelIdHex)) ?? nil
            let restored = loaded.flatMap { try? $0.epoch() }
            switch PublisherCommitEntry.replayVerdict(entry, restoredEpoch: restored) {
            case .discard:
                NSLog("Channels: a PublisherGroup commit whose epoch never became durable was dropped")
                PublisherCommitStore.shared.remove(accountIdB64: accountIdB64, id: entry.id)
            case .replay:
                do {
                    try await PublisherCommitRunner.run(entry, ports: ports(context, state: state))
                    refreshContentSeed(context, state: state)
                } catch {
                    NSLog("Channels: a recorded PublisherGroup commit did not finish; retried later: %@",
                          String(describing: error))
                }
            }
        }
    }
}

/// The reseal of a channel's CURRENT seed to its subscribers (ADR-0010 /
/// C-54), committed on the channels service with the ADR-0030 chunked
/// fanout at a recorded target epoch.
public enum ChannelReseal {
    /// Seal the group's current seed to every subscriber bound to
    /// `targetEpoch`, stage the envelopes in bounded chunks and commit. A
    /// single seal failure aborts the whole batch. `ownerIdentity` signs the
    /// channels-service requests.
    public static func resealToSubscribers(
        publisherGroup: PublisherGroup,
        identity: Identity,
        ownerIdentity: Identity,
        channelIdHex: String,
        targetEpoch: UInt32,
        client: ChannelsClient
    ) async throws {
        let seed = try publisherGroup.channelSeed(identity: identity)
        let channelIdBytes = try publisherGroup.channelId()
        let subscribers = try await client.listSubscribers(
            channelIdHex: channelIdHex, accountIdentity: ownerIdentity)
        var batch: [RotateBatchEntry] = []
        batch.reserveCapacity(subscribers.count)
        for subscriberHex in subscribers {
            guard let subscriberBytes = CoPublisher.hexToData(subscriberHex) else {
                throw CoPublisherError.malformedSubscriberHex(subscriberHex)
            }
            let envelope = try sealSubscriberSeedEnvelope(
                subscriberPub: subscriberBytes, channelId: channelIdBytes,
                epochId: targetEpoch, seed: seed)
            batch.append(RotateBatchEntry(
                subscriberPubkeyHex: subscriberHex, envelopeB64: envelope.base64EncodedString()))
        }
        let chunkSize = ChannelsClient.maxStageChunkEntries
        var offset = 0
        while offset < batch.count {
            let end = min(offset + chunkSize, batch.count)
            try await client.stageRotation(
                channelIdHex: channelIdHex, targetEpoch: targetEpoch,
                batch: Array(batch[offset..<end]), accountIdentity: ownerIdentity)
            offset = end
        }
        _ = try await client.commitRotation(
            channelIdHex: channelIdHex, targetEpoch: targetEpoch, accountIdentity: ownerIdentity)
    }
}
