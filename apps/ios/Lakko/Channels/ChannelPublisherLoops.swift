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
    private var running = false

    /// A channel created on this device: its group is open and claimed.
    func markOpened(_ channelId: String) { opened.insert(channelId) }

    func reset() {
        opened = []
        publicSeedEpoch = [:]
        knockCursor = [:]
        journalCursor = [:]
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
            await rotateIfDue(channelId: channelId, channels: channels, client: client, seed: seed, accountIdB64: accountIdB64)
            await publishPublicSeed(channelId: channelId, channels: channels, client: client, seed: seed)
            await pollJournal(channelId: channelId, seed: seed)
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

    private func rotateIfDue(
        channelId: String, channels: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String
    ) async {
        guard opened.contains(channelId) else { return }
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

    /// Rotate now, outside the schedule: a subscriber of a private channel was
    /// removed (section 7.3), the only rotation a person ever causes.
    func rotateNow(channelId: String, channels: ChannelsUiState, client: ChannelsClient, seed: Data) async throws {
        guard let accountIdB64 = AcceptedContactsStore.boundAccountIdB64 else { return }
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
    /// the stored metadata record and ban list, carried verbatim.
    private func afterRotation(
        channelId: String, epoch: UInt32, channels: ChannelsUiState, client: ChannelsClient, seed: Data
    ) async {
        channels.setEpoch(channelId: channelId, epoch: epoch)
        guard let identity = try? Identity.fromSeed(seed: seed) else { return }
        if let record = channels.sharedMeta(channelId: channelId) {
            try? await ChannelMetaRecord.post(
                state: channels, client: client, channelId: channelId, meta: record, accountIdentity: identity)
        }
        let extras = channels.extras(channelId)
        if extras.banUpdatedAtMs > 0, let keys = channels.keysByChannel[channelId],
           let bytes = try? composeBanList(list: FfiBanList(
               updatedAtMs: extras.banUpdatedAtMs, authors: extras.bannedAuthors)),
           let counter = try? channels.nextCounter(channelId: channelId),
           let wire = try? ChannelsComposer.compose(
               keys: keys, channelIdHex: channelId, epochId: epoch, messageCounter: counter, plaintext: bytes) {
            _ = try? await client.publishPost(channelIdHex: channelId, payload: wire, accountIdentity: identity)
        }
        await publishPublicSeed(channelId: channelId, channels: channels, client: client, seed: seed)
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
        let epoch: UInt32
        if let held = channels.epochByChannel[channelId] {
            epoch = held
        } else if let fetched = try? await client.fetchEpoch(channelIdHex: channelId) {
            channels.setEpoch(channelId: channelId, epoch: fetched)
            epoch = fetched
        } else {
            return
        }
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
                channelId: channelId, identity: identity, group: current, ds: ds, readKey: readKey)
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
