// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation
import LakkoCore

/// ADR-0090 — the devices of an account that publishes a channel, as the
/// PublisherGroup sees them.
///
/// - **Owner devices (2.1).** Every publisher pass, an owner device that
///   holds the PublisherGroup compares the owner-account leaves with the
///   account's DeviceGroup roster: a device in the roster with no leaf is
///   admitted by the KeyPackage it published to discovery (its binding must
///   name the owner's account), a leaf whose device left the roster is
///   retired; everything found in one STAGED commit. The Welcome goes to the
///   account's own mailbox, and the hand-over (read key, admission seeds,
///   newest epoch seeds) is sealed to the account.
/// - **Co-publisher devices (2.2, decision B).** A co-publisher's publishing
///   device runs the same comparison for its own account and SENDS a device
///   request (`publisher_device_request`) on the PublisherGroup; it never
///   commits. An owner device applies a request from a current publisher
///   account whose KeyPackage binding (admit) or leaf (retire) is that
///   account's.
/// - **Waiting devices (section 8).** A device of an account that publishes,
///   with no leaf yet, reads; once a Welcome lands it publishes, and the
///   waiting line goes.
/// - **Decision A (2.5).** A device that founded the account's DeviceGroup
///   alone founds each owned channel's PublisherGroup again (same id, journal
///   reset under the same root) and reseals; an old device that comes back
///   finds its journal refused, deletes its group and waits to be admitted.
/// - **Bookkeeping (section 6).** The admission mark travels as a
///   `channel_admission_mark` message, kept as the maximum; the owner
///   account's admission seeds as a `lakko-sync` register.
@MainActor
enum ChannelDeviceAdmission {

    // MARK: - Wiring (the composition root)

    /// The leaf keys of the account's DeviceGroup, or `nil` when unknown.
    static var deviceGroupRoster: @MainActor () -> [Data]? = { nil }
    /// Whether this device founded the account's DeviceGroup because no other
    /// device answered its knock (ADR-0081 third amendment, point 4).
    static var foundedDeviceGroupAlone: @MainActor () -> Bool = { false }
    /// The synced state's admission-seed register.
    static weak var seedRegister: DeviceGroupSyncRepository?

    // MARK: - Pure rules

    /// Section 2.1 — the devices of `accountKey` missing from the group, and
    /// its leaves whose device left the roster (never `selfLeaf`, which only
    /// another owner device can retire).
    nonisolated static func ownDeviceDiff(
        leaves: [(leaf: Data, account: Data)], accountKey: Data, roster: [Data], selfLeaf: Data
    ) -> (missing: Set<Data>, retire: [Data]) {
        let own = Set(leaves.filter { $0.account == accountKey }.map(\.leaf))
        let rosterSet = Set(roster)
        let missing = rosterSet.subtracting(own)
        let retire = own.subtracting(rosterSet).filter { $0 != selfLeaf }.sorted { $0.lexicographicallyPrecedes($1) }
        return (missing, retire)
    }

    /// Section 2.1 — the KeyPackages admitted for `missing`: only one whose
    /// device id is missing AND whose binding names `accountKey`. A device is
    /// never added on the strength of a KeyPackage alone.
    nonisolated static func admittable(
        keyPackages: [DeviceKeyPackage], missing: Set<Data>, accountKey: Data,
        accountKeyOf: (Data) -> Data?
    ) -> [Data] {
        keyPackages.filter { missing.contains($0.deviceId) && accountKeyOf($0.keyPackage) == accountKey }
            .map(\.keyPackage)
    }

    /// What an owner device does with a device request.
    enum DeviceRequestAction: Equatable {
        case admit(keyPackage: Data, account: Data)
        case retire(leaf: Data, account: Data)
    }

    /// Section 2.2 / 2.4 — whether an owner device acts on a device request
    /// read from its PublisherGroup: it names this channel; its sender
    /// resolved at receipt (C-68) to an account holding a leaf here; an admit
    /// comes from a co-publisher (owner devices are admitted by the roster),
    /// its KeyPackage is bound to the sender and its device has no leaf yet,
    /// and the account stays within 8 leaves; a retire names a leaf of the
    /// sender's own account (an owner device's retire request included).
    nonisolated static func deviceRequestAction(
        channelId: Data, requestChannelId: Data, action: FfiPublisherDeviceAction,
        sender: Data?, ownerAccountKey: Data, leaves: [(leaf: Data, account: Data)],
        keyPackageAccountKey: (Data) -> Data?, keyPackageLeaf: (Data) -> Data?
    ) -> DeviceRequestAction? {
        guard requestChannelId == channelId, let sender,
              leaves.contains(where: { $0.account == sender }) else { return nil }
        switch action {
        case .admit(let keyPackage, _, _):
            guard sender != ownerAccountKey, keyPackageAccountKey(keyPackage) == sender,
                  let leaf = keyPackageLeaf(keyPackage),
                  !leaves.contains(where: { $0.leaf == leaf }),
                  leaves.filter({ $0.account == sender }).count < maxLeavesPerAccount else { return nil }
            return .admit(keyPackage: keyPackage, account: sender)
        case .retire(let deviceKey):
            guard leaves.contains(where: { $0.leaf == deviceKey && $0.account == sender }) else { return nil }
            return .retire(leaf: deviceKey, account: sender)
        }
    }

    /// ADR-0050 section 2.4, held in a PublisherGroup too.
    nonisolated static let maxLeavesPerAccount = 8

    /// Section 6 — the admission mark kept: the maximum of the held one and
    /// one a current publisher sent.
    nonisolated static func mergedMark(held: UInt64?, received: UInt64) -> UInt64 {
        max(held ?? 0, received)
    }

    /// Whether a device's own mark must be sent: it advanced past what was
    /// last sent or received.
    nonisolated static func markOwed(mark: UInt64?, sent: UInt64?) -> UInt64? {
        guard let mark, mark > (sent ?? 0) else { return nil }
        return mark
    }

    /// Decision A — this device founds an owned channel's PublisherGroup
    /// again: it founded the DeviceGroup alone, is still the account's only
    /// device, the account owns the channel and this device holds no group.
    nonisolated static func shouldRefound(
        foundedAlone: Bool, rosterCount: Int?, accountRole: String?, holdsGroup: Bool
    ) -> Bool {
        foundedAlone && rosterCount == 1 && accountRole == ChannelAccountRole.owner && !holdsGroup
    }

    /// Decision A — the channel epoch of a PublisherGroup MLS epoch.
    nonisolated static func channelEpoch(mls: UInt64, offset: Int64?) -> UInt32 {
        UInt32(clamping: Int64(clamping: mls) &+ (offset ?? 0))
    }

    /// The offset a device learns at landing: the service's epoch minus the
    /// group's MLS epoch (the reseal ran before the Welcome was sent).
    nonisolated static func learnedOffset(serverEpoch: UInt32, mls: UInt64) -> Int64 {
        Int64(serverEpoch) &- Int64(clamping: mls)
    }

    /// Learn the epoch offset of a device just admitted. Best effort.
    static func learnEpochOffsetIfOwed(
        channelId: String, state: ChannelsUiState, group: PublisherGroup, client: ChannelsClient
    ) async {
        guard state.extras(channelId).epochOffsetToLearn == true,
              let mls = try? group.epoch(),
              let server = try? await client.fetchEpoch(channelIdHex: channelId) else { return }
        let offset = learnedOffset(serverEpoch: server, mls: mls)
        state.updateExtras(channelId) {
            $0.epochOffset = offset == 0 ? nil : offset
            $0.epochOffsetToLearn = nil
        }
    }

    /// Section 2.5 — signed journal reads refused in a row before a device
    /// takes its group as reset under it (ADR-0081 third amendment point 5's
    /// count).
    nonisolated static let refusedReadsToDrop = 3

    /// Section 2.2 — between two sends of an unanswered co-publisher device
    /// request (a delegation lives 24 h). Chosen, not measured.
    nonisolated static let deviceRequestResendMs: UInt64 = 30 * 60 * 1000

    // MARK: - Session state

    private static var refusedReads: [String: Int] = [:]
    private static var requestSentAt: [String: UInt64] = [:]

    static func reset() {
        refusedReads = [:]
        requestSentAt = [:]
        probeSuccesses = [:]
    }

    /// The owed own-account hand-overs and the queued device requests, kept
    /// per account across a relaunch (``ChannelDeviceAdmissionStore``).
    private static var store: ChannelDeviceAdmissionStore? {
        AcceptedContactsStore.boundAccountIdB64.map { ChannelDeviceAdmissionStore(accountIdB64: $0) }
    }

    static func leaves(_ group: PublisherGroup) -> [(leaf: Data, account: Data)] {
        ((try? group.members()) ?? []).map {
            (leaf: $0.signaturePublicKey, account: $0.accountKey.isEmpty ? $0.signaturePublicKey : $0.accountKey)
        }
    }

    private static func nowMs() -> UInt64 { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }

    // MARK: - Section 2.1: an owner device admits and retires its own devices

    /// Diff the owner-account leaves with the DeviceGroup roster and commit
    /// what differs in one staged commit. Best effort; the next pass tries
    /// again.
    static func reconcileOwnDevices(
        context: ChannelPublisherCommits.Context, state: ChannelsUiState, identity: Identity, group: PublisherGroup
    ) async {
        guard (try? group.isOwnerDevice(identity: identity)) == true,
              (try? group.hasStagedCommit()) == false,
              let roster = deviceGroupRoster(), !roster.isEmpty,
              let ownerKey = try? group.ownerAccountKey() else { return }
        let diff = ownDeviceDiff(
            leaves: leaves(group), accountKey: ownerKey, roster: roster, selfLeaf: identity.signaturePublicKey())
        guard !diff.missing.isEmpty || !diff.retire.isEmpty else { return }
        var admit: [Data] = []
        if !diff.missing.isEmpty, let accountId = base64Decode(context.accountIdB64) {
            let discovery = DiscoveryClient(sessionToken: AmbientSessionToken.shared.current)
            let packages = ((try? await discovery.fetchKeyPackages(accountId: accountId)) ?? nil) ?? []
            admit = admittable(
                keyPackages: packages, missing: diff.missing, accountKey: ownerKey,
                accountKeyOf: { (try? keyPackageAccountKey(keyPackage: $0)) ?? nil })
        }
        guard !admit.isEmpty || !diff.retire.isEmpty else { return }
        NSLog("ADR-0090: an owner device admits %d and retires %d of the account's devices",
              admit.count, diff.retire.count)
        await commitDevices(
            context: context, state: state, admit: admit, retire: diff.retire, welcomeTo: ownerKey)
        if !admit.isEmpty {
            ChannelDeviceAdmissionStore(accountIdB64: context.accountIdB64).markHandoverOwed(context.channelIdHex)
        }
    }

    /// One staged admission / retirement commit; the Welcome (when anything
    /// was admitted) goes to `welcomeTo`'s mailbox.
    @discardableResult
    static func commitDevices(
        context: ChannelPublisherCommits.Context, state: ChannelsUiState,
        admit: [Data], retire: [Data], welcomeTo: Data
    ) async -> Bool {
        do {
            let current = try await context.channelsClient.fetchEpoch(channelIdHex: context.channelIdHex)
            let target = current &+ 1
            guard target > current else { return false }
            let result = try await ChannelPublisherCommits.stageAndRun(
                context, state: state, kind: .devices, joinerKey: welcomeTo,
                serverTargetEpoch: target, serverEpoch: current
            ) { identity, group in
                let staged = try group.stageCommit(identity: identity, admit: admit, retire: retire, rotate: false)
                return .init(commit: staged.commit, welcome: staged.welcome)
            }
            guard result.landed else { return false }
            // Section 4 — a seed-changing commit counts as a rotation for the
            // policy and restarts its clock.
            ChannelRotationStateStore.save(
                accountIdB64: context.accountIdB64, channelId: context.channelIdHex,
                state: channelRotationStateAfterRotation(nowMs: nowMs()))
            await ChannelPublisherLoops.shared.afterRotation(
                channelId: context.channelIdHex, epoch: target, channels: state,
                client: context.channelsClient, seed: context.currentSeed)
            return true
        } catch {
            NSLog("ADR-0090: a device admission commit did not complete; the next pass tries again (%@)",
                  String(describing: type(of: error)))
            return false
        }
    }

    /// Section 6 — after an own-device admission merged: the read key, every
    /// admission seed and the newest epoch seeds, sealed to the OWN account
    /// (the added device opens them with the account identity). Best effort;
    /// receivers drop what they hold.
    static func sendOwnHandoverIfOwed(
        channelId: String, state: ChannelsUiState, identity: Identity, group: PublisherGroup, ds: DsClient
    ) async {
        guard let store, store.handoverOwed(channelId),
              let account = try? ChannelPublisherIdentity.ownerAccountIdentity() else { return }
        let extras = state.extras(channelId)
        let sent = await ChannelAdmissionSeedShare.sendAll(
            channelId: channelId, identity: identity, group: group, ds: ds,
            accountKey: account.signaturePublicKey(), x25519: account.x25519PublicKey(),
            readKey: extras.isPublic == true ? extras.readKey : nil,
            epochSeeds: state.readSeeds(channelId: channelId))
        if sent { store.clearHandoverOwed(channelId) }
    }

    // MARK: - Security pass: an applied entry is reconciled at once

    /// Whether a commit this device applied added a leaf of the owner's
    /// account (an owner device's entry, ADR-0064 section 7, or another
    /// owner device's admission): leaves in `after` that `before` lacked.
    nonisolated static func addedOwnerLeaves(
        before: [(leaf: Data, account: Data)], after: [(leaf: Data, account: Data)], ownerAccountKey: Data
    ) -> Bool {
        let held = Set(before.map(\.leaf))
        return after.contains { $0.account == ownerAccountKey && !held.contains($0.leaf) }
    }

    /// The own-device reconcile run after an applied entry (a seam for
    /// tests; production runs ``reconcileOwnDevices(context:state:identity:group:)``).
    static var reconcileNow: @MainActor (
        ChannelPublisherCommits.Context, ChannelsUiState, Identity, PublisherGroup
    ) async -> Void = { context, state, identity, group in
        await reconcileOwnDevices(context: context, state: state, identity: identity, group: group)
    }

    /// The leaves of `group`, each with its account (for ``afterApplied``).
    static func leafAccounts(_ group: PublisherGroup) -> [(leaf: Data, account: Data)] { leaves(group) }

    /// Security pass — after a journal poll applied commits: when one added
    /// a leaf of the owner's account, the own-device reconcile of section
    /// 2.1 runs now, with its usual roster rules, so a leaf of a device not
    /// in the account's DeviceGroup roster is retired in this pass. Owner
    /// devices only. Whether it ran.
    @discardableResult
    static func afterApplied(
        before: [(leaf: Data, account: Data)], context: ChannelPublisherCommits.Context, state: ChannelsUiState,
        identity: Identity, group: PublisherGroup
    ) async -> Bool {
        guard (try? group.isOwnerDevice(identity: identity)) == true,
              let ownerKey = try? group.ownerAccountKey(),
              addedOwnerLeaves(before: before, after: leaves(group), ownerAccountKey: ownerKey) else { return false }
        NSLog("ADR-0090: an applied commit added an owner device leaf; the own devices are reconciled now")
        await reconcileNow(context, state, identity, group)
        return true
    }

    // MARK: - Section 2.2: a co-publisher's device asks for its own devices

    /// A co-publisher's publishing device: the same diff for its own account,
    /// sent as device requests (never a commit). An admit is sent again after
    /// ``deviceRequestResendMs`` while the device is still missing.
    static func requestOwnDevices(
        channelId: String, identity: Identity, group: PublisherGroup, ds: DsClient, accountIdB64: String
    ) async {
        guard (try? group.isOwnerDevice(identity: identity)) == false,
              let roster = deviceGroupRoster(), !roster.isEmpty,
              let ownKey = identity.accountPublicKey(),
              let ownerKey = try? group.ownerAccountKey(),
              let channelBytes = ChannelsClient.hexToData(channelId) else { return }
        let diff = ownDeviceDiff(
            leaves: leaves(group), accountKey: ownKey, roster: roster, selfLeaf: identity.signaturePublicKey())
        guard !diff.missing.isEmpty || !diff.retire.isEmpty else { return }
        var envelopes: [(slot: String, envelope: Data)] = []
        if !diff.missing.isEmpty, let accountId = base64Decode(accountIdB64) {
            let discovery = DiscoveryClient(sessionToken: AmbientSessionToken.shared.current)
            let packages = ((try? await discovery.fetchKeyPackages(accountId: accountId)) ?? nil) ?? []
            let admit = packages.filter {
                diff.missing.contains($0.deviceId)
                    && ((try? keyPackageAccountKey(keyPackage: $0.keyPackage)) ?? nil) == ownKey
            }
            let delegations = ReachCenter.shared.ownMailboxDelegations(delegates: [ownerKey])
            for package in admit {
                let slot = "\(channelId):admit:\(hexEncode(package.deviceId))"
                if let last = requestSentAt[slot], nowMs() < last &+ deviceRequestResendMs { continue }
                guard let delegations,
                      let envelope = try? composePublisherDeviceAdmit(
                        channelId: channelBytes, keyPackage: package.keyPackage,
                        mailboxBinding: delegations.binding, delegations: delegations.blobs) else { continue }
                envelopes.append((slot, envelope))
            }
        }
        for leaf in diff.retire {
            let slot = "\(channelId):retire:\(hexEncode(leaf))"
            if let last = requestSentAt[slot], nowMs() < last &+ deviceRequestResendMs { continue }
            guard let envelope = try? composePublisherDeviceRetire(channelId: channelBytes, deviceKey: leaf)
            else { continue }
            envelopes.append((slot, envelope))
        }
        for item in envelopes {
            if await publish(item.envelope, identity: identity, group: group, ds: ds) {
                requestSentAt[item.slot] = nowMs()
            }
        }
    }

    /// Section 2.4 — Forget device / removal: a retire request for this
    /// device's own leaf in every channel PublisherGroup it holds, best
    /// effort, before the wipe.
    static func sendOwnRetireRequests(state: ChannelsUiState, currentSeed: Data) async {
        guard let identity = try? ChannelPublisherIdentity.shared.identity(currentSeed: currentSeed) else { return }
        let ds = DsClient()
        for channelId in state.keysByChannel.keys.sorted() {
            guard let channelBytes = ChannelsClient.hexToData(channelId),
                  let group = (try? CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId)) ?? nil,
                  let envelope = try? composePublisherDeviceRetire(
                    channelId: channelBytes, deviceKey: identity.signaturePublicKey()) else { continue }
            _ = await publish(envelope, identity: identity, group: group, ds: ds)
        }
    }

    /// An application message on the channel's PublisherGroup journal, after
    /// its ratchet step is on disk (the G-34 rule). Whether it published.
    static func publish(_ plaintext: Data, identity: Identity, group: PublisherGroup, ds: DsClient) async -> Bool {
        guard let groupId = try? group.groupId() else { return false }
        do {
            let message = try group.createApplicationMessage(identity: identity, plaintext: plaintext)
            try ChannelPublisherIdentity.shared.persistOrThrow()
            _ = try await ds.publishGroup(
                groupId: groupId, payload: message,
                auth: CoPublisher.auth(ds: ds, identity: identity, publisherGroup: group))
            return true
        } catch {
            NSLog("ADR-0090: a PublisherGroup message was not sent; the next pass tries again")
            return false
        }
    }

    // MARK: - Section 2.2 on an owner device: apply device requests

    /// Keep the device requests a journal poll read (applied at the pass's
    /// next step, after the poll's own commits).
    static func noteRequests(_ applications: [CoPublisher.JournalApplication], channelId: String) {
        let queued = applications.compactMap { app -> (plaintext: Data, sender: Data)? in
            guard let sender = app.senderAccount,
                  (try? decodePublisherDeviceRequestEnvelope(plaintext: app.data)) != nil else { return nil }
            return (app.data, sender)
        }
        guard !queued.isEmpty else { return }
        store?.enqueue(queued, channelId: channelId)
    }

    /// Apply the queued requests: each admit in its own commit (its Welcome
    /// goes to the requester's mailbox, under the delegation it carried),
    /// every retire of one account together.
    static func applyQueuedRequests(
        context: ChannelPublisherCommits.Context, state: ChannelsUiState, identity: Identity, group: PublisherGroup
    ) async {
        let store = ChannelDeviceAdmissionStore(accountIdB64: context.accountIdB64)
        let queued = store.queued(channelId: context.channelIdHex)
        guard !queued.isEmpty,
              (try? group.isOwnerDevice(identity: identity)) == true,
              let ownerKey = try? group.ownerAccountKey(),
              let channelBytes = ChannelsClient.hexToData(context.channelIdHex) else { return }
        // Taken off the queue now: what is not acted on is asked again by
        // the requesting device (``deviceRequestResendMs``).
        store.dropQueued(channelId: context.channelIdHex)
        var retire: [Data] = []
        var admits: [(keyPackage: Data, account: Data, plaintext: Data)] = []
        for item in queued {
            guard let request = try? decodePublisherDeviceRequestEnvelope(plaintext: item.plaintext),
                  let action = deviceRequestAction(
                    channelId: channelBytes, requestChannelId: request.channelId, action: request.action,
                    sender: item.sender, ownerAccountKey: ownerKey, leaves: leaves(group),
                    keyPackageAccountKey: { (try? keyPackageAccountKey(keyPackage: $0)) ?? nil },
                    keyPackageLeaf: { try? keyPackageSignatureKey(keyPackage: $0) }) else { continue }
            switch action {
            case .admit(let keyPackage, let account):
                guard !admits.contains(where: { $0.keyPackage == keyPackage }) else { continue }
                admits.append((keyPackage, account, item.plaintext))
            case .retire(let leaf, _):
                if !retire.contains(leaf), leaf != identity.signaturePublicKey() { retire.append(leaf) }
            }
        }
        if !retire.isEmpty {
            NSLog("ADR-0090: an owner device retires %d devices on request", retire.count)
            await commitDevices(context: context, state: state, admit: [], retire: retire, welcomeTo: ownerKey)
        }
        for admit in admits {
            // The requester's delegations let this device write the Welcome
            // to the requester's own mailbox without a grant (ADR-0060).
            if let envelope = (try? publisherDeviceRequestDelegations(
                plaintext: admit.plaintext, requesterAccountKey: admit.account)) ?? nil {
                ReachCenter.shared.keepDelegation(
                    envelope, fromAccountKeyB64: base64Encode(admit.account),
                    groupIdHex: DsClient.pseudonymHex((try? group.groupId()) ?? Data()))
            }
            guard let current = (try? context.open())?.group,
                  (try? current.members())?.contains(where: {
                      $0.signaturePublicKey == ((try? keyPackageSignatureKey(keyPackage: admit.keyPackage)) ?? Data())
                  }) != true else { continue }
            NSLog("ADR-0090: an owner device admits a publisher's device on request")
            await commitDevices(
                context: context, state: state, admit: [admit.keyPackage], retire: [], welcomeTo: admit.account)
        }
    }

    // MARK: - Section 6: the admission mark

    /// Send this device's mark when it advanced past what it last sent or
    /// received. Best effort.
    static func sendMarkIfOwed(
        channelId: String, state: ChannelsUiState, identity: Identity, group: PublisherGroup, ds: DsClient
    ) async {
        let extras = state.extras(channelId)
        guard let through = markOwed(mark: extras.admittedThroughSeq, sent: extras.admissionMarkSentSeq),
              let channelBytes = ChannelsClient.hexToData(channelId),
              let envelope = try? composeChannelAdmissionMarkEnvelope(channelId: channelBytes, throughSeq: through)
        else { return }
        if await publish(envelope, identity: identity, group: group, ds: ds) {
            state.updateExtras(channelId) { $0.admissionMarkSentSeq = max($0.admissionMarkSentSeq ?? 0, through) }
        }
    }

    /// ADR-0090 security pass — the mark this device counts: its own, raised
    /// by what the owner sent only up to the newest row it has itself read.
    nonisolated static func clampedMark(held: UInt64?, received: UInt64?, readThrough: UInt64?) -> UInt64? {
        guard let received, let readThrough else { return held }
        let counted = min(received, readThrough)
        guard counted > (held ?? 0) else { return held }
        return counted
    }

    /// Whether a received mark is taken at all: only from a sender that
    /// resolved at receipt (C-68) to the channel OWNER's account. A
    /// co-publisher's mark is ignored.
    nonisolated static func takesMark(sender: Data?, ownerAccountKey: Data?) -> Bool {
        guard let sender, let ownerAccountKey else { return false }
        return sender == ownerAccountKey
    }

    /// Keep the highest mark the owner's devices sent (pending), and count it
    /// up to the rows this device has read. The received mark counts as sent
    /// (it need not travel back).
    static func receiveMarks(
        _ applications: [CoPublisher.JournalApplication], channelId: String, group: PublisherGroup,
        state: ChannelsUiState
    ) {
        guard let channelBytes = ChannelsClient.hexToData(channelId) else { return }
        let owner = try? group.ownerAccountKey()
        for app in applications {
            guard takesMark(sender: app.senderAccount, ownerAccountKey: owner),
                  let mark = try? decodeChannelAdmissionMarkEnvelope(plaintext: app.data),
                  mark.channelId == channelBytes else { continue }
            state.updateExtras(channelId) {
                $0.admissionMarkReceived = max($0.admissionMarkReceived ?? 0, mark.throughSeq)
                $0.admissionMarkSentSeq = max($0.admissionMarkSentSeq ?? 0, mark.throughSeq)
            }
        }
        applyReceivedMark(channelId: channelId, state: state)
    }

    /// Count the pending received mark up to the rows read so far (also run
    /// after an admission pass read further).
    static func applyReceivedMark(channelId: String, state: ChannelsUiState) {
        let extras = state.extras(channelId)
        let next = clampedMark(
            held: extras.admittedThroughSeq, received: extras.admissionMarkReceived,
            readThrough: extras.subscriberRowsReadSeq)
        guard next != extras.admittedThroughSeq else { return }
        state.updateExtras(channelId) { $0.admittedThroughSeq = next }
    }

    // MARK: - Section 6 / open question 4: the admission-seed register

    /// Write every admission seed this device holds for `channelId` into the
    /// account's register (write-once; a seed already held is a no-op).
    static func syncAdmissionSeeds(channelId: String) {
        guard let register = seedRegister, let channelBytes = ChannelsClient.hexToData(channelId) else { return }
        for held in ChannelInvites.localAdmissionSeeds(channelIdHex: channelId) {
            _ = register.setChannelAdmissionSeed(channelId: channelBytes, version: held.version, seed: held.seed)
        }
    }

    /// Every seed the register holds for `channelId`.
    static func registerSeeds(channelId: String) -> [(seed: Data, version: UInt16)] {
        guard let register = seedRegister, let channelBytes = ChannelsClient.hexToData(channelId) else { return [] }
        return register.channelAdmissionSeeds(channelId: channelBytes).map { ($0.seed, $0.admissionVersion) }
    }

    // MARK: - Section 8: a waiting device becomes a publisher

    /// The channels this device reads as an account that owns or co-publishes
    /// them, with no publisher keys here.
    static func waitingChannels(_ state: ChannelsUiState) -> [String] {
        state.channels.map(\.id).filter {
            state.keysByChannel[$0] == nil && state.extras($0).accountRole != nil
        }
    }

    /// The channels a mailbox Welcome may admit this device to: every channel
    /// the account owns or co-publishes, INCLUDING one whose PublisherGroup
    /// (and publisher keys) this device still holds. A device that returns
    /// after another owner device took the channel over (its leaf retired)
    /// still holds its old group, and the owner-signed Welcome that admits
    /// it again is for exactly that group; the replace rule of
    /// ``landWelcome(_:)`` decides (walk of 2026-11-14).
    static func welcomeTargets(_ state: ChannelsUiState) -> [String] {
        state.channels.map(\.id).filter { state.extras($0).accountRole != nil }
    }

    /// Whether a mailbox Welcome may be an admission into one of `state`'s
    /// channels (``welcomeTargets(_:)``).
    static var hasWelcomeTargets: Bool {
        guard let state = waitingState else { return false }
        return !welcomeTargets(state).isEmpty
    }

    static weak var waitingState: ChannelsUiState?
    static var currentSeed: () -> Data? = { nil }

    /// A mailbox Welcome: join it into each waiting channel whose
    /// PublisherGroup it opens. An owned channel: the id and the owner key
    /// this device computes. A co-published one: the register's group id and
    /// the owner key the account recorded when it joined (contract 3a); with
    /// none recorded nothing is joined. The core refuses a Welcome whose
    /// sender is not a device of that owner, or for any other group. Returns
    /// the channel joined, or `nil`.
    static func landWelcome(_ welcome: Data) -> String? {
        guard let state = waitingState, let seed = currentSeed(),
              let identity = try? ChannelPublisherIdentity.shared.identity(currentSeed: seed),
              let ownerSeed = ChannelPublisherIdentity.ownerAccountEd25519Seed else { return nil }
        // Peeked once, and only when a held group needs it: the founder of a
        // Welcome does not depend on the channel it is tried against.
        var founderRead = false
        var founder: FfiWelcomeFounder?
        let welcomeHash = welcomeReplayHash(welcome)
        for channelId in welcomeTargets(state) {
            // Security pass 2026-11-15 (F2) — a Welcome this device already
            // joined for this channel is an old one served again (the
            // KeyPackage is last-resort): never joined again, never held.
            if store?.hasJoinedWelcome(welcomeHash, channelId: channelId) == true { continue }
            guard let channelBytes = ChannelsClient.hexToData(channelId) else { continue }
            let groupId: Data
            let ownerKey: Data
            if state.extras(channelId).accountRole == ChannelAccountRole.owner {
                guard let id = try? publisherGroupIdForSeed(ownerAccountSeed: ownerSeed, channelId: channelBytes),
                      let key = try? publisherOwnerAccountKey(ownerAccountSeed: ownerSeed) else { continue }
                groupId = id
                ownerKey = key
            } else {
                guard let groupHex = CoPublishedJournals.shared.journalIdHex(channelIdHex: channelId),
                      let id = ChannelsClient.hexToData(groupHex),
                      let key = recordedOwnerKey(state.extras(channelId)) else { continue }
                groupId = id
                ownerKey = key
            }
            // Security pass — a group this device still holds (a reset was
            // suspected, or its leaf was retired while it was away and another
            // owner device admits it again) is deleted only for a Welcome to
            // the same group whose sender verifies as a device of the owner,
            // right before joining.
            let joined: PublisherGroup?
            let replaced: Bool
            if let held = (try? CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId)) ?? nil {
                if !founderRead {
                    founder = try? peekWelcomeFounder(welcome: welcome, identity: identity)
                    founderRead = true
                }
                guard welcomeReplaces(founder: founder, groupId: groupId, ownerAccountKey: ownerKey) else { continue }
                // Replay guard: a device's KeyPackage is last-resort, so the
                // Delivery Service can serve an OLD owner-signed Welcome for
                // this group again. A held group is replaced only when this
                // device can no longer use it: its leaf was removed, or a
                // reset is suspected. An owner-signed Welcome for a group
                // still active here may have come before the commit that
                // removes this device's leaf was applied: it is held and
                // tried again (``retryHeldWelcomes(nowMs:)``), never dropped.
                guard mayReplaceHeldGroup(
                    isActive: try? held.isActive(),
                    resetSuspected: state.extras(channelId).resetSuspected == true) else {
                    store?.holdWelcome(welcome, channelId: channelId, nowMs: nowMs())
                    continue
                }
                NSLog("ADR-0090: an owner-signed Welcome replaces this device's publisher group")
                joined = replaceHeldGroup(held, identity: identity) {
                    try publisherGroupJoinFromWelcome(
                        identity: identity, welcome: welcome, groupId: groupId, channelId: channelBytes,
                        ownerAccountKey: ownerKey)
                }
                replaced = true
            } else {
                replaced = false
                // Security pass 2026-11-15 (F3) — no group held here: a group
                // this device deleted (before an entry, or after one that
                // lost) left its epoch, and a join not above it is an old
                // Welcome, unless a reset is suspected (a re-founded channel
                // restarts low).
                joined = joinWithoutHeldGroup(
                    identity: identity,
                    floor: store?.deletedGroupEpoch(channelId: channelId),
                    resetSuspected: state.extras(channelId).resetSuspected == true
                ) {
                    try publisherGroupJoinFromWelcome(
                        identity: identity, welcome: welcome, groupId: groupId, channelId: channelBytes,
                        ownerAccountKey: ownerKey)
                }
            }
            guard joined != nil else { continue }
            store?.recordJoinedWelcome(welcomeHash, channelId: channelId)
            store?.dropHeldWelcomes(channelId: channelId)
            state.updateExtras(channelId) { $0.resetSuspected = nil }
            NSLog("ADR-0090: this device was admitted to a channel's publisher group")
            ChannelPublisherIdentity.shared.persist()
            // The old group's keys serve an epoch this device no longer
            // publishes in: they go (the signing seed stays, it is per
            // device), and the keys of the joined group take their place.
            if replaced { state.dropPublisherKeysKeepingRole(channelId) }
            _ = becomePublisher(channelId: channelId, state: state, identity: identity)
            // The publisher loops open the joined group afresh: claim, cursors.
            ChannelPublisherLoops.shared.readAgain(channelId)
            return channelId
        }
        return nil
    }

    /// Held Welcomes older than this are dropped: the device stayed active in
    /// the group that long, so the removal they answered never came here.
    /// Chosen, not measured.
    nonisolated static let heldWelcomeMaxAgeMs: UInt64 = 7 * 24 * 60 * 60 * 1000

    nonisolated static func heldWelcomeExpired(heldAtMs: UInt64, nowMs: UInt64) -> Bool {
        nowMs > heldAtMs && nowMs - heldAtMs > heldWelcomeMaxAgeMs
    }

    /// Try every held Welcome again (each publisher pass and after a journal
    /// poll found this device's leaf removed). A Welcome whose group is still
    /// active and unsuspected here stays held; any other runs the normal
    /// replace rule and leaves the hold whatever it came to. A Welcome for a
    /// channel the account no longer owns or co-publishes, or older than
    /// ``heldWelcomeMaxAgeMs``, is dropped.
    static func retryHeldWelcomes(nowMs now: UInt64? = nil) {
        guard let store, let state = waitingState else { return }
        let now = now ?? nowMs()
        let targets = Set(welcomeTargets(state).map { $0.lowercased() })
        for (channelId, held) in store.heldWelcomes() {
            guard targets.contains(channelId) else {
                store.dropHeldWelcomes(channelId: channelId)
                continue
            }
            // Newest first: one landed Welcome ends the channel's holds.
            for entry in held.reversed() {
                guard let welcome = Data(base64Encoded: entry.welcomeB64) else {
                    store.dropHeldWelcome(entry, channelId: channelId)
                    continue
                }
                if heldWelcomeExpired(heldAtMs: entry.heldAtMs, nowMs: now) {
                    store.dropHeldWelcome(entry, channelId: channelId)
                    continue
                }
                if stillUsable(channelId: channelId, state: state) { break }
                store.dropHeldWelcome(entry, channelId: channelId)
                if landWelcome(welcome) != nil { break }
            }
        }
    }

    /// The group this device holds for `channelId` is active and not under
    /// reset suspicion: a held Welcome for it waits.
    private static func stillUsable(channelId: String, state: ChannelsUiState) -> Bool {
        guard let seed = currentSeed(),
              let identity = try? ChannelPublisherIdentity.shared.identity(currentSeed: seed),
              let held = (try? CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId)) ?? nil
        else { return false }
        return !mayReplaceHeldGroup(
            isActive: try? held.isActive(), resetSuspected: state.extras(channelId).resetSuspected == true)
    }

    /// Delete `held` and run `join`; if the join throws, the provider is put
    /// back as it was before the delete (a snapshot taken first; nothing else
    /// runs in between, all of it on the main actor), so a failed join never
    /// loses both groups. `nil` when the join failed (the held group is back,
    /// the suspicion stays).
    static func replaceHeldGroup(
        _ held: PublisherGroup, identity: Identity, join: () throws -> PublisherGroup
    ) -> PublisherGroup? {
        guard let before = try? identity.snapshotStorage(), !before.isEmpty,
              let heldEpoch = try? held.epoch() else { return nil }
        // Unreadable reads as inactive: the epochs are then compared.
        let heldActive = (try? held.isActive()) == true
        struct NotNewer: Error {}
        do {
            try held.delete(identity: identity)
            let joined = try join()
            // Replay guard: for an inactive held group, a Welcome that lands
            // at or below its epoch is an old one; the held group is put back.
            guard joinedReplacesHeld(joined: try joined.epoch(), held: heldEpoch, heldActive: heldActive)
            else { throw NotNewer() }
            return joined
        } catch {
            NSLog("ADR-0090: the replacing join failed; the held publisher group is put back")
            do {
                try identity.restoreStorage(snapshot: before)
            } catch {
                NSLog("ADR-0090: the held publisher group could not be put back")
            }
            return nil
        }
    }

    /// Security pass 2026-11-15 (F2) — the replay-set key of a PublisherGroup
    /// Welcome: SHA-256 of its bytes.
    nonisolated static func welcomeReplayHash(_ welcome: Data) -> Data {
        Data(SHA256.hash(data: welcome))
    }

    /// Security pass 2026-11-15 (F3) — whether a join with no held group is
    /// kept: above the epoch of the group this device last deleted for the
    /// channel, or anything under reset suspicion (a re-founded chain starts
    /// low), or anything when no group was ever deleted here.
    nonisolated static func joinAboveDeletedEpoch(joined: UInt64, floor: UInt64?, resetSuspected: Bool) -> Bool {
        guard let floor, !resetSuspected else { return true }
        return joined > floor
    }

    /// Run `join` with no group held; a joined group not kept by
    /// ``joinAboveDeletedEpoch(joined:floor:resetSuspected:)`` is undone (the
    /// provider is put back as it was before the join). `nil` when nothing
    /// was joined.
    static func joinWithoutHeldGroup(
        identity: Identity, floor: UInt64?, resetSuspected: Bool, join: () throws -> PublisherGroup
    ) -> PublisherGroup? {
        guard floor != nil, !resetSuspected else { return try? join() }
        guard let before = try? identity.snapshotStorage(), !before.isEmpty else { return nil }
        do {
            let joined = try join()
            if joinAboveDeletedEpoch(joined: try joined.epoch(), floor: floor, resetSuspected: resetSuspected) {
                return joined
            }
            NSLog("ADR-0090: a Welcome joined no higher than the group this device deleted; undone")
        } catch {
            return nil
        }
        do {
            try identity.restoreStorage(snapshot: before)
        } catch {
            NSLog("ADR-0090: the provider could not be put back after an old Welcome")
        }
        return nil
    }

    /// What a device does once the channel's PublisherGroup it holds is no
    /// longer active (a commit it applied removed its own leaf). The same
    /// verdict as web and Android `inactivePublisherGroupVerdict`.
    enum InactiveGroupVerdict: Equatable {
        /// Only this DEVICE's leaf went (an owner device with other devices of
        /// the account, or a co-publisher's device whose account still holds a
        /// leaf): it stops publishing, keeps the old group and waits for the
        /// owner device's Welcome that admits it again (``landWelcome(_:)``).
        case waitForWelcome
        /// An owner device that is the account's only device: nobody is left
        /// to admit it. It stops publishing and the waiting pass takes the
        /// channel (entry from the saved copy, else decision A); the inactive
        /// group is deleted right before (``deleteInactiveGroup(identity:channelId:)``).
        case reader
        /// The account itself was removed as a publisher (or nothing says
        /// otherwise): the existing removal path (UX 15.2).
        case removed
        /// The account's device roster is not loaded: nothing changes this
        /// pass (an owner device must not take the channel alone on a roster
        /// it has not read).
        case undecided
    }

    /// The verdict for an inactive held group. `ownAccountHasLeaf`: whether
    /// the inactive group's roster (the one after the removal: OpenMLS merges
    /// the removing commit's tree before it marks the group inactive) still
    /// holds a leaf of this account, `nil` when unread. `otherOwnDevices`:
    /// whether the DeviceGroup roster names a device besides this one, `nil`
    /// when not loaded.
    ///
    /// Security pass 2026-11-15 (F4) — a leaf of this account still in the
    /// inactive group is another device of the account (this device's own
    /// leaf is gone): it waits for that device's Welcome whatever the
    /// DeviceGroup roster says. For an owner device the removing commit came
    /// from a device of its own account (the peer gate, C-90), and a roster
    /// left stale by a re-founded DeviceGroup can show this device alone;
    /// reading that as "the account's only device" would make it a reader
    /// that takes the channel from a sibling that still publishes.
    nonisolated static func inactiveGroupVerdict(
        ownerDevice: Bool, ownAccountHasLeaf: Bool?, otherOwnDevices: Bool?
    ) -> InactiveGroupVerdict {
        if ownAccountHasLeaf == true { return .waitForWelcome }
        guard let otherOwnDevices else { return .undecided }
        if ownerDevice { return otherOwnDevices ? .waitForWelcome : .reader }
        return .removed
    }

    /// Whether the DeviceGroup roster names a device besides `selfLeaf`;
    /// `nil` when the roster is not loaded.
    static func otherOwnDevices(selfLeaf: Data) -> Bool? {
        guard let roster = deviceGroupRoster(), !roster.isEmpty else { return nil }
        return roster.contains { $0 != selfLeaf }
    }

    /// The verdict for `group` held by `identity`, or `nil` when the group is
    /// active (or its state is unreadable).
    static func inactiveVerdict(group: PublisherGroup, identity: Identity, ownerDevice: Bool) -> InactiveGroupVerdict? {
        guard (try? group.isActive()) == false else { return nil }
        return inactiveGroupVerdict(
            ownerDevice: ownerDevice,
            ownAccountHasLeaf: identity.accountPublicKey().map { key in leaves(group).contains { $0.account == key } },
            otherOwnDevices: otherOwnDevices(selfLeaf: identity.signaturePublicKey()))
    }

    /// Delete `channelId`'s stored group when it is inactive, right before an
    /// entry or a re-founding (the core refuses an entry while a group is
    /// stored under that id). `true` when no group is stored now; `false`
    /// when an active one is (or the delete failed): nothing may be entered.
    @discardableResult
    static func deleteInactiveGroup(identity: Identity, channelId: String) -> Bool {
        guard let stored = (try? CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId)) ?? nil
        else { return true }
        guard (try? stored.isActive()) == false else { return false }
        // Security pass 2026-11-15 (F3) — the floor a later Welcome must
        // join above (``joinAboveDeletedEpoch(joined:floor:resetSuspected:)``).
        if let epoch = try? stored.epoch() {
            store?.recordDeletedGroupEpoch(epoch, channelId: channelId)
        }
        do {
            try stored.delete(identity: identity)
            ChannelPublisherIdentity.shared.persist()
            NSLog("ADR-0090: the inactive publisher group is deleted before this device takes the channel")
            return true
        } catch {
            NSLog("ADR-0090: the inactive publisher group could not be deleted")
            return false
        }
    }

    /// Replay guard — whether a held group may be replaced by a Welcome at
    /// all: only one this device can no longer use (its own leaf removed, or a
    /// reset suspected). An active, unsuspected group is never replaced; the
    /// Welcome is ignored for that channel.
    nonisolated static func mayReplaceHeldGroup(isActive: Bool?, resetSuspected: Bool) -> Bool {
        resetSuspected || isActive == false
    }

    /// Replay guard — whether a replacing join is kept. For an INACTIVE held
    /// group (the same chain: this device applied the commit that removed its
    /// leaf) the joined group's epoch must be strictly above the held one's.
    /// An ACTIVE held group is replaced only under reset suspicion, and then
    /// the epochs are not compared: a channel founded again elsewhere
    /// (decision A) starts a new chain at a low epoch, and comparing would
    /// keep this device a reader for ever. The same rule as Android
    /// `joinedGroupReplacesHeld`.
    nonisolated static func joinedReplacesHeld(joined: UInt64, held: UInt64, heldActive: Bool) -> Bool {
        heldActive || joined > held
    }

    /// Security pass — whether a Welcome replaces a group this device holds:
    /// it is for the same group and its sender verifies (ADR-0050 binding)
    /// as a device of the owner's account.
    nonisolated static func welcomeReplaces(
        founder: FfiWelcomeFounder?, groupId: Data, ownerAccountKey: Data
    ) -> Bool {
        guard let founder else { return false }
        return founder.groupId == groupId && founder.founderAccountKey == ownerAccountKey
    }

    /// Contract 3a — the owner key a co-published channel's Welcome must come
    /// from: the one the account recorded at join, else `nil` (join nothing).
    nonisolated static func recordedOwnerKey(_ extras: ChannelExtras) -> Data? {
        guard let key = extras.ownerAccountKey, key.count == 32 else { return nil }
        return key
    }

    /// This device holds the channel's PublisherGroup now: publisher keys,
    /// the composer replaces the waiting line. `false` when the group is not
    /// here (still waiting).
    @discardableResult
    static func becomePublisher(channelId: String, state: ChannelsUiState, identity: Identity) -> Bool {
        guard state.keysByChannel[channelId] == nil,
              let fresh = try? ChannelsComposer.freshPublisherKeys(
                identity: identity, accountIdB64: AcceptedContactsStore.boundAccountIdB64,
                channelIdHex: channelId,
                existingSigningSeed: state.persistedPublisherSigningSeed(channelId: channelId))
        else { return false }
        // The channel epoch the seed serves is not known until the offset is
        // learned from the service (decision A): `nil` until then.
        state.setKeys(channelId: channelId, keys: ChannelPublisherKeys(
            seed: fresh.seed, signingKeySeed: fresh.signingKeySeed, seedEpoch: nil))
        state.updateExtras(channelId) { $0.epochOffsetToLearn = true }
        return true
    }

    /// The pass over the waiting channels: a group that is here now (a
    /// Welcome landed by another path) makes this device a publisher;
    /// decision A re-founds an owned channel when this device is the
    /// account's only one and founded its DeviceGroup alone.
    static func passWaiting(state: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String) async {
        guard let identity = try? ChannelPublisherIdentity.shared.identity(currentSeed: seed) else { return }
        for channelId in waitingChannels(state) {
            var held = (try? CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: channelId)) ?? nil
            if let stored = held, let verdict = inactiveVerdict(
                group: stored, identity: identity,
                ownerDevice: state.extras(channelId).accountRole == ChannelAccountRole.owner) {
                // This device's leaf was removed from the group. With another
                // device of the account to admit it, it waits for that
                // device's Welcome (``landWelcome(_:)``); as the account's
                // only device it takes the channel below (the inactive group
                // counts as not held and is deleted right before).
                guard verdict == .reader else { continue }
                held = nil
            }
            if let held {
                // A commit of this group still owed (an entry whose DS verdict
                // or reseal has not landed) finishes first (``ChannelPublisherCommits/replayAll``).
                if (try? held.hasStagedCommit()) == true
                    || PublisherCommitStore.shared.entries(accountIdB64: accountIdB64)
                        .contains(where: { $0.channelIdHex == channelId.lowercased() }) { continue }
                // A suspected reset keeps the group but never posts under it
                // again until an owner-signed Welcome replaces it, or the
                // suspicion lifts (a false alarm: the DS takes this group's
                // epoch key again and its journal holds nothing unaccounted).
                if state.extras(channelId).resetSuspected == true {
                    guard await probeSuspected(channelId: channelId, identity: identity, group: held) else { continue }
                    NSLog("ADR-0090: the journal takes this device's group again; it publishes again")
                    state.updateExtras(channelId) { $0.resetSuspected = nil }
                }
                if becomePublisher(channelId: channelId, state: state, identity: identity) {
                    // ADR-0064 section 7 — an owner device that holds the
                    // group now keeps the channel's head current.
                    await ChannelVaultHead.writeCurrent(channelIdHex: channelId, group: held, identity: identity)
                }
                continue
            }
            // ADR-0064 section 7 (2026-11-14) — an owner device with no group
            // enters from the channel's vault head; with no head, or every
            // saved epoch refused, it falls back to what it did before.
            if ChannelEntry.shouldEnter(
                accountRole: state.extras(channelId).accountRole,
                coPublished: CoPublishedJournals.shared.journalIdHex(channelIdHex: channelId) != nil,
                holdsGroup: false) {
                let result = await ChannelEntry.enterIfPossible(
                    channelId: channelId, state: state, client: client, seed: seed, accountIdB64: accountIdB64)
                if result.entered || !result.fallBack { continue }
            }
            guard shouldRefound(
                foundedAlone: foundedDeviceGroupAlone(), rosterCount: deviceGroupRoster()?.count,
                accountRole: state.extras(channelId).accountRole, holdsGroup: false) else { continue }
            await refound(channelId: channelId, state: state, client: client, seed: seed, accountIdB64: accountIdB64)
        }
    }

    // MARK: - Section 2.5: decision A and the returning old device

    /// Decision A — found the channel's PublisherGroup again at the same id,
    /// reset its journal under the same root, and reseal the new seed to the
    /// subscribers (the admission pass seals to the rows whose tickets open
    /// under the register's seeds). Co-publishers are invited again.
    static func refound(
        channelId: String, state: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String
    ) async {
        do {
            let identity = try ChannelPublisherIdentity.shared.identity(currentSeed: seed)
            // An inactive group under the id would be reopened as the
            // "founded" one: it goes first. An active one is never re-founded.
            guard deleteInactiveGroup(identity: identity, channelId: channelId) else { return }
            NSLog("ADR-0090: decision A, this device founds an owned channel's publisher group again")
            let group = try CoPublisher.foundPublisherGroup(
                identity: identity, accountIdB64: accountIdB64, channelIdHex: channelId)
            try ChannelPublisherIdentity.shared.persistOrThrow()
            let signer = try group.dsAccessSigner(identity: identity)
            let groupId = try group.groupId()
            try await JournalOwnership.claimOrReset(
                root: try CoPublisher.rootSigner(identity: identity, publisherGroup: group),
                journalId: groupId, epoch: signer.epoch(), epochKey: signer.verifyingKey(), ds: DsClient())
            JournalFoundingMarks.shared.clear(journalIdHex: DsClient.pseudonymHex(groupId))
            // ADR-0064 section 7 — the re-founded group's head REPLACES the
            // old one, so a later device enters this group and never the dead
            // one (whose higher epoch numbers would outrank it in a merge).
            await ChannelVaultHead.writeCurrent(
                channelIdHex: channelId, group: group, identity: identity, replace: true)
            guard becomePublisher(channelId: channelId, state: state, identity: identity) else { return }
            let current = try await client.fetchEpoch(channelIdHex: channelId)
            let target = current &+ 1
            guard target > current else { return }
            // The new group's MLS epoch restarts; the channel goes on at
            // `target`.
            let mls = try group.epoch()
            state.updateExtras(channelId) {
                $0.epochOffset = Int64(target) &- Int64(clamping: mls)
                $0.epochOffsetToLearn = nil
            }
            let extras = state.extras(channelId)
            try await ChannelReseal.resealToSubscribers(
                publisherGroup: group, identity: identity, ownerIdentity: try Identity.fromSeed(seed: seed),
                channelIdHex: channelId, targetEpoch: target, client: client,
                throughSeq: ChannelReseal.resealThrough(isPublic: extras.isPublic, mark: extras.admittedThroughSeq),
                ownReaderKeyHex: extras.readerKeyRegisteredHex)
            state.notePublisherSeedAt(channelId: channelId, epoch: target, seed: try group.channelSeed(identity: identity))
            await ChannelPublisherLoops.shared.afterRotation(
                channelId: channelId, epoch: target, channels: state, client: client, seed: seed)
        } catch {
            NSLog("ADR-0090: re-founding a channel's publisher group did not complete (%@)",
                  String(describing: type(of: error)))
        }
    }

    /// A signed read of the channel's journal was refused. After
    /// ``refusedReadsToDrop`` in a row a reset is SUSPECTED (decision A on the
    /// account's restored device), never taken as fact: a 401 from the DS is
    /// not authenticated. The device stops posting and rotating under the old
    /// seed (it reads, waiting) and KEEPS its group state; only an
    /// owner-signed Welcome for the same group replaces it
    /// (``landWelcome(_:)``). Never on the account's only device. Returns
    /// whether it stopped publishing.
    @discardableResult
    static func noteRefusedRead(
        channelId: String, state: ChannelsUiState, identity: Identity, group: PublisherGroup
    ) -> Bool {
        let count = (refusedReads[channelId] ?? 0) + 1
        refusedReads[channelId] = count
        guard suspectsReset(refusedReads: count, rosterCount: deviceGroupRoster()?.count) else { return false }
        refusedReads[channelId] = nil
        NSLog("ADR-0090: the channel's journal refuses this device; it stops posting and keeps its group")
        state.updateExtras(channelId) { $0.resetSuspected = true }
        state.dropPublisherKeysKeepingRole(channelId)
        return true
    }

    /// Successful probes in a row, by channel (a false-alarm suspicion).
    private static var probeSuccesses: [String: Int] = [:]

    /// Signed reads that must succeed in a row before a suspicion lifts.
    nonisolated static let readsToLift = 2

    /// Whether a suspected reset lifts: the held group's epoch key was taken
    /// for ``readsToLift`` signed reads in a row, and the last read found no
    /// journal row at or past the held epoch that this group could not
    /// process (a commit of a chain it does not hold).
    nonisolated static func liftsSuspicion(successfulReads: Int, unaccountedRows: Int) -> Bool {
        successfulReads >= readsToLift && unaccountedRows == 0
    }

    /// Whether a journal row is unaccounted for: not processed by the held
    /// group and framed PAST its epoch (a commit this group never applied).
    /// A row at the held epoch that does not process is this device's own
    /// message (OpenMLS opens none of its own) or one already applied; a row
    /// with no readable epoch is not an MLS row.
    nonisolated static func isUnaccounted(rowEpoch: UInt64?, heldEpoch: UInt64, processed: Bool) -> Bool {
        guard !processed, let rowEpoch else { return false }
        return rowEpoch > heldEpoch
    }

    /// One signed read of the journal with the held group's current-epoch
    /// key, applying what it can; true once the suspicion lifts.
    static func probeSuspected(channelId: String, identity: Identity, group: PublisherGroup) async -> Bool {
        guard let groupId = try? group.groupId(), let heldEpoch = try? group.epoch(),
              let signer = try? group.dsAccessSigner(identity: identity) else { return false }
        let ds = DsClient()
        var unaccounted = 0
        var after: Int64 = 0
        do {
            for _ in 0..<CoPublisher.maxJournalPages {
                let page = try await ds.fetchGroup(
                    groupId: groupId, after: after, waitSecs: 0, auth: DsAuth(signer: signer))
                for row in page.messages {
                    after = max(after, row.id)
                    let processed = (try? group.processIncomingVerifyingRoles(
                        identity: identity, bytes: row.payload)) != nil
                    if isUnaccounted(
                        rowEpoch: epochOfMessage(bytes: row.payload), heldEpoch: heldEpoch, processed: processed) {
                        unaccounted += 1
                    }
                }
                if !page.hasMore || page.messages.isEmpty { break }
            }
        } catch {
            probeSuccesses[channelId] = nil
            return false
        }
        ChannelPublisherIdentity.shared.persist()
        let successes = unaccounted == 0 ? (probeSuccesses[channelId] ?? 0) + 1 : 0
        probeSuccesses[channelId] = successes
        guard liftsSuspicion(successfulReads: successes, unaccountedRows: unaccounted) else { return false }
        probeSuccesses[channelId] = nil
        return true
    }

    /// Whether refused reads make a reset suspected: enough of them in a row,
    /// and a sibling in the DeviceGroup (decision A runs on another device).
    nonisolated static func suspectsReset(refusedReads: Int, rosterCount: Int?) -> Bool {
        refusedReads >= refusedReadsToDrop && (rosterCount ?? 0) > 1
    }

    /// A signed read succeeded.
    static func noteReadOk(channelId: String) {
        refusedReads[channelId] = nil
    }

    /// Whether `error` is a signed read refused by the DS.
    nonisolated static func isRefusedRead(_ error: Error) -> Bool {
        if let ds = error as? DsClientError, case .http(let status, _, _) = ds { return status == 401 }
        return false
    }
}

/// ADR-0090 — what an owner device still owes across a relaunch: the
/// own-account hand-over after an admission merged, and the co-publisher
/// device requests a journal poll read and the pass has not applied yet. One
/// record per account in `UserDefaults`, written behind the account store
/// gate, removed by the account wipe.
struct ChannelDeviceAdmissionStore {
    let accountIdB64: String
    var defaults: UserDefaults = .standard

    static func key(_ accountIdB64: String) -> String { "lakko.channel-device-admission.v1.\(accountIdB64)" }

    struct Queued: Codable, Equatable {
        let plaintextB64: String
        let senderB64: String
    }

    /// An owner-signed Welcome held for a channel whose group is still active
    /// here (``ChannelDeviceAdmission/retryHeldWelcomes(nowMs:)``).
    struct HeldWelcome: Codable, Equatable {
        let welcomeB64: String
        let heldAtMs: UInt64
    }

    struct Record: Codable, Equatable {
        var handoverOwed: [String] = []
        var queued: [String: [Queued]] = [:]
        /// Optional so a record written before it decodes.
        var heldWelcomes: [String: [HeldWelcome]]?
        /// Security pass 2026-11-15 (F2) — SHA-256 (hex) of the PublisherGroup
        /// Welcomes this device joined, per channel, newest last.
        var joinedWelcomes: [String: [String]]?
        /// Security pass 2026-11-15 (F3) — the epoch of the group this device
        /// last deleted for a channel.
        var deletedGroupEpochs: [String: UInt64]?
    }

    /// Joined Welcome hashes kept per channel, oldest dropped first. Chosen,
    /// not measured.
    static let maxJoinedWelcomesPerChannel = 16

    /// Welcomes held per channel at most, oldest dropped first (the newest
    /// wins). Chosen, not measured.
    static let maxHeldWelcomesPerChannel = 3

    /// Requests kept per channel at most (the oldest dropped first; a
    /// requester asks again). Chosen, not measured.
    static let maxQueuedPerChannel = 64

    func load() -> Record {
        guard let data = defaults.data(forKey: Self.key(accountIdB64)),
              let record = try? JSONDecoder().decode(Record.self, from: data) else { return Record() }
        return record
    }

    private func save(_ record: Record) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        if record == Record() {
            defaults.removeObject(forKey: Self.key(accountIdB64))
        } else if let data = try? JSONEncoder().encode(record) {
            defaults.set(data, forKey: Self.key(accountIdB64))
        }
    }

    func handoverOwed(_ channelId: String) -> Bool { load().handoverOwed.contains(channelId.lowercased()) }

    func markHandoverOwed(_ channelId: String) {
        var record = load()
        guard !record.handoverOwed.contains(channelId.lowercased()) else { return }
        record.handoverOwed.append(channelId.lowercased())
        save(record)
    }

    func clearHandoverOwed(_ channelId: String) {
        var record = load()
        record.handoverOwed.removeAll { $0 == channelId.lowercased() }
        save(record)
    }

    func enqueue(_ items: [(plaintext: Data, sender: Data)], channelId: String) {
        var record = load()
        var list = record.queued[channelId.lowercased()] ?? []
        for item in items {
            let entry = Queued(plaintextB64: item.plaintext.base64EncodedString(), senderB64: item.sender.base64EncodedString())
            if !list.contains(entry) { list.append(entry) }
        }
        if list.count > Self.maxQueuedPerChannel { list.removeFirst(list.count - Self.maxQueuedPerChannel) }
        record.queued[channelId.lowercased()] = list
        save(record)
    }

    func queued(channelId: String) -> [(plaintext: Data, sender: Data)] {
        (load().queued[channelId.lowercased()] ?? []).compactMap { q in
            guard let p = Data(base64Encoded: q.plaintextB64), let s = Data(base64Encoded: q.senderB64) else { return nil }
            return (p, s)
        }
    }

    func dropQueued(channelId: String) {
        var record = load()
        guard record.queued.removeValue(forKey: channelId.lowercased()) != nil else { return }
        save(record)
    }

    func holdWelcome(_ welcome: Data, channelId: String, nowMs: UInt64) {
        var record = load()
        var all = record.heldWelcomes ?? [:]
        var list = all[channelId.lowercased()] ?? []
        let b64 = welcome.base64EncodedString()
        guard !list.contains(where: { $0.welcomeB64 == b64 }) else { return }
        list.append(HeldWelcome(welcomeB64: b64, heldAtMs: nowMs))
        if list.count > Self.maxHeldWelcomesPerChannel {
            list.removeFirst(list.count - Self.maxHeldWelcomesPerChannel)
        }
        all[channelId.lowercased()] = list
        record.heldWelcomes = all
        save(record)
        NSLog("ADR-0090: an owner-signed Welcome for a group still active here is held")
    }

    /// The held Welcomes by channel, each list oldest first.
    func heldWelcomes() -> [String: [HeldWelcome]] { load().heldWelcomes ?? [:] }

    func dropHeldWelcome(_ entry: HeldWelcome, channelId: String) {
        var record = load()
        guard var all = record.heldWelcomes, var list = all[channelId.lowercased()] else { return }
        list.removeAll { $0 == entry }
        all[channelId.lowercased()] = list.isEmpty ? nil : list
        record.heldWelcomes = all.isEmpty ? nil : all
        save(record)
    }

    func dropHeldWelcomes(channelId: String) {
        var record = load()
        guard var all = record.heldWelcomes, all.removeValue(forKey: channelId.lowercased()) != nil else { return }
        record.heldWelcomes = all.isEmpty ? nil : all
        save(record)
    }

    func hasJoinedWelcome(_ hash: Data, channelId: String) -> Bool {
        let hex = hash.map { String(format: "%02x", $0) }.joined()
        return load().joinedWelcomes?[channelId.lowercased()]?.contains(hex) == true
    }

    func recordJoinedWelcome(_ hash: Data, channelId: String) {
        let hex = hash.map { String(format: "%02x", $0) }.joined()
        var record = load()
        var all = record.joinedWelcomes ?? [:]
        var list = all[channelId.lowercased()] ?? []
        guard !list.contains(hex) else { return }
        list.append(hex)
        if list.count > Self.maxJoinedWelcomesPerChannel {
            list.removeFirst(list.count - Self.maxJoinedWelcomesPerChannel)
        }
        all[channelId.lowercased()] = list
        record.joinedWelcomes = all
        save(record)
    }

    func deletedGroupEpoch(channelId: String) -> UInt64? {
        load().deletedGroupEpochs?[channelId.lowercased()]
    }

    func recordDeletedGroupEpoch(_ epoch: UInt64, channelId: String) {
        var record = load()
        var all = record.deletedGroupEpochs ?? [:]
        guard all[channelId.lowercased()] != epoch else { return }
        all[channelId.lowercased()] = epoch
        record.deletedGroupEpochs = all
        save(record)
    }

    /// The channel left this device: its replay guard goes with it.
    func forgetReplayGuard(channelId: String) {
        var record = load()
        let id = channelId.lowercased()
        let hadJoined = record.joinedWelcomes?.removeValue(forKey: id) != nil
        let hadEpoch = record.deletedGroupEpochs?.removeValue(forKey: id) != nil
        guard hadJoined || hadEpoch else { return }
        if record.joinedWelcomes?.isEmpty == true { record.joinedWelcomes = nil }
        if record.deletedGroupEpochs?.isEmpty == true { record.deletedGroupEpochs = nil }
        save(record)
    }

    /// Forget device / Delete account.
    static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key(accountIdB64))
    }
}
