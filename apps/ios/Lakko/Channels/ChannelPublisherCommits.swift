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

    /// Record `entry` for a commit just STAGED in the device provider: the
    /// entry FIRST, then the provider snapshot carrying the staged commit,
    /// both fail-closed. On a failure the staged commit is abandoned (the
    /// group stays at N), the entry removed, and the error rethrown: nothing
    /// is sent.
    public static func record(
        _ entry: PublisherCommitEntry, accountIdB64: String, abandon: () -> Void = {}
    ) throws {
        do {
            try PublisherCommitStore.shared.put(accountIdB64: accountIdB64, entry)
        } catch {
            abandon()
            throw error
        }
        do {
            try ChannelPublisherIdentity.shared.persistOrThrow()
        } catch {
            PublisherCommitStore.shared.remove(accountIdB64: accountIdB64, id: entry.id)
            abandon()
            throw error
        }
    }

    /// What one staged commit carries.
    public struct Staged {
        public let commit: Data
        public let welcome: Data?
    }

    /// Entries whose steps are running now (a replay skips them).
    static var running: Set<String> = []

    /// Thrown when a commit is asked for while an earlier one of the group is
    /// still staged (its owed verdict runs first, ``replayAll``).
    public struct CommitStillStaged: Error {}

    /// ADR-0090 section 3 — stage one PublisherGroup commit with `build`,
    /// record it (fail closed) with the epoch-N proof seed, then run its
    /// steps: the commit route, the merge on acceptance (or the abandon on a
    /// `409`), then the Welcome / reseal / appointment. Returns the entry as
    /// recorded; the caller reads `journalLost` from the store-less result
    /// through ``PublisherCommitRunner``'s effects (`landed`).
    @discardableResult
    public static func stageAndRun(
        _ context: Context, state: ChannelsUiState?, kind: PublisherCommitEntry.Kind,
        joinerKey: Data?, serverTargetEpoch: UInt32, serverEpoch: UInt32,
        build: (Identity, PublisherGroup) throws -> Staged
    ) async throws -> (entry: PublisherCommitEntry, landed: Bool) {
        let (identity, group) = try context.open()
        guard (try? group.hasStagedCommit()) != true else { throw CommitStillStaged() }
        guard try group.isOwnerDevice(identity: identity) else { throw CoPublisherError.notOnThisDevice }
        let proofEpoch = try group.epoch()
        let proofSeed = try group.dsAccessSeed(identity: identity)
        let staged = try build(identity, group)
        let entry = PublisherCommitEntry(
            kind: kind,
            channelIdHex: context.channelIdHex.lowercased(),
            groupIdHex: DsClient.pseudonymHex(try group.groupId()),
            targetEpoch: proofEpoch &+ 1,
            serverTargetEpoch: serverTargetEpoch,
            commitB64: staged.commit.base64EncodedString(),
            welcomeB64: staged.welcome?.base64EncodedString(),
            joinerKeyB64: joinerKey?.base64EncodedString(),
            proofEpoch: proofEpoch,
            proofSeedB64: proofSeed.base64EncodedString())
        try record(entry, accountIdB64: context.accountIdB64) {
            _ = try? group.abandonStagedCommit(identity: identity)
        }
        // Section 13 — the seed this commit leaves still opens the posts of
        // the current server epoch here; kept now (the commit may merge).
        keepOutgoingSeed(
            state: state, channelIdHex: context.channelIdHex, serverEpoch: serverEpoch,
            group: group, identity: identity)
        // ADR-0064 section 7 (2026-11-14) — the channel's vault head gets the
        // epoch this commit moves to before it is sent: one attempt, and a
        // failure never stops the commit.
        await ChannelVaultHead.writeStaged(channelIdHex: context.channelIdHex, group: group, identity: identity)
        let landed = try await runRecorded(entry, context: context, state: state)
        if landed {
            refreshContentSeed(context, state: state, landedEpoch: serverTargetEpoch)
        }
        return (entry, landed)
    }

    /// Run the steps of an entry recorded here (``record(_:accountIdB64:abandon:)``)
    /// while marked running. Whether its commit landed (`false`: the journal
    /// lost it and it was abandoned). Throws a step's failure, the entry kept.
    static func runRecorded(
        _ entry: PublisherCommitEntry, context: Context, state: ChannelsUiState?
    ) async throws -> Bool {
        var landed = true
        running.insert(entry.id)
        defer { running.remove(entry.id) }
        let base = ports(context, state: state)
        var watched = base
        watched.finish = { finished in
            if finished.journalLost == true { landed = false }
            base.finish(finished)
        }
        try await PublisherCommitRunner.run(entry, ports: watched)
        return landed
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
                // ADR-0090 — the N+1 key of the STAGED commit is the one the
                // statement registers; the proof is the epoch-N key.
                guard let next = try group.dsStagedAccessSigner(identity: identity) else {
                    throw CommitStillStaged()
                }
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
            alreadyMerged: {
                guard let (_, group) = try? context.open(),
                      (try? group.hasStagedCommit()) == false else { return false }
                return true
            },
            adjudicate: { accepted in
                let (identity, group) = try context.open()
                if accepted {
                    _ = try group.mergeStagedCommit(identity: identity)
                } else {
                    _ = try group.abandonStagedCommit(identity: identity)
                }
                try ChannelPublisherIdentity.shared.persistOrThrow()
            },
            deliverWelcome: { welcome, joinerKey in
                // ADR-0090 — an approve records the joiner's DEVICE leaf; the
                // Welcome goes to the mailbox of the account it is bound to.
                let account = (try? context.open()).flatMap { opened in
                    CoPublisher.accountKey(ofLeaf: joinerKey, in: opened.group)
                } ?? joinerKey
                try await CoPublisher.deliverWelcome(ds: context.ds, welcome: welcome, joinerKey: account)
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
            appoint: { joinerLeaf in
                let (_, group) = try context.open()
                let groupId = try group.groupId()
                let joinerKey = CoPublisher.accountKey(ofLeaf: joinerLeaf, in: group) ?? joinerLeaf
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
        // ADR-0090 decision A — the group's MLS epoch maps to the channel's
        // through the recorded offset.
        guard let epoch = landedEpoch ?? (try? opened.group.epoch()).map({
            ChannelDeviceAdmission.channelEpoch(mls: $0, offset: state.extras(context.channelIdHex).epochOffset)
        }) else { return }
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
        for entry in PublisherCommitStore.shared.entries(accountIdB64: accountIdB64)
        where !running.contains(entry.id) {
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
            let staged = (loaded.flatMap { try? $0.hasStagedCommit() }) ?? false
            switch PublisherCommitEntry.replayVerdict(entry, restoredEpoch: restored, staged: staged) {
            case .discard:
                NSLog("Channels: a PublisherGroup commit whose staged state never became durable was dropped")
                // A staged commit with no entry left would freeze the group's
                // membership: abandon what this entry staged.
                if staged, let loaded { _ = try? loaded.abandonStagedCommit(identity: identity) }
                PublisherCommitStore.shared.remove(accountIdB64: accountIdB64, id: entry.id)
            case .replay:
                // ADR-0064 section 7 — a commit still owed to the journal: the
                // head carries its epoch (a no-op when this session wrote it).
                if staged, entry.kind != .entry, !entry.done.contains(.journal), let loaded {
                    await ChannelVaultHead.writeStaged(
                        channelIdHex: entry.channelIdHex, group: loaded, identity: identity)
                }
                running.insert(entry.id)
                defer { running.remove(entry.id) }
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
                subscriberPubkeyHex: subscriberHex, envelopeB64: base64UrlEncodeNoPad(envelope)))
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
