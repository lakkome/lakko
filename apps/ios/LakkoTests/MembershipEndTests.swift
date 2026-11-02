// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// The account's synced state as the pipeline sees it, with the order of what
/// was asked of it.
@MainActor
private final class FakeEndSync: ConversationEndSync {
    var endedConversations: [ConversationEndRegister] = []
    var ready = true
    var calls: [String] = []
    var ended: [(FolderConversationRef, UInt64)] = []
    var joined: [(FolderConversationRef, UInt64)] = []
    var cleared: [[FolderConversationRef]] = []

    func publishConversationEnded(_ conversation: FolderConversationRef, epoch: UInt64) async -> Bool {
        guard ready else { return false }
        calls.append("ended")
        ended.append((conversation, epoch))
        return true
    }

    func publishConversationJoined(_ conversation: FolderConversationRef, epoch: UInt64) async {
        calls.append("joined")
        joined.append((conversation, epoch))
    }

    func clearConversation(_ conversations: [FolderConversationRef]) async -> Bool {
        guard ready else { return false }
        calls.append("cleared")
        cleared.append(conversations)
        return true
    }
}

/// ADR-0087 section 9, `docs/ux/group-and-channel-privacy.md` section 9.
@MainActor
final class MembershipEndTests: XCTestCase {

    private var accounts: [String] = []

    override func tearDown() {
        for account in accounts {
            RestoredKnownGroupsStore.clear(accountIdB64: account)
            IdentitySnapshotStore().deleteSnapshot(accountIdB64: account)
            MessageLogStore().deleteLog(accountIdB64: account)
        }
        accounts = []
        super.tearDown()
    }

    private func suite() throws -> UserDefaults {
        try XCTUnwrap(UserDefaults(suiteName: "membership-end-\(UUID().uuidString)"))
    }

    private func key(_ byte: UInt8) -> Data { Data(repeating: byte, count: 32) }

    private func member(leaf: UInt32, key leafKey: Data, account: Data = Data()) -> GroupMember {
        GroupMember(leafIndex: leaf, signaturePublicKey: leafKey, accountKey: account)
    }

    private func waitUntil(_ condition: @MainActor () -> Bool) async {
        for _ in 0..<800 {
            if condition() { return }
            try? await Task.sleep(nanoseconds: 5_000_000)
        }
    }

    // MARK: - The trigger

    func testNoLeafOfThisAccountLeftIsTheTrigger() {
        let own = Set([key(1)])
        let others = [member(leaf: 0, key: key(2), account: key(3))]
        XCTAssertTrue(MembershipEndRules.personOut(isActive: false, roster: others, ownKeys: own))
    }

    func testARetiredOwnLeafIsNotTheEndOfThePerson() {
        // This device's leaf is gone; another device of the account is there.
        let roster = [
            member(leaf: 0, key: key(2), account: key(3)),
            member(leaf: 1, key: key(4), account: key(1)),
        ]
        XCTAssertFalse(
            MembershipEndRules.personOut(isActive: false, roster: roster, ownKeys: [key(1)]))
    }

    func testAnUnknownAnswerIsNeverAnEnd() {
        let others = [member(leaf: 0, key: key(2))]
        for (isActive, roster) in [
            (true, others), (nil, others), (false, nil), (false, []),
        ] as [(Bool?, [GroupMember]?)] {
            XCTAssertFalse(
                MembershipEndRules.personOut(isActive: isActive, roster: roster, ownKeys: [key(1)]))
        }
        XCTAssertFalse(MembershipEndRules.personOut(isActive: false, roster: others, ownKeys: []))
    }

    // MARK: - A heard register

    func testAGroupStoredAboveTheEndAnswersJoinedAndStopsNothing() {
        XCTAssertEqual(
            MembershipEndRules.heard(
                registerEpoch: 5, held: true, alreadyOwed: false, isActive: true, epoch: 6),
            .answerJoined(epoch: 6))
    }

    func testAGroupAtOrBelowTheEndGoes() {
        for epoch: UInt64 in [4, 5] {
            XCTAssertEqual(
                MembershipEndRules.heard(
                    registerEpoch: 5, held: true, alreadyOwed: false, isActive: true, epoch: epoch),
                .wipe(endedEpoch: 5))
        }
        XCTAssertEqual(
            MembershipEndRules.heard(
                registerEpoch: 5, held: true, alreadyOwed: false, isActive: false, epoch: 5),
            .wipe(endedEpoch: 5))
    }

    func testARetiredGroupAboveTheEndIsLeftToItsOwnCommit() {
        XCTAssertEqual(
            MembershipEndRules.heard(
                registerEpoch: 5, held: true, alreadyOwed: false, isActive: false, epoch: 6),
            .nothing)
    }

    func testNothingToActOnIsNothing() {
        XCTAssertEqual(
            MembershipEndRules.heard(
                registerEpoch: 5, held: false, alreadyOwed: false, isActive: nil, epoch: nil),
            .nothing, "a wiped group has nothing left to act on")
        XCTAssertEqual(
            MembershipEndRules.heard(
                registerEpoch: 5, held: true, alreadyOwed: true, isActive: true, epoch: 4),
            .nothing)
        XCTAssertEqual(
            MembershipEndRules.heard(
                registerEpoch: 5, held: true, alreadyOwed: false, isActive: nil, epoch: 4),
            .nothing, "a torn read decides nothing")
    }

    // MARK: - A held Welcome

    func testTheHeldWelcomeSteps() {
        typealias R = MembershipEndRules
        XCTAssertEqual(R.heldWelcomeStep(held: false, owed: false, isActive: nil, drained: false), .join)
        XCTAssertEqual(R.heldWelcomeStep(held: true, owed: true, isActive: false, drained: false), .waitForWipe)
        XCTAssertEqual(R.heldWelcomeStep(held: true, owed: false, isActive: false, drained: false), .join)
        XCTAssertEqual(R.heldWelcomeStep(held: true, owed: false, isActive: true, drained: false), .readJournal)
        XCTAssertEqual(R.heldWelcomeStep(held: true, owed: false, isActive: true, drained: true), .drop)
        XCTAssertEqual(
            R.heldWelcomeStep(held: true, owed: false, isActive: nil, drained: true), .readJournal,
            "a torn read is not a drained, active group")
    }

    func testTheRefusalNamesTheGroup() {
        let refusal = FfiError.MembershipGroupStillActive(message: "ab01")
        XCTAssertEqual(MembershipEndRules.stillActiveGroupIdHex(refusal), "ab01")
        XCTAssertEqual(
            MembershipEndRules.stillActiveGroupIdHex(ChatError.wrapped("join", underlying: refusal)),
            "ab01", "seen through the wrapper the join puts around it")
        XCTAssertNil(MembershipEndRules.stillActiveGroupIdHex(ChatError.argument("no")))
    }

    // MARK: - The store

    func testTheStoreKeepsTheEndAndItsNoticeTogether() throws {
        let store = MembershipEndStore(defaults: try suite())
        let end = OwedMembershipEnd(groupIdHex: "ab", endedEpoch: 7, heard: false, oneToOne: false)
        XCTAssertTrue(store.record(
            end, notice: MembershipEndNotice(groupIdHex: "ab", name: "Hikers"), accountIdB64: "a"))
        XCTAssertEqual(store.owed(accountIdB64: "a"), [end])
        XCTAssertEqual(store.notices(accountIdB64: "a").map(\.name), ["Hikers"])
        XCTAssertTrue(store.owed(accountIdB64: "other").isEmpty, "per account")

        var next = end
        next.published = true
        store.put(next, accountIdB64: "a")
        XCTAssertEqual(store.owed(accountIdB64: "a", groupIdHex: "ab")?.published, true)
        store.removeOwed(accountIdB64: "a", groupIdHex: "ab")
        XCTAssertTrue(store.owed(accountIdB64: "a").isEmpty)
        XCTAssertEqual(store.notices(accountIdB64: "a").count, 1, "the notice outlives the end")
        store.removeNotice(accountIdB64: "a", groupIdHex: "ab")
        XCTAssertTrue(store.notices(accountIdB64: "a").isEmpty)
    }

    func testHeldWelcomesAreBoundedAndDeduplicated() throws {
        let store = MembershipEndStore(defaults: try suite())
        let one = HeldWelcome(groupIdHex: "ab", welcome: Data([1]))
        store.hold(one, accountIdB64: "a")
        store.hold(one, accountIdB64: "a")
        XCTAssertEqual(store.heldWelcomes(accountIdB64: "a"), [one])
        for byte in 2...UInt8(MembershipEndStore.maxHeldWelcomes + 2) {
            store.hold(HeldWelcome(groupIdHex: "ab", welcome: Data([byte])), accountIdB64: "a")
        }
        XCTAssertEqual(store.heldWelcomes(accountIdB64: "a").count, MembershipEndStore.maxHeldWelcomes)
        XCTAssertFalse(store.heldWelcomes(accountIdB64: "a").contains(one), "the oldest goes first")
    }

    func testTheStoreIsSealedAtRest() throws {
        let defaults = try suite()
        let store = MembershipEndStore(defaults: defaults)
        store.record(
            OwedMembershipEnd(groupIdHex: "ab", endedEpoch: 7, heard: false, oneToOne: false),
            notice: MembershipEndNotice(groupIdHex: "ab", name: "Hikers"), accountIdB64: "a")
        let raw = try XCTUnwrap(defaults.dictionaryRepresentation().values.compactMap { $0 as? Data }.first)
        XCTAssertNil(raw.range(of: Data("Hikers".utf8)), "a notice names a group")
    }

    // MARK: - The registers

    func testAConversationEndAloneIsAChangeSet() {
        let register = ConversationEndRegister(
            hlc: 1, conversation: FolderConversationRef(kind: .group, id: Data([1])),
            state: .ended, epoch: 3)
        XCTAssertFalse(ChatFolders.isEmpty(FolderChangeSet(
            metas: [], members: [], pins: [], removed: [], order: nil,
            saved: [], conversationEnds: [register])))
    }

    func testTheFoldedRegisterIsReadBack() throws {
        let state = SyncedState()
        let ref = FolderConversationRef(kind: .group, id: Data(repeating: 7, count: 32))
        let register = try XCTUnwrap(
            try state.composeConversationEnded(hlc: 10, conversation: ref, epoch: 4))
        _ = try state.applyPayload(payload: ParsedDeviceGroupPayload(
            hlc: 10, senderLeaf: 0,
            body: .folderChanges(changes: FolderChangeSet(
                metas: [], members: [], pins: [], removed: [], order: nil,
                saved: [], conversationEnds: [register]))))
        XCTAssertEqual(try state.endedConversations().map(\.epoch), [4])
        XCTAssertNil(
            try state.composeConversationJoined(
                hlc: 11, conversation: FolderConversationRef(kind: .group, id: Data([9])), epoch: 1),
            "a first join writes nothing")
        XCTAssertNotNil(try state.composeConversationJoined(hlc: 11, conversation: ref, epoch: 5))
    }

    func testAnEmptyReadCursorIsALegalTombstone() throws {
        // `DeviceGroupSyncRepository.clearConversation` writes this.
        let state = SyncedState()
        let group = Data(repeating: 7, count: 32)
        _ = try state.applyPayload(payload: ParsedDeviceGroupPayload(
            hlc: 10, senderLeaf: 0,
            body: .readCursor(groupId: group, upToMessageId: Data([0, 0, 0, 0, 0, 0, 0, 9]), readAtMs: 5)))
        _ = try state.applyPayload(payload: ParsedDeviceGroupPayload(
            hlc: 11, senderLeaf: 0,
            body: .readCursor(groupId: group, upToMessageId: Data(), readAtMs: 0)))
        XCTAssertEqual(try state.readCursor(groupId: group)?.upToMessageId, Data())
        XCTAssertNil(LineageRules.decodeReadPosition(Data()), "and reads as no position")
    }

    func testAConversationLeavesEveryFolderByHandAndItsPins() {
        let gone = FolderConversationRef(kind: .group, id: Data([1]))
        let kept = FolderConversationRef(kind: .group, id: Data([2]))
        let meta = FolderMetaRegister(
            hlc: 1, folderId: Data([9]), name: "F", includeKinds: 0, excludeFlags: 0)
        let folder = FolderView(
            folderId: Data([9]), meta: meta, included: [gone, kept], excluded: [gone], pinned: [gone, kept])
        let changes = DeviceGroupSyncRepository.conversationClearChanges(
            folders: [folder], conversations: [gone.id], hlc: 5)
        XCTAssertEqual(changes.members.map(\.state), [.none, .none])
        XCTAssertEqual(changes.members.map(\.conversation), [gone, gone])
        XCTAssertEqual(changes.pins.map(\.pinned), [[kept]])
    }

    func testDeliveredNotificationsAreChosenByThread() {
        XCTAssertEqual(
            QuietNotifications.identifiers(
                [(identifier: "a", thread: "t1"), (identifier: "b", thread: "t2")],
                inThreads: ["t2"]),
            ["b"])
    }

    // MARK: - The pipeline, with real groups

    private struct Fixture {
        let repo: ChatRepository
        let sync: FakeEndSync
        let groupIdHex: String
        let groupId: Data
        let accountIdB64: String
        let identity: Identity
        let owner: Identity
        let ownerGroup: Group
        let knownGroups: KnownGroupsStore
        let endStore: MembershipEndStore
        let scope: String
    }

    /// Two groups' worth of real MLS: `owner` founds, `person` joins. `make`
    /// runs before the repository starts, on the person's handle.
    private func fixture(
        seed: UInt8 = 0x55,
        deviceIdentity: Identity? = nil,
        addAlso: Identity? = nil,
        endStore: MembershipEndStore? = nil,
        arrivals: GroupArrivalStore? = nil,
        before: (_ ownerGroup: Group, _ owner: Identity, _ personGroup: Group, _ person: Identity) throws -> Void = { _, _, _, _ in }
    ) throws -> Fixture {
        let owner = try Identity.fromSeed(seed: Data(repeating: 0x11, count: 32))
        let accountSeed = Data(repeating: seed, count: 32)
        let person = try deviceIdentity ?? Identity.fromSeed(seed: accountSeed)
        let ownerGroup = try Group.create(identity: owner, kind: .group)
        let added = try ownerGroup.addMemberFromWire(
            identity: owner, keyPackage: try person.createKeyPackage())
        let personGroup = try Group.joinFromWelcome(welcome: added.welcome, identity: person)
        if let addAlso {
            let more = try ownerGroup.addMemberFromWire(
                identity: owner, keyPackage: try addAlso.createKeyPackage())
            _ = try personGroup.processIncomingVerifyingRoles(identity: person, bytes: more.commit)
        }
        try before(ownerGroup, owner, personGroup, person)
        let groupId = try personGroup.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)

        let accountIdBytes = Data((0..<16).map { _ in UInt8.random(in: 0...255) })
        let accountIdB64 = base64Encode(accountIdBytes)
        accounts.append(accountIdB64)
        let snapshotStore = IdentitySnapshotStore()
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: try person.snapshotStorage())
        SnapshotLeafKeyStore().save(
            accountIdB64: accountIdB64, leafKeyB64: base64Encode(person.signaturePublicKey()))
        let knownGroups = KnownGroupsStore(defaults: try suite())
        knownGroups.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)

        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [ScriptedDsURLProtocol.self]
        let scope = ScriptedDsURLProtocol.freshScope()
        let ds = DsClient(baseURL: ScriptedDsURLProtocol.baseURL(scope), session: URLSession(configuration: config))
        let end = try endStore ?? MembershipEndStore(defaults: suite())
        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: Offline.discovery(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroups,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: FileManager.default.temporaryDirectory
                .appendingPathComponent("membership-end-\(UUID().uuidString)")),
            pushRouting: Offline.pushRouting(),
            beaconStore: BeaconObservationStore(defaults: try suite()),
            pendingRequestStore: PendingRequestStore(defaults: try suite()),
            lastObservedOwnerStore: LastObservedOwnerStore(defaults: try suite()),
            leaveRequestStore: LeaveRequestStore(defaults: try suite()),
            membershipEndStore: end,
            groupNameStore: GroupNameStore(defaults: try suite()),
            sharedPinStore: SharedPinStore(defaults: try suite()),
            personalPinStore: PersonalPinStore(defaults: try suite()),
            readWatermarkStore: ReadWatermarkStore(defaults: try suite()),
            groupCallStore: GroupCallStore(defaults: try suite()),
            registrarSeeds: RegistrarSeedStore(defaults: try suite()),
            groupArrivalStore: try arrivals ?? GroupArrivalStore(defaults: suite()))
        repo.restoredGroupsDefaults = try suite()
        repo.deviceIdentityInstaller = { _, _ in deviceIdentity }
        repo.membershipEndMayRun = { true }
        let sync = FakeEndSync()
        repo.conversationEndSync = sync
        repo.bootstrap(secrets: IdentitySecrets(
            accountId: accountIdBytes,
            exportKey: Data(repeating: 0x22, count: 32),
            accountIdentitySeed: accountSeed,
            sessionToken: "",
            sessionExpiresAtMs: 0))
        return Fixture(
            repo: repo, sync: sync, groupIdHex: groupIdHex, groupId: groupId,
            accountIdB64: accountIdB64, identity: try XCTUnwrap(repo.canonicalIdentity),
            owner: owner, ownerGroup: ownerGroup, knownGroups: knownGroups,
            endStore: end, scope: scope)
    }

    /// The owner removes `leaf`: the proposal and the commit the person's
    /// handle must apply.
    private func removal(
        ownerGroup: Group, owner: Identity, key leafKey: Data
    ) throws -> (proposal: Data, commit: Data) {
        let leaf = try XCTUnwrap(
            try ownerGroup.members().first { $0.signaturePublicKey == leafKey }?.leafIndex)
        let proposal = try ownerGroup.proposeRemoveMember(identity: owner, leafIndex: leaf)
        let swept = try XCTUnwrap(try ownerGroup.commitPendingProposals(identity: owner))
        return (proposal, swept.commit)
    }

    /// A person already out when the repository starts: the launch sweep.
    private func evicted(endStore: MembershipEndStore? = nil, ready: Bool = true) throws -> Fixture {
        try fixture(endStore: endStore) { ownerGroup, owner, personGroup, person in
            let steps = try self.removal(
                ownerGroup: ownerGroup, owner: owner, key: person.signaturePublicKey())
            _ = try personGroup.processIncomingVerifyingRoles(identity: person, bytes: steps.proposal)
            _ = try personGroup.processIncomingVerifyingRoles(identity: person, bytes: steps.commit)
        }
    }

    func testARemovedPersonLosesTheGroupFromEveryStore() async throws {
        let f = try fixture()
        let repo = f.repo
        let hex = f.groupIdHex
        let b64 = base64Encode(f.groupId)
        let account = f.accountIdB64
        // Something in every per-group store.
        let record = try XCTUnwrap(repo.groups[hex])
        record.appendLocalRow(text: "hello", messageId: 1, createdAt: "")
        repo.persistMessageLogBestEffort()
        XCTAssertNotNil(repo.messageLogStore.getLog(accountIdB64: account)?.groups[hex])
        repo.groupNameStore.record(
            accountIdB64: account, groupIdHex: hex, value: StoredGroupName(name: "Hikers", updatedAtMs: 1))
        repo.sharedPinStore.record(
            accountIdB64: account, groupIdHex: hex, messageId: 1,
            value: StoredSharedPin(pinned: true, updatedAtMs: 1))
        repo.personalPinStore.set(accountIdB64: account, groupIdHex: hex, messageId: 1, pinnedAtMs: 1)
        _ = repo.readWatermarkStore.merge(accountIdB64: account, groupIdHex: hex, sender: "k", upTo: 1)
        repo.registrarSeeds.put(accountIdB64: account, groupIdHex: hex, seed: key(7), rank: .member)
        repo.registrarSeeds.markRootHeld(accountIdB64: account, groupIdHex: hex)
        repo.registrarSeeds.markHandoverOwed(accountIdB64: account, groupIdHex: hex)
        repo.pushRouting.persistSecret(
            accountIdB64: account, groupIdHex: hex, secret: key(8), distributed: true, mintEpoch: 1)
        DeviceReadmissionMemory.recordRemoved(
            accountIdB64: account, groupIdHex: hex, before: ["a"], after: [])
        try repo.commitOutbox.putNewDurable(
            accountIdB64: account,
            entry: NewCommitEntry(id: "e1", groupId: b64, targetEpoch: "9", commitB64: "AA=="))

        let steps = try removal(
            ownerGroup: f.ownerGroup, owner: f.owner, key: f.identity.signaturePublicKey())
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.proposal)
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.commit)
        let frozenEpoch = try record.group.epoch()
        repo.endMembershipIfOut(groupIdHex: hex)
        XCTAssertTrue(repo.isEnding(groupIdHex: hex))
        XCTAssertTrue(repo.buildThreadList().isEmpty, "the row leaves at once")

        await waitUntil { repo.groups[hex] == nil }
        XCTAssertNil(repo.groups[hex])
        XCTAssertFalse(repo.isEnding(groupIdHex: hex), "the record goes last")
        XCTAssertFalse(f.knownGroups.load(accountIdB64: account).contains(hex))

        // Steps 2 to 4, in order.
        XCTAssertEqual(f.sync.calls, ["ended", "cleared"])
        XCTAssertEqual(f.sync.ended.first?.0, FolderConversationRef(kind: .group, id: f.groupId))
        XCTAssertEqual(f.sync.ended.first?.1, frozenEpoch, "the frozen epoch")
        XCTAssertEqual(f.sync.cleared.first?.map(\.id), [f.groupId])

        XCTAssertNil(repo.messageLogStore.getLog(accountIdB64: account)?.groups[hex])
        XCTAssertNil(repo.groupNameStore.load(accountIdB64: account, groupIdHex: hex))
        XCTAssertTrue(repo.sharedPinStore.load(accountIdB64: account, groupIdHex: hex).isEmpty)
        XCTAssertTrue(repo.personalPinStore.load(accountIdB64: account, groupIdHex: hex).isEmpty)
        XCTAssertTrue(repo.readWatermarkStore.load(accountIdB64: account, groupIdHex: hex).isEmpty)
        XCTAssertNil(repo.registrarSeeds.get(accountIdB64: account, groupIdHex: hex))
        XCTAssertFalse(repo.registrarSeeds.isRootHeld(accountIdB64: account, groupIdHex: hex))
        XCTAssertFalse(repo.registrarSeeds.owedHandovers(accountIdB64: account).contains(hex))
        XCTAssertNil(repo.pushRouting.secret(accountIdB64: account, groupIdHex: hex))
        XCTAssertTrue(DeviceReadmissionMemory.removed(accountIdB64: account, groupIdHex: hex).isEmpty)
        XCTAssertTrue(
            repo.commitOutbox.loadUnacked(accountIdB64: account, groupIdB64: b64).isEmpty)

        // The MLS group is gone from the durable snapshot too.
        let restarted = try Identity.fromSeed(seed: Data(repeating: 0x55, count: 32))
        try restarted.restoreStorage(
            snapshot: try XCTUnwrap(IdentitySnapshotStore().getSnapshot(accountIdB64: account)))
        XCTAssertNil(try loadGroup(identity: restarted, groupId: f.groupId))

        // Told once, by name.
        XCTAssertEqual(repo.membershipEndNotices.notices.map(\.name), ["Hikers"])
    }

    func testAPersonWhoLeftSeesNoNotice() async throws {
        let f = try fixture()
        f.repo.noteLeaveRequested(groupIdHex: f.groupIdHex)
        XCTAssertEqual(f.repo.state.threads.first?.leaveRequested, true, "the row says it waits")
        XCTAssertEqual(f.repo.leaveRequestedAt(groupIdHex: f.groupIdHex) != nil, true)
        let record = try XCTUnwrap(f.repo.groups[f.groupIdHex])
        let steps = try removal(
            ownerGroup: f.ownerGroup, owner: f.owner, key: f.identity.signaturePublicKey())
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.proposal)
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.commit)
        f.repo.endMembershipIfOut(groupIdHex: f.groupIdHex)
        await waitUntil { f.repo.groups[f.groupIdHex] == nil }
        XCTAssertNil(f.repo.groups[f.groupIdHex])
        XCTAssertTrue(f.repo.membershipEndNotices.notices.isEmpty)
    }

    func testARetiredOwnLeafKeepsItsHistory() async throws {
        let accountSeed = Data(repeating: 0x66, count: 32)
        let thisDevice = try Identity.fromSeedBound(
            seed: Data(repeating: 0x67, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1)
        let otherDevice = try Identity.fromSeedBound(
            seed: Data(repeating: 0x68, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1)
        let f = try fixture(seed: 0x66, deviceIdentity: thisDevice, addAlso: otherDevice)
        let record = try XCTUnwrap(f.repo.groups[f.groupIdHex])
        record.appendLocalRow(text: "kept", messageId: 1, createdAt: "")
        let steps = try removal(
            ownerGroup: f.ownerGroup, owner: f.owner, key: thisDevice.signaturePublicKey())
        _ = try record.group.processIncomingVerifyingRoles(identity: thisDevice, bytes: steps.proposal)
        _ = try record.group.processIncomingVerifyingRoles(identity: thisDevice, bytes: steps.commit)
        XCTAssertFalse(try record.group.isActive(), "this device's leaf is out")

        f.repo.endMembershipIfOut(groupIdHex: f.groupIdHex)
        XCTAssertFalse(f.repo.isEnding(groupIdHex: f.groupIdHex))
        XCTAssertNotNil(f.repo.groups[f.groupIdHex])
        XCTAssertEqual(f.repo.buildThreadList().first?.membershipEnded, true, "the row stays, ended")
        XCTAssertTrue(f.repo.membershipEndNotices.notices.isEmpty)

        // Re-admitted by a Welcome: the old transcript carries.
        let again = try f.ownerGroup.addMemberFromWire(
            identity: f.owner, keyPackage: try thisDevice.createKeyPackage())
        let joined = try await f.repo.joinGroupFromWelcome(welcomeBytes: again.welcome)
        XCTAssertEqual(joined, f.groupIdHex)
        XCTAssertEqual(f.repo.groups[joined]?.messages.map(\.text), ["kept"])
        XCTAssertTrue(try XCTUnwrap(f.repo.groups[joined]).group.isActive())
    }

    func testTheLaunchSweepFindsAStoredGroupThePersonIsOutOf() async throws {
        let f = try evicted()
        await waitUntil { f.repo.groups[f.groupIdHex] == nil }
        XCTAssertNil(f.repo.groups[f.groupIdHex])
        XCTAssertEqual(f.sync.calls, ["ended", "cleared"])
        XCTAssertEqual(f.repo.membershipEndNotices.notices.count, 1, "a closed app shows it at the next start")
    }

    func testAnOwedEndResumesAfterACrash() async throws {
        let store = MembershipEndStore(defaults: try suite())
        let f = try fixture(endStore: store)
        f.sync.ready = false
        let record = try XCTUnwrap(f.repo.groups[f.groupIdHex])
        let steps = try removal(
            ownerGroup: f.ownerGroup, owner: f.owner, key: f.identity.signaturePublicKey())
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.proposal)
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.commit)
        f.repo.persistSnapshotBestEffort()
        f.repo.endMembershipIfOut(groupIdHex: f.groupIdHex)
        await waitUntil { !f.repo.endsRunning.contains(f.groupIdHex) }

        // Step 2 could not run: the group is held, the row is gone, and the
        // record is what a restart finds.
        XCTAssertNotNil(f.repo.groups[f.groupIdHex])
        XCTAssertTrue(f.repo.buildThreadList().isEmpty)
        XCTAssertEqual(store.owed(accountIdB64: f.accountIdB64).map(\.groupIdHex), [f.groupIdHex])

        // A restart of the same repository: everything in memory is gone.
        let accountId = try XCTUnwrap(f.repo.currentAccountId)
        f.repo.clear()
        f.sync.ready = true
        f.repo.bootstrap(secrets: IdentitySecrets(
            accountId: accountId,
            exportKey: Data(repeating: 0x22, count: 32),
            accountIdentitySeed: Data(repeating: 0x55, count: 32),
            sessionToken: "",
            sessionExpiresAtMs: 0))
        XCTAssertTrue(f.repo.isEnding(groupIdHex: f.groupIdHex), "read back before anything runs")
        await waitUntil { f.repo.groups[f.groupIdHex] == nil }
        XCTAssertNil(f.repo.groups[f.groupIdHex])
        XCTAssertTrue(store.owed(accountIdB64: f.accountIdB64).isEmpty)
        XCTAssertEqual(f.sync.calls, ["ended", "cleared"], "each step once")
        XCTAssertEqual(f.repo.membershipEndNotices.notices.count, 1)
    }

    func testAResumedEndDoesNotPublishTwice() async throws {
        let store = MembershipEndStore(defaults: try suite())
        let f = try evicted(endStore: store)
        await waitUntil { f.repo.groups[f.groupIdHex] == nil && f.repo.endsRunning.isEmpty }
        var end = OwedMembershipEnd(
            groupIdHex: f.groupIdHex, endedEpoch: 2, heard: false, oneToOne: false)
        end.published = true
        end.stopped = true
        store.put(end, accountIdB64: f.accountIdB64)
        f.repo.owedEnds[f.groupIdHex] = end
        f.repo.resumeOwedEnds()
        await waitUntil { f.repo.owedEnds.isEmpty }
        XCTAssertEqual(f.sync.ended.count, 1)
        XCTAssertTrue(store.owed(accountIdB64: f.accountIdB64).isEmpty, "a group already gone still closes")
    }

    // MARK: - The register, heard

    private func register(_ f: Fixture, epoch: UInt64) -> ConversationEndRegister {
        ConversationEndRegister(
            hlc: 1, conversation: FolderConversationRef(kind: .group, id: f.groupId),
            state: .ended, epoch: epoch)
    }

    func testAHeardEndWipesAGroupThisDeviceHoldsActive() async throws {
        let f = try fixture()
        let record = try XCTUnwrap(f.repo.groups[f.groupIdHex])
        XCTAssertTrue(try record.group.isActive())
        f.sync.endedConversations = [register(f, epoch: try record.group.epoch() + 1)]
        f.repo.applyHeardEnds()
        XCTAssertTrue(f.repo.isEnding(groupIdHex: f.groupIdHex), "before the commit was applied here")
        await waitUntil { f.repo.groups[f.groupIdHex] == nil }
        XCTAssertNil(f.repo.groups[f.groupIdHex])
        XCTAssertTrue(f.sync.ended.isEmpty, "a device that heard has nothing to tell")
        XCTAssertEqual(f.sync.calls, ["cleared"])
        XCTAssertEqual(f.repo.membershipEndNotices.notices.count, 1)

        let restarted = try Identity.fromSeed(seed: Data(repeating: 0x55, count: 32))
        try restarted.restoreStorage(
            snapshot: try XCTUnwrap(IdentitySnapshotStore().getSnapshot(accountIdB64: f.accountIdB64)))
        XCTAssertNil(try loadGroup(identity: restarted, groupId: f.groupId))

        // Idempotent: a wiped group has nothing left to act on.
        f.repo.applyHeardEnds()
        XCTAssertFalse(f.repo.isEnding(groupIdHex: f.groupIdHex))
    }

    func testABelowEpochRegisterStopsNothingAndIsAnsweredWithJoined() async throws {
        let f = try fixture()
        let record = try XCTUnwrap(f.repo.groups[f.groupIdHex])
        let epoch = try record.group.epoch()
        XCTAssertGreaterThan(epoch, 0)
        f.sync.endedConversations = [register(f, epoch: epoch - 1)]
        f.repo.applyHeardEnds()
        await waitUntil { !f.sync.joined.isEmpty }
        XCTAssertFalse(f.repo.isEnding(groupIdHex: f.groupIdHex), "the check comes before any stopping")
        XCTAssertNotNil(f.repo.groups[f.groupIdHex])
        XCTAssertEqual(f.sync.joined.first?.1, epoch)
        XCTAssertTrue(f.repo.membershipEndNotices.notices.isEmpty)
    }

    func testAChannelEndGoesToTheChannelLayer() throws {
        let f = try fixture()
        var told: [String] = []
        f.repo.onChannelEndedElsewhere = { told.append($0) }
        f.sync.endedConversations = [ConversationEndRegister(
            hlc: 1, conversation: FolderConversationRef(kind: .channel, id: Data([0xAB, 0xCD])),
            state: .ended, epoch: 0)]
        f.repo.applyHeardEnds()
        XCTAssertEqual(told, ["abcd"])
    }

    func testAChannelUnsubscribeIsToldToTheOtherDevicesFromTheRecord() async throws {
        let f = try fixture()
        f.repo.noteChannelUnsubscribed(channelIdHex: "abcd")
        await waitUntil { f.repo.owedEnds.isEmpty }
        XCTAssertEqual(
            f.sync.ended.first?.0, FolderConversationRef(kind: .channel, id: Data([0xAB, 0xCD])))
        XCTAssertEqual(f.sync.ended.first?.1, 0, "a channel has no epoch")
        XCTAssertEqual(f.sync.cleared.count, 1)
        XCTAssertNotNil(f.repo.groups[f.groupIdHex], "and no group is touched")
    }

    // MARK: - A Welcome for a group still held as active

    func testAWelcomeWhileTheRemovalIsStillInTheJournalIsHeldAndLands() async throws {
        let f = try fixture()
        let hex = f.groupIdHex
        let record = try XCTUnwrap(f.repo.groups[hex])
        record.appendLocalRow(text: "before", messageId: 1, createdAt: "")
        let steps = try removal(
            ownerGroup: f.ownerGroup, owner: f.owner, key: f.identity.signaturePublicKey())
        let again = try f.ownerGroup.addMemberFromWire(
            identity: f.owner, keyPackage: try f.identity.createKeyPackage())

        // The Welcome arrives first: refused, and named.
        var refusal: Error?
        do {
            _ = try await f.repo.joinGroupFromWelcome(welcomeBytes: again.welcome)
        } catch {
            refusal = error
        }
        let named = try MembershipEndRules.stillActiveGroupIdHex(XCTUnwrap(refusal))
        XCTAssertEqual(named, hex)
        XCTAssertTrue(f.repo.holdWelcomeForActiveGroup(groupIdHex: hex, welcome: again.welcome))
        XCTAssertEqual(f.endStore.heldWelcomes(accountIdB64: f.accountIdB64).count, 1)

        // The journal holds the removal: reading it ends the group, the wipe
        // runs, and the Welcome lands on empty stores.
        ScriptedDsURLProtocol.seed(steps.proposal, scope: f.scope)
        ScriptedDsURLProtocol.seed(steps.commit, scope: f.scope)
        await f.repo.resolveHeldWelcomes()
        await waitUntil {
            f.endStore.heldWelcomes(accountIdB64: f.accountIdB64).isEmpty
                && (try? f.repo.groups[hex]?.group.isActive()) == true
        }
        let rejoined = try XCTUnwrap(f.repo.groups[hex])
        XCTAssertTrue(try rejoined.group.isActive())
        XCTAssertTrue(rejoined.messages.isEmpty, "nothing from before returns")
        XCTAssertEqual(f.sync.calls.first, "ended")
        await waitUntil { !f.sync.joined.isEmpty }
        XCTAssertEqual(f.sync.joined.first?.1, try rejoined.group.epoch())
        XCTAssertTrue(f.repo.membershipEndNotices.notices.isEmpty, "they are back")
    }

    func testAHeldWelcomeForAGroupStillActiveAtTheHeadIsDropped() async throws {
        let f = try fixture()
        let again = try f.ownerGroup.addMemberFromWire(
            identity: f.owner, keyPackage: try Identity.fromSeed(
                seed: Data(repeating: 0x77, count: 32)).createKeyPackage())
        XCTAssertTrue(f.repo.holdWelcomeForActiveGroup(groupIdHex: f.groupIdHex, welcome: again.welcome))
        await f.repo.resolveHeldWelcomes()
        XCTAssertTrue(f.endStore.heldWelcomes(accountIdB64: f.accountIdB64).isEmpty)
        XCTAssertTrue(try XCTUnwrap(f.repo.groups[f.groupIdHex]).group.isActive())
    }

    // MARK: - The handle

    func testAWelcomeReplacesTheEndedHandle() async throws {
        let f = try fixture()
        let hex = f.groupIdHex
        f.sync.ready = false
        let old = try XCTUnwrap(f.repo.groups[hex])
        old.appendLocalRow(text: "before", messageId: 1, createdAt: "")
        let steps = try removal(
            ownerGroup: f.ownerGroup, owner: f.owner, key: f.identity.signaturePublicKey())
        _ = try old.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.proposal)
        _ = try old.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.commit)
        f.repo.endMembershipIfOut(groupIdHex: hex)
        await waitUntil { !f.repo.endsRunning.contains(hex) }
        XCTAssertTrue(f.repo.isEnding(groupIdHex: hex))

        let again = try f.ownerGroup.addMemberFromWire(
            identity: f.owner, keyPackage: try f.identity.createKeyPackage())
        _ = try await f.repo.joinGroupFromWelcome(welcomeBytes: again.welcome)
        let live = try XCTUnwrap(f.repo.groups[hex])
        XCTAssertFalse(live === old, "the old handle is gone")
        XCTAssertTrue(try live.group.isActive())
        XCTAssertTrue(live.messages.isEmpty)
        XCTAssertFalse(f.repo.isEnding(groupIdHex: hex), "the owed end went with it")
        XCTAssertEqual(f.repo.buildThreadList().map(\.groupIdHex), [hex], "an ordinary new row")
        XCTAssertNil(f.repo.messageLogStore.getLog(accountIdB64: f.accountIdB64)?.groups[hex]?.messages.first)
    }

    // MARK: - The open conversation

    func testTheOpenConversationIsToldByDialogWithItsDraftAndTheBannerIsNot() async throws {
        let f = try fixture()
        let model = ChatViewModel(repository: f.repo, groupIdHex: f.groupIdHex)
        model.screenVisible = true
        model.draft = "see you tomorrow"
        let record = try XCTUnwrap(f.repo.groups[f.groupIdHex])
        let steps = try removal(
            ownerGroup: f.ownerGroup, owner: f.owner, key: f.identity.signaturePublicKey())
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.proposal)
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.commit)
        f.repo.endMembershipIfOut(groupIdHex: f.groupIdHex)
        await waitUntil { model.endDialog != nil }
        XCTAssertEqual(model.endDialog?.draft, "see you tomorrow")
        XCTAssertEqual(model.draft, "", "it has nowhere else to go")
        XCTAssertTrue(f.repo.membershipEndNotices.notices.isEmpty, "shown once: not the banner too")
        XCTAssertFalse(model.conversationGone)
        model.endDialogClosed()
        XCTAssertNil(model.endDialog)
        XCTAssertTrue(model.conversationGone)
    }

    /// UX 9.1 — behind the dialog the conversation is still the one that ended:
    /// the wipe takes its name and its handle, and a group this device does not
    /// hold reads as one being set up, with a working composer.
    func testBehindTheDialogTheConversationKeepsItsNameAndIsNotBeingSetUp() async throws {
        let f = try fixture()
        let hex = f.groupIdHex
        f.repo.groupNameStore.record(
            accountIdB64: f.accountIdB64, groupIdHex: hex,
            value: StoredGroupName(name: "Hikers", updatedAtMs: 1))
        let model = ChatViewModel(repository: f.repo, groupIdHex: hex)
        model.screenVisible = true
        let record = try XCTUnwrap(f.repo.groups[hex])
        let steps = try removal(
            ownerGroup: f.ownerGroup, owner: f.owner, key: f.identity.signaturePublicKey())
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.proposal)
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.commit)
        f.repo.endMembershipIfOut(groupIdHex: hex)
        await waitUntil { f.repo.groups[hex] == nil && !f.repo.isEnding(groupIdHex: hex) }
        await waitUntil { model.endDialog != nil }
        // The repository has published the end of the wipe by now; give the
        // sink its turn so this reads the state the screen draws.
        try? await Task.sleep(nanoseconds: 50_000_000)

        XCTAssertNil(f.repo.groups[hex], "the wipe finished under the dialog")
        XCTAssertEqual(model.conversationTitle, "Hikers")
        XCTAssertNil(model.conversationSubtitle, "no \"1 member\" for a group that is gone")
        XCTAssertTrue(model.membershipHasEnded, "no handle is not a live membership")
        XCTAssertFalse(model.pollLoopShouldContinue)
        XCTAssertFalse(model.messageActionsAvailable)
        XCTAssertEqual(model.chatCallAction, .unavailable)
        if case .composer = model.composerSlot { XCTFail("the composer is withdrawn") }
        XCTAssertNil(
            model.terminalRowCopy, "the terminal row says the messages are still here")
    }

    /// The group's own screen: the dialog stands over it, and its roster stays.
    func testBehindTheDialogTheInfoScreenKeepsItsRoster() async throws {
        let f = try fixture()
        let hex = f.groupIdHex
        let model = ChatViewModel(repository: f.repo, groupIdHex: hex)
        model.groupInfoAppeared()
        let before = model.members.count
        XCTAssertGreaterThan(before, 0)
        let record = try XCTUnwrap(f.repo.groups[hex])
        let steps = try removal(
            ownerGroup: f.ownerGroup, owner: f.owner, key: f.identity.signaturePublicKey())
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.proposal)
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.commit)
        f.repo.endMembershipIfOut(groupIdHex: hex)
        await waitUntil { model.endDialog != nil && f.repo.groups[hex] == nil }
        try? await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertFalse(model.members.isEmpty, "not \"0 members\"")
        XCTAssertFalse(model.canManageMembers)
        XCTAssertEqual(model.exitBlockState, .hidden, "no Delete this chat for a group that is gone")
        XCTAssertNotNil(model.groupName, "the name field keeps the name")
        XCTAssertTrue(model.endedUnderScreen)
    }

    // MARK: - The arrival (section 10)

    func testAJoinIsStampedARestartKeepsItARejoinMovesItAndTheEndDropsIt() async throws {
        let arrivals = GroupArrivalStore(defaults: try suite())
        let f = try fixture(arrivals: arrivals)
        let hex = f.groupIdHex
        XCTAssertNil(
            f.repo.buildThreadList().first?.arrivedAtMs,
            "a group reloaded at launch is not a new arrival")

        arrivals.remember(accountIdB64: f.accountIdB64, groupIdHex: hex, atMs: 1)
        XCTAssertEqual(f.repo.buildThreadList().first?.arrivedAtMs, 1)

        let record = try XCTUnwrap(f.repo.groups[hex])
        let steps = try removal(
            ownerGroup: f.ownerGroup, owner: f.owner, key: f.identity.signaturePublicKey())
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.proposal)
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.commit)
        f.repo.endMembershipIfOut(groupIdHex: hex)
        await waitUntil { f.repo.groups[hex] == nil && !f.repo.isEnding(groupIdHex: hex) }
        XCTAssertTrue(arrivals.load(accountIdB64: f.accountIdB64).isEmpty, "gone with the group")

        // Added back: nothing from before, so the arrival is what places it.
        let before = ChatRepository.nowMs()
        let again = try f.ownerGroup.addMemberFromWire(
            identity: f.owner, keyPackage: try f.identity.createKeyPackage())
        _ = try await f.repo.joinGroupFromWelcome(welcomeBytes: again.welcome)
        let thread = try XCTUnwrap(f.repo.state.threads.first { $0.groupIdHex == hex })
        XCTAssertGreaterThanOrEqual(try XCTUnwrap(thread.arrivedAtMs), before)
        XCTAssertEqual(
            ChatListViewModel(repository: f.repo).activityMs(thread: thread), thread.arrivedAtMs)
    }

    func testAConversationNotOnScreenLeavesItToTheBanner() async throws {
        let f = try fixture()
        let model = ChatViewModel(repository: f.repo, groupIdHex: f.groupIdHex)
        let record = try XCTUnwrap(f.repo.groups[f.groupIdHex])
        let steps = try removal(
            ownerGroup: f.ownerGroup, owner: f.owner, key: f.identity.signaturePublicKey())
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.proposal)
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.commit)
        f.repo.endMembershipIfOut(groupIdHex: f.groupIdHex)
        await waitUntil { f.repo.groups[f.groupIdHex] == nil }
        try? await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertNil(model.endDialog)
        XCTAssertEqual(f.repo.membershipEndNotices.notices.count, 1)
    }

    func testAPersonWhoLeftWithTheConversationOpenGoesBackWithNoDialog() async throws {
        let f = try fixture()
        let model = ChatViewModel(repository: f.repo, groupIdHex: f.groupIdHex)
        model.screenVisible = true
        f.repo.noteLeaveRequested(groupIdHex: f.groupIdHex)
        let record = try XCTUnwrap(f.repo.groups[f.groupIdHex])
        let steps = try removal(
            ownerGroup: f.ownerGroup, owner: f.owner, key: f.identity.signaturePublicKey())
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.proposal)
        _ = try record.group.processIncomingVerifyingRoles(identity: f.identity, bytes: steps.commit)
        f.repo.endMembershipIfOut(groupIdHex: f.groupIdHex)
        await waitUntil { model.conversationGone }
        XCTAssertTrue(model.conversationGone)
        XCTAssertNil(model.endDialog)
    }
}
