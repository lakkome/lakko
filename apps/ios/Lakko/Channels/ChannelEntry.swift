// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0064 section 7, amendment of 2026-11-14 (items 4 and 10) — an owner's
/// device that holds no PublisherGroup for an owned channel enters it by
/// itself from the GroupInfos the owner's devices keep in the channel's vault
/// head (``ChannelVaultHead``), newest saved epoch first: an external commit
/// (ExternalInit alone, its own bound device leaf) STAGED and recorded like
/// any PublisherGroup commit (``PublisherCommitEntry/Kind/entry``), sent
/// through the DS commit route under the journal root and proven with the
/// saved epoch's access seed; on `201` merged and the new seed resealed to
/// the subscribers; on `409` or a refusal abandoned and the older epoch
/// tried; none left: the device waits as before (decision A when it
/// applies). Nothing new is shown: the composer appears.
///
/// A co-publisher's device never enters (it is admitted by an owner device,
/// ADR-0090 decision B), and a device that holds the group never does.
@MainActor
enum ChannelEntry {

    // MARK: - Pure rules

    /// Whether this device tries to enter: the account OWNS the channel, the
    /// channel is not one this account co-publishes, and this device holds
    /// no PublisherGroup for it (a held group, a suspected reset included, is
    /// never entered over).
    nonisolated static func shouldEnter(accountRole: String?, coPublished: Bool, holdsGroup: Bool) -> Bool {
        accountRole == ChannelAccountRole.owner && !coPublished && !holdsGroup
    }

    /// What one entry from one saved epoch came to.
    enum Attempt: Equatable {
        /// The DS took the commit; merged and resealed.
        case landed
        /// `409`, a refused proof, a GroupInfo the core refused: try the
        /// older saved epoch.
        case lost
        /// Not answered (a network failure, a failed record): the staged
        /// commit, when there is one, stays owed; stop here, the next pass
        /// goes on.
        case unfinished
    }

    /// What an entry pass came to.
    enum Outcome: Equatable {
        /// Entered at the saved epoch `epoch`.
        case entered(epoch: UInt64)
        /// No head, or a head with no saved epoch.
        case noHead
        /// Every saved epoch was refused.
        case allRefused
        /// Stopped on an unanswered attempt or an unreadable head.
        case unfinished
    }

    /// Try `saved` newest first until one lands; an unanswered attempt stops
    /// the pass.
    static func enter(
        saved: [VaultReentryEpoch]?, attempt: (VaultReentryEpoch) async -> Attempt
    ) async -> Outcome {
        guard let saved else { return .unfinished }
        guard !saved.isEmpty else { return .noHead }
        for entry in saved.sorted(by: { $0.epoch > $1.epoch }) {
            switch await attempt(entry) {
            case .landed: return .entered(epoch: entry.epoch)
            case .lost: continue
            case .unfinished: return .unfinished
            }
        }
        return .allRefused
    }

    /// After a head that held nothing usable, the next try waits this long.
    /// Chosen, not measured.
    nonisolated static let retryAfterNothingMs: UInt64 = 5 * 60_000

    private static var nextTryAtMs: [String: UInt64] = [:]

    /// The Delivery Service client an entry sends through (a seam for tests).
    static var dsClient: @MainActor () -> DsClient = { DsClient() }

    static func reset() { nextTryAtMs = [:] }

    private static func nowMs() -> UInt64 { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }

    // MARK: - The live entry

    /// One pass over a waiting owned channel: read the head and enter.
    /// Whether this device now publishes the channel. Falls back (returns
    /// `false`) when there is no head, every epoch was refused, or the pass
    /// stopped unanswered.
    static func enterIfPossible(
        channelId: String, state: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String
    ) async -> (entered: Bool, fallBack: Bool) {
        if let next = nextTryAtMs[channelId], nowMs() < next { return (false, true) }
        let saved = await ChannelVaultHead.readReentry(channelIdHex: channelId)
        let outcome = await enter(saved: saved) { entry in
            await attempt(entry, channelId: channelId, state: state, client: client, seed: seed,
                          accountIdB64: accountIdB64)
        }
        switch outcome {
        case .entered(let epoch):
            NSLog("ADR-0064: this owner device entered a channel's publisher group (saved epoch %llu)", epoch)
            nextTryAtMs[channelId] = nil
            return (true, false)
        case .noHead, .allRefused:
            nextTryAtMs[channelId] = nowMs() &+ retryAfterNothingMs
            return (false, true)
        case .unfinished:
            return (false, false)
        }
    }

    /// Enter from one saved epoch, record, send, adjudicate, reseal; then
    /// publish from here.
    static func attempt(
        _ saved: VaultReentryEpoch, channelId: String, state: ChannelsUiState, client: ChannelsClient,
        seed: Data, accountIdB64: String
    ) async -> Attempt {
        guard let identity = try? ChannelPublisherIdentity.shared.identity(currentSeed: seed),
              let ownerSeed = ChannelPublisherIdentity.ownerAccountSeed,
              let channelBytes = ChannelsClient.hexToData(channelId) else { return .unfinished }
        let current: UInt32
        do {
            current = try await client.fetchEpoch(channelIdHex: channelId)
        } catch {
            return .unfinished
        }
        let target = current &+ 1
        guard target > current else { return .unfinished }
        // The core refuses an entry while a group is stored under the id: an
        // inactive one (this device's leaf was removed) is deleted right
        // before; an active one means this device does not enter.
        guard ChannelDeviceAdmission.deleteInactiveGroup(identity: identity, channelId: channelId) else {
            return .unfinished
        }
        let entered: FfiPublisherGroupEntry
        do {
            // G-55: the commit comes back wrapped under the saved epoch's
            // access seed; the Delivery Service never reads the leaf's binding.
            entered = try publisherGroupEnter(
                identity: identity, groupInfo: saved.groupInfo, ownerAccountSeed: ownerSeed,
                channelId: channelBytes, dsAccessSeed: saved.dsAccessSeed)
        } catch {
            NSLog("ADR-0064: a saved GroupInfo was refused (%@); trying the older one",
                  String(describing: type(of: error)))
            return .lost
        }
        let group = entered.group
        guard let groupId = try? group.groupId() else {
            _ = try? group.abandonStagedCommit(identity: identity)
            return .unfinished
        }
        let left = (try? group.epoch()) ?? saved.epoch
        let entry = PublisherCommitEntry(
            kind: .entry, channelIdHex: channelId.lowercased(), groupIdHex: DsClient.pseudonymHex(groupId),
            targetEpoch: left &+ 1, serverTargetEpoch: target,
            commitB64: entered.commit.base64EncodedString(),
            proofEpoch: saved.epoch, proofSeedB64: saved.dsAccessSeed.base64EncodedString())
        do {
            try ChannelPublisherCommits.record(entry, accountIdB64: accountIdB64) {
                _ = try? group.abandonStagedCommit(identity: identity)
            }
        } catch {
            return .unfinished
        }
        let context = ChannelPublisherCommits.Context(
            channelIdHex: channelId, currentSeed: seed, accountIdB64: accountIdB64, channelsClient: client,
            ds: dsClient())
        let landed: Bool
        do {
            landed = try await ChannelPublisherCommits.runRecorded(entry, context: context, state: state)
        } catch {
            NSLog("ADR-0064: a channel entry is owed; the next pass sends it again")
            return .unfinished
        }
        guard landed else { return .lost }
        await afterLanding(channelId: channelId, state: state, client: client, seed: seed,
                           accountIdB64: accountIdB64, context: context, target: target)
        return .landed
    }

    /// The entry merged and resealed at `target`: publish from here, the
    /// channel's epoch offset known, and the head written at the new epoch.
    static func afterLanding(
        channelId: String, state: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String,
        context: ChannelPublisherCommits.Context, target: UInt32
    ) async {
        ChannelPublisherIdentity.shared.persist()
        guard let opened = try? context.open() else { return }
        state.updateExtras(channelId) { $0.resetSuspected = nil }
        guard ChannelDeviceAdmission.becomePublisher(
            channelId: channelId, state: state, identity: opened.identity) else { return }
        if let mls = try? opened.group.epoch() {
            state.updateExtras(channelId) {
                $0.epochOffset = Int64(target) &- Int64(clamping: mls)
                $0.epochOffsetToLearn = nil
            }
        }
        if let seedBytes = try? opened.group.channelSeed(identity: opened.identity) {
            state.notePublisherSeedAt(channelId: channelId, epoch: target, seed: seedBytes)
        }
        // A seed-changing commit counts as a rotation for the policy.
        ChannelRotationStateStore.save(
            accountIdB64: accountIdB64, channelId: channelId,
            state: channelRotationStateAfterRotation(nowMs: nowMs()))
        await ChannelVaultHead.writeCurrent(channelIdHex: channelId, group: opened.group, identity: opened.identity)
        await ChannelPublisherLoops.shared.afterRotation(
            channelId: channelId, epoch: target, channels: state, client: client, seed: seed)
    }
}
