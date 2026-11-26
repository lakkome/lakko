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

    func requireSendAllowedWhileBehind(groupIdHex: String, isPersonContent: Bool, personAction: Bool = false) throws {
        let behind = reentryBehind(groupIdHex: groupIdHex)
        // Walk-3 rule W3-R2 — a refused read with no answered read since
        // holds the automatic sends too. Quiet, like the behind refusal.
        let readRefused = chatClient.reentryObservations.snapshot(groupIdHex: groupIdHex).refusedReads > 0
        guard ConversationReentryRules.sendAllowed(
            behind: behind, isPersonContent: isPersonContent, readRefused: readRefused,
            personAction: personAction) else {
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
        reentryRowClock.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        reentryKnockKeySent.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        reentryLeafFirstSeen.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        chatClient.reentryObservations.clear(groupIdHex: groupIdHex)
    }

    /// Section 7 — record the first time this device saw each leaf of the
    /// group's roster (only the first observation is kept). Called after
    /// every applied poll.
    func noteReentryLeavesSeen(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex],
              let members = try? record.group.members() else { return }
        reentryLeafFirstSeen.noteRoster(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex,
            leafKeys: members.map(\.signaturePublicKey), nowMs: nowU64)
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
        // Walk-3 rule W3-R6 — whether this answer ends a refused-read hold.
        let refusedBefore = observations.snapshot(groupIdHex: groupIdHex).refusedReads > 0
        observations.noteReadAnswered(groupIdHex: groupIdHex)
        // Walk-2 rule R1 — what this read showed of the conversation is
        // durable before any request can go out.
        noteConversationKnown(
            groupIdHex: groupIdHex, rowAtMs: observations.takeLastAppliedRowAtMs(groupIdHex: groupIdHex),
            readOk: true)
        let evidence = observations.snapshot(groupIdHex: groupIdHex)
        let held = chatClient.registeredCommitHold.hold(groupIdHex: groupIdHex) != nil
        let observation = ConversationReentryRules.observation(
            ownCommitOwed: ownCommitOwed(groupIdHex: groupIdHex, record: record),
            rowsReturned: raw.messages.count, hasMore: raw.hasMore, held: held,
            latestEpoch: raw.latestEpoch, localEpoch: localEpoch, evidence: evidence)
        foldReentry(
            groupIdHex: groupIdHex, observation: observation, evidence: evidence,
            answeredAfterRefusal: refusedBefore)
    }

    /// Walk-2 rule R1 — raise the durable clock: a row stamped `rowAtMs`
    /// applied (or an own publish was accepted), and, with `readOk`, a read
    /// of the journal was answered now.
    func noteConversationKnown(groupIdHex: String, rowAtMs: UInt64?, readOk: Bool = false) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        if let rowAtMs {
            reentryRowClock.noteRowKnown(accountIdB64: accountIdB64, groupIdHex: groupIdHex, atMs: rowAtMs)
        }
        if readOk {
            reentryRowClock.noteReadOk(accountIdB64: accountIdB64, groupIdHex: groupIdHex, nowMs: nowU64)
        }
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

    /// `answeredAfterRefusal`: this observation is an answered read that
    /// follows one or more refused reads (walk-3 rule W3-R6).
    private func foldReentry(
        groupIdHex: String, observation: FfiBehindObservation, evidence: ReentryObservations.Snapshot,
        answeredAfterRefusal: Bool = false
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
            kept.ownDeviceAsked = nil
            chatClient.reentryObservations.clear(groupIdHex: groupIdHex)
            saveReentry(kept, groupIdHex: groupIdHex)
            state = state.with(threads: buildThreadList())
            resendOwnStateBackInStep(groupIdHex: groupIdHex)
            return
        }
        if next != previous {
            saveReentry(next, groupIdHex: groupIdHex)
            if next.behind != previous.behind { state = state.with(threads: buildThreadList()) }
        }
        // Walk-3 rule W3-R6 — a refused-read hold that ended with the
        // conversation in step throughout: what was held goes as after
        // coming back in step.
        if answeredAfterRefusal, previous.behind == .inStep, next.behind == .inStep {
            resendOwnStateBackInStep(groupIdHex: groupIdHex)
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

    /// Walk-3 rule W3-R5 — back in step (behind -> in step by a read, or the
    /// re-entry joined; W3-R6: or a refused-read hold ended in step): this device re-sends its own growth-time state for
    /// the conversation, the re-send a newcomer's growth triggers (profile,
    /// group name, photo, reactions setting, handle, shared pins, shared
    /// channels, the knock key as owner or admin; each envelope's own rule
    /// decides whether this device sends it). What this client stores first
    /// by design goes too: an owed group photo, and an anonymous ballot that
    /// failed while behind.
    func resendOwnStateBackInStep(groupIdHex: String) {
        guard groups[groupIdHex] != nil, !membershipEnded(membershipStateNow(groupIdHex: groupIdHex)) else { return }
        resendGroupStateForReturningDevice(groupIdHex: groupIdHex, includeRoutingSecret: false)
        Task { @MainActor [weak self] in await self?.sendOwedGroupPhotos() }
        BallotQueue.shared.retryFailed(groupIdHex: groupIdHex)
        reentryBackInStepObserver?(groupIdHex)
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
    /// (`reentryRequestDue`: as soon as behind, 2 minutes after a request to
    /// another own device, otherwise every 30 minutes). The
    /// re-entry KeyPackage's private keys (the MLS snapshot) and the record
    /// naming it are durable BEFORE anything is sent.
    func sendReentryRequestIfDue(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64, let identity = canonicalIdentity,
              let record = groups[groupIdHex], !reentrySending.contains(groupIdHex),
              !membershipEnded(membershipStateNow(groupIdHex: groupIdHex)) else { return }
        var current = reentryStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let now = nowU64
        guard current.behind.stopsActing,
              reentryRequestDue(
                  state: current.behind.ffi, lastSentMs: current.lastSentMs,
                  lastRouteWasOwnDevice: current.lastRouteWasOwnDevice, nowMs: now)
        else { return }
        reentrySending.insert(groupIdHex)
        defer { reentrySending.remove(groupIdHex) }
        let bound = binding.capture()
        // Walk-3 rule W3-R1 — the destination first: a request with nowhere
        // to go mints nothing.
        let kind = groupKind(groupIdHex: groupIdHex) ?? .group
        // Live walk 5 (2026-11-20): the own device is asked once per behind
        // episode; it may not be running, so later requests go by the
        // conversation's own route (the core decides). With no DeviceGroup
        // sender on this device route 1 cannot send at all, so the own device
        // counts as absent and the request goes by route 2 or 3 at once.
        let route = reentryRoute(
            ownDeviceInConversation: ConversationReentryRules.ownDeviceReachable(
                inConversation: ownDeviceInConversation(group: record.group),
                senderRegistered: reentryOwnDeviceSender != nil),
            kind: kind,
            ownDeviceUnanswered: current.ownDeviceUnanswered)
        guard let destination = reentryDestination(route: route, groupIdHex: groupIdHex) else {
            ReentryLog.emit("conversation re-entry request not sent: no destination by route "
                + "\(ConversationReentryRules.routeName(route))")
            retrySoon(groupIdHex: groupIdHex)
            return
        }
        // Then `epoch_left`: EVERY request (the first and every resend) asks
        // the journal right before minting, with an empty knock (one
        // `ds-knock` token, ADR-0053). Never a stored latest epoch: one kept
        // from an earlier read can be stale by many commits, and a request
        // naming it is never acted on.
        let probed = try? await dsClient.knock(groupId: record.groupId, payloads: []).epoch
        // The probe waited: what it answered is for the handle and the state
        // it was asked for.
        guard bound.isCurrent, groups[groupIdHex] === record else { return }
        current = reentryStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        guard current.behind.stopsActing else { return }
        guard let epochLeft = probed else {
            ReentryLog.emit("conversation re-entry request not sent: the latest epoch is unknown")
            retrySoon(groupIdHex: groupIdHex)
            return
        }
        // Walk-2 rule R1 — the gap starts no later than when this device last
        // knew the conversation, so a device that fell behind through refused
        // reads (nothing skipped) still names it. Kept as the record's start,
        // so the gap line later starts at the value the request carried.
        let clock = reentryRowClock.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let gapFromMs = ConversationReentryRules.gapStart(
            firstSkippedMs: current.gapFromMs, lastKnownRowMs: clock.lastKnownRowMs,
            lastReadOkMs: clock.lastReadOkMs, suspicionSinceMs: current.behind.sinceMs, nowMs: now)
        // Section 7 — a fresh gap key per request: the gap's history is
        // sealed to its public half, which the request carries; its secret is
        // kept, sealed, with the request record, so only this device opens
        // the history.
        let gapKey = generateGapKey()
        guard let sealedGapSecret = reentryGapLooks.sealSecret(gapKey.secret, accountIdB64: accountIdB64) else {
            ReentryLog.emit("conversation re-entry request not sent: the gap key could not be kept")
            retrySoon(groupIdHex: groupIdHex)
            return
        }
        let keyPackage: Data
        let envelope: Data
        do {
            keyPackage = try identity.createReentryKeyPackage(groupId: record.groupId, epochLeft: epochLeft)
            envelope = try composeConversationReentryEnvelope(
                groupId: record.groupId, epochLeft: epochLeft,
                gapFromMs: gapFromMs, gapKey: gapKey.publicKey, keyPackage: keyPackage)
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
                route: ConversationReentryRules.routeName(route), cursorAtSend: record.cursor,
                gapPublicKey: gapKey.publicKey, sealedGapSecret: sealedGapSecret))
        for old in dropped { try? identity.forgetKeyPackage(keyPackage: old) }
        do {
            try persistSnapshotDurableOrThrow()
        } catch {
            ReentryLog.emit("conversation re-entry request not sent: the snapshot did not persist")
            return
        }
        current.requests = kept
        current.gapFromMs = gapFromMs
        current.lastSentMs = now
        current.lastRoute = ConversationReentryRules.routeName(route)
        guard bound.isCurrent, saveReentry(current, groupIdHex: groupIdHex) else { return }
        do {
            try await sendReentryEnvelope(envelope, destination: destination, record: record)
            ReentryLog.emit("conversation re-entry request sent by route \(ConversationReentryRules.routeName(route))")
            if route == .ownDevice, bound.isCurrent {
                var sent = reentryStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
                if sent.behind.stopsActing {
                    sent.ownDeviceAsked = true
                    saveReentry(sent, groupIdHex: groupIdHex)
                }
            }
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

    /// Where a re-entry request goes, resolved before anything is minted.
    enum ReentryDestination {
        case ownDevice(@MainActor (Data) async -> Void)
        case mailbox(ReachCenter.WritePlan)
        case knock(version: UInt16, publicKey: Data)
    }

    /// The destination of `route`, or `nil` when this device has none yet
    /// (no DeviceGroup sender, no reach grant to the other person, no knock
    /// key).
    func reentryDestination(route: FfiReentryRoute, groupIdHex: String) -> ReentryDestination? {
        switch route {
        case .ownDevice:
            // Route 1 — a DeviceGroup service envelope to the account's other
            // devices.
            return reentryOwnDeviceSender.map { .ownDevice($0) }
        case .mailbox:
            // Route 2 — a sealed row to the other person's account mailbox,
            // under a reach grant this device holds.
            guard let other = otherPersonKey(groupIdHex: groupIdHex),
                  let plan = ReachCenter.shared.writePlan(toAccountKey: other) else { return nil }
            return .mailbox(plan)
        case .knock:
            // Route 3 — sealed to the group's knock key.
            guard let knock = reentryKnockKey(groupIdHex: groupIdHex) else { return nil }
            return .knock(version: knock.version, publicKey: knock.publicKey)
        }
    }

    private func sendReentryEnvelope(
        _ envelope: Data, destination: ReentryDestination, record: GroupRecord
    ) async throws {
        switch destination {
        case .ownDevice(let sender):
            await sender(envelope)
        case .mailbox(let plan):
            let row = try plan.target.seal(kind: .conversationReentry, body: envelope)
            _ = try await RefusedMailboxWrite.tryingCandidates(plan.candidates) { auth in
                try await self.dsClient.publishMailbox(pseudonym: plan.target.address, payload: row, writeAuth: auth)
            }
        case .knock(let version, let publicKey):
            let row = try sealReentryKnockRow(knockPk: publicKey, knockVersion: version, envelope: envelope)
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

    /// The owner or an admin (both hold the knock seeds) sends the group's
    /// knock key to the members (section 3, route 3): a plain member holds
    /// no knock seed. Walk-2 rule R4: due when the version differs from the
    /// last delivered one, or a current member was not in the set at the last
    /// delivery (``ConversationReentryRules/knockKeySendDue(mark:version:members:)``);
    /// the mark is set only after the broadcast went out, so a send refused
    /// while behind or failed stays due. Called from the owner's knock
    /// reconcile, on growth, after a replacement and at every poll tail.
    func publishKnockKeyIfOwed(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex],
              !reentryKnockKeySending.contains(groupIdHex),
              !membershipEnded(membershipStateNow(groupIdHex: groupIdHex)),
              !reentryStopsActing(groupIdHex: groupIdHex) else { return }
        let role = ownRole(groupIdHex: groupIdHex)
        guard role == .owner || role == .admin, let knock = knockKey(groupIdHex: groupIdHex),
              let members = PushGroupRouting.memberSignatureKeys(group: record.group) else { return }
        guard ConversationReentryRules.knockKeySendDue(
            mark: reentryKnockKeySent.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex),
            version: knock.version, members: members) else { return }
        reentryKnockKeySending.insert(groupIdHex)
        defer { reentryKnockKeySending.remove(groupIdHex) }
        let bound = binding.capture()
        do {
            let envelope = try composeKnockKeyEnvelope(knockVersion: knock.version, knockPk: knock.publicKey)
            try await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: envelope)
            guard bound.isCurrent else { return }
            // The members captured BEFORE the send: one added meanwhile is
            // not in the mark, so the key goes again (never one too few).
            reentryKnockKeySent.save(
                ReentryKnockKeySentMark(version: knock.version, members: members),
                accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        } catch {
            if !ConversationReentryRules.isBehindRefusal(error) {
                NSLog("[knock] WARN the knock key was not sent in-group; retried at a later poll")
            }
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
        // Section 7 — the rows sent start no earlier than this device's own
        // evidence that the leaf was in the conversation (the first time it
        // saw the leaf's key in the roster, else the last message from it);
        // with neither, nothing is sent.
        if let start = gapHistoryStart(
            claimedMs: request.gapFromMs,
            leafFirstSeenMs: reentryLeafFirstSeen.firstSeen(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, leafKey: target.signatureKey),
            leafLastWriteMs: chatClient.reentryObservations.lastWrite(
                groupIdHex: groupIdHex, leaf: target.replacedLeaf)) {
            reentryHistoryOwed.add(
                ReentryOwedHistory(
                    groupIdHex: groupIdHex, keyPackage: request.keyPackage, leafKey: target.signatureKey,
                    gapKey: request.gapKey, proposedAtEpoch: epoch, gapFromMs: start, owedAtMs: nowU64),
                accountIdB64: accountIdB64)
        } else {
            ReentryLog.emit("conversation re-entry: no gap history owed: the leaf was never seen here")
        }
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
        // Section 7 — the gap keys of the requests out: which one the
        // re-admitter answered is not known here, so the history is looked
        // for under each.
        let gapKeys = ConversationReentryRules.gapKeyPairs(record.requests) {
            reentryGapLooks.openSecret($0, accountIdB64: accountIdB64)
        }
        let back = GroupRecord(group: joined, groupId: groupId)
        back.adoptReentryRecord(old, readFrom: record.readFromAfterReentry ?? old.cursor)
        // UX 2.4 — where messages were sent while out, one local line.
        let now = nowU64
        // The gap's start: the value the request carried (walk-2 rule R1);
        // the gap history is looked for from there.
        let clock = reentryRowClock.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let lookFrom = record.gapFromMs.map { min($0, now) } ?? ConversationReentryRules.gapStart(
            firstSkippedMs: nil, lastKnownRowMs: clock.lastKnownRowMs, lastReadOkMs: clock.lastReadOkMs,
            suspicionSinceMs: record.behind.sinceMs, nowMs: now)
        // Walk-2 rule R2 — the line sits where the gap was by time, after the
        // last row at or before its start, not at the end.
        back.insertReentryGapLine(fromMs: lookFrom, toMs: now)
        groups[groupIdHex] = back
        // Back by a Welcome: no row of the new group applied yet.
        reentryRowClock.noteReentryJoined(accountIdB64: accountIdB64, groupIdHex: groupIdHex, nowMs: now)
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
        // Walk-3 rule W3-R5 — and this device re-sends its own.
        resendOwnStateBackInStep(groupIdHex: groupIdHex)
        if !gapKeys.isEmpty {
            // Section 7 — looked for until it arrives or the Delivery Service's
            // retention passes (rule 1 of the 2026-11-19 walk fix), not by one
            // timed wait: the re-admitter sends it only once its own poll
            // sees the replacement landed, which may be days later.
            reentryGapLooks.save(
                ReentryGapLook(
                    groupIdHex: groupIdHex, keys: gapKeys, gapFromMs: lookFrom, gapToMs: now,
                    route: record.lastRoute),
                accountIdB64: accountIdB64)
            Task { @MainActor [weak self] in
                await self?.lookForReentryGapHistory(groupIdHex: groupIdHex)
            }
        }
        return true
    }

    // MARK: - 7. The gap history

    /// The re-admitter: send every gap history owed for `groupIdHex` whose
    /// replacement has landed (``ConversationReentryRules/owedHistoryVerdict(_:epoch:rosterHasLeaf:nowMs:)``),
    /// sealed to the request's gap key and signed by this device's leaf.
    func sendOwedReentryHistory(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64, let identity = canonicalIdentity,
              let record = groups[groupIdHex], !reentryStopsActing(groupIdHex: groupIdHex) else { return }
        let all = reentryHistoryOwed.load(accountIdB64: accountIdB64)
        let mine = all.filter { $0.groupIdHex == groupIdHex }
        guard !mine.isEmpty, let epoch = try? record.group.epoch() else { return }
        let now = nowU64
        let roster = Set(((try? record.group.members()) ?? []).map(\.signaturePublicKey))
        var keep = all.filter { $0.groupIdHex != groupIdHex }
        var due: [ReentryOwedHistory] = []
        for owed in mine {
            switch ConversationReentryRules.owedHistoryVerdict(
                owed, epoch: epoch, rosterHasLeaf: roster.contains(owed.leafKey), nowMs: now) {
            case .wait: keep.append(owed)
            case .send: due.append(owed)
            case .drop: break
            }
        }
        reentryHistoryOwed.save(keep, accountIdB64: accountIdB64)
        guard !due.isEmpty else { return }
        persistMessageLogBestEffort()
        let rows = messageLogStore.getLog(accountIdB64: accountIdB64)?.groups[groupIdHex]?.messages ?? []
        for owed in due {
            do {
                guard let rendezvous = ConversationReentryRules.gapRendezvous(gapKey: owed.gapKey) else {
                    ReentryLog.emit("conversation re-entry: the gap history was not sent: bad gap key")
                    continue
                }
                let gapRows = ConversationReentryRules.gapRows(rows, gapFromMs: owed.gapFromMs)
                if let sealed = try HistoryBundleCodec.buildGapForGroup(
                    hex: groupIdHex, rows: gapRows, accountIdB64: accountIdB64,
                    readmitter: identity, gapKey: owed.gapKey, groupId: record.groupId,
                    heldBeforeGap: ConversationReentryRules.gapHeldBefore(rows, gapFromMs: owed.gapFromMs),
                    ownKey: AuthorIdentity.own(group: record.group).flatMap { Data(base64Encoded: $0) }) {
                    try await WelcomeCourier.publishHistoryBundle(
                        rendezvousPseudonym: rendezvous, sealedBytes: sealed, ds: dsClient)
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
    /// extension (C-59). Reads every page of the rendezvous
    /// (``RendezvousRead``) and tries its rows newest first; the first row
    /// that opens and files ends the look and narrows the gap line; a failed
    /// read, no row, or rows that do not open keep it.
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
        for key in look.keys {
            guard let rendezvous = ConversationReentryRules.gapRendezvous(gapKey: key.publicKey) else { continue }
            let rows: [MailboxMessage]
            do {
                // Every page, not the first: a member who saw the request
                // can post junk rows ahead of the real bundle.
                let ds = dsClient
                rows = try await RendezvousRead.readAll { after, limit in
                    try await ds.fetchRendezvous(id: rendezvous, after: after, limit: limit, waitSecs: 0)
                }
            } catch {
                continue
            }
            guard bound.isCurrent, currentAccountIdB64 == accountIdB64 else { return }
            // Newest first; a row that does not open, or whose signer is not
            // accepted, is passed over.
            for row in Self.gapLookCandidates(rows) {
                guard let live = groups[groupIdHex], let identity = canonicalIdentity else { return }
                let opened: FfiOpenedGapHistoryStructured
                do {
                    opened = try openGapHistoryStructured(
                        gapSecret: key.secret, groupId: live.groupId, sealed: row.payload)
                } catch {
                    continue
                }
                // Kept only when its signer is this account, the owner or an
                // admin, or the other person of a one-to-one (the rule a
                // re-entry Welcome's sender passes); otherwise dropped and
                // the gap line stays.
                guard (try? live.group.gapHistorySignerAccepted(
                    identity: identity, signerLeafKey: opened.signerLeafKey)) == true else {
                    ReentryLog.emit("chat re-entry: a gap history from a signer not trusted here was dropped")
                    continue
                }
                // Walk-3 rule W3-R3 — each row takes its author, and its
                // direction from it, against this account's own key here.
                let ownKey = AuthorIdentity.own(group: live.group).flatMap { Data(base64Encoded: $0) }
                // ADR-0101 section 9 (D2, D3) — a page of the history asked
                // for after a reset carries this person's rows under the key
                // the reset retired (drawn as their own), and the originals
                // of re-sends already drawn here (left out).
                let drawn = ResendOriginLog().read(accountIdB64: accountIdB64)[groupIdHex] ?? []
                let resolver = ReachCenter.shared.keyResolver
                let filed = HistoryBundleCodec.applyOpenedGapBundle(
                    opened.bundle, accountIdB64: accountIdB64, ownKey: ownKey, route: look.route,
                    formerOwnKeys: formerOwnAccountKeys(accountIdB64: accountIdB64),
                    skip: drawn.isEmpty ? nil : { messageId, author in
                        ResendMarker.historyRowAlreadyShown(
                            messageId: messageId, authorKeyB64: author.map(base64Encode), drawn: drawn,
                            canonical: { key in
                                guard let bytes = base64Decode(key) else { return key }
                                return base64Encode(resolver.current(of: bytes))
                            })
                    },
                    store: messageLogStore)
                reentryGapLooks.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
                // ADR-0101 section 9 — a page of the history asked for after
                // this account's key reset: the next page is asked for.
                if look.route == HistoryReturnRules.lookRoute {
                    historyPageFiled(groupIdHex: groupIdHex, messageIds: Self.historyMessageIds(opened.bundle))
                }
                if live.narrowReentryGap(
                    gapToMs: look.gapToMs, complete: filed.complete, earliestMs: filed.earliestMs) {
                    persistMessageLogBestEffort()
                    state = state.with(threads: buildThreadList())
                }
                ReentryLog.emit("chat re-entry: missed messages received: rows=\(filed.applied)")
                return
            }
        }
    }

    /// The rows of a gap look worth opening, newest first: everything but a
    /// recovery or state row (not the history bundle; skipped as the timed
    /// wait did).
    nonisolated static func gapLookCandidates(_ rows: [MailboxMessage]) -> [MailboxMessage] {
        rows.reversed().filter {
            !RecoveryPayload.isRecoveryRow($0.payload) && !StateBundle.isStateRow($0.payload)
        }
    }
}
