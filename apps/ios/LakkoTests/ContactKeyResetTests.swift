// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import XCTest
@testable import Lakko

/// ADR-0065 phase 2 — "a contact reset their account keys"
/// (`docs/ux/identity-recovery.md` section 16): the pure decisions, each
/// pinned where a host test can run it. The decisions are the trigger, what is
/// blocked, which branch a shared group falls in, the `applied` test, the
/// add-arm suppression of the re-admission check, the late-message drop of a
/// closed one-to-one, the group-closed receive gate, the re-pin marker, and the
/// contact record's transitions through the repository.
@MainActor
final class ContactKeyResetTests: XCTestCase {

    // MARK: - 16.1 the trigger

    func testOnlyAnAbsentProofWithALoggedResetIsAReset() {
        XCTAssertTrue(KeyResetDetection.isReset(reason: .proofAbsent, loggedReset: true))
        // A log that does not confirm, or cannot, keeps today's error copy.
        XCTAssertFalse(KeyResetDetection.isReset(reason: .proofAbsent, loggedReset: false))
        XCTAssertFalse(KeyResetDetection.isReset(reason: .proofAbsent, loggedReset: nil))
        // A proof that was PRESENT and failed is the compromise-shaped case.
        for reason in [SupersessionRefused.Reason.proofInvalid, .keyMismatch, .markerBackward] {
            XCTAssertFalse(KeyResetDetection.isReset(reason: reason, loggedReset: true), "\(reason)")
        }
    }

    // MARK: - 16.1 what is blocked

    func testAnUnverifiedResetBlocksTheActionsAndNothingElseDoes() {
        XCTAssertTrue(KeyResetBlocking.blocksActions(.resetUnverified))
        XCTAssertFalse(KeyResetBlocking.blocksActions(.normal))
        XCTAssertEqual(AcceptedContact(request: request(1, byte: 0xA1)).keyState, .normal)
        XCTAssertEqual(
            AcceptedContact(request: request(1, byte: 0xA1), keyReset: reset(previous: 0xA1, pending: 0xA2))
                .keyState,
            .resetUnverified)
    }

    // MARK: - 16.3 / 16.4 / 16.5 the branch

    func testAOneToOneClosesWhateverTheRoles() {
        XCTAssertEqual(
            sharedGroupResetBranch(
                isOneToOne: true,
                roles: SharedGroupRoleFacts(ownerKeyB64: key(0xA1), ownAccountIsOwnerOrAdmin: true),
                previousKeyB64: key(0xA1)),
            .closeOneToOne)
    }

    func testAnOwnerOrAdminEvictsInARoleTableGroupTheOldIdentityDidNotOwn() {
        XCTAssertEqual(
            sharedGroupResetBranch(
                isOneToOne: false,
                roles: SharedGroupRoleFacts(ownerKeyB64: key(0x01), ownAccountIsOwnerOrAdmin: true),
                previousKeyB64: key(0xA1)),
            .evict)
    }

    func testAPlainMemberOnlyFlags() {
        XCTAssertEqual(
            sharedGroupResetBranch(
                isOneToOne: false,
                roles: SharedGroupRoleFacts(ownerKeyB64: key(0x01), ownAccountIsOwnerOrAdmin: false),
                previousKeyB64: key(0xA1)),
            .flagOnly)
    }

    func testAGroupWithNoRoleTableOnlyFlags() {
        XCTAssertEqual(
            sharedGroupResetBranch(
                isOneToOne: false,
                roles: SharedGroupRoleFacts(ownerKeyB64: nil, ownAccountIsOwnerOrAdmin: true),
                previousKeyB64: key(0xA1)),
            .flagOnly)
    }

    func testAGroupTheOldIdentityOwnedIsNeverEvictedOnlyRecreated() {
        XCTAssertEqual(
            sharedGroupResetBranch(
                isOneToOne: false,
                roles: SharedGroupRoleFacts(ownerKeyB64: key(0xA1), ownAccountIsOwnerOrAdmin: true),
                previousKeyB64: key(0xA1)),
            .ownedGroup(canRecreate: true))
        XCTAssertEqual(
            sharedGroupResetBranch(
                isOneToOne: false,
                roles: SharedGroupRoleFacts(ownerKeyB64: key(0xA1), ownAccountIsOwnerOrAdmin: false),
                previousKeyB64: key(0xA1)),
            .ownedGroup(canRecreate: false))
    }

    // MARK: - 16.3 the `applied` test

    func testTheEvictionIsAppliedOnlyWhenNoLeafResolvesToThePreviousKey() {
        XCTAssertEqual(
            keyResetEvictionApplied(rosterPersonKeysB64: [key(0x01), key(0xA1)], previousKeyB64: key(0xA1)),
            false)
        XCTAssertEqual(
            keyResetEvictionApplied(rosterPersonKeysB64: [key(0x01), key(0xA2)], previousKeyB64: key(0xA1)),
            true)
        XCTAssertNil(
            keyResetEvictionApplied(rosterPersonKeysB64: nil, previousKeyB64: key(0xA1)),
            "a torn roster read claims nothing")
    }

    // MARK: - 16.8 the add arm of the re-admission check

    func testAFlaggedOrUncheckedAccountIsSuppressedAndAConfirmedOneIsNot() {
        let suppressed = readmissionSuppressedPersons([
            ReadmissionAccountFacts(personKeyB64: key(0xA1), keyState: .resetUnverified, pinMatchesPublished: true),
            ReadmissionAccountFacts(personKeyB64: key(0xB1), keyState: .normal, pinMatchesPublished: false),
            ReadmissionAccountFacts(personKeyB64: key(0xC1), keyState: .normal, pinMatchesPublished: nil),
            ReadmissionAccountFacts(personKeyB64: key(0xD1), keyState: .normal, pinMatchesPublished: true),
        ])
        XCTAssertEqual(suppressed, [key(0xA1), key(0xB1), key(0xC1)])
    }

    func testThePlanProposesNoAddForASuppressedPersonAndStillRetiresOwnLeaves() {
        let newDevice = DeviceReadmission.DirectoryDevice(deviceKeyB64: "new-device", keyPackage: Data([1]))
        let plan = DeviceReadmission.plan(
            roster: [
                DeviceReadmission.RosterLeafKeys(leafKeyB64: "own-leaf", personKeyB64: "me"),
                DeviceReadmission.RosterLeafKeys(leafKeyB64: "own-gone", personKeyB64: "me"),
                DeviceReadmission.RosterLeafKeys(leafKeyB64: "their-old", personKeyB64: key(0xA1)),
            ],
            ownPersonKeyB64: "me",
            accounts: [
                DeviceReadmission.AccountDevices(
                    personKeyB64: "me",
                    devices: [DeviceReadmission.DirectoryDevice(deviceKeyB64: "own-leaf", keyPackage: Data())]),
                DeviceReadmission.AccountDevices(personKeyB64: key(0xA1), devices: [newDevice]),
            ],
            removed: [],
            suppressAdmit: [key(0xA1)])
        XCTAssertTrue(plan.admit.isEmpty, "the new identity's devices are not proposed ahead of verification")
        XCTAssertEqual(plan.retire, ["own-gone"])

        let unsuppressed = DeviceReadmission.plan(
            roster: [], ownPersonKeyB64: "me",
            accounts: [DeviceReadmission.AccountDevices(personKeyB64: key(0xA1), devices: [newDevice])],
            removed: [])
        XCTAssertEqual(unsuppressed.admit, [newDevice])
    }

    // MARK: - 16.4 a late message into a closed one-to-one

    func testEveryRowIntoAClosedOneToOneIsDroppedBeforeItIsStored() {
        XCTAssertEqual(closedOneToOneRowDisposition(closed: true), .drop)
        XCTAssertEqual(closedOneToOneRowDisposition(closed: false), .store)
    }

    // MARK: - 16.2 the re-pin marker

    func testTheRepinMarkerNeverMovesBackwards() {
        XCTAssertEqual(keyResetRepinMarker(existingMarker: nil, nowSecs: 100), 100)
        XCTAssertEqual(keyResetRepinMarker(existingMarker: 50, nowSecs: 100), 100)
        XCTAssertEqual(keyResetRepinMarker(existingMarker: 500, nowSecs: 100), 500,
                       "a proof expiry already past now stays the floor")
    }

    // MARK: - 16.5 the group-closed receive gate

    private let owner = Data(repeating: 0x01, count: 32)
    private let admin = Data(repeating: 0x02, count: 32)
    private let member = Data(repeating: 0x03, count: 32)

    private func classify(
        stamp: UInt64, successor: String? = "new", stored: StoredGroupClosed? = nil,
        sender: Data?, authority: PushGroupRouting.SecretAuthority
    ) -> GroupClosedVerdict {
        GroupClosedGate.classify(
            incomingClosedAtMs: stamp, incomingSuccessorGroupIdHex: successor,
            stored: stored, senderKey: sender, authority: authority,
            isNewer: { $0 > $1 })
    }

    func testAnAdminOrOwnerClosesAndAMemberDoesNot() {
        let table = PushGroupRouting.SecretAuthority.roleTable(owner: owner, admins: [admin])
        XCTAssertEqual(classify(stamp: 10, sender: owner, authority: table), .accept)
        XCTAssertEqual(classify(stamp: 10, sender: admin, authority: table), .accept)
        XCTAssertEqual(classify(stamp: 10, sender: member, authority: table), .rejectNotAuthorized)
        XCTAssertEqual(classify(stamp: 10, sender: nil, authority: table), .rejectUnverifiable)
        XCTAssertEqual(classify(stamp: 10, sender: admin, authority: .unreadable), .rejectUnverifiable)
    }

    func testAGroupWithNoRoleTableHasNoAdminToCloseIt() {
        XCTAssertEqual(classify(stamp: 10, sender: member, authority: .noRoleTable), .rejectNotAuthorized)
    }

    func testTheIdenticalNoticeIsAcceptedBeforeAuthorityAndAStaleOneRefused() {
        let stored = StoredGroupClosed(closedAtMs: 10, successorGroupIdHex: "new")
        let table = PushGroupRouting.SecretAuthority.roleTable(owner: owner, admins: [admin])
        XCTAssertEqual(
            classify(stamp: 10, stored: stored, sender: member, authority: table), .acceptIdentical,
            "a re-send changes nothing and needs no authority")
        XCTAssertEqual(classify(stamp: 9, stored: stored, sender: admin, authority: table), .rejectStale)
        XCTAssertEqual(
            classify(stamp: 11, successor: "newer", stored: stored, sender: admin, authority: table), .accept,
            "the later stamp wins the Open target")
    }

    // MARK: - the system row marker

    func testTheEvictionRowMarkerRoundTripsAndOtherRowsDoNotParse() {
        let marker = KeyResetSystemRow.marker(accountIdB64: "acct==")
        XCTAssertEqual(KeyResetSystemRow.parse(marker), "acct==")
        XCTAssertNil(KeyResetSystemRow.parse("[mls commit]"))
        XCTAssertNil(KeyResetSystemRow.parse("[keys-reset-removed]"))
        XCTAssertFalse(ChatTimeline.isHiddenSystemRow(
            ChatMessage(messageId: -1, direction: .system, text: marker, createdAt: "")))
    }

    // MARK: - the stores

    func testTheClosedStoreAndTheEvictionStoreRoundTripPerAccount() {
        let suite = "key-reset-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { UserDefaults().removePersistentDomain(forName: suite) }

        let closed = ConversationClosedStore(defaults: defaults)
        closed.record(
            accountIdB64: "me", groupIdHex: "g1",
            reason: .contactKeysReset(accountIdB64: "them", closedAtMs: 5))
        closed.record(
            accountIdB64: "me", groupIdHex: "g2",
            reason: .groupClosed(closedByKeyB64: key(0x02), closedAtMs: 6, successorGroupIdHex: "g3"))
        XCTAssertEqual(
            closed.load(accountIdB64: "me", groupIdHex: "g1"),
            .contactKeysReset(accountIdB64: "them", closedAtMs: 5))
        XCTAssertEqual(closed.all(accountIdB64: "me").count, 2)
        XCTAssertNil(closed.load(accountIdB64: "other", groupIdHex: "g1"))
        closed.remove(accountIdB64: "me", groupIdHex: "g1")
        XCTAssertNil(closed.load(accountIdB64: "me", groupIdHex: "g1"))

        let evictions = KeyResetEvictionStore(defaults: defaults)
        evictions.record(
            accountIdB64: "me", groupIdHex: "g2", previousKeyB64: key(0xA1),
            KeyResetEvictionRecord(accountIdB64: "them", proposedAtMs: 7, applied: false))
        XCTAssertEqual(
            evictions.records(accountIdB64: "me", groupIdHex: "g2")[key(0xA1)]?.proposedAtMs, 7)
        evictions.rememberRetired(accountIdB64: "me", previousKeyB64: key(0xA1), contactAccountIdB64: "them")
        XCTAssertEqual(evictions.retiredKeys(accountIdB64: "me"), [key(0xA1): "them"])
        XCTAssertTrue(evictions.retiredKeys(accountIdB64: "other").isEmpty)
    }

    // MARK: - 16.1 / 16.2 the contact record through the repository

    private var suiteName = ""
    private var defaults: UserDefaults!
    private let account = "key-reset-test-account"

    override func setUp() {
        super.setUp()
        suiteName = "key-reset-contacts-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)!
        AcceptedContactsStore.defaults = defaults
    }

    override func tearDown() {
        AcceptedContactsStore.defaults = .standard
        UserDefaults().removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    func testARecordedResetIsFoundByAccountIdKeepsTheRenameAndSurvivesARelaunch() {
        AcceptedContactsStore.save(
            [AcceptedContact(request: request(1, byte: 0xA1), groupIdHex: "chat", displayName: "Anna")],
            accountIdB64: account)
        let repository = makeRepository()

        repository.noteKeyReset(
            accountId: Data(repeating: 0xA1, count: 16), previousKeyB64: key(0xA1), pendingKeyB64: key(0xA2))

        let flagged = repository.contact(accountIdB64: base64Encode(Data(repeating: 0xA1, count: 16)))
        XCTAssertEqual(flagged?.keyState, .resetUnverified)
        XCTAssertEqual(flagged?.keyReset?.pendingKeyB64, key(0xA2))
        XCTAssertEqual(flagged?.displayName, "Anna", "the rename stays with the person")
        XCTAssertEqual(flagged?.groupIdHex, "chat")
        XCTAssertEqual(
            AcceptedContactsStore.load(accountIdB64: account).first?.keyReset?.previousKeyB64, key(0xA1),
            "persisted across restarts")
        XCTAssertEqual(repository.flaggedContacts.count, 1)
    }

    func testASecondResetBeforeVerificationKeepsThePinAsPreviousAndAdvancesPending() {
        AcceptedContactsStore.save([AcceptedContact(request: request(1, byte: 0xA1))], accountIdB64: account)
        let repository = makeRepository()
        let id = Data(repeating: 0xA1, count: 16)
        repository.noteKeyReset(accountId: id, previousKeyB64: key(0xA1), pendingKeyB64: key(0xA2))
        repository.noteKeyReset(accountId: id, previousKeyB64: key(0xA1), pendingKeyB64: key(0xA3))
        let reset = repository.contact(accountIdB64: base64Encode(id))?.keyReset
        XCTAssertEqual(reset?.previousKeyB64, key(0xA1))
        XCTAssertEqual(reset?.pendingKeyB64, key(0xA3))
    }

    func testAResetForAnAccountWithNoContactRecordsNothing() {
        AcceptedContactsStore.save([AcceptedContact(request: request(1, byte: 0xA1))], accountIdB64: account)
        let repository = makeRepository()
        repository.noteKeyReset(
            accountId: Data(repeating: 0xEE, count: 16), previousKeyB64: key(0xEE), pendingKeyB64: key(0xEF))
        XCTAssertTrue(repository.flaggedContacts.isEmpty)
    }

    func testConfirmingRepinsTheNewKeyMovesTheContactToItAndClearsTheState() {
        AcceptedContactsStore.save(
            [AcceptedContact(request: request(1, byte: 0xA1), groupIdHex: "chat", displayName: "Anna")],
            accountIdB64: account)
        let repository = makeRepository()
        let id = Data(repeating: 0xA1, count: 16)
        let pins = PeerPinStore(defaults: defaults, ownerAccountIdB64: account)
        pins.setPin(accountIdB64: base64Encode(id), pinnedSignaturePubkeyB64: key(0xA1), rotationMarker: 0)
        repository.noteKeyReset(accountId: id, previousKeyB64: key(0xA1), pendingKeyB64: key(0xA2))

        XCTAssertTrue(repository.confirmKeyReset(messageId: 1, pinStore: pins))

        let verified = repository.contact(accountIdB64: base64Encode(id))
        XCTAssertEqual(verified?.keyState, .normal)
        XCTAssertEqual(verified?.request.fromPubkey, Data(repeating: 0xA2, count: 32))
        XCTAssertEqual(verified?.request.messageId, 1, "the row keeps its identity")
        XCTAssertEqual(verified?.displayName, "Anna")
        XCTAssertEqual(verified?.groupIdHex, "chat")
        let pin = pins.readPin(accountIdB64: base64Encode(id))
        XCTAssertEqual(pin?.pinnedSignaturePubkeyB64, key(0xA2))
        XCTAssertGreaterThan(pin?.rotationMarker ?? 0, 0, "the marker moved forward")
        XCTAssertEqual(pin?.isReset, true)
    }

    func testConfirmingAContactThatIsNotFlaggedChangesNothing() {
        AcceptedContactsStore.save([AcceptedContact(request: request(1, byte: 0xA1))], accountIdB64: account)
        let repository = makeRepository()
        XCTAssertFalse(repository.confirmKeyReset(
            messageId: 1, pinStore: PeerPinStore(defaults: defaults, ownerAccountIdB64: account)))
    }

    /// 16.5 — the closer's key is what the store keeps (so the name is
    /// resolved when the row is drawn, after a relaunch too), and a closer
    /// that resolves to no name reads as the roster's unnamed member.
    func testTheClosedGroupRowKeepsTheClosersKeyAndFallsBackToTheUnnamedMember() {
        let suite = "closer-\(UUID().uuidString)"
        guard let defaults = UserDefaults(suiteName: suite) else { return XCTFail("no defaults") }
        defer { defaults.removePersistentDomain(forName: suite) }
        ConversationClosedStore(defaults: defaults).record(
            accountIdB64: "me", groupIdHex: "g",
            reason: .groupClosed(closedByKeyB64: "a2V5", closedAtMs: 9, successorGroupIdHex: nil))
        // A fresh store over the same defaults is what a relaunch reads.
        guard case .groupClosed(let by, _, _)? =
                ConversationClosedStore(defaults: defaults).load(accountIdB64: "me", groupIdHex: "g")
        else { return XCTFail("not stored") }
        XCTAssertEqual(by, "a2V5")
        let reason = ConversationClosedReason.groupClosed(
            closedByKeyB64: by, closedAtMs: 9, successorGroupIdHex: nil)
        XCTAssertEqual(
            closedTerminalRow(reason, name: "Ivan").headline,
            Localization.text("group.closed.system", texts: ["name": "Ivan"]))
        XCTAssertEqual(
            closedTerminalRow(reason, name: nil).headline,
            Localization.text(
                "group.closed.system", texts: ["name": Localization.text("chat.members.unnamed")]))
    }

    /// 16.1 — `Open contact` rides only the refusal worded as the state: a
    /// confirmed reset of a contact this device holds.
    func testOpenContactIsOfferedOnlyBesideTheVerifyFirstRefusal() {
        XCTAssertEqual(
            ContactsViewModel.refusalOpenContactTarget(
                loggedReset: true, contactFlagged: true, contactMessageId: 7), 7)
        XCTAssertNil(
            ContactsViewModel.refusalOpenContactTarget(
                loggedReset: true, contactFlagged: false, contactMessageId: 7))
        XCTAssertNil(
            ContactsViewModel.refusalOpenContactTarget(
                loggedReset: false, contactFlagged: true, contactMessageId: 7))
        XCTAssertNil(
            ContactsViewModel.refusalOpenContactTarget(
                loggedReset: nil, contactFlagged: true, contactMessageId: 7))
        XCTAssertNil(
            ContactsViewModel.refusalOpenContactTarget(
                loggedReset: true, contactFlagged: true, contactMessageId: nil))
    }

    /// 16.4 — a closed one-to-one holds no push subscription.
    func testAClosedOneToOneIsNeverProvisionedForPush() {
        XCTAssertFalse(ChatRepository.pushRoutingAllowed(closedOneToOne: true))
        XCTAssertTrue(ChatRepository.pushRoutingAllowed(closedOneToOne: false))
    }

    func testTheErrorCopyPointsAtTheStateOnlyForAFlaggedContact() {
        XCTAssertEqual(
            ContactsViewModel.supersessionRefusedMessage(
                loggedReset: true, peerLabel: "Anna", contactFlagged: true),
            Localization.text("contacts.keys_reset.verify_first", texts: ["name": "Anna"]))
        XCTAssertEqual(
            ContactsViewModel.supersessionRefusedMessage(
                loggedReset: true, peerLabel: nil, contactFlagged: true),
            Localization.text("contacts.keys_reset.verify_first_short"))
        XCTAssertEqual(
            ContactsViewModel.supersessionRefusedMessage(
                loggedReset: true, peerLabel: "@anna", contactFlagged: false),
            Localization.text("contacts.identity_changed.reset", texts: ["name": "@anna"]),
            "no contact, no state: today's copy")
        XCTAssertEqual(
            ContactsViewModel.supersessionRefusedMessage(
                loggedReset: false, peerLabel: "@anna", contactFlagged: true),
            Localization.text("contacts.identity_changed.unexplained", texts: ["name": "@anna"]),
            "an unexplained change is never a reset, flagged or not")
    }

    // MARK: - Fixtures

    private func key(_ byte: UInt8) -> String {
        base64Encode(Data(repeating: byte, count: 32))
    }

    private func reset(previous: UInt8, pending: UInt8) -> ContactKeyReset {
        ContactKeyReset(previousKeyB64: key(previous), pendingKeyB64: key(pending), observedAtMs: 1)
    }

    private func request(_ messageId: Int64, byte: UInt8) -> IncomingContactRequest {
        IncomingContactRequest(
            messageId: messageId,
            fromPubkey: Data(repeating: byte, count: 32),
            fromAccountId: Data(repeating: byte, count: 16),
            fromPseudonym: Data(repeating: byte, count: 32),
            validAfterMs: 0,
            intro: nil,
            createdAt: "2026-10-04T00:00:00Z")
    }

    private func makeRepository() -> ContactsRepository {
        ContactsRepository(
            authRepository: AuthRepository(
                authClientFactory: { AuthClient(baseURL: "http://auth.test") },
                persistence: KeyResetNoSessionPersistence(),
                nowMs: { 1_000_000 }),
            accountBinding: Just<String?>(account).eraseToAnyPublisher())
    }
}

/// A ``SessionPersistence`` that holds nothing, so building an
/// ``AuthRepository`` here never reads or writes the real Keychain.
private struct KeyResetNoSessionPersistence: SessionPersistence {
    func hasDeviceKey(accountIdB64: String) -> Bool { false }
    func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { nil }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {}
    func deleteSessionToken(accountIdB64: String) {}
}
