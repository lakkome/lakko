// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import UIKit

/// ADR-0087 section 12, "The machinery leaves the screen" — the work a
/// channel's publisher device does by itself, app-wide, beside
/// ``SubscriberAdmission``, so a channel rotates whether or not its owner
/// opens it. Each pass reads only channels this device publishes.
///
/// One pass, for every published channel:
/// - opens its PublisherGroup once a session, and claims its journal;
/// - runs the rotation policy (``RotationCron``); after a rotation, re-issues
///   the stored metadata record with its original stamp (ADR-0051 section 4);
/// - for a public channel, writes the public seed envelope of the current
///   epoch under the channel's read key (section 6), once per epoch;
/// - reads the PublisherGroup journal from this session's cursor, paged to
///   the end, so commits apply; the owner reads the knocks too (co-publisher
///   requests, ADR-0028 OQ3), at most once a pass;
/// - section 7: the owner seals each admission-seed version to every
///   publisher that lacks it, and a co-publisher keeps the versions sealed
///   to it (``ChannelAdmissionSeedShare``).
@MainActor
final class ChannelPublisherLoops {
    static let shared = ChannelPublisherLoops()

    /// Between passes (chosen, not measured; web and Android use the same):
    /// the rotation policy's own clock is a day.
    static let intervalSeconds: UInt64 = 60

    private var opened: Set<String> = []
    private var publicSeedEpoch: [String: UInt32] = [:]
    private var knockCursor: [String: Int64] = [:]
    private var journalCursor: [String: Int64] = [:]
    /// Channels a current publisher asked this owner's device to rotate
    /// (ADR-0087 section 15, the rotate request); several collapse into one.
    private var rotateRequested: Set<String> = []
    private var running = false

    /// A channel created on this device: its group is open and claimed.
    func markOpened(_ channelId: String) { opened.insert(channelId) }

    func reset() {
        opened = []
        publicSeedEpoch = [:]
        knockCursor = [:]
        journalCursor = [:]
        rotateRequested = []
    }

    func runLoop(
        channels: ChannelsUiState, client: ChannelsClient = ChannelsClient(),
        accountIdentitySeed: () -> Data?
    ) async {
        while !Task.isCancelled {
            if UIApplication.shared.applicationState != .background, let seed = accountIdentitySeed() {
                await pass(channels: channels, client: client, seed: seed)
            }
            try? await Task.sleep(nanoseconds: Self.intervalSeconds * 1_000_000_000)
        }
    }

    func pass(channels: ChannelsUiState, client: ChannelsClient, seed: Data) async {
        guard !running, let accountIdB64 = AcceptedContactsStore.boundAccountIdB64 else { return }
        channelsRef = channels
        running = true
        defer { running = false }
        for channelId in channels.keysByChannel.keys.sorted() {
            if Task.isCancelled { return }
            if channels.orphanedByReset(channelId: channelId) { continue }
            await openOnce(channelId: channelId, accountIdB64: accountIdB64, seed: seed)
            refreshPublisherSeed(channelId: channelId, channels: channels, seed: seed)
            if rotateRequested.contains(channelId) {
                await rotateRequestedNow(channelId: channelId, channels: channels, client: client, seed: seed)
            }
            await rotateIfDue(channelId: channelId, channels: channels, client: client, seed: seed, accountIdB64: accountIdB64)
            await reissueOwedRecord(channelId: channelId, channels: channels, client: client, seed: seed)
            if !Self.rotates(channelId: channelId, channels: channels) {
                await sendOwedRotateRequest(channelId: channelId, channels: channels, seed: seed)
            }
            await publishPublicSeed(channelId: channelId, channels: channels, client: client, seed: seed)
            await pollJournal(channelId: channelId, seed: seed)
            // The owner's commits the poll applied (a rotation, an approve, a
            // removal) moved a co-publisher's content seed: the composer takes
            // it now, the old one stays under its epoch, and posts keep going
            // out under the committed epoch until the owner's reseal moves the
            // channel there (``ChannelPublisherSeed``).
            refreshPublisherSeed(channelId: channelId, channels: channels, seed: seed)
        }
    }

    private weak var channelsRef: ChannelsUiState?

    private func openOnce(channelId: String, accountIdB64: String, seed: Data) async {
        guard !opened.contains(channelId) else { return }
        do {
            let identity = try ChannelPublisherIdentity.shared.identity(currentSeed: seed)
            let group = try CoPublisher.openPublisherGroup(
                identity: identity, accountIdB64: accountIdB64, channelIdHex: channelId)
            ChannelPublisherIdentity.shared.persist()
            await CoPublisher.claimJournal(ds: DsClient(), identity: identity, publisherGroup: group)
            opened.insert(channelId)
            if let channelsRef {
                ChannelInvites.repairFlagIfMissing(channelIdHex: channelId, state: channelsRef, currentSeed: seed)
            }
        } catch {
            NSLog("[channels] a publisher group did not open")
        }
    }

    /// The composer's seed and the epoch it serves, from the PublisherGroup.
    /// A co-publisher follows the group every pass (the owner's commits move
    /// it). The owner's commits set it themselves at the epoch their reseal
    /// landed, so the owner's is read from the group only when not known
    /// yet (keys rebuilt at sign-in).
    private func refreshPublisherSeed(channelId: String, channels: ChannelsUiState, seed: Data) {
        guard opened.contains(channelId), let held = channels.keysByChannel[channelId] else { return }
        let owner = ChannelOwnership.isOwner(channelId: channelId, state: channels)
        guard !owner || held.seedEpoch == nil,
              let identity = try? ChannelPublisherIdentity.shared.identity(currentSeed: seed),
              let group = try? CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId),
              (try? group.isActive()) != false,
              let epoch = try? group.epoch(),
              let groupSeed = try? group.channelSeed(identity: identity) else { return }
        channels.notePublisherSeedAt(channelId: channelId, epoch: UInt32(clamping: epoch), seed: groupSeed)
    }

    private func rotateIfDue(
        channelId: String, channels: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String
    ) async {
        // Maintainer, 2026-11-08: only the channel's owner rotates its key; a
        // co-publisher's device never runs the rotation policy.
        guard opened.contains(channelId), Self.rotates(channelId: channelId, channels: channels) else { return }
        let cron = RotationCron(context: RotationCron.Context(
            commits: ChannelPublisherCommits.Context(
                channelIdHex: channelId, currentSeed: seed, accountIdB64: accountIdB64, channelsClient: client),
            state: channels,
            saveState: { newState in
                ChannelRotationStateStore.save(accountIdB64: accountIdB64, channelId: channelId, state: newState)
            },
            onRotated: nil))
        let state = ChannelRotationStateStore.loadOrSeed(
            accountIdB64: accountIdB64, channelId: channelId,
            nowMs: UInt64(Date().timeIntervalSince1970 * 1000))
        do {
            let result = try await cron.runOnce(policy: channelRotationPolicyDefault(), state: state)
            if let epoch = result.newEpoch {
                await afterRotation(channelId: channelId, epoch: epoch, channels: channels, client: client, seed: seed)
            }
        } catch {
            NSLog("[channels] a scheduled rotation did not complete; tried again next pass")
        }
    }

    /// A subscriber of a private channel was removed (section 7.3): the
    /// owner's device rotates now; a co-publisher's never rotates and asks
    /// the owner's device instead with a rotate request on the
    /// PublisherGroup, which the owner acts on at its next pass.
    func rotateAfterSubscriberRemoval(
        channelId: String, channels: ChannelsUiState, client: ChannelsClient, seed: Data
    ) async {
        if Self.rotates(channelId: channelId, channels: channels) {
            try? await rotateNow(channelId: channelId, channels: channels, client: client, seed: seed)
        } else if channels.keysByChannel[channelId] != nil {
            // Owed before it is published: a failed or killed send is retried
            // by every pass until it goes out.
            channels.updateExtras(channelId) { $0.rotateRequestOwed = true }
            await sendOwedRotateRequest(channelId: channelId, channels: channels, seed: seed)
        }
    }

    /// The owed rotate request, when owed, sent; the mark goes once it
    /// published.
    func sendOwedRotateRequest(channelId: String, channels: ChannelsUiState, seed: Data) async {
        await Self.sendOwedRotateRequest(channelId: channelId, channels: channels) {
            await self.sendRotateRequest(channelId: channelId, seed: seed)
        }
    }

    /// The rule over its port: nothing is sent unless owed; the mark is
    /// cleared only after a send that published.
    static func sendOwedRotateRequest(
        channelId: String, channels: ChannelsUiState, send: () async -> Bool
    ) async {
        guard channels.extras(channelId).rotateRequestOwed == true else { return }
        if await send() {
            channels.updateExtras(channelId) { $0.rotateRequestOwed = nil }
        } else {
            NSLog("[channels] the rotate request was not sent yet; the next pass tries again")
        }
    }

    /// A co-publisher's rotate request, as an application message on the
    /// channel's PublisherGroup journal. Returns whether it published.
    private func sendRotateRequest(channelId: String, seed: Data) async -> Bool {
        guard let channelBytes = ChannelsClient.hexToData(channelId),
              let identity = try? ChannelPublisherIdentity.shared.identity(currentSeed: seed),
              let group = try? CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId),
              let groupId = try? group.groupId() else { return false }
        let ds = DsClient()
        do {
            let envelope = try composeChannelRotateRequestEnvelope(channelId: channelBytes)
            let message = try group.createApplicationMessage(identity: identity, plaintext: envelope)
            // The send ratchet moved: on disk before the message leaves, so a
            // crash never reuses a generation (the G-34 rule).
            try ChannelPublisherIdentity.shared.persistOrThrow()
            _ = try await ds.publishGroup(
                groupId: groupId, payload: message,
                auth: CoPublisher.auth(ds: ds, identity: identity, publisherGroup: group))
            return true
        } catch {
            NSLog("[channels] the rotate request was not sent")
            return false
        }
    }

    /// Whether the owner's device acts on a rotate request: it names this
    /// channel, its sender resolved at receipt to a current publisher (a
    /// member of the PublisherGroup other than the owner), and it was sent
    /// after this channel's last rotation (a request already answered, read
    /// again after a restart, does nothing).
    nonisolated static func honorsRotateRequest(
        requestChannelId: Data?, channelId: Data, sender: Data?, ownerAccountKey: Data?,
        publisherAccountKeys: Set<Data>, sentAtMs: Int64?, lastRotationMs: UInt64?
    ) -> Bool {
        guard let requestChannelId, requestChannelId == channelId,
              let sender, sender != ownerAccountKey, publisherAccountKeys.contains(sender) else { return false }
        guard let lastRotationMs, lastRotationMs > 0 else { return true }
        guard let sentAtMs else { return false }
        return sentAtMs > Int64(clamping: lastRotationMs)
    }

    /// The owner reads the rotate requests a poll carried.
    private func noteRotateRequests(_ outcome: CoPublisher.PollOutcome, channelId: String, group: PublisherGroup) {
        guard let channelBytes = ChannelsClient.hexToData(channelId) else { return }
        let members = Set(((try? group.members()) ?? []).map {
            $0.accountKey.isEmpty ? $0.signaturePublicKey : $0.accountKey
        })
        let last = ChannelRotationStateStore.load(
            accountIdB64: AcceptedContactsStore.boundAccountIdB64, channelId: channelId)?.lastRotationMs
        for app in outcome.applications where Self.honorsRotateRequest(
            requestChannelId: try? decodeChannelRotateRequestEnvelope(plaintext: app.data),
            channelId: channelBytes, sender: app.senderAccount, ownerAccountKey: outcome.ownerAccountKey,
            publisherAccountKeys: members, sentAtMs: app.createdAtMs, lastRotationMs: last) {
            rotateRequested.insert(channelId)
        }
    }

    /// The owner rotates for a rotate request; the mark goes once it did.
    private func rotateRequestedNow(
        channelId: String, channels: ChannelsUiState, client: ChannelsClient, seed: Data
    ) async {
        guard Self.rotates(channelId: channelId, channels: channels) else {
            rotateRequested.remove(channelId)
            return
        }
        do {
            try await rotateNow(channelId: channelId, channels: channels, client: client, seed: seed)
            rotateRequested.remove(channelId)
        } catch {
            NSLog("[channels] a requested rotation did not complete; tried again next pass")
        }
    }

    /// Whether this device rotates `channelId`: only the owner's does.
    static func rotates(channelId: String, channels: ChannelsUiState) -> Bool {
        rotates(
            publishes: channels.keysByChannel[channelId] != nil,
            coPublished: CoPublishedJournals.shared.journalIdHex(channelIdHex: channelId) != nil)
    }

    /// The rule: a device that publishes the channel and did not join its
    /// PublisherGroup as a co-publisher is the owner's, and only it rotates.
    nonisolated static func rotates(publishes: Bool, coPublished: Bool) -> Bool {
        ChannelRowRole.of(publishes: publishes, coPublished: coPublished) == .owner
    }

    /// Rotate now, outside the schedule: a subscriber of a private channel was
    /// removed (section 7.3), the only rotation a person ever causes.
    func rotateNow(channelId: String, channels: ChannelsUiState, client: ChannelsClient, seed: Data) async throws {
        guard let accountIdB64 = AcceptedContactsStore.boundAccountIdB64,
              Self.rotates(channelId: channelId, channels: channels) else { return }
        await openOnce(channelId: channelId, accountIdB64: accountIdB64, seed: seed)
        let cron = RotationCron(context: RotationCron.Context(
            commits: ChannelPublisherCommits.Context(
                channelIdHex: channelId, currentSeed: seed, accountIdB64: accountIdB64, channelsClient: client),
            state: channels,
            saveState: { newState in
                ChannelRotationStateStore.save(accountIdB64: accountIdB64, channelId: channelId, state: newState)
            },
            onRotated: nil))
        let result = try await cron.runOnce(
            policy: ChannelRotationPolicy(intervalMs: 1, maxPosts: 1),
            state: ChannelRotationState(lastRotationMs: 0, postsSinceRotation: 0))
        if let epoch = result.newEpoch {
            await afterRotation(channelId: channelId, epoch: epoch, channels: channels, client: client, seed: seed)
        }
    }

    /// ADR-0051 section 4 and ADR-0087 section 13a — the newest epoch holds
    /// the stored metadata record and ban list, carried verbatim. Run after
    /// every commit that moved the channel's epoch: a rotation, and an
    /// approve or a removal of a publisher (a new publisher holds only the
    /// new epoch's seed, so the record must be there to be read).
    func afterRotation(
        channelId: String, epoch: UInt32, channels: ChannelsUiState, client: ChannelsClient, seed: Data
    ) async {
        channels.setEpoch(channelId: channelId, epoch: epoch)
        // Owed until it lands: a failed re-post is retried by every pass
        // (``reissueOwedRecord``), so the new epoch always carries the record.
        channels.updateExtras(channelId) { $0.recordOwed = true }
        await reissueOwedRecord(channelId: channelId, channels: channels, client: client, seed: seed)
        await publishPublicSeed(channelId: channelId, channels: channels, client: client, seed: seed)
    }

    /// The record and ban list re-post a commit made owed, when owed. Both
    /// are sealed under an epoch the channel service has reached
    /// (``ChannelPublisherSeed``). Cleared only once every part landed.
    func reissueOwedRecord(
        channelId: String, channels: ChannelsUiState, client: ChannelsClient, seed: Data
    ) async {
        guard channels.extras(channelId).recordOwed == true,
              let identity = try? Identity.fromSeed(seed: seed) else { return }
        let landed = await Self.reissueRecord(
            meta: channels.sharedMeta(channelId: channelId),
            banList: banListPlaintext(channels.extras(channelId)),
            post: { plaintext in
                guard let keys = channels.keysByChannel[channelId],
                      let sealing = channels.composerSeed(channelId: channelId) else {
                    throw ChannelPublisherSeedUnavailable()
                }
                let counter = try channels.nextCounter(channelId: channelId)
                let wire = try ChannelsComposer.compose(
                    keys: ChannelPublisherKeys(seed: sealing.seed, signingKeySeed: keys.signingKeySeed),
                    channelIdHex: channelId, epochId: sealing.epoch, messageCounter: counter, plaintext: plaintext)
                _ = try await client.publishPost(channelIdHex: channelId, payload: wire, accountIdentity: identity)
            },
            postMeta: { meta in
                try await ChannelMetaRecord.post(
                    state: channels, client: client, channelId: channelId, meta: meta, accountIdentity: identity)
            })
        if landed {
            channels.updateExtras(channelId) { $0.recordOwed = nil }
        } else {
            NSLog("[channels] the channel record was not re-posted yet; the next pass tries again")
        }
    }

    /// One attempt at the owed re-post: the record, then the ban list. True
    /// when every part that exists landed. Pure over its ports.
    static func reissueRecord(
        meta: SharedChannelMeta?, banList: Data?,
        post: (Data) async throws -> Void,
        postMeta: (SharedChannelMeta) async throws -> Void
    ) async -> Bool {
        do {
            if let meta { try await postMeta(meta) }
            if let banList { try await post(banList) }
            return true
        } catch {
            return false
        }
    }

    private func banListPlaintext(_ extras: ChannelExtras) -> Data? {
        guard extras.banUpdatedAtMs > 0 else { return nil }
        return try? composeBanList(list: FfiBanList(
            updatedAtMs: extras.banUpdatedAtMs, authors: extras.bannedAuthors))
    }

    /// The epoch a publisher files its public seed under: the one its
    /// composer seed serves, never one the channel has not reached (`nil`).
    nonisolated static func publicSeedEpoch(keys: ChannelPublisherKeys, channelEpoch: UInt32) -> UInt32? {
        let epoch = keys.seedEpoch ?? channelEpoch
        return epoch > channelEpoch ? nil : epoch
    }

    /// Section 6 — a public channel's seed for the current epoch, sealed under
    /// its read key, written once per epoch (the route is idempotent).
    private func publishPublicSeed(
        channelId: String, channels: ChannelsUiState, client: ChannelsClient, seed: Data
    ) async {
        let extras = channels.extras(channelId)
        guard extras.isPublic == true, let keys = channels.keysByChannel[channelId],
              let id = ChannelsClient.hexToData(channelId) else { return }
        var readKey = extras.readKey
        // A co-publisher writes the seed under the read key the owner sealed
        // to it; deriving one of its own would give its lineage's, not the
        // channel's.
        if readKey == nil, ChannelOwnership.isOwner(channelId: channelId, state: channels) {
            readKey = ChannelLineage.readKey(
                accountIdB64: AcceptedContactsStore.boundAccountIdB64, currentSeed: seed, channelId: id)
            if let readKey { channels.updateExtras(channelId) { $0.readKey = readKey } }
        }
        guard let readKey, let identity = try? Identity.fromSeed(seed: seed) else { return }
        let current: UInt32
        if let held = channels.epochByChannel[channelId] {
            current = held
        } else if let fetched = try? await client.fetchEpoch(channelIdHex: channelId) {
            channels.setEpoch(channelId: channelId, epoch: fetched)
            current = fetched
        } else {
            return
        }
        // ADR-0087 section 15 — the composer's seed under the epoch it serves,
        // and only once the channel reached that epoch (the owner's reseal
        // files the next one).
        guard let epoch = Self.publicSeedEpoch(keys: keys, channelEpoch: current) else { return }
        guard publicSeedEpoch[channelId] != epoch else { return }
        do {
            let envelope = try sealPublicSeedEnvelope(
                readKey: readKey, channelId: id, epochId: epoch, epochSeed: keys.seed)
            try await client.putPublicSeed(
                channelIdHex: channelId, epoch: epoch, envelope: envelope, accountIdentity: identity)
            publicSeedEpoch[channelId] = epoch
        } catch let error as ChannelsApiError where error.code == "public-seed-exists" {
            publicSeedEpoch[channelId] = epoch
        } catch {
            NSLog("[channels] a public seed was not written; tried again next pass")
        }
    }

    private func pollJournal(channelId: String, seed: Data) async {
        guard opened.contains(channelId),
              let identity = try? ChannelPublisherIdentity.shared.identity(currentSeed: seed),
              let group = try? CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId)
        else { return }
        let owner = channelsRef.map { ChannelOwnership.isOwner(channelId: channelId, state: $0) } ?? false
        let ds = DsClient()
        if let outcome = try? await CoPublisher.pollJournal(
            ds: ds, identity: identity, publisherGroup: group,
            knockAfter: knockCursor[channelId] ?? 0,
            readKnocks: owner,
            after: journalCursor[channelId] ?? 0) {
            knockCursor[channelId] = outcome.knockCursor
            journalCursor[channelId] = outcome.journalCursor
            ChannelPublisherIdentity.shared.persist()
            if owner {
                ChannelAdmissionSeedShare.rememberRequests(outcome.requests, channelId: channelId)
                if let groupId = try? group.groupId() {
                    PublisherLinkApproval.remember(outcome.requests, groupId: groupId)
                }
                CoPublisherKeyBook.noteBindings(
                    outcome.applications, ownerAccountKey: outcome.ownerAccountKey, channelIdHex: channelId)
                await applyStepDowns(outcome, channelId: channelId, seed: seed)
                noteRotateRequests(outcome, channelId: channelId, group: group)
            } else {
                ChannelAdmissionSeedShare.receive(
                    outcome.applications, ownerAccountKey: outcome.ownerAccountKey,
                    channelId: channelId, currentSeed: seed, state: channelsRef)
            }
        }
        // ADR-0087 section 15 — removed (or stepped down and applied): this
        // device publishes no more.
        if !owner, (try? group.isActive()) == false {
            endPublishing(channelId: channelId)
            return
        }
        // Reopened: a step-down applied above moved the group.
        let current = (try? CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId)) ?? group
        CoPublisher.refreshPublishers(identity: identity, group: current, channelIdHex: channelId)
        if !owner {
            await sendOwnBindingIfOwed(channelId: channelId, identity: identity, group: current, ds: ds)
        }
        if owner {
            // ADR-0087 section 15 — a request by a link this account named
            // a person with is approved without asking.
            await PublisherLinkApproval.approveNamed(
                channelId: channelId, seed: seed, group: current, state: channelsRef)
            // An approve moved the group: reopen before sealing anything to it.
            let current = (try? CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId)) ?? current
            let extras = channelsRef?.extras(channelId)
            let readKey = extras?.isPublic == true ? extras?.readKey : nil
            await ChannelAdmissionSeedShare.sendOwed(
                channelId: channelId, identity: identity, group: current, ds: ds, readKey: readKey,
                epochSeeds: channelsRef?.readSeeds(channelId: channelId) ?? [:])
        }
    }

    /// A publisher sends its own co-publisher binding (ADR-0059) into the
    /// PublisherGroup once per epoch, so every device of the owner's account
    /// (one added later included: it reads only the epochs after its join)
    /// holds the key it appointed and any of them can revoke the appointment
    /// when this publisher is removed (ADR-0087 section 15.2).
    private func sendOwnBindingIfOwed(
        channelId: String, identity: Identity, group: PublisherGroup, ds: DsClient
    ) async {
        guard let epoch = try? group.epoch(),
              CoPublisherKeyBook.bindingSentEpoch(channelIdHex: channelId) != epoch,
              let channelBytes = ChannelsClient.hexToData(channelId),
              let groupId = try? group.groupId() else { return }
        do {
            let binding = try composeChannelCopublisherBinding(accountIdentity: identity, channelId: channelBytes)
            let message = try group.createApplicationMessage(identity: identity, plaintext: binding)
            // The send ratchet moved: on disk before the message leaves, so a
            // crash never reuses a generation (the G-34 rule).
            try ChannelPublisherIdentity.shared.persistOrThrow()
            _ = try await ds.publishGroup(
                groupId: groupId, payload: message,
                auth: CoPublisher.auth(ds: ds, identity: identity, publisherGroup: group))
            CoPublisherKeyBook.noteBindingSent(channelIdHex: channelId, epoch: epoch)
        } catch {
            NSLog("[channels] the publisher binding was not sent; the next pass sends it")
        }
    }

    /// The owner applies each publisher's step-down as a removal of the
    /// account its sender resolved to at receipt (C-68).
    private func applyStepDowns(_ outcome: CoPublisher.PollOutcome, channelId: String, seed: Data) async {
        guard let channelBytes = ChannelsClient.hexToData(channelId),
              let account = AcceptedContactsStore.boundAccountIdB64 else { return }
        var removed: Set<Data> = []
        for app in outcome.applications {
            guard isPublisherStepDown(plaintext: app.data, channelId: channelBytes),
                  let sender = app.senderAccount, sender != outcome.ownerAccountKey,
                  !removed.contains(sender) else { continue }
            removed.insert(sender)
            do {
                try await CoPublisher.removePublisher(
                    context: ChannelPublisherCommits.Context(
                        channelIdHex: channelId, currentSeed: seed, accountIdB64: account),
                    state: channelsRef, accountKey: sender)
            } catch {
                NSLog("[channels] a publisher's step-down was not applied; the next pass reads it again")
                journalCursor[channelId] = nil
            }
        }
    }

    /// This device no longer publishes `channelId`: its publisher keys go,
    /// and the channel with them unless the account also reads it as a
    /// subscriber (UX 15.2: they stay subscribed if they were).
    private func endPublishing(channelId: String) {
        guard let channels = channelsRef else { return }
        opened.remove(channelId)
        journalCursor[channelId] = nil
        knockCursor[channelId] = nil
        if channels.subscriberKeysByChannel[channelId] != nil {
            channels.dropPublisherKeys(channelId)
        } else {
            let name = channels.channels.first { $0.id == channelId }?.title
            channels.removeChannel(channelId)
            channels.onRemovedHere?(channelId, name)
        }
    }
}
