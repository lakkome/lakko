// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0091 phase 2 — a device that fell behind a conversation notices it and
/// comes back; the pipeline over the rules of `Chat/ConversationReentry.swift`.
///
/// 1. Detection (section 1): every poll of a conversation journal folds one
///    observation into the durable record (``noteReentryAfterRead(groupIdHex:raw:)``);
///    refused reads and refused publishes feed it too.
/// 2. The stop list (section 2): ``reentryStopsActing(groupIdHex:)`` is
///    consulted by the sweep, the beacon, the re-admission arm, the
///    registrar reconcile, proposals and the send path (in
///    `ChatRepository.swift`).
/// 3. The request (section 3): a re-entry KeyPackage, recorded durably before
///    it is sent, by the route the core picks.
/// 4. The re-admitter (section 4): checks a request, proposes the
///    replacement through the ADR-0045 path; the sweep commits it through the
///    ADR-0081 commit route and posts the Welcome to the account mailbox.
/// 5. Coming back (section 5): ``joinReentry(welcome:groupIdHex:)`` from the
///    held-Welcome pass.
/// 6. The gap history (section 7).
///
/// Log lines name no group, row, key, account or name.
@MainActor
extension ChatRepository {

    // MARK: - State

    private var nowU64: UInt64 { UInt64(max(0, Self.nowMs())) }

    /// This device's behind record for `groupIdHex` (empty when none).
    func reentryRecord(groupIdHex: String) -> ConversationReentryRecord {
        guard let accountIdB64 = currentAccountIdB64 else { return ConversationReentryRecord() }
        return reentryStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// Where this device stands in `groupIdHex`.
    public func reentryBehind(groupIdHex: String) -> ReentryBehind {
        reentryRecord(groupIdHex: groupIdHex).behind
    }

    /// C-93: no commit, sweep, beacon, proposal, registration or repeat
    /// envelope in this conversation.
    func reentryStopsActing(groupIdHex: String) -> Bool {
        reentryBehind(groupIdHex: groupIdHex).stopsActing
    }

    /// Journal repair is held while behind with evidence (section 2).
    func reentryBlocksRepair(groupIdHex: String) -> Bool {
        ConversationReentryRules.blocksRepair(reentryRecord(groupIdHex: groupIdHex))
    }

    /// Thrown in place of a proposal or a send this device must not make
    /// while behind.
    struct BehindInConversation: Error {}

    func requireNotBehind(groupIdHex: String) throws {
        if reentryStopsActing(groupIdHex: groupIdHex) { throw BehindInConversation() }
    }

    func requireSendAllowedWhileBehind(groupIdHex: String, isPersonContent: Bool) throws {
        let behind = reentryBehind(groupIdHex: groupIdHex)
        guard ConversationReentryRules.sendAllowed(behind: behind, isPersonContent: isPersonContent) else {
            throw BehindInConversation()
        }
    }

    @discardableResult
    private func saveReentry(_ record: ConversationReentryRecord, groupIdHex: String) -> Bool {
        guard let accountIdB64 = currentAccountIdB64 else { return false }
        let before = reentryStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        guard reentryStore.save(record, accountIdB64: accountIdB64, groupIdHex: groupIdHex) else { return false }
        if before.behind != record.behind || before.lastRoute != record.lastRoute {
            reentryRevision &+= 1
        }
        return true
    }

    /// The group's record goes with the group (``wipeGroupStores``).
    func wipeReentry(accountIdB64: String, groupIdHex: String) {
        reentryStore.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        reentryAdmissions.removeGroup(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        reentryKnockKeys.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        reentryHistoryOwed.save(
            reentryHistoryOwed.load(accountIdB64: accountIdB64).filter { $0.groupIdHex != groupIdHex },
            accountIdB64: accountIdB64)
        reentryGapLooks.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        chatClient.reentryObservations.clear(groupIdHex: groupIdHex)
    }

    // MARK: - 1. Detection

    /// An ADR-0040 outbox entry or a staged sweep awaits a verdict here.
    func ownCommitOwed(groupIdHex: String, record: GroupRecord) -> Bool {
        if (try? record.group.hasStagedSweep()) == true { return true }
        guard let accountIdB64 = currentAccountIdB64 else { return false }
        return !commitOutbox.loadUnacked(
            accountIdB64: accountIdB64, groupIdB64: base64Encode(record.groupId)).isEmpty
    }

    /// Fold what one read of the conversation journal showed.
    func noteReentryAfterRead(groupIdHex: String, raw: FetchResult) {
        guard let record = groups[groupIdHex],
              readIsActive({ try record.group.isActive() }) == true,
              let localEpoch = try? record.group.epoch() else { return }
        let observations = chatClient.reentryObservations
        observations.noteReadAnswered(groupIdHex: groupIdHex)
        let evidence = observations.snapshot(groupIdHex: groupIdHex)
        let held = chatClient.registeredCommitHold.hold(groupIdHex: groupIdHex) != nil
        let observation = ConversationReentryRules.observation(
            ownCommitOwed: ownCommitOwed(groupIdHex: groupIdHex, record: record),
            rowsReturned: raw.messages.count, hasMore: raw.hasMore, held: held,
            latestEpoch: raw.latestEpoch, localEpoch: localEpoch, evidence: evidence)
        foldReentry(groupIdHex: groupIdHex, observation: observation, evidence: evidence)
    }

    /// A read of the journal was refused.
    func noteReentryReadRefused(groupIdHex: String, error: Error) {
        guard Self.isUnskewedRefusal(error), let record = groups[groupIdHex],
              readIsActive({ try record.group.isActive() }) == true,
              let localEpoch = try? record.group.epoch() else { return }
        let observations = chatClient.reentryObservations
        observations.noteReadRefused(groupIdHex: groupIdHex)
        let evidence = observations.snapshot(groupIdHex: groupIdHex)
        let observation = ConversationReentryRules.observation(
            ownCommitOwed: ownCommitOwed(groupIdHex: groupIdHex, record: record),
            rowsReturned: 0, hasMore: true, held: false,
            latestEpoch: nil, localEpoch: localEpoch, evidence: evidence)
        foldReentry(groupIdHex: groupIdHex, observation: observation, evidence: evidence)
    }

    /// A publish of this device was refused.
    func noteReentryPublishRefused(groupIdHex: String, error: Error) {
        guard Self.isUnskewedRefusal(error) else { return }
        chatClient.reentryObservations.notePublishRefused(groupIdHex: groupIdHex)
    }

    /// A `401` from the Delivery Service that is not a skewed clock
    /// (`DsClient` reports a skewed one as ``DsClientError/clockSkew(endpoint:)``).
    nonisolated static func isUnskewedRefusal(_ error: Error) -> Bool {
        var current: Error = error
        for _ in 0..<4 {
            if case DsClientError.http(let status, _, _) = current { return status == 401 }
            guard case ChatError.wrapped(_, let underlying) = current else { return false }
            current = underlying
        }
        return false
    }

    private func foldReentry(
        groupIdHex: String, observation: FfiBehindObservation, evidence: ReentryObservations.Snapshot
    ) {
        let previous = reentryRecord(groupIdHex: groupIdHex)
        let (next, cleared) = ConversationReentryRules.fold(
            previous, observation: observation, evidence: evidence, nowMs: nowU64)
        if next.behind != previous.behind {
            ReentryLog.emit("conversation behind state: \(ReentryLog.state(previous.behind)) -> "
                + "\(ReentryLog.state(next.behind)) (local=\(observation.localEpoch) "
                + "latest=\(observation.latestEpoch.map { String($0) } ?? "-"))")
        }
        if cleared {
            forgetRecordedKeyPackages(previous.recordedKeyPackages)
            var kept = next
            kept.requests = []
            kept.lastSentMs = nil
            kept.lastRoute = nil
            chatClient.reentryObservations.clear(groupIdHex: groupIdHex)
            saveReentry(kept, groupIdHex: groupIdHex)
            state = state.with(threads: buildThreadList())
            return
        }
        if next != previous {
            saveReentry(next, groupIdHex: groupIdHex)
            if next.behind != previous.behind { state = state.with(threads: buildThreadList()) }
        }
        if next.behind.stopsActing {
            Task { @MainActor [weak self] in await self?.sendReentryRequestIfDue(groupIdHex: groupIdHex) }
        }
    }

    private func forgetRecordedKeyPackages(_ keyPackages: [Data]) {
        guard !keyPackages.isEmpty, let identity = canonicalIdentity else { return }
        for keyPackage in keyPackages { try? identity.forgetKeyPackage(keyPackage: keyPackage) }
        persistSnapshotBestEffort()
    }

    // MARK: - The waiting note (UX 2.2)

    /// The lines of the waiting note for `groupIdHex`, or `nil` when the chat
    /// is not waiting on this device.
    public func reentryWaitingNote(groupIdHex: String) -> [String]? {
        let record = reentryRecord(groupIdHex: groupIdHex)
        guard record.behind.isConfirmed else { return nil }
        return ConversationReentryRules.note(
            behind: record.behind, waiter: reentryWaiter(groupIdHex: groupIdHex, record: record), nowMs: nowU64)
    }

    private func reentryWaiter(
        groupIdHex: String, record: ConversationReentryRecord
    ) -> ConversationReentryRules.Waiter? {
        switch record.lastRoute {
        case "own-device": return .ownDevice
        case "knock": return .ownerOrAdmin
        case "mailbox":
            guard let other = otherPersonKey(groupIdHex: groupIdHex) else { return nil }
            return .person(name: ProfileDirectory.shared.resolve(signatureKey: other).name)
        default: return nil
        }
    }

    // MARK: - 3. The request

    /// This device's own person key in `group` (the account its leaf binds).
    private func ownPersonKey(group: Group) -> Data? {
        guard let own = try? group.ownLeafIndex(),
              let me = (try? group.members())?.first(where: { $0.leafIndex == own }) else { return nil }
        return PersonKey.of(me)
    }

    /// Another device of this account holds a leaf in the conversation.
    private func ownDeviceInConversation(group: Group) -> Bool {
        guard let own = try? group.ownLeafIndex(), let members = try? group.members(),
              let me = members.first(where: { $0.leafIndex == own }) else { return false }
        let person = PersonKey.of(me)
        return members.contains { $0.leafIndex != own && PersonKey.of($0) == person }
    }

    /// The other person of a one-to-one, `nil` otherwise.
    private func otherPersonKey(groupIdHex: String) -> Data? {
        guard let record = groups[groupIdHex], let ownPerson = ownPersonKey(group: record.group),
              let members = try? record.group.members() else { return nil }
        return members.map { PersonKey.of($0) }.first { $0 != ownPerson }
    }

    /// Section 3 — send the request to be brought back, when it is due
    /// (`reentryRequestDue`: as soon as behind, then every 30 minutes). The
    /// re-entry KeyPackage's private keys (the MLS snapshot) and the record
    /// naming it are durable BEFORE anything is sent.
    func sendReentryRequestIfDue(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64, let identity = canonicalIdentity,
              let record = groups[groupIdHex], !reentrySending.contains(groupIdHex),
              !membershipEnded(membershipStateNow(groupIdHex: groupIdHex)) else { return }
        var current = reentryStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let now = nowU64
        guard current.behind.stopsActing,
              reentryRequestDue(state: current.behind.ffi, lastSentMs: current.lastSentMs, nowMs: now)
        else { return }
        reentrySending.insert(groupIdHex)
        defer { reentrySending.remove(groupIdHex) }
        let bound = binding.capture()
        // `epoch_left` is the latest epoch the device read. One suspected by
        // refused reads read none: an empty knock is the epoch probe (one
        // `ds-knock` token, ADR-0053).
        var epochLeft = current.latestSeen
        if epochLeft == nil {
            epochLeft = try? await dsClient.knock(groupId: record.groupId, payloads: []).epoch
            guard bound.isCurrent else { return }
        }
        guard let epochLeft else {
            ReentryLog.emit("conversation re-entry request not sent: the latest epoch is unknown")
            retrySoon(groupIdHex: groupIdHex)
            return
        }
        let kind = groupKind(groupIdHex: groupIdHex) ?? .group
        let route = reentryRoute(ownDeviceInConversation: ownDeviceInConversation(group: record.group), kind: kind)
        let keyPackage: Data
        let envelope: Data
        do {
            keyPackage = try identity.createReentryKeyPackage(groupId: record.groupId, epochLeft: epochLeft)
            envelope = try composeConversationReentryEnvelope(
                groupId: record.groupId, epochLeft: epochLeft,
                gapFromMs: current.gapFromMs ?? current.behind.sinceMs ?? now, keyPackage: keyPackage)
        } catch {
            ReentryLog.emit("conversation re-entry request not built: \(ErrorLogTag.of(error))")
            retrySoon(groupIdHex: groupIdHex)
            return
        }
        // Durable first: the KeyPackage's private keys, then the record.
        let (kept, dropped) = ConversationReentryRules.keeping(
            current.requests,
            adding: ReentrySentRequest(
                keyPackage: keyPackage, epochLeft: epochLeft, sentAtMs: now,
                route: ConversationReentryRules.routeName(route), cursorAtSend: record.cursor))
        for old in dropped { try? identity.forgetKeyPackage(keyPackage: old) }
        do {
            try persistSnapshotDurableOrThrow()
        } catch {
            ReentryLog.emit("conversation re-entry request not sent: the snapshot did not persist")
            return
        }
        current.requests = kept
        current.lastSentMs = now
        current.lastRoute = ConversationReentryRules.routeName(route)
        guard bound.isCurrent, saveReentry(current, groupIdHex: groupIdHex) else { return }
        do {
            try await sendReentryEnvelope(envelope, route: route, record: record, groupIdHex: groupIdHex)
            ReentryLog.emit("conversation re-entry request sent by route \(ConversationReentryRules.routeName(route))")
        } catch {
            guard bound.isCurrent else { return }
            ReentryLog.emit("conversation re-entry request failed by route "
                + "\(ConversationReentryRules.routeName(route)): \(ErrorLogTag.of(error))")
            retrySoon(groupIdHex: groupIdHex)
        }
    }

    /// A request that did not go out is tried again after a minute rather
    /// than after the 30-minute interval. Chosen, not measured.
    private func retrySoon(groupIdHex: String) {
        var record = reentryRecord(groupIdHex: groupIdHex)
        let interval = reentryRequestIntervalMs()
        record.lastSentMs = nowU64 &+ 60_000 >= interval ? nowU64 &+ 60_000 &- interval : 0
        saveReentry(record, groupIdHex: groupIdHex)
    }

    private struct NoRoute: Error {}

    private func sendReentryEnvelope(
        _ envelope: Data, route: FfiReentryRoute, record: GroupRecord, groupIdHex: String
    ) async throws {
        switch route {
        case .ownDevice:
            // Route 1 — a DeviceGroup service envelope to the account's other
            // devices.
            guard let sender = reentryOwnDeviceSender else { throw NoRoute() }
            await sender(envelope)
        case .mailbox:
            // Route 2 — a sealed row to the other person's account mailbox,
            // under a reach grant this device holds.
            guard let other = otherPersonKey(groupIdHex: groupIdHex),
                  let plan = ReachCenter.shared.writePlan(toAccountKey: other) else { throw NoRoute() }
            let row = try plan.target.seal(kind: .conversationReentry, body: envelope)
            _ = try await RefusedMailboxWrite.tryingCandidates(plan.candidates) { auth in
                try await self.dsClient.publishMailbox(pseudonym: plan.target.address, payload: row, writeAuth: auth)
            }
        case .knock:
            // Route 3 — sealed to the group's knock key.
            guard let knock = reentryKnockKey(groupIdHex: groupIdHex) else { throw NoRoute() }
            let row = try sealReentryKnockRow(knockPk: knock.publicKey, knockVersion: knock.version, envelope: envelope)
            _ = try await dsClient.knock(groupId: record.groupId, payloads: [row])
        }
    }

    /// The knock key this device can seal to: derived or handed over when it
    /// is the owner or an admin, else the one the owner sent in-group.
    func reentryKnockKey(groupIdHex: String) -> (version: UInt16, publicKey: Data)? {
        if let own = knockKey(groupIdHex: groupIdHex) { return own }
        guard let accountIdB64 = currentAccountIdB64,
              let held = reentryKnockKeys.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex) else { return nil }
        return (held.version, held.publicKey)
    }

    // MARK: - The knock key in-group (route 3)

    /// The owner sends the group's knock key to every member once per
    /// version (section 3, route 3): a plain member holds no knock seed.
    /// Called from the owner's knock reconcile.
    func publishKnockKeyIfOwed(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64, ownsGroup(groupIdHex: groupIdHex),
              !reentryStopsActing(groupIdHex: groupIdHex),
              let knock = knockKey(groupIdHex: groupIdHex) else { return }
        var row = KnockVersionStore.load(accountIdB64, groupIdHex)
            ?? KnockVersionStore.Row(version: knock.version, admins: [], sent: [])
        let tag = "pk|\(knock.version)"
        guard !row.sent.contains(tag) else { return }
        do {
            let envelope = try composeKnockKeyEnvelope(knockVersion: knock.version, knockPk: knock.publicKey)
            try await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: envelope)
            row = KnockVersionStore.load(accountIdB64, groupIdHex) ?? row
            row.sent.append(tag)
            KnockVersionStore.save(row, accountIdB64, groupIdHex)
        } catch {
            if !ConversationReentryRules.isBehindRefusal(error) { NSLog("[knock] WARN the knock key was not sent in-group; retried on the next change") }
        }
    }

    /// The growth re-send of the knock key, by the owner or an admin (both
    /// hold the knock seeds): the newest version this device holds.
    func knockKeyEnvelopeForResend(groupIdHex: String) -> Data? {
        let role = ownRole(groupIdHex: groupIdHex)
        guard role == .owner || role == .admin,
              !membershipEnded(membershipStateNow(groupIdHex: groupIdHex)),
              let knock = knockKey(groupIdHex: groupIdHex) else { return nil }
        return try? composeKnockKeyEnvelope(knockVersion: knock.version, knockPk: knock.publicKey)
    }

    /// A knock-key envelope arrived: kept only from the owner or an admin
    /// resolved at receipt (C-68), and only when strictly newer.
    func receiveKnockKey(groupIdHex: String, key: FfiKnockKey, senderLeaf: UInt32?) {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex],
              let roles = (try? record.group.groupRoles()) ?? nil,
              let sender = senderLeaf.flatMap({ leaf in
                  (try? record.group.members())?.first(where: { $0.leafIndex == leaf })?.accountKey
              }),
              sender == roles.owner || roles.admins.contains(sender) else {
            NSLog("[knock] a knock key was dropped")
            return
        }
        reentryKnockKeys.keepIfNewer(
            ReentryKnockKeyStore.Held(version: key.knockVersion, publicKey: key.knockPk),
            accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    // MARK: - 4. The re-admitter

    /// A request arrived by `route`: decode it, then judge it.
    public func receiveConversationReentry(envelope: Data, route: FfiReentryRoute) async {
        guard let request = try? decodeConversationReentryEnvelope(plaintext: envelope) else {
            ReentryLog.emit("conversation re-entry request dropped: it does not decode")
            return
        }
        await receiveReentryRequest(request, route: route)
    }

    /// Section 4. The core checks the KeyPackage (`reentryRequestTarget`) and
    /// decides the admission (`reentryAdmissionVerdict`); this device supplies
    /// the facts and, on `admit`, proposes the replacement and sweeps.
    func receiveReentryRequest(_ request: FfiConversationReentry, route: FfiReentryRoute) async {
        let groupIdHex = DsClient.pseudonymHex(request.groupId)
        guard let accountIdB64 = currentAccountIdB64, let identity = canonicalIdentity,
              let record = groups[groupIdHex],
              !membershipEnded(membershipStateNow(groupIdHex: groupIdHex)),
              !reentryStopsActing(groupIdHex: groupIdHex) else {
            ReentryLog.emit("conversation re-entry request not handled here")
            return
        }
        guard let epoch = try? record.group.epoch(), epoch == request.epochLeft else {
            ReentryLog.emit("conversation re-entry request names another epoch; not handled")
            return
        }
        let target: FfiReentryTarget
        do {
            target = try record.group.reentryRequestTarget(identity: identity, keyPackage: request.keyPackage)
        } catch {
            ReentryLog.emit("conversation re-entry request refused: \(ErrorLogTag.of(error))")
            return
        }
        guard let ownPerson = ownPersonKey(group: record.group) else { return }
        // Who may act by which route (section 4's table).
        switch route {
        case .ownDevice:
            guard target.accountKey == ownPerson else { return }
        case .mailbox:
            guard groupKind(groupIdHex: groupIdHex) == .oneToOne, target.accountKey != ownPerson else { return }
        case .knock:
            let role = ownRole(groupIdHex: groupIdHex)
            guard role == .owner || role == .admin else { return }
        }
        let bound = binding.capture()
        guard let still = await reentryDeviceStillTheAccounts(
            route: route, target: target, ownPerson: ownPerson) else {
            ReentryLog.emit("conversation re-entry request waits: the device list could not be read")
            return
        }
        guard bound.isCurrent, let live = groups[groupIdHex], (try? live.group.epoch()) == epoch else { return }
        let facts = FfiReentryAdmissionFacts(
            deviceStillTheAccounts: still,
            lastCurrentEpochMessageFromLeafMs: chatClient.reentryObservations.lastActivity(
                groupIdHex: groupIdHex, leaf: target.replacedLeaf, epoch: epoch),
            handledForLeafAtThisEpoch: reentryAdmissions.handledAt(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, leafKey: target.signatureKey, epoch: epoch),
            lastAcceptedForLeafMs: reentryAdmissions.lastAcceptedMs(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, leafKey: target.signatureKey))
        let verdict = reentryAdmissionVerdict(facts: facts, nowMs: nowU64)
        reentryAdmissions.markHandled(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, leafKey: target.signatureKey, epoch: epoch)
        guard verdict == .admit else {
            ReentryLog.emit("conversation re-entry request not admitted: \(String(describing: verdict))")
            return
        }
        do {
            try await publishReentryReplacement(groupIdHex: groupIdHex, keyPackage: request.keyPackage)
        } catch {
            ReentryLog.emit("conversation re-entry replacement not proposed: \(ErrorLogTag.of(error))")
            return
        }
        guard bound.isCurrent else { return }
        reentryAdmissions.markAccepted(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, leafKey: target.signatureKey, atMs: nowU64)
        reentryHistoryOwed.add(
            ReentryOwedHistory(
                groupIdHex: groupIdHex, keyPackage: request.keyPackage, leafKey: target.signatureKey,
                targetEpoch: epoch &+ 1, gapFromMs: request.gapFromMs, owedAtMs: nowU64),
            accountIdB64: accountIdB64)
        ReentryLog.emit("conversation re-entry replacement proposed by route \(ConversationReentryRules.routeName(route))")
        await sweepIfCommitter(groupIdHex: groupIdHex)
        // A sweep of this device's own merged the replacement: the history
        // is owed at exactly this epoch, before anything moves it on.
        await sendOwedReentryHistory(groupIdHex: groupIdHex)
    }

    /// Whether the device is still the account's: route 1 the own
    /// DeviceGroup roster, routes 2 and 3 the account's directory listing.
    /// `nil` when it could not be read (nothing is decided then).
    private func reentryDeviceStillTheAccounts(
        route: FfiReentryRoute, target: FfiReentryTarget, ownPerson: Data
    ) async -> Bool? {
        if route == .ownDevice {
            guard let roster = ownDeviceGroupRosterProvider() else { return nil }
            return roster.contains(target.signatureKey)
        }
        let accountId: Data?
        if target.accountKey == ownPerson {
            accountId = currentAccountId
        } else {
            let keyB64 = base64Encode(target.accountKey)
            accountId = readmissionContactsProvider().first {
                $0.signaturePubkeyB64 == keyB64 || $0.previousKeysB64.contains(keyB64)
            }?.accountId ?? ProfileDirectory.shared.accountId(signatureKeyB64: keyB64)
        }
        guard let accountId, accountId.count == 16 else { return false }
        do {
            let listed = try await expectedPersonDevices(accountId: accountId, expectedKey: target.accountKey)
            return listed.contains { $0.deviceId == target.signatureKey }
        } catch {
            return nil
        }
    }

    /// Propose `Add(K)` then `Remove(L)` (the core builds both, the `Add`
    /// first), on the group's commit chain like every membership proposal.
    private func publishReentryReplacement(groupIdHex: String, keyPackage: Data) async throws {
        try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            guard let identity = canonicalIdentity, let record = groups[groupIdHex] else {
                throw ChatError.argument("no live Group handle")
            }
            let proposals = try record.group.proposeReentryReplacement(identity: identity, keyPackage: keyPackage)
            try persistSnapshotBeforePublish()
            let bound = binding.capture()
            for bytes in [proposals.add, proposals.remove] {
                let publish = try await dsClient.publishGroup(
                    groupId: record.groupId, payload: bytes, auth: dsAuth(groupIdHex: groupIdHex))
                try requireCommitRegion(bound)
                chatClient.markOwnPublishedCommit(groupIdHex: groupIdHex, messageId: publish.id)
            }
        }
    }

    // MARK: - 5. Coming back

    /// The held-Welcome pass offers every Welcome a stored active group
    /// refused here first. `true` when it was this device's re-entry and the
    /// conversation is back (section 5); `false` changes nothing.
    func joinReentry(welcome: Data, groupIdHex: String) async -> Bool {
        guard let accountIdB64 = currentAccountIdB64, let identity = canonicalIdentity,
              let old = groups[groupIdHex] else { return false }
        let record = reentryStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        guard !record.recordedKeyPackages.isEmpty else { return false }
        let joined: Group
        do {
            joined = try Group.joinReentry(
                welcome: welcome, identity: identity, recordedKeyPackages: record.recordedKeyPackages)
        } catch {
            ReentryLog.emit("conversation re-entry Welcome not joined: \(ErrorLogTag.of(error))")
            return false
        }
        guard let groupId = try? joined.groupId(), DsClient.pseudonymHex(groupId) == groupIdHex else {
            return false
        }
        // The history secret is exported at the epoch the replacement created,
        // before anything moves the group on.
        let historySecret = (try? joined.ownLeafIndex()).flatMap { own in
            (try? joined.members())?.first(where: { $0.leafIndex == own })?.signaturePublicKey
        }.flatMap { leafKey in
            try? joined.exportSecret(
                identity: identity, label: ConversationReentryRules.historyExporterLabel,
                context: ConversationReentryRules.historyContext(leafKey: leafKey),
                length: ConversationReentryRules.historySecretLength)
        }
        let back = GroupRecord(group: joined, groupId: groupId)
        back.adoptReentryRecord(old, readFrom: record.readFromAfterReentry ?? old.cursor)
        // UX 2.4 — where messages were sent while out, one local line.
        let now = nowU64
        // The gap's start: when the first row could not be read (the time
        // stated in the request); the gap history is looked for from there.
        let lookFrom = record.gapFromMs.map { min($0, now) } ?? now
        if let gapFrom = record.gapFromMs, gapFrom <= now {
            let lowest = back.messages.map(\.id).min() ?? 0
            back.appendLocalRow(
                text: ConversationReentryRules.gapMarker(fromMs: gapFrom, toMs: now),
                messageId: min(0, lowest) - 1,
                createdAt: Self.isoTimestamp(ms: Int64(clamping: now)))
        }
        groups[groupIdHex] = back
        if let epoch = try? joined.epoch() {
            chatClient.registeredCommitHold.observeEpoch(groupIdHex: groupIdHex, epoch: epoch)
        }
        forgetRecordedKeyPackages(record.recordedKeyPackages)
        chatClient.reentryObservations.clear(groupIdHex: groupIdHex)
        reentryStore.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        reentryRevision &+= 1
        persistSnapshotBestEffort()
        persistMessageLogBestEffort()
        state = state.with(threads: buildThreadList())
        ReentryLog.emit("conversation re-entry: this device is back in the conversation")
        // Everyone whose profile was sealed while out re-introduces itself.
        requestProfilesAfterJoin(groupIdHex: groupIdHex)
        if let historySecret {
            // Section 7 — looked for until it arrives or the Delivery Service's
            // retention passes (rule 1 of the 2026-11-19 walk fix), not by one
            // timed wait: the re-admitter sends it only once its own poll
            // reaches the new epoch, which may be days later.
            reentryGapLooks.save(
                ReentryGapLook(groupIdHex: groupIdHex, secret: historySecret, gapFromMs: lookFrom, gapToMs: now),
                accountIdB64: accountIdB64)
            Task { @MainActor [weak self] in
                await self?.lookForReentryGapHistory(groupIdHex: groupIdHex)
            }
        }
        return true
    }

    // MARK: - 7. The gap history

    /// The re-admitter: send every gap history owed for `groupIdHex` once
    /// this device holds the group at the epoch the replacement created.
    func sendOwedReentryHistory(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64, let identity = canonicalIdentity,
              let record = groups[groupIdHex], !reentryStopsActing(groupIdHex: groupIdHex) else { return }
        let all = reentryHistoryOwed.load(accountIdB64: accountIdB64)
        let mine = all.filter { $0.groupIdHex == groupIdHex }
        guard !mine.isEmpty, let epoch = try? record.group.epoch() else { return }
        let now = nowU64
        var keep = all.filter { $0.groupIdHex != groupIdHex }
        var due: [ReentryOwedHistory] = []
        for owed in mine {
            guard ConversationReentryRules.historyStillOwed(owed, nowMs: now) else { continue }
            if epoch < owed.targetEpoch { keep.append(owed) } else if epoch == owed.targetEpoch { due.append(owed) }
        }
        reentryHistoryOwed.save(keep, accountIdB64: accountIdB64)
        guard !due.isEmpty else { return }
        persistMessageLogBestEffort()
        let rows = messageLogStore.getLog(accountIdB64: accountIdB64)?.groups[groupIdHex]?.messages ?? []
        let roster = Set(((try? record.group.members()) ?? []).map(\.signaturePublicKey))
        for owed in due where roster.contains(owed.leafKey) {
            do {
                let secret = try record.group.exportSecret(
                    identity: identity, label: ConversationReentryRules.historyExporterLabel,
                    context: ConversationReentryRules.historyContext(leafKey: owed.leafKey),
                    length: ConversationReentryRules.historySecretLength)
                let gapRows = ConversationReentryRules.gapRows(rows, gapFromMs: owed.gapFromMs)
                if let sealed = try HistoryBundleCodec.buildForGroup(
                    hex: groupIdHex, rows: gapRows, accountIdB64: accountIdB64, secret: secret,
                    heldBeforeGap: ConversationReentryRules.gapHeldBefore(rows, gapFromMs: owed.gapFromMs)) {
                    try await WelcomeCourier.publishHistoryBundle(
                        rendezvousPseudonym: ConversationReentryRules.historyRendezvous(secret: secret),
                        sealedBytes: sealed, ds: dsClient)
                    ReentryLog.emit("conversation re-entry: the gap history was sent")
                }
            } catch {
                ReentryLog.emit("conversation re-entry: the gap history was not sent: \(ErrorLogTag.of(error))")
            }
        }
        // The returning device missed the epochs the group's name, photo,
        // pins and routing secret were sent in, and its leaf key did not
        // change, so no growth was seen: they go again.
        resendGroupStateForReturningDevice(groupIdHex: groupIdHex)
    }

    /// The device that came back: one short read of the gap history's
    /// rendezvous, at most once a minute per group (``ConversationReentryRules/gapLookVerdict(_:nowMs:)``).
    /// Called right after the join, at the tail of every applied poll of the
    /// group, and when the chat is opened; never from the notification
    /// extension (C-59). The first row that opens and files ends the look and
    /// narrows the gap line; a failed read, no row, or a row that does not
    /// open keeps it.
    func lookForReentryGapHistory(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64, !reentryHistoryReading.contains(groupIdHex),
              let look = reentryGapLooks.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex) else { return }
        let now = nowU64
        switch ConversationReentryRules.gapLookVerdict(look, nowMs: now) {
        case .wait:
            return
        case .drop:
            reentryGapLooks.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            ReentryLog.emit("chat re-entry: gap look dropped: retention passed")
            return
        case .look:
            break
        }
        reentryHistoryReading.insert(groupIdHex)
        defer { reentryHistoryReading.remove(groupIdHex) }
        reentryGapLooks.markLooked(accountIdB64: accountIdB64, groupIdHex: groupIdHex, atMs: now)
        let bound = binding.capture()
        let rows: [MailboxMessage]
        do {
            rows = try await dsClient.fetchRendezvous(
                id: ConversationReentryRules.historyRendezvous(secret: look.secret),
                after: 0, limit: Self.gapLookReadLimit, waitSecs: 0).messages
        } catch {
            return
        }
        guard bound.isCurrent, currentAccountIdB64 == accountIdB64 else { return }
        for row in rows {
            // Not the history bundle: skip it, as the timed wait did.
            if RecoveryPayload.isRecoveryRow(row.payload) || StateBundle.isStateRow(row.payload) { continue }
            let filed: HistoryBundleCodec.GapApplied
            do {
                filed = try HistoryBundleCodec.applyGapBundle(
                    accountIdB64: accountIdB64, secret: look.secret, sealedBytes: row.payload,
                    store: messageLogStore)
            } catch {
                continue
            }
            reentryGapLooks.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            if let record = groups[groupIdHex],
               record.narrowReentryGap(
                   gapToMs: look.gapToMs, complete: filed.complete, earliestMs: filed.earliestMs) {
                persistMessageLogBestEffort()
                state = state.with(threads: buildThreadList())
            }
            ReentryLog.emit("chat re-entry: missed messages received: rows=\(filed.applied)")
            return
        }
    }

    /// Rows one gap look reads from the rendezvous. Chosen, not measured:
    /// the bundle is normally the first and only row.
    static let gapLookReadLimit = 16
}
