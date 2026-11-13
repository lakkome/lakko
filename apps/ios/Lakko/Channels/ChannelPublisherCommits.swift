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
    ///
    /// ADR-0081: the journal step is proven under the recorded seed of the
    /// epoch the commit left (``PublisherCommitEntry/proofSeedB64``), live
    /// and on a replay after a restart alike. An entry without one can
    /// complete only an identical re-send, which the DS recognises under any
    /// retained key.
    public static func ports(
        _ context: Context, state: ChannelsUiState?
    ) -> PublisherCommitPorts {
        let account = context.accountIdB64
        return PublisherCommitPorts(
            commitToJournal: { commit, proof in
                let (identity, group) = try context.open()
                let next = try group.dsAccessSigner(identity: identity)
                let proofSigner: DsGroupSigner
                if let proof {
                    proofSigner = try DsProofSigner.forGroup(epoch: proof.epoch, accessSeed: proof.seed)
                } else {
                    proofSigner = next
                }
                try await CoPublisher.commitEpoch(
                    ds: context.ds, identity: identity, publisherGroup: group,
                    next: next, proof: proofSigner, row: commit)
            },
            deliverWelcome: { welcome, joinerKey in
                try await CoPublisher.deliverWelcome(ds: context.ds, welcome: welcome, joinerKey: joinerKey)
            },
            serverEpoch: {
                try await context.channelsClient.fetchEpoch(channelIdHex: context.channelIdHex)
            },
            reseal: { targetEpoch in
                let (identity, group) = try context.open()
                // The mark as this device keeps it, from the store when no
                // screen state is bound (a replay at start): never a reseal
                // to every row for want of a state object.
                let extras = state?.extras(context.channelIdHex)
                    ?? ChannelExtrasStore.load(context.accountIdB64)[context.channelIdHex]
                try await ChannelReseal.resealToSubscribers(
                    publisherGroup: group, identity: identity,
                    ownerIdentity: try context.ownerIdentity(),
                    channelIdHex: context.channelIdHex, targetEpoch: targetEpoch,
                    client: context.channelsClient,
                    throughSeq: ChannelReseal.resealThrough(
                        isPublic: extras?.isPublic, mark: extras?.admittedThroughSeq),
                    ownReaderKeyHex: extras?.readerKeyRegisteredHex)
                state?.setEpoch(channelId: context.channelIdHex, epoch: targetEpoch)
            },
            appoint: { joinerKey in
                let (_, group) = try context.open()
                let groupId = try group.groupId()
                guard let key = await CoPublisherKeyBook.shared.key(
                    groupId: groupId, joinerKey: joinerKey, accountIdB64: account) else {
                    NSLog("ADR-0059: no co-publisher binding read for the approved joiner; not appointed")
                    return
                }
                try await context.channelsClient.setCoPublisherAppointment(
                    appoint: true, channelIdHex: context.channelIdHex,
                    accountIdentity: try context.ownerIdentity(), copublisherKey: key)
                CoPublisherKeyBook.noteAppointed(
                    channelIdHex: context.channelIdHex, accountKey: joinerKey, copublisherKey: key,
                    accountIdB64: account)
                await CoPublisherKeyBook.shared.forget(groupId: groupId, joinerKey: joinerKey, accountIdB64: account)
            },
            unappoint: { accountKey in
                // The key this device appointed, else the one the publisher's
                // own binding named (sent in the group, or read off the knock
                // queue): a device that did not approve the publisher still
                // revokes the appointment.
                var known = CoPublisherKeyBook.appointedKey(
                    channelIdHex: context.channelIdHex, accountKey: accountKey, accountIdB64: account)
                if known == nil, let groupId = try? context.open().1.groupId() {
                    known = await CoPublisherKeyBook.shared.key(
                        groupId: groupId, joinerKey: accountKey, accountIdB64: account)
                }
                guard let key = known else {
                    NSLog("ADR-0087: no appointment recorded for a removed publisher on this device")
                    return
                }
                try await context.channelsClient.setCoPublisherAppointment(
                    appoint: false, channelIdHex: context.channelIdHex,
                    accountIdentity: try context.ownerIdentity(), copublisherKey: key)
                CoPublisherKeyBook.noteAppointed(
                    channelIdHex: context.channelIdHex, accountKey: accountKey, copublisherKey: nil,
                    accountIdB64: account)
            },
            save: { entry in try PublisherCommitStore.shared.put(accountIdB64: account, entry) },
            finish: { entry in PublisherCommitStore.shared.remove(accountIdB64: account, id: entry.id) }
        )
    }

    /// Section 13 — the epoch seed a commit is about to leave, and the server
    /// epoch the posts sealed under it carry, kept like an older epoch's seed
    /// so this device still opens them (the channel's record at creation
    /// first). Without it the owner's own channel read "Earlier posts aren't
    /// available." after every approve, removal or rotation. `nil` when there
    /// is nothing to keep: no seed, or that epoch already held.
    nonisolated static func outgoingSeedToKeep(
        seed: Data?, serverEpoch: UInt32, held: [UInt32: Data]
    ) -> (epoch: UInt32, seed: Data)? {
        guard let seed, seed.count == 32, held[serverEpoch] == nil else { return nil }
        return (serverEpoch, seed)
    }

    /// Keep the seed `group` derives NOW (before its commit) under
    /// `serverEpoch`, the channels-service epoch read before the commit.
    /// Called before the commit is created: a commit that is then refused
    /// leaves the same epoch, so what was kept is still right.
    static func keepOutgoingSeed(
        state: ChannelsUiState?, channelIdHex: String, serverEpoch: UInt32,
        group: PublisherGroup, identity: Identity
    ) {
        guard let state,
              let kept = outgoingSeedToKeep(
                  seed: try? group.channelSeed(identity: identity), serverEpoch: serverEpoch,
                  held: state.subscribedSeeds(channelId: channelIdHex))
        else { return }
        state.setSubscribedSeed(channelId: channelIdHex, epoch: kept.epoch, seed: kept.seed)
    }

    /// Write the group's CURRENT content seed back to `state` (the seed every
    /// later post seals under), with the channel epoch it serves, the
    /// replaced seed kept under its own. `landedEpoch`: the epoch this
    /// owner's own reseal landed the channel at (a commit just run); `nil`
    /// reads the group's epoch. Best effort: a replay writes it again.
    public static func refreshContentSeed(
        _ context: Context, state: ChannelsUiState?, landedEpoch: UInt32? = nil
    ) {
        guard let state, state.keysByChannel[context.channelIdHex] != nil,
              let opened = try? context.open(),
              let seed = try? opened.group.channelSeed(identity: opened.identity) else { return }
        guard let epoch = landedEpoch ?? (try? opened.group.epoch()).map({ UInt32(clamping: $0) }) else { return }
        state.notePublisherSeedAt(channelId: context.channelIdHex, epoch: epoch, seed: seed)
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
                    refreshContentSeed(context, state: state, landedEpoch: entry.serverTargetEpoch)
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
    ///
    /// `throughSeq` (``resealThrough(isPublic:mark:)``):
    /// `nil` reseals to every row; a value reseals only to the rows at or
    /// below it, the rows this device let in (a private channel).
    public static func resealToSubscribers(
        publisherGroup: PublisherGroup,
        identity: Identity,
        ownerIdentity: Identity,
        channelIdHex: String,
        targetEpoch: UInt32,
        client: ChannelsClient,
        throughSeq: UInt64? = nil,
        ownReaderKeyHex: String? = nil
    ) async throws {
        let seed = try publisherGroup.channelSeed(identity: identity)
        let channelIdBytes = try publisherGroup.channelId()
        let subscribers: [String]
        if let throughSeq {
            var rows: [ChannelsClient.SubscriberRow] = []
            var after: UInt64 = 0
            for _ in 0..<10_000 {
                let page = try await client.subscriberPage(
                    channelIdHex: channelIdHex, after: after, accountIdentity: ownerIdentity)
                rows.append(contentsOf: page.rows)
                guard let last = page.rows.last, last.seq <= throughSeq, page.nextAfter > after else { break }
                after = page.nextAfter
            }
            subscribers = resealKeys(rows: rows, throughSeq: throughSeq)
        } else {
            subscribers = try await client.listSubscribers(
                channelIdHex: channelIdHex, accountIdentity: ownerIdentity)
        }
        // 2026-11-13 — the account's own reader key (its other devices read
        // with it) gets every new seed, above the mark too, like this
        // device's own key is never removed.
        let resealed = withOwnReaderKey(subscribers, ownReaderKeyHex)
        var batch: [RotateBatchEntry] = []
        batch.reserveCapacity(resealed.count)
        for subscriberHex in resealed {
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

    /// ADR-0087 section 7 — whom a rotation reseals to per key, the web
    /// `resealPlan`: in a private channel only the rows at or below this
    /// device's mark — the readers from before the channel went private and
    /// the rows a let-in pass let in — never a row with no valid ticket a
    /// server could have added, whether or not this device holds an
    /// admission seed (one without has let nobody in, so it reseals to the
    /// readers only). `nil` (every row) for a public channel, whose readers
    /// read its public seed besides, and for a channel of unknown type.
    nonisolated static func resealThrough(isPublic: Bool?, mark: UInt64?) -> UInt64? {
        guard isPublic == false else { return nil }
        return mark ?? 0
    }

    /// The per-key reseal set with the account's own reader key added once
    /// (2026-11-13; web `withOwnReaderKey`).
    nonisolated static func withOwnReaderKey(_ keys: [String], _ own: String?) -> [String] {
        guard let own = own?.lowercased() else { return keys }
        return keys.contains { $0.lowercased() == own } ? keys : keys + [own]
    }

    /// The rows a per-key reseal seals to: those at or below `throughSeq`.
    nonisolated static func resealKeys(rows: [ChannelsClient.SubscriberRow], throughSeq: UInt64) -> [String] {
        rows.filter { $0.seq <= throughSeq }.map(\.pubkeyHex)
    }
}
