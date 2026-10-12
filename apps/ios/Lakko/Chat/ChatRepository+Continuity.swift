// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0065 "Phase 2 revised" - the repository half of silent continuity:
//
//   * R.6: the receive rule for a successor claim (`group_closed`) and the
//     claim store;
//   * R.7 cases 1 and 3: the concrete `ContinuityPort` the pure sequence in
//     `ContinuityRuntime.swift` drives (claim, THEN build);
//   * R.7 case 1: nothing goes into a dead one-to-one but the claim; a
//     message written there is held and flushed into the successor.
//
// iOS mirror of the concrete port in web `chat/BackgroundReceiveContext.tsx`
// and of `chat/GroupClosedContext.tsx`. Every decision is a pure function of
// `SuccessorClaim.swift`, `HeldSend.swift`, `ResetRepair.swift` and
// `ResendMarker.swift`; this file reads the world for them and does what
// they say. Nothing here shows anything.
//
// Everything runs on the main actor. The claim store's read-decide-write
// regions contain no suspension point, which is what serializes them (C-62
// sub-invariant 5) without a lock of their own.

extension ChatRepository {

    // MARK: - Facts

    /// The pins of the signed-in account (the keys accepted resets retired
    /// are read off them).
    private var continuityPins: PeerPinStore { readmissionPins() }

    /// Every key an accepted reset retired -> its account id (base64).
    func supersededKeyMap() -> [String: String] {
        continuityPins.loadSupersededKeys()
    }

    /// The claim store, sealed under the device key (it can hold a prepared
    /// Welcome): the wrap the held-send queue uses.
    var groupClosedStore: GroupClosedStore {
        GroupClosedStore(ownerAccountIdB64: currentAccountIdB64, sealer: heldSendSealer)
    }

    /// What the continuity rules read off one live group, or `nil` when it
    /// cannot be read.
    func continuityFacts(group: Group) -> ContinuityFacts? {
        guard let members = try? group.members(), let kind = try? group.groupKind() else { return nil }
        let roles = try? group.groupRoles()
        return ContinuityFacts(
            kind: kind == .oneToOne ? .chat : .group,
            rosterPersonKeysB64: PersonKey.people(members).map { base64Encode($0) },
            ownerKeyB64: (roles ?? nil).map { base64Encode($0.owner) },
            adminKeysB64: (roles ?? nil)?.admins.map { base64Encode($0) } ?? [],
            active: readIsActive { try group.isActive() } != false)
    }

    func continuityFacts(groupIdB64: String) -> ContinuityFacts? {
        guard let hex = groupIdHex(ofB64: groupIdB64), let record = groups[hex] else { return nil }
        return continuityFacts(group: record.group)
    }

    /// The key that makes `group` unrepairable for an accepted reset, or
    /// `nil` (`SuccessorClaimRules.unrepairableBy` over the live group).
    func unrepairableKeyB64(group: Group) -> String? {
        let superseded = supersededKeyMap()
        guard !superseded.isEmpty, let facts = continuityFacts(group: group) else { return nil }
        return SuccessorClaimRules.unrepairableBy(
            kind: facts.kind,
            rosterPersonKeysB64: facts.rosterPersonKeysB64,
            ownPersonKeyB64: AuthorIdentity.own(group: group),
            ownerKeyB64: facts.ownerKeyB64,
            supersededKeys: Set(superseded.keys))
    }

    func unrepairableKeyB64(groupIdHex: String) -> String? {
        groups[groupIdHex].flatMap { unrepairableKeyB64(group: $0.group) }
    }

    private func heldKind(groupIdHex: String) -> HeldSend.Kind {
        isOneToOne(groupIdHex: groupIdHex) ? .chat : .group
    }

    /// Where a message composed for `groupIdHex` goes now.
    func sendRoute(groupIdHex: String) -> HeldSend.SendRoute {
        HeldSend.sendRoute(
            kind: heldKind(groupIdHex: groupIdHex),
            unrepairableKeyB64: unrepairableKeyB64(groupIdHex: groupIdHex))
    }

    /// Whether `groupIdHex` is a DEAD SESSION: a one-to-one whose other
    /// person's reset this device accepted.
    func isDeadSession(groupIdHex: String) -> Bool {
        sendRoute(groupIdHex: groupIdHex) == .hold
    }

    /// Install the process-wide publish guard and the reset-accepted trigger.
    /// Called once per bootstrap.
    func installContinuity() {
        ChatClient.deadSessionGuard = { [weak self] group in
            guard let self else { return false }
            let kind: HeldSend.Kind = (try? group.groupKind()) == .oneToOne ? .chat : .group
            return HeldSend.isDeadSession(
                kind: kind, unrepairableKeyB64: self.unrepairableKeyB64(group: group))
        }
        if resetEventsUnsubscribe == nil {
            resetEventsUnsubscribe = ResetEvents.subscribe { [weak self] _ in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    // A reset was accepted: the claims kept without a verdict
                    // are judged again, and every held group is offered a
                    // pass.
                    for hex in Array(self.groups.keys) { self.rejudgeClaims(groupIdHex: hex) }
                    // The extension learns the key this reset retired.
                    self.mirrorContinuityFacts()
                    self.continueAllSoon(force: true)
                }
            }
        }
        restoreHeldRows()
        mirrorLineage()
        continueAllSoon(force: true)
    }

    // MARK: - R.6: the receive rule

    private func claimInputs(groupIdHex: String) -> (
        facts: ContinuityFacts, own: String?, superseded: Set<String>, unrepairable: String?
    )? {
        guard let record = groups[groupIdHex], let facts = continuityFacts(group: record.group) else {
            return nil
        }
        let own = AuthorIdentity.own(group: record.group)
        let superseded = Set(supersededKeyMap().keys)
        let unrepairable = superseded.isEmpty ? nil : SuccessorClaimRules.unrepairableBy(
            kind: facts.kind,
            rosterPersonKeysB64: facts.rosterPersonKeysB64,
            ownPersonKeyB64: own,
            ownerKeyB64: facts.ownerKeyB64,
            supersededKeys: superseded)
        return (facts, own, superseded, unrepairable)
    }

    /// A `group_closed` envelope arrived in `groupIdHex`: a successor CLAIM.
    ///
    /// (i) the receiver has itself accepted a reset of the account that makes
    /// this group unrepairable; (ii) the sender, resolved at receipt from the
    /// MLS-authenticated leaf (C-68), is eligible; (iii) it is the first such
    /// claim in the journal's order. Never drawn.
    func receiveGroupClosed(
        groupIdHex: String, messageId: Int64, successorGroupId: Data?, senderLeaf: UInt32?
    ) {
        guard currentAccountIdB64 != nil, let record = groups[groupIdHex],
              let successorGroupId, !successorGroupId.isEmpty,
              let senderLeaf,
              let sender = (try? record.group.members())?
                .first(where: { $0.leafIndex == senderLeaf }).map({ base64Encode(PersonKey.of($0)) }),
              let inputs = claimInputs(groupIdHex: groupIdHex)
        else { return }
        let groupIdB64 = base64Encode(record.groupId)
        let store = groupClosedStore
        let state = store.read(groupIdB64: groupIdB64) ?? .noClaims
        let claim = SuccessorClaim(
            messageId: messageId,
            successorGroupIdB64: base64Encode(successorGroupId),
            senderKeyB64: sender,
            senderWasAdmin: inputs.facts.adminKeysB64.contains(sender),
            adminKeysAtReceiptB64: inputs.facts.adminKeysB64)
        let verdict = SuccessorClaimRules.claimVerdict(
            kind: inputs.facts.kind,
            groupIdB64: groupIdB64,
            claim: claim,
            held: state.accepted,
            unrepairableKeyB64: inputs.unrepairable,
            supersededKeys: inputs.superseded,
            survivingKeyB64: inputs.own)
        switch verdict {
        case .accept:
            store.write(groupIdB64: groupIdB64, GroupClosedRules.withAcceptedClaim(state, claim))
            claimAccepted(oldGroupIdHex: groupIdHex)
        case .keepPending:
            store.write(groupIdB64: groupIdB64, GroupClosedRules.withPendingClaim(state, claim))
            // The receiver has no accepted reset for this group YET: the
            // claim itself triggers the identity check, and is judged again
            // after it.
            recheckIdentitiesForClaim(groupIdHex: groupIdHex)
        case .ignoreLater, .dropIneligible, .dropMalformed:
            break
        }
    }

    /// Judge again the claims kept while this device had no verdict.
    func rejudgeClaims(groupIdHex: String) {
        guard currentAccountIdB64 != nil, let record = groups[groupIdHex] else { return }
        let groupIdB64 = base64Encode(record.groupId)
        let store = groupClosedStore
        guard let state = store.read(groupIdB64: groupIdB64), !state.pending.isEmpty,
              let inputs = claimInputs(groupIdHex: groupIdHex), let unrepairable = inputs.unrepairable
        else { return }
        let first = SuccessorClaimRules.firstEligibleClaim(
            kind: inputs.facts.kind,
            groupIdB64: groupIdB64,
            claims: state.pending,
            unrepairableKeyB64: unrepairable,
            supersededKeys: inputs.superseded,
            survivingKeyB64: inputs.own)
        let held = state.accepted
        let winner: SuccessorClaim?
        if let first, held.map({ first.messageId < $0.messageId }) ?? true {
            winner = first
        } else {
            winner = held
        }
        store.write(
            groupIdB64: groupIdB64,
            StoredGroupClosed(accepted: winner, pending: [], own: state.own, prepared: state.prepared))
        if winner != nil, winner != held { claimAccepted(oldGroupIdHex: groupIdHex) }
    }

    /// A claim became this conversation's successor.
    private func claimAccepted(oldGroupIdHex: String) {
        continueAllSoon(force: true)
    }

    /// How often the roster identities of one group are re-checked because a
    /// claim is waiting for a verdict. Chosen.
    private static let claimIdentityCheckIntervalMs: Int64 = 60_000

    /// A claim arrived and this device has accepted no reset for the group:
    /// check the identities of the roster's people against the directory (one
    /// of the acceptance rule's triggers). An accepted reset comes back
    /// through ``ResetEvents`` and re-judges the kept claims.
    private func recheckIdentitiesForClaim(groupIdHex: String) {
        let now = Self.nowMs()
        guard HeldSend.identityCheckDue(
            lastCheckedAtMs: claimIdentityCheckedAtMs[groupIdHex], nowMs: now,
            intervalMs: Self.claimIdentityCheckIntervalMs)
        else { return }
        claimIdentityCheckedAtMs[groupIdHex] = now
        Task { @MainActor [weak self] in
            await self?.checkRosterIdentities(groupIdHex: groupIdHex)
        }
    }

    /// Put every roster person whose account id this device knows (or was
    /// told) through the identity gate. Refusals, inconclusive checks and
    /// network failures change nothing.
    func checkRosterIdentities(groupIdHex: String) async {
        guard let record = groups[groupIdHex], let own = AuthorIdentity.own(group: record.group),
              let members = try? record.group.members() else { return }
        let candidates = DeviceReadmission.readmissionCandidates(
            rosterPersonKeysB64: PersonKey.people(members).map { base64Encode($0) },
            ownPersonKeyB64: own,
            contacts: readmissionContactsProvider(),
            profileAccountIdOf: { ProfileDirectory.shared.accountId(signatureKeyB64: $0) })
        for candidate in candidates {
            _ = try? await identityGateClient().checkContactIdentity(
                accountId: candidate.accountId, heldKeyB64: candidate.rosterKeyB64)
        }
        rejudgeClaims(groupIdHex: groupIdHex)
    }

    // MARK: - The pass

    /// How often every held group is offered a continuity pass from the tail
    /// of ANY poll: an old one-to-one is no longer polled once it moved, so
    /// its own poll cannot be what resumes an interrupted build. Chosen.
    static let continuitySweepIntervalMs: Int64 = 30_000

    /// Schedule ``continueAll(force:)``; never awaited by the caller (the
    /// apply region stays non-suspending).
    func continueAllSoon(force: Bool) {
        Task { @MainActor [weak self] in
            await self?.continueAll(force: force)
        }
    }

    /// Offer every held group a continuity pass, flush what is held for
    /// conversations whose successor now exists, and retire the push
    /// subscription of groups a conversation moved away from.
    func continueAll(force: Bool) async {
        guard currentAccountIdB64 != nil, canonicalIdentity != nil else { return }
        let now = Self.nowMs()
        if !force, now - lastContinuitySweepAtMs < Self.continuitySweepIntervalMs { return }
        lastContinuitySweepAtMs = now
        // What the notification extension could not withhold (a push for a
        // row this app shows nothing for) is taken down.
        removeQuietNotifications()
        // Not gated on a superseded key: a device that learned the lineage
        // from another device of the account retires the old subscription and
        // ends a call in the moved conversation too.
        for hex in Array(groups.keys).sorted() {
            // A live one-to-one call in a conversation that moved, or whose
            // session is dead, ends (the host ignores a group it has no call
            // in). Nothing is said.
            if isOneToOne(groupIdHex: hex),
               CallContinuity.oneToOneCallEndsOnMove(
                callGroupIdB64: hex,
                unrepairableKeyB64: isDeadSession(groupIdHex: hex)
                    ? unrepairableKeyB64(groupIdHex: hex) : nil,
                headGroupIdB64: conversationPlan(ofGroup: hex)?.headHex) {
                onOneToOneMoved?(hex)
            }
            await retireMovedPush(groupIdHex: hex)
        }
        if !supersededKeyMap().isEmpty {
            for hex in Array(groups.keys).sorted() {
                await continueFor(oldGroupIdHex: hex)
            }
        }
        await flushHeldSends()
    }

    /// One continuity pass over one group this device holds.
    private func continueFor(oldGroupIdHex: String) async {
        guard let record = groups[oldGroupIdHex], !continuityInFlight.contains(oldGroupIdHex) else {
            return
        }
        continuityInFlight.insert(oldGroupIdHex)
        defer { continuityInFlight.remove(oldGroupIdHex) }
        let port = RepositoryContinuityPort(repository: self)
        let step = await ContinuityRuntime.continueConversation(
            port: port, oldGroupIdB64: base64Encode(record.groupId))
        if step != .nothing && step != .adopted {
            NSLog("ADR-0065 continuity: \(step)")
        }
    }

    /// The conversation `oldGroupIdHex` belongs to has a successor on this
    /// device: stamp when (the old-segment poll window starts here) and end a
    /// live one-to-one call in it.
    func noteConversationMoved(oldGroupIdHex: String) {
        guard let oldB64 = groupIdB64(ofHex: oldGroupIdHex) else { return }
        continuityStore.noteConversationMoved(oldGroupIdB64: oldB64, nowMs: Self.nowMs())
        if isOneToOne(groupIdHex: oldGroupIdHex) {
            onOneToOneMoved?(oldGroupIdHex)
        }
    }

    /// Where a one-to-one call asked for in `groupIdHex` is dialled
    /// (`CallContinuity.oneToOneCallTarget`): a view still open on an older
    /// segment dials where the conversation is now; a dead session with no
    /// live successor dials nowhere. Group ids are the app's hex keys.
    func oneToOneCallTarget(groupIdHex: String) -> CallContinuity.Target {
        let headHex = conversationPlan(ofGroup: groupIdHex)?.headHex
        return CallContinuity.oneToOneCallTarget(
            groupIdB64: groupIdHex,
            unrepairableKeyB64: isDeadSession(groupIdHex: groupIdHex)
                ? unrepairableKeyB64(groupIdHex: groupIdHex) : nil,
            headGroupIdB64: headHex,
            headUnrepairableKeyB64: headHex.flatMap {
                $0 != groupIdHex && isDeadSession(groupIdHex: $0) ? unrepairableKeyB64(groupIdHex: $0) : nil
            })
    }

    // MARK: - R.6 step 1 and step 4a: prepare, deliver

    /// Step 1 — create the successor of `oldGroupIdHex` locally, add the new
    /// identity's devices and this account's other devices in ONE commit,
    /// claim its DS journal, and keep the Welcome for step 4. Sends nothing
    /// to anyone. Returns the successor's id (standard base64).
    func prepareSuccessor(oldGroupIdHex: String, newIdentityAccountId: Data) async throws -> String {
        guard let identity = canonicalIdentity, let old = groups[oldGroupIdHex],
              let kind = try? old.group.groupKind() else {
            throw ChatError.argument("no session")
        }
        let accountIdB64 = base64Encode(newIdentityAccountId)
        guard let newKeyB64 = continuityPins.readPin(accountIdB64: accountIdB64)?.pinnedSignaturePubkeyB64,
              let newKey = base64Decode(newKeyB64) else {
            throw ChatError.argument("the reset identity is not pinned")
        }
        memberAccountIdHints[newKey] = newIdentityAccountId
        // Only the devices bound to the key this device ACCEPTED: the
        // directory is not trusted to say whose devices these are.
        // The gate runs first, like every other add (``expectedPersonDevices``).
        let recipientDevices = try await expectedPersonDevices(
            accountId: newIdentityAccountId, expectedKey: newKey)
        guard !recipientDevices.isEmpty else {
            throw ChatError.argument("the reset identity publishes no device yet")
        }
        let devices = recipientDevices + (await ownOtherDevices(identity: identity))
        let group: Group
        let welcome: Data
        let joinSigner: DsProofSigner
        do {
            group = try Group.create(identity: identity, kind: kind)
            welcome = try group.addMembersFromWire(
                identity: identity, keyPackages: devices.map { $0.keyPackage }).welcome
            joinSigner = try group.dsAccessSigner(identity: identity)
        } catch {
            throw ChatError.wrapped("successor creation failed", underlying: error)
        }
        let groupId = try group.groupId()
        // ADR-0057 obligation 1 — the journal is claimed at the epoch the
        // Welcome lands its members at, before any Welcome can go out.
        do {
            try await JournalOwnership.claim(
                root: try journalRootSigner(groupId: groupId),
                journalId: groupId,
                epoch: joinSigner.epoch(),
                epochKey: joinSigner.verifyingKey(),
                ds: dsClient)
        } catch {
            throw ChatError.wrapped("DS journal claim failed", underlying: error)
        }
        let successorHex = DsClient.pseudonymHex(groupId)
        if let ownAccount = currentAccountIdB64 {
            registrarSeeds.markRootHeld(accountIdB64: ownAccount, groupIdHex: successorHex)
        }
        // The successor must be in the durable MLS snapshot before the claim
        // names it: a restart between the claim and the delivery loads it
        // from there.
        try persistSnapshotDurableOrThrow()
        let successorB64 = base64Encode(groupId)
        preparedSuccessors[successorB64] = group
        dsSigners.observe(journal: successorHex, signer: joinSigner)
        var targets = [newKeyB64]
        if devices.count > recipientDevices.count,
           let own = ReachCenter.shared.ownAccountKey,
           !ReachCenter.shared.keyResolver.samePerson(own, newKey) {
            targets.append(base64Encode(own))
        }
        let oldB64 = base64Encode(old.groupId)
        let store = groupClosedStore
        let state = store.read(groupIdB64: oldB64) ?? .noClaims
        store.write(
            groupIdB64: oldB64,
            StoredGroupClosed(
                accepted: state.accepted, pending: state.pending, own: state.own,
                prepared: PreparedSuccessor(
                    successorGroupIdB64: successorB64,
                    welcomeB64: base64Encode(wrapEnvelope(kind: .welcome, body: welcome)),
                    targetKeysB64: targets,
                    delivered: false)))
        return successorB64
    }

    /// Step 2 — post the claim into the OLD group; resolves the DS message
    /// id. The own-claim record is written before the publish.
    func postClaim(oldGroupIdHex: String, successorGroupIdB64: String) async throws -> Int64 {
        guard let identity = canonicalIdentity, let old = groups[oldGroupIdHex],
              let successorGroupId = base64Decode(successorGroupIdB64) else {
            throw ChatError.argument("the old group is not held")
        }
        guard !membershipEnded(membershipStateNow(groupIdHex: oldGroupIdHex)) else {
            throw ChatError.argument("no longer in this group")
        }
        let oldB64 = base64Encode(old.groupId)
        let store = groupClosedStore
        let before = store.read(groupIdB64: oldB64) ?? .noClaims
        store.write(
            groupIdB64: oldB64,
            StoredGroupClosed(
                accepted: before.accepted, pending: before.pending,
                own: OwnClaim(successorGroupIdB64: successorGroupIdB64, messageId: nil),
                prepared: before.prepared))
        let envelope = try composeGroupClosedEnvelope(
            closedAtMs: UInt64(max(0, Self.nowMs())), successorGroupId: successorGroupId)
        // The ONE publish a dead session admits.
        let published = try await withCommitLock(groupIdHex: oldGroupIdHex) { [self] in
            guard let record = groups[oldGroupIdHex] else {
                throw ChatError.argument("the old group is not held")
            }
            return try await chatClient.sendMessage(
                identity: identity, group: record.group, plaintext: envelope,
                intoSupersededSession: true)
        }
        let after = store.read(groupIdB64: oldB64) ?? .noClaims
        store.write(
            groupIdB64: oldB64,
            StoredGroupClosed(
                accepted: after.accepted, pending: after.pending,
                own: OwnClaim(successorGroupIdB64: successorGroupIdB64, messageId: published.messageId),
                prepared: after.prepared))
        return published.messageId
    }

    /// Step 4a — make the prepared successor a held conversation and deliver
    /// the Welcomes. Idempotent: `false` when the successor could not be
    /// brought up (the next pass tries again).
    func deliverSuccessor(oldGroupIdHex: String, successorGroupIdB64: String) async throws -> Bool {
        guard let identity = canonicalIdentity, let old = groups[oldGroupIdHex],
              let successorGroupId = base64Decode(successorGroupIdB64) else { return false }
        let oldB64 = base64Encode(old.groupId)
        let store = groupClosedStore
        let prepared = store.read(groupIdB64: oldB64)?.prepared
        let successorHex = DsClient.pseudonymHex(successorGroupId)
        // A device of this account that did not create the successor joins it
        // from the Welcome the founder sent to the account's own mailbox.
        guard let prepared, prepared.successorGroupIdB64 == successorGroupIdB64 else {
            return groups[successorHex] != nil
        }
        if groups[successorHex] == nil {
            let group: Group
            if let live = preparedSuccessors[successorGroupIdB64] {
                group = live
            } else if let loaded = try? loadGroup(identity: identity, groupId: successorGroupId) {
                // A restart between the claim and the delivery: the
                // successor is in the MLS snapshot the creation persisted.
                group = loaded
            } else {
                return false
            }
            preparedSuccessors[successorGroupIdB64] = nil
            groups[successorHex] = GroupRecord(group: group, groupId: successorGroupId)
            mirrorLineage()
            claimedGroupsThisSession.insert(successorHex)
            persistKnownGroupBestEffort(groupIdHex: successorHex)
            persistSnapshotBestEffort()
            state = state.with(threads: buildThreadList())
            subscribeGroupUnderPushGid(groupIdHex: successorHex)
            reconcileJournalRegistrarsSoon(groupIdHex: successorHex)
            publishOwnProfileSoon(groupIdHex: successorHex)
            requestProfilesAfterJoin(groupIdHex: successorHex)
        }
        if !prepared.delivered {
            guard let welcomeB64 = prepared.welcomeB64, let welcomeEnvelope = base64Decode(welcomeB64) else {
                return false
            }
            for keyB64 in prepared.targetKeysB64 {
                guard let key = base64Decode(keyB64) else { continue }
                try await deliverWelcome(
                    welcomeEnvelope: welcomeEnvelope, toAccountKey: key,
                    auth: ReachCenter.shared.writeAuth(toAccountKey: key))
            }
            // Kept as the mark "this device founded it"; the Welcome is
            // dropped.
            let state = store.read(groupIdB64: oldB64) ?? .noClaims
            store.write(
                groupIdB64: oldB64,
                StoredGroupClosed(
                    accepted: state.accepted, pending: state.pending, own: state.own,
                    prepared: PreparedSuccessor(
                        successorGroupIdB64: successorGroupIdB64, welcomeB64: nil,
                        targetKeysB64: [], delivered: true)))
        }
        return true
    }

    /// A lost claim: drop the local successor (never announced, no Welcome
    /// sent).
    func discardSuccessor(successorGroupIdB64: String) {
        preparedSuccessors[successorGroupIdB64] = nil
        let store = groupClosedStore
        for (oldB64, state) in store.entries() {
            if let prepared = state.prepared, prepared.successorGroupIdB64 == successorGroupIdB64,
               !prepared.delivered {
                store.write(
                    groupIdB64: oldB64,
                    StoredGroupClosed(
                        accepted: state.accepted, pending: state.pending, own: state.own, prepared: nil))
            }
        }
    }

    // MARK: - R.7 case 3: people, roles

    /// The account id a person's CURRENT devices may be fetched under, or
    /// `nil`. An id is followed only when this device trusts the roster key
    /// for it: the pin record, or a binding it verified itself against the
    /// directory. A claimed id (a profile's, or a contact row adopted from
    /// one) is first put through the identity check, which records a binding
    /// when the directory serves exactly this key; when it does not, nobody
    /// is added under that id.
    func verifiedAccountId(ofPersonKeyB64 personKeyB64: String) async -> Data? {
        let contacts = readmissionContactsProvider()
        let known = contacts.first {
            $0.signaturePubkeyB64 == personKeyB64 || $0.previousKeysB64.contains(personKeyB64)
        }?.accountId
        guard let accountId = known ?? ProfileDirectory.shared.accountId(signatureKeyB64: personKeyB64)
        else { return nil }
        let accountIdB64 = base64Encode(accountId)
        let standing: () -> DeviceReadmission.LookupStanding = { [self] in
            let pin = continuityPins.readPin(accountIdB64: accountIdB64)
            return DeviceReadmission.lookupStanding(
                rosterKeyB64: personKeyB64,
                pin: pin,
                provenPredecessorsB64: pin.map {
                    provenPredecessorsProvider().predecessors(of: $0.pinnedSignaturePubkeyB64)
                } ?? [],
                binding: pin == nil ? readmissionBindings().read(accountIdB64: accountIdB64) : nil)
        }
        if standing() == .samePerson { return accountId }
        guard (try? await identityGateClient().checkContactIdentity(
            accountId: accountId, heldKeyB64: personKeyB64)) != nil else { return nil }
        return standing() == .samePerson ? accountId : nil
    }

    // MARK: - R.7 case 1 / D5: the founder's re-send

    /// Send again, into the successor, this account's messages sent in the
    /// old one-to-one after the reset was signed. The founder alone, once.
    /// Each carries the re-send marker naming the ORIGINAL group and message
    /// id, so a device that holds the original shows the message once. This
    /// device adds no row for them: its history row is the message.
    func resendOwnMessages(
        oldGroupIdHex: String, successorGroupIdB64: String, accountIdB64: String
    ) async throws {
        guard let old = groups[oldGroupIdHex],
              let successorHex = groupIdHex(ofB64: successorGroupIdB64), groups[successorHex] != nil
        else { return }
        let oldB64 = base64Encode(old.groupId)
        // Another device of this account that joined the successor holds no
        // prepared record for it.
        guard let prepared = groupClosedStore.read(groupIdB64: oldB64)?.prepared,
              prepared.successorGroupIdB64 == successorGroupIdB64 else { return }
        let marks = continuityStore
        if marks.wasResent(oldGroupIdB64: oldB64) { return }
        guard let signedAtMs = continuityPins.readPin(accountIdB64: accountIdB64)?.resetSignedAtMs else {
            return
        }
        let stored = old.messages
        // An interrupted re-send continues after the last message that
        // landed (the durable cursor), never from the first.
        let ids = ResetRepair.messagesToResend(
            stored.filter { $0.messageId > 0 && $0.ack == .sent }.map {
                ResetRepair.OldSegmentMessage(
                    messageId: $0.messageId,
                    own: $0.direction == .outgoing,
                    sentAtMs: Self.parseDsTimestampMs($0.createdAt),
                    resendable: !$0.deleted && (!$0.text.isEmpty || !$0.attachments.isEmpty))
            },
            resetSignedAtMs: signedAtMs,
            afterMessageId: marks.resendCursor(oldGroupIdB64: oldB64))
        for id in ids {
            guard let message = stored.first(where: { $0.messageId == id }) else { continue }
            // The same content, sealed again for the new session. The stored
            // attachment references are reused: it is the same conversation.
            // A reply keeps its quote and a forward its attribution and
            // origin (`ResetRepair.resendParts`).
            let parts = ResetRepair.resendParts(
                replyAuthor: message.reply?.author, replyExcerpt: message.reply?.excerpt,
                isReply: message.reply != nil,
                forwardAuthor: message.forward?.author,
                forwardOriginGroupB64: message.forward?.originGroupIdHex
                    .flatMap(ForwardPayload.groupIdB64(fromHex:)),
                forwardOriginMessageId: message.forward?.originMessageId,
                isForward: message.forward != nil,
                originGroupB64: oldB64, originMessageId: id)
            let envelope = try LakkoCore.composeChatEnvelope(
                text: message.text,
                attachments: message.attachments,
                parts: ChatEnvelopeParts(
                    replyTo: parts.replyTo,
                    replyAuthor: parts.replyAuthor,
                    replyExcerpt: parts.replyExcerpt,
                    forwardedFrom: parts.forwardedFrom,
                    forwardOriginGroupB64: parts.forwardOriginGroupB64,
                    forwardOriginMessageId: parts.forwardOriginMessageId,
                    resendOriginGroupB64: parts.resendOriginGroupB64,
                    resendOriginMessageId: parts.resendOriginMessageId))
            _ = try await withCommitLock(groupIdHex: successorHex) { [self] in
                try await sealAndPublishLocked(
                    groupIdHex: successorHex, plaintextBytes: envelope, wake: .message)
            }
            // It landed: a failure of a later message resumes after it.
            marks.advanceResendCursor(oldGroupIdB64: oldB64, to: id)
        }
        marks.markResent(oldGroupIdB64: oldB64)
    }

    // MARK: - The two silent drops of the receive paths

    /// Drop, before anything is counted, drawn or acted on:
    ///
    ///   * EVERY application row whose sender is a key an accepted reset
    ///     retired (`ResetRepair.dropsSupersededSender`): a chat row, an op,
    ///     a control or service envelope (a profile, a group name, a pin, a
    ///     push routing secret, a successor claim, ...) and call signaling. A
    ///     removed device that still holds the old keys can post until its
    ///     leaf is removed, and nothing it sends acts: its call does not
    ///     ring, its envelope changes no state. An MLS handshake row has no
    ///     application sender and is never dropped here (it was applied by
    ///     the core; skipping it would fork the group);
    ///   * a re-send whose marker names a row this device already holds, in
    ///     an older segment of the same conversation and from the same author
    ///     (`ResendMarker`). A marker that matches nothing changes nothing.
    ///
    /// `eventSenders` is the application sender of each event, in order
    /// (`PollMessagesResult.eventSenders`).
    func silentlyFiltered(
        _ events: [IncomingChatEvent],
        resendOrigins: [Int64: ResendMarker.Origin],
        eventSenders: [MemberIdentity?],
        groupIdHex: String
    ) -> [IncomingChatEvent] {
        let superseded = Set(supersededKeyMap().keys)
        if superseded.isEmpty && resendOrigins.isEmpty { return events }
        let kept = ResetRepair.keptAfterSupersededSenders(
            events, senders: eventSenders, supersededKeys: superseded)
        if resendOrigins.isEmpty { return kept }
        var received: [ResendMarker.Received] = []
        for event in kept {
            if case .application(let messageId, _, _, let senderKey, _, _, _) = event {
                received.append(ResendMarker.Received(
                    resendOrigin: resendOrigins[messageId], senderKeyB64: senderKey))
            }
        }
        let own = groups[groupIdHex].flatMap { AuthorIdentity.own(group: $0.group) }
        let resolver = ReachCenter.shared.keyResolver
        let alreadyHeld = ResendMarker.heldResendPredicate(
            batch: received,
            port: ResendMarker.ReceivePort(
                receivedGroupIdHex: groupIdHex,
                conversationOf: { [conversationLineage] in conversationLineage.conversationOf($0) },
                segmentIndexOf: { [conversationLineage] in conversationLineage.segmentIndexOf($0) },
                ownPersonKeyB64: own,
                canonical: { key in
                    guard let bytes = base64Decode(key) else { return key }
                    return base64Encode(resolver.current(of: bytes))
                },
                loadLog: { [weak self] in
                    guard let self else { return nil }
                    return ResendMarker.HeldLog(rows: { hex in
                        self.groups[hex]?.messages.filter { $0.messageId > 0 }.map {
                            ResendMarker.HeldRow(
                                messageId: $0.messageId,
                                direction: $0.direction == .outgoing
                                    ? .outgoing : ($0.direction == .incoming ? .incoming : .system),
                                authorKeyB64: $0.authorKeyB64)
                        }
                    })
                }))
        return kept.filter { event in
            guard case .application(let messageId, _, _, let senderKey, _, _, _) = event else {
                return true
            }
            return !alreadyHeld(ResendMarker.Received(
                resendOrigin: resendOrigins[messageId], senderKeyB64: senderKey))
        }
    }

    // MARK: - R.7 case 1: the held send

    /// The durable queue of the signed-in account, or `nil` when signed out.
    private var heldQueue: HeldSendQueue? {
        guard let accountIdB64 = currentAccountIdB64 else { return nil }
        return HeldSendQueue(ownerAccountId: accountIdB64, sealer: heldSendSealer)
    }

    private static func heldReply(_ reply: ReplyRef?) -> HeldSend.Reply? {
        reply.map { HeldSend.Reply(target: $0.target, author: $0.author, excerpt: $0.excerpt) }
    }

    private static func replyRef(_ reply: HeldSend.Reply?) -> ReplyRef? {
        reply.map { ReplyRef(target: $0.target, author: $0.author, excerpt: $0.excerpt) }
    }

    /// Hold a message written into a dead one-to-one: it shows the ordinary
    /// sending state and goes out when the successor exists. Returns `false`
    /// when it could not be queued durably (the caller then fails the send
    /// the ordinary way).
    func holdMessage(
        groupIdHex: String, text: String, attachments: [AttachmentRef],
        reply: ReplyRef?, forward: ForwardRef?, existingLocalId: Int64? = nil
    ) -> Bool {
        guard let record = groups[groupIdHex], let queue = heldQueue else { return false }
        let encoder = JSONEncoder()
        var encodedAttachments: [Data] = []
        for attachment in attachments {
            guard let data = try? encoder.encode(PersistedAttachment(from: attachment)) else { return false }
            encodedAttachments.append(data)
        }
        let encodedForward: Data?
        if let forward {
            guard let data = try? encoder.encode(PersistedForward(
                author: forward.author, originGroupIdHex: forward.originGroupIdHex,
                originMessageId: forward.originMessageId)) else { return false }
            encodedForward = data
        } else {
            encodedForward = nil
        }
        let entryId = UUID().uuidString
        let now = Self.nowMs()
        let entry = HeldSendQueue.Entry(
            localId: entryId, queuedAtMs: now, text: text, attachments: encodedAttachments,
            reply: Self.heldReply(reply), replyGroupIdB64: base64Encode(record.groupId),
            forward: encodedForward)
        guard queue.enqueue(
            conversationHex: conversationIdHex(ofGroup: groupIdHex), entry: entry) else { return false }
        let localId: Int64
        if let existingLocalId {
            localId = existingLocalId
            record.markPending(localId: existingLocalId)
        } else {
            localId = record.nextLocalId()
            record.appendPending(
                localId: localId, text: text, attachments: attachments,
                createdAt: Self.isoTimestamp(ms: now),
                authorKeyB64: AuthorIdentity.own(group: record.group),
                reply: reply, forward: forward)
        }
        heldRows[entryId] = (groupIdHex, localId)
        state = state.with(threads: buildThreadList())
        // The successor is what lets it go out: offer the pass now, and look
        // again when the give-up is due.
        continueAllSoon(force: true)
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(HeldSend.giveUpMs + 1_000) * 1_000_000)
            await self?.flushHeldSends()
        }
        return true
    }

    private func decodeHeld(_ entry: HeldSendQueue.Entry) -> (attachments: [AttachmentRef], forward: ForwardRef?)? {
        let decoder = JSONDecoder()
        var attachments: [AttachmentRef] = []
        for data in entry.attachments {
            guard let persisted = try? decoder.decode(PersistedAttachment.self, from: data),
                  let ref = persisted.toAttachmentRef() else { return nil }
            attachments.append(ref)
        }
        var forward: ForwardRef?
        if let data = entry.forward {
            guard let persisted = try? decoder.decode(PersistedForward.self, from: data) else { return nil }
            forward = ForwardRef(
                author: persisted.author, originGroupIdHex: persisted.originGroupIdHex,
                originMessageId: persisted.originMessageId)
        }
        return (attachments, forward)
    }

    /// After a restart the queue is on disk and the pending rows are not
    /// (an unsent row is never written to the message log): draw each held
    /// message again in its conversation's current group, with the give-up
    /// clock restarted.
    func restoreHeldRows() {
        guard let queue = heldQueue else { return }
        for (conversationHex, entries) in queue.restored(nowMs: Self.nowMs()) {
            let headHex = headGroupIdHex(of: conversationHex)
            guard let record = groups[headHex] else { continue }
            var changed = false
            for entry in entries where heldRows[entry.localId] == nil {
                guard let decoded = decodeHeld(entry) else { continue }
                let localId = record.nextLocalId()
                record.appendPending(
                    localId: localId, text: entry.text, attachments: decoded.attachments,
                    createdAt: Self.isoTimestamp(ms: entry.queuedAtMs),
                    authorKeyB64: AuthorIdentity.own(group: record.group),
                    reply: Self.replyRef(entry.reply), forward: decoded.forward)
                heldRows[entry.localId] = (headHex, localId)
                changed = true
            }
            if changed { state = state.with(threads: buildThreadList()) }
        }
    }

    /// Send the messages held for conversations whose successor now exists on
    /// this device, each into the conversation's current group; give up on
    /// the ones held too long while there is still nowhere to send them (the
    /// ordinary failed state, with its ordinary retry). A send that fails
    /// keeps that message and everything after it held, in order.
    ///
    /// On this client the repository owns every send, whether or not the
    /// conversation is on screen, so there is no "a view is mounted" case:
    /// the rows a flush sends are in the published state the view reads, and
    /// ``HeldFlushHub`` is told what went out.
    func flushHeldSends() async {
        guard let queue = heldQueue, canonicalIdentity != nil, !heldFlushRunning else { return }
        heldFlushRunning = true
        defer { heldFlushRunning = false }
        restoreHeldRows()
        migrateUnsentRows()
        let now = Self.nowMs()
        for (conversationHex, entries) in queue.all() {
            let headHex = headGroupIdHex(of: conversationHex)
            guard let head = groups[headHex] else { continue }
            let route = sendRoute(groupIdHex: headHex)
            let verdict = HeldSend.flushVerdict(
                held: entries.count,
                viewMounted: false,
                headActive: !membershipEnded(membershipStateNow(groupIdHex: headHex)),
                headRoute: route)
            if verdict == .wait {
                var changed = false
                for entry in entries
                where HeldSend.heldVerdict(route: route, heldAtMs: entry.queuedAtMs, nowMs: now) == .giveUp {
                    if let row = heldRows.removeValue(forKey: entry.localId) {
                        groups[row.groupIdHex]?.markFailed(localId: row.localId)
                        changed = true
                    }
                    queue.remove(conversationHex: conversationHex, localId: entry.localId)
                }
                if changed { state = state.with(threads: buildThreadList()) }
                continue
            }
            let headB64 = base64Encode(head.groupId)
            var sent: [HeldFlushReport.Message] = []
            for entry in entries {
                guard let decoded = decodeHeld(entry) else {
                    queue.remove(conversationHex: conversationHex, localId: entry.localId)
                    continue
                }
                // A reply composed in the old segment keeps its quote and
                // loses its jump target: the id belongs to the old journal.
                let reply = Self.replyRef(HeldSend.replyForSend(
                    entry.reply, composedInGroupIdB64: entry.replyGroupIdB64,
                    sendIntoGroupIdB64: headB64))
                // The row moves with the message: out of the old segment,
                // into the group it is sent in.
                if let row = heldRows[entry.localId], row.groupIdHex != headHex {
                    groups[row.groupIdHex]?.removeLocalRow(localId: row.localId)
                    heldRows[entry.localId] = nil
                }
                let localId: Int64
                if let row = heldRows[entry.localId] {
                    localId = row.localId
                } else {
                    localId = head.nextLocalId()
                    head.appendPending(
                        localId: localId, text: entry.text, attachments: decoded.attachments,
                        createdAt: Self.isoTimestamp(ms: entry.queuedAtMs),
                        authorKeyB64: AuthorIdentity.own(group: head.group),
                        reply: reply, forward: decoded.forward)
                    heldRows[entry.localId] = (headHex, localId)
                }
                state = state.with(threads: buildThreadList())
                do {
                    let envelope = try Self.composeChatEnvelope(
                        text: entry.text, attachments: decoded.attachments,
                        reply: reply, forward: decoded.forward)
                    let result = try await withCommitLock(groupIdHex: headHex) { [self] in
                        try await sealAndPublishLocked(
                            groupIdHex: headHex, plaintextBytes: envelope, wake: .message)
                    }
                    groups[headHex]?.markSent(
                        localId: localId, messageId: result.messageId, createdAt: result.createdAt)
                    heldRows[entry.localId] = nil
                    queue.remove(conversationHex: conversationHex, localId: entry.localId)
                    sent.append(HeldFlushReport.Message(localId: entry.localId, messageId: result.messageId))
                } catch {
                    // Not sent: this one and everything after it stay held,
                    // in order.
                    NSLog("ADR-0065 held send: a held message was not sent yet")
                    break
                }
            }
            if !sent.isEmpty {
                persistMessageLogBestEffort()
                state = state.with(threads: buildThreadList())
                // A view that was mounting while this ran is told what went
                // out, so it shows those rows at once.
                HeldFlushHub.shared.notify(HeldFlushReport(groupIdB64: headB64, messages: sent))
            }
        }
    }

    /// A conversation moved to a successor group: the rows this device
    /// wrote in an OLDER segment that never reached the server (a failed
    /// send, a held message past its give-up) move to the group the
    /// conversation is now, in the state they were in, so their ordinary
    /// retry sends where the conversation continues. A reply among them
    /// keeps its quote and loses its jump target. Held rows still in the
    /// queue are moved by the flush itself.
    func migrateUnsentRows() {
        let tracked = Set(heldRows.values.map { "\($0.groupIdHex):\($0.localId)" })
        var changed = false
        for plan in conversationPlans() {
            guard let head = groups[plan.headHex],
                  let headAt = plan.heldSegments.firstIndex(of: plan.headHex), headAt > 0 else { continue }
            let headB64 = base64Encode(head.groupId)
            for hex in plan.heldSegments.prefix(headAt) {
                guard let record = groups[hex] else { continue }
                for row in record.unsentRows
                where row.ack == .failed && !tracked.contains("\(hex):\(row.localId)") {
                    let reply = Self.replyRef(HeldSend.replyForSend(
                        Self.heldReply(row.reply),
                        composedInGroupIdB64: base64Encode(record.groupId),
                        sendIntoGroupIdB64: headB64))
                    record.removeLocalRow(localId: row.localId)
                    head.adoptUnsentRow(row, reply: reply)
                    changed = true
                }
            }
        }
        if changed { state = state.with(threads: buildThreadList()) }
    }

    /// A conversation was deleted here: what was held for it goes with it.
    func forgetHeld(conversationHex: String) {
        guard let queue = heldQueue else { return }
        for entry in queue.list(conversationHex: conversationHex) { heldRows[entry.localId] = nil }
        queue.clear(conversationHex: conversationHex)
    }

    /// Forget everything held (sign-out, account wipe).
    func clearHeldSends() {
        heldRows = [:]
        preparedSuccessors = [:]
        continuityInFlight = []
        claimIdentityCheckedAtMs = [:]
    }
}

// MARK: - The concrete port

/// What `ContinuityRuntime` does to the world, over one ``ChatRepository``.
@MainActor
final class RepositoryContinuityPort: ContinuityPort {
    private let repository: ChatRepository

    init(repository: ChatRepository) {
        self.repository = repository
    }

    private func hex(_ groupIdB64: String) throws -> String {
        guard let hex = repository.groupIdHex(ofB64: groupIdB64) else {
            throw ChatError.argument("malformed group id")
        }
        return hex
    }

    private var pins: PeerPinStore { repository.readmissionPins() }

    func ownPersonKeyB64() -> String? { repository.ownPersonKeyB64() }

    func supersededKeys() -> [String: String] { repository.supersededKeyMap() }

    func currentKeyOf(accountIdB64: String) -> String? {
        pins.readPin(accountIdB64: accountIdB64)?.pinnedSignaturePubkeyB64
    }

    func facts(groupIdB64: String) -> ContinuityFacts? {
        repository.continuityFacts(groupIdB64: groupIdB64)
    }

    func claimState(oldGroupIdB64: String) -> StoredGroupClosed {
        repository.groupClosedStore.read(groupIdB64: oldGroupIdB64) ?? .noClaims
    }

    func cursorOf(oldGroupIdB64: String) -> Int64? {
        repository.groupIdHex(ofB64: oldGroupIdB64).flatMap { repository.groups[$0]?.cursor }
    }

    func newIdentityHasDevices(accountIdB64: String) async throws -> Bool {
        guard let keyB64 = currentKeyOf(accountIdB64: accountIdB64), let key = base64Decode(keyB64),
              let accountId = base64Decode(accountIdB64) else { return false }
        // Only a device bound to the key this device ACCEPTED counts. A
        // question, not an add: the acceptance that pinned the key is the
        // gate, so it is not run again here.
        guard let devices = try? await repository.expectedPersonDevices(
            accountId: accountId, expectedKey: key, gateAlreadyRan: true) else { return false }
        return !devices.isEmpty
    }

    func prepareSuccessor(oldGroupIdB64: String, newIdentityAccountIdB64: String) async throws -> String {
        guard let accountId = base64Decode(newIdentityAccountIdB64) else {
            throw ChatError.argument("malformed account id")
        }
        return try await repository.prepareSuccessor(
            oldGroupIdHex: try hex(oldGroupIdB64), newIdentityAccountId: accountId)
    }

    func postClaim(oldGroupIdB64: String, successorGroupIdB64: String) async throws -> Int64 {
        try await repository.postClaim(
            oldGroupIdHex: try hex(oldGroupIdB64), successorGroupIdB64: successorGroupIdB64)
    }

    func abandonOwnClaim(oldGroupIdB64: String) async throws {
        let store = repository.groupClosedStore
        guard let state = store.read(groupIdB64: oldGroupIdB64), state.own != nil else { return }
        store.write(
            groupIdB64: oldGroupIdB64,
            StoredGroupClosed(
                accepted: state.accepted, pending: state.pending, own: nil, prepared: state.prepared))
    }

    func discardSuccessor(successorGroupIdB64: String) async throws {
        repository.discardSuccessor(successorGroupIdB64: successorGroupIdB64)
    }

    func settle(oldGroupIdB64: String, winner: SuccessorClaim) async throws {
        let store = repository.groupClosedStore
        let state = store.read(groupIdB64: oldGroupIdB64) ?? .noClaims
        let next: SuccessorClaim
        if let held = state.accepted, held.messageId <= winner.messageId {
            next = held
        } else {
            next = winner
        }
        store.write(
            groupIdB64: oldGroupIdB64,
            StoredGroupClosed(accepted: next, pending: [], own: nil, prepared: state.prepared))
    }

    func recordLineage(oldGroupIdB64: String, successorGroupIdB64: String) async throws {
        let oldHex = try hex(oldGroupIdB64)
        guard repository.groups[oldHex] != nil, let successor = base64Decode(successorGroupIdB64),
              let root = ChatRepository.hexToBytes(repository.conversationIdHex(ofGroup: oldHex)) else { return }
        repository.noteConversationMoved(oldGroupIdHex: oldHex)
        _ = await repository.lineageWriter(successor, root)
    }

    func deliverSuccessor(oldGroupIdB64: String, successorGroupIdB64: String) async throws -> Bool {
        try await repository.deliverSuccessor(
            oldGroupIdHex: try hex(oldGroupIdB64), successorGroupIdB64: successorGroupIdB64)
    }

    func carryName(oldGroupIdB64: String, successorGroupIdB64: String) async throws {
        try await repository.carryGroupName(
            oldGroupIdHex: try hex(oldGroupIdB64), successorGroupIdHex: try hex(successorGroupIdB64))
    }

    func addPerson(successorGroupIdB64: String, personKeyB64: String) async throws -> Bool {
        let successorHex = try hex(successorGroupIdB64)
        guard repository.groups[successorHex] != nil, let personKey = base64Decode(personKeyB64),
              let accountId = await repository.verifiedAccountId(ofPersonKeyB64: personKeyB64)
        else { return false }
        do {
            _ = try await repository.addMemberToGroup(
                groupIdHex: successorHex, recipientAccountId: accountId,
                recipientSignaturePubkey: personKey)
            await repository.sweepIfCommitter(groupIdHex: successorHex)
            return true
        } catch {
            // Not resolvable or not reachable now: they keep writing in the
            // old group and the completeness re-check tries again.
            NSLog("ADR-0065 continuity: a member was not added to the successor yet")
            return false
        }
    }

    func recheckDue(successorGroupIdB64: String) -> Bool {
        // Its own slot name, so the re-check and the ADR-0050 arm of the same
        // group do not consume each other's interval.
        DeviceReadmissionMemory.takeSlot(
            groupIdHex: "completeness:\(successorGroupIdB64)", nowMs: ChatRepository.nowMs())
    }

    func setAdmins(successorGroupIdB64: String, adminKeysB64: [String]) async throws {
        let successorHex = try hex(successorGroupIdB64)
        let have = repository.continuityFacts(groupIdB64: successorGroupIdB64)?.adminKeysB64 ?? []
        // One role change per sweep (C-65): one admin per pass.
        guard let next = adminKeysB64.first(where: { !have.contains($0) }),
              let key = base64Decode(next) else { return }
        do {
            _ = try await repository.setMemberAdmin(
                groupIdHex: successorHex, memberSignaturePubkey: key, makeAdmin: true)
            await repository.sweepIfCommitter(groupIdHex: successorHex)
        } catch {
            // A pending role change refuses a second one; the next pass tries.
            NSLog("ADR-0065 continuity: an admin was not restored yet")
        }
    }

    func transferOwnership(successorGroupIdB64: String, newOwnerKeyB64: String, stayAdmin: Bool) async throws {
        let successorHex = try hex(successorGroupIdB64)
        guard let key = base64Decode(newOwnerKeyB64) else { return }
        do {
            _ = try await repository.transferGroupOwnership(
                groupIdHex: successorHex, newOwnerSignaturePubkey: key,
                outgoing: stayAdmin ? .admin : .member)
            await repository.sweepIfCommitter(groupIdHex: successorHex)
        } catch {
            // Already pending, or a role change is in the way: the next pass.
            NSLog("ADR-0065 continuity: the owner was not restored yet")
        }
    }

    func resendOwnMessages(oldGroupIdB64: String, successorGroupIdB64: String, accountIdB64: String) async throws {
        try await repository.resendOwnMessages(
            oldGroupIdHex: try hex(oldGroupIdB64), successorGroupIdB64: successorGroupIdB64,
            accountIdB64: accountIdB64)
    }
}
