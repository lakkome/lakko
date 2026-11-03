// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0087 section 9 — the end of a membership, on this device.
///
/// One seam (``endMembershipIfOut(groupIdHex:)``), called from where every
/// applied commit passes, plus a launch sweep over the stored groups, plus a
/// reaction to the account's own register (``applyHeardEnds()``). All three
/// lead to one durable owed record, and the pipeline runs from that record in
/// this order, so a crash anywhere resumes it:
///
///   1. Record the end, with the notice (``MembershipEndStore``).
///   2. Publish `conversation_ended` over the DeviceGroup.
///   3. Stop: the poll, the beacon and the shrink rotation stop on the record
///      itself (``membershipStateNow(groupIdHex:)``), the push subscription
///      is retired here.
///   4. Wipe, delete the MLS group, persist the provider snapshot durably,
///      and drop the known-groups row and the registry entry LAST.
///
/// Never on a refused DS read: nothing here reads the network to decide. Never
/// from a background path (C-59): the notification extension holds a throwaway
/// group and writes nothing, and this app's own VoIP wake only records the end
/// and leaves the rest to the foreground (``membershipEndMayRun``).
@MainActor
extension ChatRepository {

    /// An end is owed for this group: the row is gone and everything that
    /// would use the group stops, whatever step the end has reached.
    func isEnding(groupIdHex: String) -> Bool {
        owedEnds[groupIdHex] != nil
    }

    /// Every key that names this account or this device in a roster: the
    /// current account key, the one the device binding names, the account's
    /// own predecessors (ADR-0074), and the device's own leaf key.
    func ownRosterKeys() -> Set<Data> {
        var keys = Set<Data>()
        if let key = accountIdentity?.signaturePublicKey() { keys.insert(key) }
        if let key = canonicalIdentity?.signaturePublicKey() { keys.insert(key) }
        if let key = canonicalIdentity?.accountPublicKey() { keys.insert(key) }
        if let accountIdB64 = currentAccountIdB64 {
            for seed in KeychainOwnPredecessorStore().load(accountIdB64: accountIdB64) {
                if let key = (try? Identity.fromSeed(seed: seed))?.signaturePublicKey() {
                    keys.insert(key)
                }
            }
        }
        return keys
    }

    private func personIsOut(of record: GroupRecord) -> Bool {
        MembershipEndRules.personOut(
            isActive: readIsActive { try record.group.isActive() },
            roster: try? record.group.members(),
            ownKeys: ownRosterKeys())
    }

    /// The seam. Called where every applied commit passes, and at launch.
    func endMembershipIfOut(groupIdHex: String) {
        guard let record = groups[groupIdHex], !isEnding(groupIdHex: groupIdHex),
              personIsOut(of: record), let epoch = try? record.group.epoch() else { return }
        recordEnd(groupIdHex: groupIdHex, epoch: epoch, heard: false)
    }

    /// Step 1. Synchronous: this runs inside the commit lock's apply region.
    private func recordEnd(groupIdHex: String, epoch: UInt64, heard: Bool, quiet: Bool = false) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        let left = quiet || leaveRequestedAt(groupIdHex: groupIdHex) != nil
        let end = OwedMembershipEnd(
            groupIdHex: groupIdHex, endedEpoch: epoch, heard: heard,
            oneToOne: isOneToOne(groupIdHex: groupIdHex),
            // A device that heard of it has nothing to tell.
            published: heard)
        // UX 9.4 — the person asked for this: no notice. A device that heard
        // of a leave made on another device cannot tell, and shows it.
        let notice = left ? nil : MembershipEndNotice(
            groupIdHex: groupIdHex, name: titleForNotice(groupIdHex: groupIdHex))
        guard membershipEndStore.record(end, notice: notice, accountIdB64: accountIdB64) else {
            return
        }
        owedEnds[groupIdHex] = end
        refreshEndNotices()
        state = state.with(threads: buildThreadList())
        onMembershipEndRecorded?(groupIdHex, left)
        resumeEnd(groupIdHex: groupIdHex)
    }

    /// ADR-0087 sections 10 and 11 — `Delete group`, for an owner alone in
    /// the group: the group and its messages go from every device of the
    /// person, through the same owed pipeline as a leave (no notice: the
    /// person asked for it). `false` when the person is not alone in it or is
    /// not its owner. The @name is not built on this client, so there is none
    /// to retire.
    @discardableResult
    public func deleteAloneGroup(groupIdHex: String) -> Bool {
        guard let record = groups[groupIdHex], !isEnding(groupIdHex: groupIdHex),
              isAloneInGroup(groupIdHex: groupIdHex), ownRole(groupIdHex: groupIdHex) == .owner,
              let epoch = try? record.group.epoch() else { return false }
        recordEnd(groupIdHex: groupIdHex, epoch: epoch, heard: false, quiet: true)
        return true
    }

    /// Section 11 — retire the group's `@name` first (freed in 30 days), then
    /// end the group as ``deleteAloneGroup(groupIdHex:)`` does. A failed
    /// retire changes nothing and throws.
    public func deleteAloneGroupRetiringName(groupIdHex: String) async throws -> Bool {
        guard isAloneInGroup(groupIdHex: groupIdHex), ownRole(groupIdHex: groupIdHex) == .owner else {
            return false
        }
        if let held = groupHandle(groupIdHex: groupIdHex), let record = groups[groupIdHex],
           let accountIdentity {
            do {
                try await ConversationHandles.retire(
                    discovery: DiscoveryClient(), accountSeed: accountIdentity.ed25519Seed(),
                    journalId: record.groupId, handle: held.handle)
            } catch let refusal as DiscoveryClient.HandleWriteError where refusal.code == "no-such-handle" {
                // Already gone (lapsed): nothing to retire.
            }
            if let accountIdB64 = currentAccountIdB64 {
                HeldHandleStore.remove(journalIdHex: AttachmentBridge.hex(record.groupId), accountIdB64)
            }
        }
        return deleteAloneGroup(groupIdHex: groupIdHex)
    }

    /// ADR-0087 section 10 — nobody but this person (all of whose devices
    /// count as one) is in the group.
    public func isAloneInGroup(groupIdHex: String) -> Bool {
        guard let record = groups[groupIdHex], conversationKind(groupIdHex: groupIdHex) == .group,
              let members = try? record.group.members() else { return false }
        let own = ownRosterKeys()
        return PersonKey.people(members).allSatisfy { own.contains($0) }
    }

    /// The title the list draws, while the roster can still be read.
    private func titleForNotice(groupIdHex: String) -> String {
        if let shared = groupName(groupIdHex: groupIdHex) { return shared }
        return ConversationTitle.groupTitle(
            otherMemberNames: ChatListViewModel.otherMemberNames(
                repository: self, groupIdHex: groupIdHex))
    }

    /// Run every owed end that may run now. Idempotent.
    func resumeOwedEnds() {
        for groupIdHex in owedEnds.keys.sorted() { resumeEnd(groupIdHex: groupIdHex) }
    }

    private func resumeEnd(groupIdHex: String) {
        guard owedEnds[groupIdHex] != nil, !endsRunning.contains(groupIdHex),
              membershipEndMayRun() else { return }
        endsRunning.insert(groupIdHex)
        let bound = binding.capture()
        Task { @MainActor [weak self] in
            await self?.runEnd(groupIdHex: groupIdHex, bound: bound)
            guard let self, bound.isCurrent else { return }
            self.endsRunning.remove(groupIdHex)
            if self.owedEnds[groupIdHex] == nil { await self.resolveHeldWelcomes() }
        }
    }

    private func saveOwed(_ end: OwedMembershipEnd, accountIdB64: String) {
        owedEnds[end.groupIdHex] = end
        membershipEndStore.put(end, accountIdB64: accountIdB64)
    }

    /// Steps 2 to 4 from the record. Stops where it cannot go on; a later pass
    /// (the synced state becoming ready, the foreground, the next launch)
    /// picks it up from the record.
    private func runEnd(groupIdHex: String, bound: BindingEpoch.Captured) async {
        guard let accountIdB64 = currentAccountIdB64, var end = owedEnds[groupIdHex],
              let id = Self.hexToBytes(groupIdHex), let sync = conversationEndSync else { return }
        func gone() -> Bool { !bound.isCurrent || owedEnds[groupIdHex] == nil }
        let ref = FolderConversationRef(kind: end.referenceKind, id: id)
        if !end.published {
            guard await sync.publishConversationEnded(ref, epoch: end.endedEpoch), !gone() else {
                return
            }
            end.published = true
            saveOwed(end, accountIdB64: accountIdB64)
        }
        if end.channel {
            // A channel's row and posts went when it was unsubscribed; what
            // is owed is telling the other devices and the registers.
            guard await sync.clearConversation([ref]), !gone() else { return }
            dropOwed(groupIdHex: groupIdHex, accountIdB64: accountIdB64)
            return
        }
        // Every group of the conversation this device holds, the ended one
        // last (ADR-0073 R.5): an older segment is the same conversation.
        let plan = conversationPlan(ofGroup: groupIdHex)
        let segments = plan?.heldSegments ?? [groupIdHex]
        if !end.stopped {
            // What the lock screen still shows of it: stacked by conversation,
            // or by routing id when the wake could not be routed.
            var threads = Set(segments + [plan?.conversationHex ?? groupIdHex])
            for segment in segments {
                if let gid = pushRouting.pushGid(accountIdB64: accountIdB64, groupIdHex: segment) {
                    threads.insert(PushGroupRouting.hex(gid))
                }
            }
            QuietNotifications.removeDelivered(threads: threads)
            for segment in segments {
                chatClient.forgetOwnPublished(groupIdHex: segment)
                await pushRouting.forgetGroup(accountIdB64: accountIdB64, groupIdHex: segment)
            }
            guard !gone() else { return }
            end.stopped = true
            saveOwed(end, accountIdB64: accountIdB64)
        }
        let conversationHex = plan?.conversationHex ?? groupIdHex
        let refs = Set(segments + [conversationHex]).sorted().compactMap { hex in
            Self.hexToBytes(hex).map { FolderConversationRef(kind: end.referenceKind, id: $0) }
        }
        guard await sync.clearConversation(refs), !gone() else { return }
        do {
            try await withCommitLock(groupIdHex: groupIdHex) { [self] in
                guard owedEnds[groupIdHex] != nil else { return }
                try finishEnd(end, segments: segments, conversationHex: conversationHex)
            }
        } catch {
            NSLog("membership end: the wipe did not finish, kept owed: %@", String(describing: error))
        }
    }

    /// Step 4, synchronous under the group's commit lock.
    private func finishEnd(
        _ end: OwedMembershipEnd, segments: [String], conversationHex: String
    ) throws {
        guard let accountIdB64 = currentAccountIdB64 else { throw StaleBindingError() }
        let head = end.groupIdHex
        for segment in segments where segment != head {
            // An older segment may still be live for the members who have not
            // moved. It is judged at its own epoch: it is this conversation's.
            let epoch = (try? groups[segment]?.group.epoch()) ?? 0
            try removeGroupNow(groupIdHex: segment, mls: .endedElsewhere(epoch: epoch))
        }
        try removeGroupNow(
            groupIdHex: head,
            mls: end.heard ? .endedElsewhere(epoch: end.endedEpoch) : .applied)
        forgetHeld(conversationHex: conversationHex)
        dropOwed(groupIdHex: head, accountIdB64: accountIdB64)
        // ADR-0078 section 9 — this account's `group` and `member` keys were
        // held by the people left there: the registration sees the group gone.
        ReachCenter.shared.sync()
    }

    private func dropOwed(groupIdHex: String, accountIdB64: String) {
        owedEnds[groupIdHex] = nil
        membershipEndStore.removeOwed(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        state = state.with(threads: buildThreadList())
    }

    // MARK: - The register

    /// Rule 5: read after every payload and at launch. Idempotent: a wiped
    /// group has nothing left to act on.
    func applyHeardEnds() {
        guard canonicalIdentity != nil, let sync = conversationEndSync else { return }
        for register in sync.endedConversations {
            if case .channel = register.conversation.kind {
                let channelIdHex = ChatFolders.hex(register.conversation.id)
                if owedEnds[channelIdHex] == nil { onChannelEndedElsewhere?(channelIdHex) }
                continue
            }
            let groupIdHex = DsClient.pseudonymHex(register.conversation.id)
            let record = groups[groupIdHex]
            // Rule 4 — before anything is stopped.
            let verdict = MembershipEndRules.heard(
                registerEpoch: register.epoch,
                held: record != nil,
                alreadyOwed: isEnding(groupIdHex: groupIdHex),
                isActive: record.flatMap { rec in readIsActive { try rec.group.isActive() } },
                epoch: record.flatMap { try? $0.group.epoch() })
            switch verdict {
            case .nothing:
                // Named by a backup or a pairing bundle and not joined: this
                // device is not going to join it.
                if record == nil, let accountIdB64 = currentAccountIdB64 {
                    dropRestoredKnownGroup(groupIdHex: groupIdHex)
                    StateBundle.forgetKnownGroup(groupIdHex: groupIdHex, accountIdB64: accountIdB64)
                }
            case .wipe(let endedEpoch):
                recordEnd(groupIdHex: groupIdHex, epoch: endedEpoch, heard: true)
            case .answerJoined(let epoch):
                let conversation = register.conversation
                Task { @MainActor in
                    await sync.publishConversationJoined(conversation, epoch: epoch)
                }
            }
        }
    }

    /// Rule 3 — a join after a held end tells the other devices; a first join
    /// writes nothing. Not owed: a device that misses this answers the
    /// register itself (rule 4) once it hears it.
    func publishJoinedSoon(groupIdHex: String) {
        guard let sync = conversationEndSync, let record = groups[groupIdHex],
              let epoch = try? record.group.epoch() else { return }
        let conversation = FolderConversationRef(
            kind: isOneToOne(groupIdHex: groupIdHex) ? .oneToOne : .group, id: record.groupId)
        Task { @MainActor in await sync.publishConversationJoined(conversation, epoch: epoch) }
    }

    // MARK: - A channel

    /// UX 9.5 — this device unsubscribed: the row and the posts went with it;
    /// the other devices are told through an owed record, at epoch 0 (a
    /// channel has no epoch, so the stamp orders it).
    public func noteChannelUnsubscribed(channelIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        let end = OwedMembershipEnd(
            groupIdHex: channelIdHex, endedEpoch: 0, heard: false, oneToOne: false, channel: true)
        guard membershipEndStore.record(end, notice: nil, accountIdB64: accountIdB64) else { return }
        owedEnds[channelIdHex] = end
        resumeEnd(groupIdHex: channelIdHex)
    }

    /// A subscribe after a held end clears it on the other devices.
    public func noteChannelSubscribed(channelIdHex: String) {
        guard let sync = conversationEndSync, let id = Self.hexToBytes(channelIdHex) else { return }
        let conversation = FolderConversationRef(kind: .channel, id: id)
        Task { @MainActor in await sync.publishConversationJoined(conversation, epoch: 0) }
    }

    // MARK: - The notice

    func refreshEndNotices() {
        guard let accountIdB64 = currentAccountIdB64 else {
            membershipEndNotices.replace([])
            return
        }
        membershipEndNotices.replace(membershipEndStore.notices(accountIdB64: accountIdB64))
    }

    /// UX 9.1 — the banner's close icon. The name is gone with it.
    public func closeEndNotice(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        membershipEndStore.removeNotice(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        refreshEndNotices()
    }

    /// UX 9.1 — the open conversation shows it as a dialog instead, once.
    func claimEndNotice(groupIdHex: String) -> MembershipEndNotice? {
        guard let accountIdB64 = currentAccountIdB64,
              let notice = membershipEndStore.notices(accountIdB64: accountIdB64)
                .first(where: { $0.groupIdHex == groupIdHex }) else { return nil }
        closeEndNotice(groupIdHex: groupIdHex)
        return notice
    }

    // MARK: - A Welcome for a group still held as active

    /// Keep a refused Welcome. Synchronous and before the mailbox cursor
    /// moves past its row: this is then the only copy. `false` when it could
    /// not be kept.
    @discardableResult
    func holdWelcomeForActiveGroup(groupIdHex: String, welcome: Data) -> Bool {
        guard let accountIdB64 = currentAccountIdB64 else { return false }
        return membershipEndStore.hold(
            HeldWelcome(groupIdHex: groupIdHex, welcome: welcome), accountIdB64: accountIdB64)
    }

    /// Go through the held Welcomes: read the group's journal, the removal
    /// that explains the refusal is usually in it, then try again. A journal
    /// that is drained with the group still active means a duplicate or a
    /// forgery, and the Welcome is dropped. A refused read keeps it.
    func resolveHeldWelcomes() async {
        guard !heldWelcomesRunning else {
            heldWelcomesRerun = true
            return
        }
        guard let accountIdB64 = currentAccountIdB64 else { return }
        heldWelcomesRunning = true
        let bound = binding.capture()
        repeat {
            heldWelcomesRerun = false
            for held in membershipEndStore.heldWelcomes(accountIdB64: accountIdB64) {
                await resolveHeld(held, accountIdB64: accountIdB64, bound: bound)
                guard bound.isCurrent else { return }
            }
        } while heldWelcomesRerun
        heldWelcomesRunning = false
    }

    private func resolveHeld(
        _ held: HeldWelcome, accountIdB64: String, bound: BindingEpoch.Captured
    ) async {
        var drained = false
        // Two rounds at most: read the journal once, judge again.
        for _ in 0..<2 {
            let record = groups[held.groupIdHex]
            let step = MembershipEndRules.heldWelcomeStep(
                held: record != nil,
                owed: isEnding(groupIdHex: held.groupIdHex),
                isActive: record.flatMap { rec in readIsActive { try rec.group.isActive() } },
                drained: drained)
            switch step {
            case .waitForWipe:
                // ``resumeEnd(groupIdHex:)`` comes back here when it is done.
                resumeEnd(groupIdHex: held.groupIdHex)
                return
            case .drop:
                NSLog("membership end: a held Welcome for a live group was dropped")
                membershipEndStore.removeHeld(held, accountIdB64: accountIdB64)
                return
            case .join:
                do {
                    let joined = try await joinGroupFromWelcome(welcomeBytes: held.welcome)
                    guard bound.isCurrent else { return }
                    membershipEndStore.removeHeld(held, accountIdB64: accountIdB64)
                    onHeldWelcomeJoined?(joined)
                } catch {
                    guard bound.isCurrent else { return }
                    // Still active means it stays held for the next pass.
                    if MembershipEndRules.stillActiveGroupIdHex(error) == nil {
                        NSLog("membership end: a held Welcome did not open: %@", String(describing: error))
                        membershipEndStore.removeHeld(held, accountIdB64: accountIdB64)
                    }
                }
                return
            case .readJournal:
                guard let done = await drainJournal(groupIdHex: held.groupIdHex),
                      bound.isCurrent else { return }
                drained = done
            }
        }
    }

    /// Read a journal to its head. `true` drained, `false` not yet, `nil` the
    /// read was refused: that decides nothing.
    private func drainJournal(groupIdHex: String) async -> Bool? {
        for _ in 0..<JournalRepair.maxPolls {
            guard let before = groups[groupIdHex]?.cursor else { return true }
            do {
                try await pollGroup(groupIdHex: groupIdHex, waitSecs: 0)
            } catch {
                return nil
            }
            guard let after = groups[groupIdHex]?.cursor, after != before else { return true }
        }
        return false
    }

    /// A Welcome replaced what this device held under `groupIdHex`. A person
    /// who was out comes back to a new conversation: nothing carries, the old
    /// end and its notice go. A device whose own leaf was retired while its
    /// account stayed keeps its history and its place in the journal.
    func closeReplacedGroup(groupIdHex: String, joined: GroupRecord) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        let old = groups[groupIdHex]
        let owed = owedEnds[groupIdHex] != nil
        if let old, !owed, !personIsOut(of: old) {
            joined.adoptRetiredRecord(old)
            return
        }
        // The person is back: "no longer in" would be false now, whether or
        // not the old group was still here.
        closeEndNotice(groupIdHex: groupIdHex)
        guard old != nil || owed else { return }
        wipeGroupStores(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        chatClient.forgetOwnPublished(groupIdHex: groupIdHex)
        if let secret = pushRouting.detachGroup(accountIdB64: accountIdB64, groupIdHex: groupIdHex) {
            let routing = pushRouting
            Task { @MainActor in
                await routing.retireOldPushGid(accountIdB64: accountIdB64, oldSecret: secret)
            }
        }
        owedEnds[groupIdHex] = nil
        membershipEndStore.removeOwed(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    // MARK: - Launch

    /// The ends a restart left owed, the stored groups this account is no
    /// longer in, the held Welcomes, and whatever the register says.
    func sweepMembershipEnds() {
        refreshEndNotices()
        for groupIdHex in groups.keys.sorted() { endMembershipIfOut(groupIdHex: groupIdHex) }
        resumeOwedEnds()
        applyHeardEnds()
        Task { @MainActor [weak self] in await self?.resolveHeldWelcomes() }
    }
}
