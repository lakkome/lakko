// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

// The account backup carries no MLS state (maintainer decision, 2026-10-02).
//
//   * A backup is composed without a snapshot and a restore never loads MLS
//     state from one: after a restore the device's MLS state is whatever it
//     already held locally.
//   * Every route that restores the identity from the recovery phrase fetches
//     and applies the backup; a missing backup and a failed fetch never fail
//     the sign-in; the restore is owed from before the session lands until it
//     reaches a definitive answer, one attempt per session landing.
//   * A backup always lists the account's conversations: the ids a restored
//     backup named ride in this device's own uploads until it joins them.
//   * The chat list's "Synchronizing..." label follows the pairing bundle
//     only.

// MARK: - Shared helpers

private func randomBytes(_ count: Int) -> Data {
    Data((0..<count).map { _ in UInt8.random(in: 0...255) })
}

private func hex(_ data: Data) -> String {
    data.map { String(format: "%02x", $0) }.joined()
}

private func contactRecord(_ byte: UInt8, _ name: String) -> BackupContactRecord {
    BackupContactRecord(
        ed25519Pubkey: Data(repeating: byte, count: 32),
        x25519Pubkey: Data(count: 32),
        displayName: name,
        accountId: Data(repeating: byte, count: 16)
    )
}

private func existingContact(_ byte: UInt8, _ name: String) -> AcceptedContact {
    AcceptedContact(
        request: IncomingContactRequest(
            messageId: 7,
            fromPubkey: Data(repeating: byte, count: 32),
            fromAccountId: nil,
            fromPseudonym: Data(count: 32),
            validAfterMs: 0,
            intro: nil,
            createdAt: ""
        ),
        groupIdHex: nil,
        displayName: name
    )
}

/// A synced state holding one pinned conversation, as bytes.
private func syncedStateBytes(pinning groupId: Data) throws -> Data {
    let state = SyncedState()
    _ = try state.applyPayload(payload: ParsedDeviceGroupPayload(
        hlc: 10, senderLeaf: 1,
        body: .flags(
            groupId: groupId, pinned: true, muted: false, archived: false,
            unreadOverride: false)))
    return try state.toBytes()
}

/// A backup payload as a restore parses it, built through the real composer.
private func parsedBackup(
    accountId: Data,
    contacts: [BackupContactRecord] = [],
    groupIds: [Data] = [],
    syncedState: Data? = nil
) throws -> ParsedBackupPayload {
    try parseBackupPayload(wire: try BackupClient.composePayload(
        accountId: accountId,
        createdAtMs: 1_700_000_000_000,
        ed25519Seed: Data(repeating: 0x5E, count: 32),
        contacts: contacts,
        knownGroupIdsHex: groupIds.map(hex),
        syncedState: syncedState
    ))
}

/// Remove everything a test wrote for `accountIdB64` through the real stores.
private func cleanAccount(_ accountIdB64: String) {
    DeviceKeyStore.deleteDeviceKey(accountIdB64: accountIdB64)
    AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: accountIdB64)
    try? PhraseStore().deletePhrase(accountIdB64: accountIdB64)
    AcceptedContactsStore.clear(accountIdB64: accountIdB64)
    SyncedStateStore.delete(accountIdB64: accountIdB64)
    IdentitySnapshotStore().deleteSnapshot(accountIdB64: accountIdB64)
    KnownGroupsStore().clear(accountIdB64: accountIdB64)
    RestoredKnownGroupsStore.clear(accountIdB64: accountIdB64)
    BackupRestoreRetryStore.clear(accountIdB64: accountIdB64)
    AutoBackupStore.clear(accountIdB64: accountIdB64)
    MessageLogStore().deleteLog(accountIdB64: accountIdB64)
}

// MARK: - A. Compose without a snapshot

final class BackupPayloadShapeTests: XCTestCase {

    func testTheComposedPayloadCarriesContactsConversationsAndSyncedState() throws {
        let accountId = randomBytes(16)
        let g1 = Data(repeating: 0xA1, count: 32)
        let g2 = Data(repeating: 0xA2, count: 32)
        let synced = try syncedStateBytes(pinning: g1)

        let parsed = try parsedBackup(
            accountId: accountId,
            contacts: [contactRecord(0x01, "Alice")],
            groupIds: [g1, g2],
            syncedState: synced)

        XCTAssertEqual(parsed.accountId, accountId)
        XCTAssertEqual(parsed.acceptedContacts.map(\.displayName), ["Alice"])
        XCTAssertEqual(Set(parsed.knownGroupIds), [g1, g2])
        XCTAssertEqual(parsed.syncedState, synced)
    }

    /// The parsed payload has exactly these fields. A snapshot field coming
    /// back would be an eighth.
    func testTheParsedPayloadHasNoMlsSnapshotField() throws {
        let parsed = try parsedBackup(accountId: randomBytes(16))
        let fields = Set(Mirror(reflecting: parsed).children.compactMap(\.label))
        XCTAssertEqual(fields, [
            "accountId", "createdAtMs", "ed25519Seed", "acceptedContacts",
            "knownGroupIds", "syncedState", "ownPredecessors", "ownProfile", "visibility",
            "receivedProfiles",
        ])
        XCTAssertFalse(fields.contains { $0.lowercased().contains("snapshot") })
    }

    /// D31, D37: the person's own profile and setting ride the backup as
    /// opaque envelopes, and an empty field reads as none.
    func testTheOwnEnvelopesTravelInThePayload() throws {
        let profile = Data("profile envelope".utf8)
        let setting = Data("visibility envelope".utf8)
        func parsed(_ own: OwnBackupEnvelopes) throws -> ParsedBackupPayload {
            try parseBackupPayload(wire: try BackupClient.composePayload(
                accountId: randomBytes(16),
                createdAtMs: 1,
                ed25519Seed: Data(repeating: 0x5E, count: 32),
                contacts: [],
                knownGroupIdsHex: [],
                syncedState: nil,
                own: own))
        }
        let both = try parsed(OwnBackupEnvelopes(ownProfile: profile, visibility: setting))
        XCTAssertEqual(both.ownProfile, profile)
        XCTAssertEqual(both.visibility, setting)
        let neither = try parsed(OwnBackupEnvelopes())
        XCTAssertTrue(neither.ownProfile.isEmpty)
        XCTAssertTrue(neither.visibility.isEmpty)
    }

    func testARowThatIsNotAGroupIdIsDroppedAndTheRestIsKept() throws {
        let g1 = Data(repeating: 0xB1, count: 32)
        let wire = try BackupClient.composePayload(
            accountId: randomBytes(16),
            createdAtMs: 1,
            ed25519Seed: Data(repeating: 0x5E, count: 32),
            contacts: [],
            knownGroupIdsHex: ["not-hex", "abcd", hex(g1)],
            syncedState: nil
        )
        XCTAssertEqual(try parseBackupPayload(wire: wire).knownGroupIds, [g1])
    }
}

// MARK: - B. A restore never loads MLS state

/// Section 7.2 of `docs/ux/identity-recovery.md` — what the chat list says
/// about a restored device (D34), and the marker behind it.
final class RestoredListNoticeTests: XCTestCase {

    private func record(known: Int, awaiting: Bool = true) -> RestoredDeviceStore.Record {
        RestoredDeviceStore.Record(knownConversations: known, awaitingFirstConversation: awaiting)
    }

    // The section 7.2 table: the card only when this device founded the
    // DeviceGroup, it is restored-marked, the backup named a conversation and
    // the list is empty; the ordinary list in every other case, including
    // when another device answered (there is no banner).

    func testNobodyAnsweredAndAKnownConversationShowsTheCard() {
        XCTAssertEqual(
            RestoredListNotice.decide(record: record(known: 2), listEmpty: true, rejoinAnswer: .founded),
            .card)
    }

    func testAnAnswerShowsTheOrdinaryList() {
        XCTAssertEqual(
            RestoredListNotice.decide(record: record(known: 2), listEmpty: true, rejoinAnswer: .answered),
            .none)
    }

    func testARestoredDeviceThatKnewNoConversationShowsTheOrdinaryListEitherWay() {
        XCTAssertEqual(
            RestoredListNotice.decide(record: record(known: 0), listEmpty: true, rejoinAnswer: .founded),
            .none)
        XCTAssertEqual(
            RestoredListNotice.decide(record: record(known: 0), listEmpty: true, rejoinAnswer: .answered),
            .none)
    }

    func testNothingShowsOverAConversationOrWhileTheRejoinHasNotDecided() {
        for answer in [RestoredRejoinAnswerStore.Answer.answered, .founded] {
            XCTAssertEqual(
                RestoredListNotice.decide(record: record(known: 2), listEmpty: false, rejoinAnswer: answer),
                .none)
            XCTAssertEqual(
                RestoredListNotice.decide(record: nil, listEmpty: true, rejoinAnswer: answer), .none)
            XCTAssertEqual(
                RestoredListNotice.decide(
                    record: record(known: 2, awaiting: false), listEmpty: true, rejoinAnswer: answer),
                .none)
        }
        XCTAssertEqual(
            RestoredListNotice.decide(record: record(known: 2), listEmpty: true, rejoinAnswer: nil),
            .none)
    }

    func testTheFirstConversationEndsItForGood() throws {
        let suite = "restored-device-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { UserDefaults().removePersistentDomain(forName: suite) }
        let account = "acct-\(UUID().uuidString)"

        RestoredDeviceStore.noteRestored(accountIdB64: account, knownConversations: 3, defaults: defaults)
        XCTAssertEqual(
            RestoredListNotice.decide(
                record: RestoredDeviceStore.load(accountIdB64: account, defaults: defaults),
                listEmpty: true, rejoinAnswer: .founded),
            .card)

        RestoredDeviceStore.noteConversationArrived(accountIdB64: account, defaults: defaults)
        XCTAssertEqual(
            RestoredListNotice.decide(
                record: RestoredDeviceStore.load(accountIdB64: account, defaults: defaults),
                listEmpty: true, rejoinAnswer: .founded),
            .none, "the list emptying later does not bring it back")
    }

    func testTheRejoinAnswerIsKeptPerAccountAndTheLatestWins() throws {
        let suite = "rejoin-answer-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { UserDefaults().removePersistentDomain(forName: suite) }
        let account = "acct-\(UUID().uuidString)"
        let other = "acct-\(UUID().uuidString)"

        XCTAssertNil(RestoredRejoinAnswerStore.load(accountIdB64: account, defaults: defaults))
        RestoredRejoinAnswerStore.record(.answered, accountIdB64: account, defaults: defaults)
        XCTAssertEqual(RestoredRejoinAnswerStore.load(accountIdB64: account, defaults: defaults), .answered)
        XCTAssertNil(RestoredRejoinAnswerStore.load(accountIdB64: other, defaults: defaults))

        RestoredRejoinAnswerStore.record(.founded, accountIdB64: account, defaults: defaults)
        XCTAssertEqual(RestoredRejoinAnswerStore.load(accountIdB64: account, defaults: defaults), .founded)

        RestoredRejoinAnswerStore.clear(accountIdB64: account, defaults: defaults)
        XCTAssertNil(RestoredRejoinAnswerStore.load(accountIdB64: account, defaults: defaults))
    }

    func testTheProfileRepublishIsOwedOnce() throws {
        let suite = "restored-device-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { UserDefaults().removePersistentDomain(forName: suite) }
        let account = "acct-\(UUID().uuidString)"
        XCTAssertFalse(RestoredDeviceStore.takeProfileRepublish(accountIdB64: account, defaults: defaults))
        RestoredDeviceStore.noteRestored(accountIdB64: account, knownConversations: 0, defaults: defaults)
        XCTAssertTrue(RestoredDeviceStore.takeProfileRepublish(accountIdB64: account, defaults: defaults))
        XCTAssertFalse(RestoredDeviceStore.takeProfileRepublish(accountIdB64: account, defaults: defaults))
    }
}

/// D5: the channel id of a claimed handle is kept until the channel exists.
final class PendingChannelHandleTests: XCTestCase {

    func testARetryFindsTheIdAndAFinishedChannelForgetsIt() throws {
        let suite = "pending-handles-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { UserDefaults().removePersistentDomain(forName: suite) }
        let account = "acct-\(UUID().uuidString)"

        XCTAssertNil(PendingChannelHandleStore.channelId(
            forHandle: "news", accountIdB64: account, defaults: defaults))
        PendingChannelHandleStore.keep(
            handle: "@News", channelIdHex: "aabb", accountIdB64: account, defaults: defaults)
        XCTAssertEqual(
            PendingChannelHandleStore.channelId(forHandle: "news", accountIdB64: account, defaults: defaults),
            "aabb", "the same handle however it was typed")
        XCTAssertNil(PendingChannelHandleStore.channelId(
            forHandle: "news", accountIdB64: "other", defaults: defaults), "per account")

        PendingChannelHandleStore.forget(handle: "NEWS", accountIdB64: account, defaults: defaults)
        XCTAssertNil(PendingChannelHandleStore.channelId(
            forHandle: "news", accountIdB64: account, defaults: defaults))
    }
}

/// ADR-0087 UX 3.1: Create needs a well-formed @name and, when public, a
/// name; nobody has to be picked.
final class NewGroupCreateRuleTests: XCTestCase {
    func testCreateNeedsAWellFormedNameAndAPublicGroupANamedOne() {
        XCTAssertTrue(NewGroupScreen.canCreate(name: "", handle: "group_k3x9", isPublic: false))
        XCTAssertTrue(NewGroupScreen.canCreate(name: "Book club", handle: "book_club", isPublic: true))
        XCTAssertFalse(NewGroupScreen.canCreate(name: "   ", handle: "book_club", isPublic: true))
        XCTAssertFalse(NewGroupScreen.canCreate(name: "Book club", handle: "ab", isPublic: false))
    }
}

/// D32: which line a contact with no chat reads.
final class ContactNoChatLineTests: XCTestCase {

    private func contact(messageId: Int64, accountId: Data?) -> AcceptedContact {
        AcceptedContact(
            request: IncomingContactRequest(
                messageId: messageId, fromPubkey: Data(repeating: 1, count: 32),
                fromAccountId: accountId, fromPseudonym: Data(count: 32), validAfterMs: 0,
                intro: nil, createdAt: ""),
            groupIdHex: nil)
    }

    /// 2026-11-05 — a contact with an account and no chat here gets no line:
    /// its Message starts a new one-to-one.
    func testAContactWithAnAccountAndNoChatHereHasNoLine() {
        XCTAssertNil(
            ContactDetailScreen.noChatLineKey(contact(messageId: 4, accountId: Data(repeating: 2, count: 16))))
        XCTAssertNil(
            ContactDetailScreen.noChatLineKey(contact(messageId: -3, accountId: Data(repeating: 2, count: 16))),
            "a contact a backup brought back too")
    }

    func testAContactWithNoKnownAccountCannotBeMessagedFromHere() {
        XCTAssertEqual(
            ContactDetailScreen.noChatLineKey(contact(messageId: 4, accountId: nil)),
            "contacts.detail.message_unavailable")
    }

}

@MainActor
final class BackupApplyTests: XCTestCase {

    private var accountIdB64 = ""
    private var suiteName = ""
    private var defaults: UserDefaults!

    override func setUp() async throws {
        try await super.setUp()
        accountIdB64 = base64Encode(randomBytes(16))
        suiteName = "backup-apply-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)!
    }

    override func tearDown() async throws {
        cleanAccount(accountIdB64)
        UserDefaults().removePersistentDomain(forName: suiteName)
        try await super.tearDown()
    }

    /// ADR-0065 moved the synced state to encoding version 2 and version 1 is
    /// refused. A parsed backup that still carries version 1 is a backup with
    /// no synced state: the rest of it is applied and nothing is announced.
    ///
    /// The payload is built with a readable state and the field replaced
    /// afterwards, because the core's payload codec refuses to compose (and to
    /// parse) a payload whose synced state the fold refuses. So today a real
    /// restore never reaches `apply` with such bytes; this pins what `apply`
    /// does if it is ever handed them.
    func testApplyTreatsAVersionOneSyncedStateAsNoneHeld() throws {
        let accountId = try XCTUnwrap(base64Decode(accountIdB64))
        let g1 = Data(repeating: 0xC1, count: 32)
        var parsed = try parsedBackup(
            accountId: accountId,
            contacts: [contactRecord(0x01, "Alice")],
            groupIds: [g1],
            syncedState: try syncedStateBytes(pinning: g1))
        XCTAssertEqual(parsed.syncedState.first, 0x0F)
        parsed.syncedState[parsed.syncedState.startIndex] = 0x01

        let announced = expectation(forNotification: .lakkoSyncedStateRestored, object: nil)
        announced.isInverted = true

        let applied = PhraseRestoreBackup.apply(
            parsed, accountIdB64: accountIdB64, context: "test", defaults: defaults)

        XCTAssertEqual(applied, AppliedBackup(contactsAdded: 1, syncedStateMerged: false, knownGroups: 1))
        XCTAssertNil(SyncedStateStore.bytes(accountIdB64: accountIdB64), "nothing was stored")
        XCTAssertNil(try SyncedStateStore.load(accountIdB64: accountIdB64).flags(groupId: g1))
        XCTAssertEqual(
            RestoredKnownGroupsStore.load(accountIdB64: accountIdB64, defaults: defaults), [hex(g1)])
        wait(for: [announced], timeout: 0.2)
    }

    /// A device still holding a version 1 blob offers a backup no synced
    /// state at all, so the upload composes. Handing the composer the stored
    /// bytes as they are would fail every upload until the blob was replaced.
    func testAStoredVersionOneBlobNeverReachesABackupPayload() throws {
        let accountId = try XCTUnwrap(base64Decode(accountIdB64))
        let g1 = Data(repeating: 0xC1, count: 32)
        var versionOne = try syncedStateBytes(pinning: g1)
        versionOne[versionOne.startIndex] = 0x01
        SyncedStateStore.store().putSnapshot(accountIdB64: accountIdB64, blob: versionOne)

        let offered = SyncedStateStore.bytes(accountIdB64: accountIdB64)
        XCTAssertNil(offered)
        let wire = try BackupClient.composePayload(
            accountId: accountId,
            createdAtMs: 1_700_000_000_000,
            ed25519Seed: Data(repeating: 0x5E, count: 32),
            contacts: [contactRecord(0x01, "Alice")],
            knownGroupIdsHex: [hex(g1)],
            syncedState: offered)
        let parsed = try parseBackupPayload(wire: wire)
        XCTAssertTrue(parsed.syncedState.isEmpty)
        XCTAssertEqual(parsed.acceptedContacts.count, 1)
        XCTAssertEqual(parsed.knownGroupIds, [g1])
    }

    func testApplyMergesContactsSyncedStateAndConversationsAndWritesNoMlsState() throws {
        let accountId = try XCTUnwrap(base64Decode(accountIdB64))
        let g1 = Data(repeating: 0xC1, count: 32)
        let g2 = Data(repeating: 0xC2, count: 32)
        // A contact accepted on this device after the backup was taken, under
        // a name the backup does not have.
        AcceptedContactsStore.save([existingContact(0x02, "Bob (live)")], accountIdB64: accountIdB64)

        let applied = PhraseRestoreBackup.apply(
            try parsedBackup(
                accountId: accountId,
                contacts: [contactRecord(0x01, "Alice"), contactRecord(0x02, "Bob (backup)")],
                groupIds: [g1, g2],
                syncedState: try syncedStateBytes(pinning: g1)),
            accountIdB64: accountIdB64,
            context: "test",
            defaults: defaults)

        XCTAssertEqual(applied, AppliedBackup(contactsAdded: 1, syncedStateMerged: true, knownGroups: 2))
        let contacts = AcceptedContactsStore.load(accountIdB64: accountIdB64)
        XCTAssertEqual(
            Set(contacts.compactMap(\.displayName)), ["Bob (live)", "Alice"],
            "the existing contact is neither dropped nor renamed")
        XCTAssertEqual(
            try SyncedStateStore.load(accountIdB64: accountIdB64).flags(groupId: g1)?.pinned, true)
        XCTAssertEqual(
            RestoredKnownGroupsStore.load(accountIdB64: accountIdB64, defaults: defaults),
            [hex(g1), hex(g2)])

        // No MLS state: no snapshot was written, and the conversations the
        // backup named are not listed as groups this device holds.
        XCTAssertNil(IdentitySnapshotStore().getSnapshot(accountIdB64: accountIdB64))
        XCTAssertTrue(KnownGroupsStore().load(accountIdB64: accountIdB64).isEmpty)
    }

    /// The reinstall case: the device already holds MLS state for the account.
    /// A restore leaves it byte for byte what it was.
    func testApplyLeavesAnExistingLocalMlsSnapshotAndGroupIndexUntouched() throws {
        let accountId = try XCTUnwrap(base64Decode(accountIdB64))
        let held = hex(Data(repeating: 0xD1, count: 32))
        let local = Data("the device's own snapshot".utf8)
        IdentitySnapshotStore().putSnapshot(accountIdB64: accountIdB64, blob: local)
        KnownGroupsStore().add(accountIdB64: accountIdB64, groupIdHex: held)

        PhraseRestoreBackup.apply(
            try parsedBackup(accountId: accountId, groupIds: [Data(repeating: 0xD2, count: 32)]),
            accountIdB64: accountIdB64, context: "test", defaults: defaults)

        XCTAssertEqual(IdentitySnapshotStore().getSnapshot(accountIdB64: accountIdB64), local)
        XCTAssertEqual(KnownGroupsStore().load(accountIdB64: accountIdB64), [held])
    }

    func testASecondRestoreAddsToTheNamedConversationsAndNeverDropsAny() throws {
        let accountId = try XCTUnwrap(base64Decode(accountIdB64))
        let g1 = Data(repeating: 0xE1, count: 32)
        let g2 = Data(repeating: 0xE2, count: 32)
        PhraseRestoreBackup.apply(
            try parsedBackup(accountId: accountId, groupIds: [g1]),
            accountIdB64: accountIdB64, context: "test", defaults: defaults)
        PhraseRestoreBackup.apply(
            try parsedBackup(accountId: accountId, groupIds: [g2]),
            accountIdB64: accountIdB64, context: "test", defaults: defaults)
        XCTAssertEqual(
            RestoredKnownGroupsStore.load(accountIdB64: accountIdB64, defaults: defaults),
            [hex(g1), hex(g2)])
    }
}

// MARK: - C. The backup on every phrase route

/// A scripted backup source: hands out the queued outcomes in order and
/// records what it was asked with.
private final class ScriptedBackupSource: BackupRestoreSource {
    struct Call {
        let sessionToken: String
        let phraseBytes: Data
        let accountIdB64: String
    }

    var outcomes: [RestoreOutcome] = []
    private(set) var calls: [Call] = []
    /// Run while a fetch is in flight, so a test can read what was already
    /// true before any answer came back.
    var duringFetch: (() -> Void)?

    func restore(sessionToken: String, phraseBytes: Data, gate: RestoreGateContext) async -> RestoreOutcome {
        calls.append(Call(
            sessionToken: sessionToken, phraseBytes: phraseBytes, accountIdB64: gate.accountIdB64))
        duringFetch?()
        return outcomes.isEmpty ? .noBackup : outcomes.removeFirst()
    }
}

private final class PublishedIdentityDirectory: DeviceRestoreDirectory {
    var publishedKey: Data?

    func fetchIdentityBundle(accountId: Data, sessionToken: String?) async throws -> IdentityFetchResponse? {
        guard let publishedKey else { return nil }
        return IdentityFetchResponse(signaturePubkey: base64Encode(publishedKey), x25519Pubkey: "")
    }

    func lookupAccount(username: String, accountId: Data, sessionToken: String) async throws -> Data? {
        nil
    }
}

private final class QuietAccountDirectory: AccountDirectory {
    func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
        throw AuthClientError.argument("not reached")
    }

    func replaceUsername(
        accountId: Data, username: String, sessionToken: String
    ) async throws -> ReplacedUsername {
        throw AuthClientError.argument("not reached")
    }
    func publishIdentityBundle(
        accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
        leafAuth: Data?, genesisAttest: Data?, keyResetAttest: Data?, sessionToken: String
    ) async throws {}
}

/// Session persistence a test can flip from "fresh device" to "this device
/// holds the account", which is what a relaunch reads.
private final class FlippablePersistence: SessionPersistence {
    var deviceKey = false
    var seed: Data?
    var tokens: [String: PersistedSessionToken] = [:]

    func hasDeviceKey(accountIdB64: String) -> Bool { deviceKey }
    func accountIdentitySeed(accountIdB64: String) -> Data? { seed }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { tokens[accountIdB64] }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws { tokens[accountIdB64] = token }
    func deleteSessionToken(accountIdB64: String) { tokens[accountIdB64] = nil }
}

@MainActor
final class PhraseRouteBackupTests: XCTestCase {

    private var accountId = Data()
    private var accountIdB64 = ""
    private var suiteName = ""
    private var defaults: UserDefaults!
    private var phrase = ""
    private var seed = Data()

    override func setUp() async throws {
        try await super.setUp()
        accountId = randomBytes(16)
        accountIdB64 = base64Encode(accountId)
        suiteName = "phrase-route-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)!
        let mnemonic = RecoveryMnemonic.generate()
        phrase = mnemonic.phrase()
        seed = mnemonic.toSeed()
    }

    override func tearDown() async throws {
        cleanAccount(accountIdB64)
        AcceptedContactsStore.bind(accountIdB64: nil)
        UserDefaults().removePersistentDomain(forName: suiteName)
        try await super.tearDown()
    }

    private struct Harness {
        let repository: AuthRepository
        let source: ScriptedBackupSource
        let persistence: FlippablePersistence
        let sessionStore: SessionStore
        let host: String
    }

    /// An auth service that answers the TOTP recovery for this account and
    /// the login-handle bind, and nothing else.
    private func authHost() -> String {
        let id = base64UrlEncodeNoPad(accountId)
        return NameRouteStub.scope { method, path, _ in
            if path == "/totp/recover" {
                return (200, #"{"account_id_b64":"\#(id)","session_token":"tok-totp","session_expires_at_ms":9000000,"refresh_token":"r1.t","refresh_expires_at_ms":9000000000,"key_generation":1}"#)
            }
            if method == "PUT", path == "/auth/login-handle" { return (200, #"{"bound":true}"#) }
            return (503, "")
        }
    }

    private func repository(
        host: String,
        source: ScriptedBackupSource,
        persistence: FlippablePersistence,
        sessionStore: SessionStore
    ) throws -> AuthRepository {
        let directory = PublishedIdentityDirectory()
        directory.publishedKey = try Identity.fromSeed(seed: seed).signaturePublicKey()
        return AuthRepository(
            sessionStore: sessionStore,
            authClientFactory: { NameRouteStub.client(host) },
            persistence: persistence,
            nowMs: { 1_000_000 },
            accountDirectory: QuietAccountDirectory(),
            loginHandleBinds: LoginHandleBindLedger(),
            restoreDirectory: directory,
            pushBinding: PushAccountBindingStore(defaults: defaults),
            removeDeliveredNotifications: {},
            backupRestoreSource: source,
            backupDefaults: defaults,
            earlierKeysInputs: skipEarlierKeysGate
        )
    }

    /// A fresh device that signed in with an ACCESS factor and is parked at
    /// the recovery-phrase prompt.
    private func parkedAtThePhrasePrompt(outcomes: [RestoreOutcome]) async throws -> Harness {
        let source = ScriptedBackupSource()
        source.outcomes = outcomes
        let persistence = FlippablePersistence()
        let sessionStore = SessionStore(defaults: defaults)
        let host = authHost()
        let repo = try repository(
            host: host, source: source, persistence: persistence, sessionStore: sessionStore)
        try await repo.recoverWithTotp(username: "alice", code: "123456")
        XCTAssertEqual(repo.state, .needsRecoveryPhrase(
            SessionAccount(accountIdB64: accountIdB64, username: "alice")))
        XCTAssertTrue(source.calls.isEmpty, "no backup fetch before the phrase is in hand")
        return Harness(
            repository: repo, source: source, persistence: persistence,
            sessionStore: sessionStore, host: host)
    }

    private func waitUntil(_ condition: () -> Bool, timeout: TimeInterval = 5) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        return condition()
    }

    private var landed: AuthState {
        .session(SessionAccount(accountIdB64: accountIdB64, username: "alice"))
    }

    func testThePhrasePromptFetchesAndAppliesTheBackup() async throws {
        let g1 = Data(repeating: 0x61, count: 32)
        let g2 = Data(repeating: 0x62, count: 32)
        let h = try await parkedAtThePhrasePrompt(outcomes: [
            .restored(try parsedBackup(
                accountId: accountId,
                contacts: [contactRecord(0x01, "Alice")],
                groupIds: [g1, g2],
                syncedState: try syncedStateBytes(pinning: g1))),
        ])

        // What is already true while the fetch is in flight.
        var owedDuringFetch: Bool?
        var phraseCachedDuringFetch: Bool?
        var landedDuringFetch: Bool?
        h.source.duringFetch = { [defaults, accountIdB64, repository = h.repository] in
            owedDuringFetch = BackupRestoreRetryStore.isPending(
                accountIdB64: accountIdB64, defaults: defaults!)
            phraseCachedDuringFetch =
                ((try? PhraseStore().getPhrase(accountIdB64: accountIdB64)) ?? nil) != nil
            if case .session = repository.state { landedDuringFetch = true } else { landedDuringFetch = false }
        }

        // Typed with a line break and doubled spaces: the mnemonic parser
        // accepts it, and the backup is opened under the canonical bytes.
        let typed = "  " + phrase.replacingOccurrences(of: " ", with: "  \n") + " "
        try await h.repository.provideRecoveryPhrase(mnemonicPhrase: typed)

        XCTAssertEqual(owedDuringFetch, true, "the restore is owed before the session lands")
        XCTAssertEqual(phraseCachedDuringFetch, true, "and the phrase a later attempt needs is cached")
        XCTAssertEqual(landedDuringFetch, false)
        XCTAssertEqual(h.repository.state, landed)
        XCTAssertEqual(h.source.calls.count, 1, "the route fetched the backup once")
        XCTAssertEqual(h.source.calls.first?.sessionToken, "tok-totp")
        XCTAssertEqual(h.source.calls.first?.accountIdB64, accountIdB64)
        XCTAssertEqual(h.source.calls.first?.phraseBytes, Data(phrase.utf8))

        XCTAssertEqual(
            AcceptedContactsStore.load(accountIdB64: accountIdB64).compactMap(\.displayName), ["Alice"])
        XCTAssertEqual(
            try SyncedStateStore.load(accountIdB64: accountIdB64).flags(groupId: g1)?.pinned, true)
        XCTAssertEqual(
            RestoredKnownGroupsStore.load(accountIdB64: accountIdB64, defaults: defaults),
            [hex(g1), hex(g2)])
        XCTAssertFalse(BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults))

        // The device keeps the phrase, so its own backups can be sealed.
        XCTAssertEqual(try PhraseStore().getPhrase(accountIdB64: accountIdB64), Data(phrase.utf8))
        // And no MLS state came with the backup.
        XCTAssertNil(IdentitySnapshotStore().getSnapshot(accountIdB64: accountIdB64))
        XCTAssertTrue(KnownGroupsStore().load(accountIdB64: accountIdB64).isEmpty)
    }

    /// ADR-0074 section 3.7 — the route stores the predecessors BEFORE the
    /// session lands, so the first binding is minted under the oldest.
    func testThePhrasePromptStoresTheOwnPredecessorsBeforeTheSessionLands() async throws {
        let oldest = Data(repeating: 0xA0, count: 64)
        var parsed = try parsedBackup(accountId: accountId)
        parsed.ownPredecessors = [BackupOwnPredecessor(seed: oldest, adoptedAtMs: 1_000)]
        let h = try await parkedAtThePhrasePrompt(outcomes: [.restored(parsed)])
        defer { KeychainOwnPredecessorStore().clear(accountIdB64: accountIdB64) }

        try await h.repository.provideRecoveryPhrase(mnemonicPhrase: phrase)

        XCTAssertEqual(h.repository.state, landed)
        XCTAssertEqual(KeychainOwnPredecessorStore().load(accountIdB64: accountIdB64), [oldest])
    }

    func testAnAccountWithNoBackupSignsInAndOwesNoRetry() async throws {
        let h = try await parkedAtThePhrasePrompt(outcomes: [.noBackup])

        try await h.repository.provideRecoveryPhrase(mnemonicPhrase: phrase)

        XCTAssertEqual(h.repository.state, landed)
        XCTAssertEqual(h.source.calls.count, 1)
        XCTAssertFalse(
            BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults),
            "a missing backup is not an error")
        XCTAssertTrue(AcceptedContactsStore.load(accountIdB64: accountIdB64).isEmpty)
    }

    func testARefusedBackupSignsInAppliesNothingAndRaisesTheAlarm() async throws {
        let h = try await parkedAtThePhrasePrompt(outcomes: [
            .rollbackBlocked(hard: true, reason: "rolled back"),
        ])

        try await h.repository.provideRecoveryPhrase(mnemonicPhrase: phrase)

        XCTAssertEqual(h.repository.state, landed)
        XCTAssertEqual(h.repository.backupRollbackAlarm, BackupRollbackAlarm(reason: "rolled back"))
        XCTAssertTrue(AcceptedContactsStore.load(accountIdB64: accountIdB64).isEmpty)
        XCTAssertFalse(BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults))
    }

    func testAFailedFetchSignsInAndIsRetriedAtTheNextLanding() async throws {
        let g1 = Data(repeating: 0x71, count: 32)
        let h = try await parkedAtThePhrasePrompt(outcomes: [
            .transportFailed(URLError(.notConnectedToInternet)),
            .restored(try parsedBackup(
                accountId: accountId,
                contacts: [contactRecord(0x03, "Carol")],
                groupIds: [g1])),
        ])

        try await h.repository.provideRecoveryPhrase(mnemonicPhrase: phrase)

        XCTAssertEqual(h.repository.state, landed, "a fetch failure does not fail the sign-in")
        XCTAssertEqual(h.source.calls.count, 1)
        XCTAssertTrue(
            BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults),
            "a failed fetch is not an answer: the restore stays owed")
        XCTAssertTrue(AcceptedContactsStore.load(accountIdB64: accountIdB64).isEmpty)
        // The landing that failed does not retry itself.
        try? await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertEqual(h.source.calls.count, 1)

        // The next landing: a relaunch of a device that now holds the account.
        h.persistence.deviceKey = true
        h.persistence.seed = seed
        XCTAssertNotNil(h.persistence.tokens[accountIdB64], "the session token was persisted")
        let relaunched = try repository(
            host: h.host, source: h.source, persistence: h.persistence, sessionStore: h.sessionStore)
        XCTAssertEqual(relaunched.state, landed)

        let retried = await waitUntil {
            !AcceptedContactsStore.load(accountIdB64: self.accountIdB64).isEmpty
        }
        XCTAssertTrue(retried, "the next landing retried the restore")
        XCTAssertEqual(h.source.calls.count, 2)
        XCTAssertEqual(
            h.source.calls.last?.phraseBytes, Data(phrase.utf8),
            "the retry opens the blob with the phrase cached on this device")
        XCTAssertEqual(
            AcceptedContactsStore.load(accountIdB64: accountIdB64).compactMap(\.displayName), ["Carol"])
        XCTAssertEqual(
            RestoredKnownGroupsStore.load(accountIdB64: accountIdB64, defaults: defaults), [hex(g1)])
        XCTAssertFalse(BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults))

        // Applied is a definitive answer: a further landing has nothing owed.
        let again = await relaunched.runMarkedBackupRestoreRetry(
            accountId: accountId, accountIdB64: accountIdB64, sessionToken: "tok-totp", context: "test")
        XCTAssertNil(again)
        XCTAssertEqual(h.source.calls.count, 2)
    }

    /// A device that holds the account, as a relaunch finds it.
    private func relaunch(_ h: Harness) throws -> AuthRepository {
        h.persistence.deviceKey = true
        h.persistence.seed = seed
        return try repository(
            host: h.host, source: h.source, persistence: h.persistence, sessionStore: h.sessionStore)
    }

    private final class UploadRecorder {
        var uploads: [AutoBackupDriver.Upload] = []
    }

    /// The automatic backup of this account, over the same bookkeeping suite.
    private func backupDriver(_ recorder: UploadRecorder) -> AutoBackupDriver {
        let accountId = accountId
        let accountIdB64 = accountIdB64
        let seed = seed
        return AutoBackupDriver(
            inputs: .init(
                accountId: { accountId },
                accountIdB64: { accountIdB64 },
                sessionToken: { "tok-totp" },
                accountIdentitySeed: { seed },
                knownGroupIds: { [] }
            ),
            uploader: { upload in
                recorder.uploads.append(upload)
                return .uploaded
            },
            defaults: defaults
        )
    }

    func testAFailingRetryStaysOwedHoldsUploadsAndIsTriedAgainAtTheNextLanding() async throws {
        let h = try await parkedAtThePhrasePrompt(outcomes: [
            .transportFailed(URLError(.timedOut)),
            .transportFailed(URLError(.timedOut)),
            .restored(try parsedBackup(accountId: accountId, contacts: [contactRecord(0x05, "Eve")])),
        ])
        try await h.repository.provideRecoveryPhrase(mnemonicPhrase: phrase)
        XCTAssertTrue(BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults))

        // The next landing's attempt fails too.
        let second = await h.repository.runMarkedBackupRestoreRetry(
            accountId: accountId, accountIdB64: accountIdB64, sessionToken: "tok-totp", context: "test")
        XCTAssertEqual(second, .fetchFailed)
        XCTAssertEqual(h.source.calls.count, 2)
        XCTAssertTrue(
            BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults),
            "a retry that fails leaves the restore owed")

        // And this device's own backup is held back, so it cannot replace the
        // blob the restore is still going to fetch.
        let recorder = UploadRecorder()
        let driver = backupDriver(recorder)
        let held = await driver.runOnce(nowMs: 1_000)
        XCTAssertEqual(held, .notReady)
        XCTAssertTrue(recorder.uploads.isEmpty)

        // The landing after that: a relaunch tries again and gets an answer.
        let relaunched = try relaunch(h)
        XCTAssertEqual(relaunched.state, landed)
        let applied = await waitUntil {
            !BackupRestoreRetryStore.isPending(accountIdB64: self.accountIdB64, defaults: self.defaults)
        }
        XCTAssertTrue(applied, "the next landing tried again")
        XCTAssertEqual(h.source.calls.count, 3, "one attempt per landing")
        XCTAssertEqual(
            AcceptedContactsStore.load(accountIdB64: accountIdB64).compactMap(\.displayName), ["Eve"])

        let released = await driver.runOnce(nowMs: 2_000)
        XCTAssertEqual(released, .uploaded, "with the restore answered, uploads go out again")
        XCTAssertEqual(recorder.uploads.count, 1)
        XCTAssertEqual(
            recorder.uploads.first?.contacts.map(\.displayName), ["Eve"],
            "and the upload carries what the restore brought")
    }

    /// The mark is set before anything is fetched, so an app killed while the
    /// fetch was in flight comes back owing the restore.
    func testAnAppKilledMidRestoreIsTriedAgainAtTheNextLanding() async throws {
        // What the killed process left: the cached phrase, the mark, and a
        // device that holds the account. Nothing was applied.
        try PhraseStore().putPhrase(accountIdB64: accountIdB64, phraseBytes: Data(phrase.utf8))
        BackupRestoreRetryStore.mark(accountIdB64: accountIdB64, defaults: defaults)
        let source = ScriptedBackupSource()
        source.outcomes = [
            .restored(try parsedBackup(accountId: accountId, contacts: [contactRecord(0x06, "Faye")])),
        ]
        let persistence = FlippablePersistence()
        persistence.deviceKey = true
        persistence.seed = seed
        persistence.tokens[accountIdB64] = PersistedSessionToken(
            sessionToken: "tok-relaunch", sessionExpiresAtMs: 9_000_000)
        let sessionStore = SessionStore(defaults: defaults)
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: "alice"))

        let repo = try repository(
            host: authHost(), source: source, persistence: persistence, sessionStore: sessionStore)
        XCTAssertEqual(repo.state, landed)

        let applied = await waitUntil {
            !AcceptedContactsStore.load(accountIdB64: self.accountIdB64).isEmpty
        }
        XCTAssertTrue(applied)
        XCTAssertEqual(source.calls.count, 1)
        XCTAssertEqual(source.calls.first?.sessionToken, "tok-relaunch")
        XCTAssertFalse(BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults))
    }

    func testEveryDefinitiveAnswerClearsTheOwedRestore() async throws {
        let definitive: [(String, RestoreOutcome, PhraseRestoreBackupOutcome)] = [
            ("applied", .restored(try parsedBackup(accountId: accountId)), .applied(AppliedBackup())),
            ("no backup", .noBackup, .noBackup),
            ("head gate refused", .rollbackBlocked(hard: true, reason: "tampered"), .refused),
            ("stale, declined", .rollbackBlocked(hard: false, reason: "stale"), .refused),
            ("wrong phrase or unreadable payload", .wrongPhraseOrTampered, .refused),
        ]
        try PhraseStore().putPhrase(accountIdB64: accountIdB64, phraseBytes: Data(phrase.utf8))
        for (name, served, expected) in definitive {
            let source = ScriptedBackupSource()
            source.outcomes = [served]
            let repo = try repository(
                host: authHost(), source: source, persistence: FlippablePersistence(),
                sessionStore: SessionStore(defaults: defaults))
            BackupRestoreRetryStore.mark(accountIdB64: accountIdB64, defaults: defaults)

            let outcome = await repo.runMarkedBackupRestoreRetry(
                accountId: accountId, accountIdB64: accountIdB64, sessionToken: "tok", context: "test")

            XCTAssertEqual(outcome, expected, name)
            XCTAssertEqual(source.calls.count, 1, name)
            XCTAssertFalse(
                BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults),
                "\(name) is a definitive answer")
        }
    }

    func testADeviceWithNoCachedPhraseOwesNothing() async throws {
        let source = ScriptedBackupSource()
        let repo = try repository(
            host: authHost(), source: source, persistence: FlippablePersistence(),
            sessionStore: SessionStore(defaults: defaults))
        BackupRestoreRetryStore.mark(accountIdB64: accountIdB64, defaults: defaults)

        let outcome = await repo.runMarkedBackupRestoreRetry(
            accountId: accountId, accountIdB64: accountIdB64, sessionToken: "tok", context: "test")

        XCTAssertNil(outcome)
        XCTAssertTrue(source.calls.isEmpty, "nothing on this device could open the blob")
        XCTAssertFalse(BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults))
    }

    /// The shared function the restore screen and the phrase prompt both run.
    func testTheSharedRestoreMapsEveryOutcomeWithoutThrowing() async throws {
        let source = ScriptedBackupSource()
        source.outcomes = [
            .noBackup,
            .wrongPhraseOrTampered,
            .rollbackBlocked(hard: false, reason: "stale, declined"),
            .transportFailed(URLError(.cannotFindHost)),
            .restored(try parsedBackup(accountId: accountId, contacts: [contactRecord(0x04, "Dave")])),
        ]
        let repo = try repository(
            host: authHost(), source: source, persistence: FlippablePersistence(),
            sessionStore: SessionStore(defaults: defaults))
        var outcomes: [PhraseRestoreBackupOutcome] = []
        var owedAfter: [Bool] = []
        for _ in 0..<5 {
            outcomes.append(await repo.restoreBackupFromPhrase(
                accountId: accountId, accountIdB64: accountIdB64, sessionToken: "tok",
                phrase: phrase, context: "test", writes: SignInKeyWrites()))
            owedAfter.append(
                BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: defaults))
        }
        XCTAssertEqual(outcomes, [
            .noBackup, .refused, .refused, .fetchFailed,
            .applied(AppliedBackup(contactsAdded: 1, syncedStateMerged: false, knownGroups: 0)),
        ])
        XCTAssertNil(repo.backupRollbackAlarm, "a declined stale backup is not an alarm")
        XCTAssertEqual(
            owedAfter, [false, false, false, true, false],
            "only the failed fetch leaves the restore owed")
    }
}

// MARK: - D. A backup always lists the conversations

final class RestoredKnownGroupsStoreTests: XCTestCase {

    private let account = "restored-groups-account"
    private var suiteName = ""
    private var defaults: UserDefaults!
    private let g1 = String(repeating: "a1", count: 32)
    private let g2 = String(repeating: "b2", count: 32)
    private let g3 = String(repeating: "c3", count: 32)

    override func setUp() {
        super.setUp()
        suiteName = "restored-groups-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)!
    }

    override func tearDown() {
        UserDefaults().removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    func testAddIsAdditiveNormalisedAndIgnoresWhatIsNotAGroupId() {
        RestoredKnownGroupsStore.add(accountIdB64: account, groupIdsHex: [g1], defaults: defaults)
        RestoredKnownGroupsStore.add(
            accountIdB64: account, groupIdsHex: [g2.uppercased(), "abcd", "zz" + g3.dropFirst(2)],
            defaults: defaults)
        XCTAssertEqual(RestoredKnownGroupsStore.load(accountIdB64: account, defaults: defaults), [g1, g2])
        XCTAssertTrue(
            RestoredKnownGroupsStore.load(accountIdB64: "another", defaults: defaults).isEmpty,
            "per account")
    }

    func testAnUploadCarriesOwnGroupsPlusTheRestoredOnesNotJoinedYet() {
        XCTAssertEqual(
            RestoredKnownGroupsStore.uploadGroupIds(own: [g3], restored: [g1, g2]), [g1, g2, g3])
        XCTAssertEqual(
            RestoredKnownGroupsStore.uploadGroupIds(own: [g1, g3], restored: [g1, g2]), [g1, g2, g3],
            "a group both held and restored is carried once")
        XCTAssertEqual(RestoredKnownGroupsStore.uploadGroupIds(own: [], restored: []), [])
    }

    func testRemovePruneAndClear() {
        RestoredKnownGroupsStore.add(accountIdB64: account, groupIdsHex: [g1, g2, g3], defaults: defaults)
        RestoredKnownGroupsStore.remove(accountIdB64: account, groupIdHex: g1, defaults: defaults)
        XCTAssertEqual(RestoredKnownGroupsStore.load(accountIdB64: account, defaults: defaults), [g2, g3])
        RestoredKnownGroupsStore.prune(
            accountIdB64: account, heldGroupIdsHex: [g2.uppercased(), "unrelated"], defaults: defaults)
        XCTAssertEqual(RestoredKnownGroupsStore.load(accountIdB64: account, defaults: defaults), [g3])
        RestoredKnownGroupsStore.clear(accountIdB64: account, defaults: defaults)
        XCTAssertTrue(RestoredKnownGroupsStore.load(accountIdB64: account, defaults: defaults).isEmpty)
        XCTAssertNil(defaults.object(forKey: RestoredKnownGroupsStore.key(accountIdB64: account)))
    }
}

@MainActor
final class BackupUploadCarriesConversationsTests: XCTestCase {

    private var accountId = Data()
    private var accountIdB64 = ""
    private var suiteName = ""
    private var defaults: UserDefaults!
    private let seed = Data(repeating: 0x21, count: 32)

    override func setUp() async throws {
        try await super.setUp()
        accountId = randomBytes(16)
        accountIdB64 = base64Encode(accountId)
        suiteName = "backup-upload-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)!
    }

    override func tearDown() async throws {
        cleanAccount(accountIdB64)
        UserDefaults().removePersistentDomain(forName: suiteName)
        try await super.tearDown()
    }

    private final class Recorder {
        var uploads: [AutoBackupDriver.Upload] = []
        var own: [String]? = []
    }

    private func driver(_ recorder: Recorder) -> AutoBackupDriver {
        let accountId = accountId
        let accountIdB64 = accountIdB64
        let seed = seed
        return AutoBackupDriver(
            inputs: .init(
                accountId: { accountId },
                accountIdB64: { accountIdB64 },
                sessionToken: { "tok" },
                accountIdentitySeed: { seed },
                knownGroupIds: { recorder.own }
            ),
            uploader: { upload in
                recorder.uploads.append(upload)
                return .uploaded
            },
            defaults: defaults
        )
    }

    func testTheFirstUploadAfterARestoreStillNamesTheRestoredConversations() async throws {
        let g1 = Data(repeating: 0x81, count: 32)
        let g2 = Data(repeating: 0x82, count: 32)
        let own = hex(Data(repeating: 0x83, count: 32))
        PhraseRestoreBackup.apply(
            try parsedBackup(accountId: accountId, groupIds: [g1, g2]),
            accountIdB64: accountIdB64, context: "test", defaults: defaults)

        let recorder = Recorder()
        recorder.own = [own]
        let driver = driver(recorder)
        let first = await driver.runOnce(nowMs: 1_000)

        XCTAssertEqual(first, .uploaded)
        XCTAssertEqual(recorder.uploads.count, 1)
        XCTAssertEqual(recorder.uploads[0].knownGroupIdsHex, [hex(g1), hex(g2), own].sorted())

        // The device joins G1: it leaves the restored set and is carried as
        // one of the device's own groups from then on.
        RestoredKnownGroupsStore.remove(
            accountIdB64: accountIdB64, groupIdHex: hex(g1), defaults: defaults)
        recorder.own = [own, hex(g1)]
        let second = await driver.runOnce(nowMs: 1_000 + AutoBackup.minIntervalMs + 1)

        XCTAssertEqual(second, .uploaded)
        XCTAssertEqual(recorder.uploads[1].knownGroupIdsHex, [hex(g1), hex(g2), own].sorted())
        XCTAssertEqual(
            RestoredKnownGroupsStore.load(accountIdB64: accountIdB64, defaults: defaults), [hex(g2)])
    }

    /// The payload a restored device with no groups of its own would seal.
    func testTheSealedPayloadOfARestoredDeviceWithNoGroupsNamesTheRestoredOnes() async throws {
        let g1 = Data(repeating: 0x91, count: 32)
        PhraseRestoreBackup.apply(
            try parsedBackup(accountId: accountId, groupIds: [g1]),
            accountIdB64: accountIdB64, context: "test", defaults: defaults)
        let recorder = Recorder()
        let outcome = await driver(recorder).runOnce(nowMs: 1_000)
        XCTAssertEqual(outcome, .uploaded)
        let upload = try XCTUnwrap(recorder.uploads.first)
        let wire = try BackupClient.composePayload(
            accountId: upload.accountId, createdAtMs: 1, ed25519Seed: upload.ed25519Seed,
            contacts: upload.contacts, knownGroupIdsHex: upload.knownGroupIdsHex,
            syncedState: upload.syncedState)
        XCTAssertEqual(try parseBackupPayload(wire: wire).knownGroupIds, [g1])
    }

    /// ADR-0050: the payload's `ed25519Seed` is the ACCOUNT identity's (the
    /// phrase-derived one), never the per-device leaf identity the chats run
    /// on. The driver derives it from the account identity seed it is given
    /// and holds no chat-layer identity at all.
    func testTheUploadCarriesTheAccountIdentitySeedNotADeviceIdentitys() async throws {
        let recorder = Recorder()
        let outcome = await driver(recorder).runOnce(nowMs: 1_000)
        XCTAssertEqual(outcome, .uploaded)
        let upload = try XCTUnwrap(recorder.uploads.first)
        let account = try Identity.fromSeed(seed: seed)
        XCTAssertEqual(upload.ed25519Seed, account.ed25519Seed())
        let device = try Identity.fromSeed(seed: Data(repeating: 0x77, count: 32))
        XCTAssertNotEqual(upload.ed25519Seed, device.ed25519Seed())
    }

    func testNothingIsUploadedBeforeTheChatLayerHasLoaded() async {
        let recorder = Recorder()
        recorder.own = nil
        AutoBackupStore.markDirty(accountIdB64: accountIdB64, nowMs: 1, defaults: defaults)
        let outcome = await driver(recorder).runOnce(nowMs: 1_000)
        XCTAssertEqual(outcome, .notReady)
        XCTAssertTrue(recorder.uploads.isEmpty)
        XCTAssertNotNil(
            AutoBackupStore.load(accountIdB64: accountIdB64, defaults: defaults).dirtySinceMs,
            "the change is still owed a backup")
    }

    /// An upload now would replace the blob the owed retry is going to fetch.
    func testNothingIsUploadedWhileARestoreRetryIsOwed() async {
        BackupRestoreRetryStore.mark(accountIdB64: accountIdB64, defaults: defaults)
        let recorder = Recorder()
        let driver = driver(recorder)
        let held = await driver.runOnce(nowMs: 1_000)
        XCTAssertEqual(held, .notReady)
        XCTAssertTrue(recorder.uploads.isEmpty)

        BackupRestoreRetryStore.clear(accountIdB64: accountIdB64, defaults: defaults)
        let released = await driver.runOnce(nowMs: 2_000)
        XCTAssertEqual(released, .uploaded)
        XCTAssertEqual(recorder.uploads.count, 1)
    }
}

/// The chat layer drops an id from the restored set when the device holds the
/// group, when the user deletes the chat, and the wipe / log out rules.
@MainActor
final class RestoredKnownGroupsLifecycleTests: XCTestCase {

    private var suiteName = ""
    private var defaults: UserDefaults!
    private var accounts: [String] = []
    private var outboxDirs: [URL] = []

    override func setUp() async throws {
        try await super.setUp()
        suiteName = "restored-life-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)!
    }

    override func tearDown() async throws {
        for account in accounts { cleanAccount(account) }
        for dir in outboxDirs { try? FileManager.default.removeItem(at: dir) }
        accounts = []
        outboxDirs = []
        UserDefaults().removePersistentDomain(forName: suiteName)
        try await super.tearDown()
    }

    private struct Fixture {
        let repo: ChatRepository
        let groupIdHex: String
        let accountIdB64: String
    }

    /// A repository restarted from `identity`'s snapshot, holding `group`.
    /// `restoredBeforeBootstrap` is what a restore named before the chat
    /// layer came up.
    private func bootstrapped(
        identity: Identity,
        seed: Data,
        group: Group,
        restoredBeforeBootstrap: [String]
    ) throws -> Fixture {
        let groupIdHex = DsClient.pseudonymHex(try group.groupId())
        let accountIdBytes = randomBytes(16)
        let accountIdB64 = base64Encode(accountIdBytes)
        accounts.append(accountIdB64)
        let suffix = UUID().uuidString

        let snapshotStore = IdentitySnapshotStore()
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: try identity.snapshotStorage())
        let knownGroupsStore = KnownGroupsStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "restored-life-k-\(suffix)")))
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("restored-life-\(suffix)")
        try FileManager.default.createDirectory(at: outboxDir, withIntermediateDirectories: true)
        outboxDirs.append(outboxDir)

        RestoredKnownGroupsStore.add(
            accountIdB64: accountIdB64, groupIdsHex: restoredBeforeBootstrap, defaults: defaults)

        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [ScriptedDsURLProtocol.self]
        let ds = DsClient(
            baseURL: ScriptedDsURLProtocol.baseURL(ScriptedDsURLProtocol.freshScope()),
            session: URLSession(configuration: config))
        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: DiscoveryClient(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir),
            pushRouting: PushGroupRouting(),
            beaconStore: BeaconObservationStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "restored-life-b-\(suffix)"))),
            pendingRequestStore: PendingRequestStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "restored-life-p-\(suffix)"))),
            lastObservedOwnerStore: LastObservedOwnerStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "restored-life-o-\(suffix)"))),
            leaveRequestStore: LeaveRequestStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "restored-life-l-\(suffix)"))))
        repo.restoredGroupsDefaults = defaults
        // The chats run on the identity the snapshot was taken under.
        repo.deviceIdentityInstaller = { _, _ in nil }
        repo.bootstrap(secrets: IdentitySecrets(
            accountId: accountIdBytes,
            exportKey: Data(repeating: 0x22, count: 32),
            accountIdentitySeed: seed,
            sessionToken: "t",
            sessionExpiresAtMs: 0))
        XCTAssertNotNil(repo.groups[groupIdHex], "the fixture holds the group")
        return Fixture(repo: repo, groupIdHex: groupIdHex, accountIdB64: accountIdB64)
    }

    private let other1 = String(repeating: "d4", count: 32)
    private let other2 = String(repeating: "e5", count: 32)

    private func restored(_ f: Fixture) -> Set<String> {
        RestoredKnownGroupsStore.load(accountIdB64: f.accountIdB64, defaults: defaults)
    }

    /// A live two-member group, seen from its founder.
    private func heldGroup(restoredBeforeBootstrap: (String) -> [String]) throws -> Fixture {
        let seed = Data(repeating: 0x21, count: 32)
        let founder = try Identity.fromSeed(seed: seed)
        let peer = try Identity.fromSeed(seed: Data(repeating: 0x65, count: 32))
        let group = try Group.create(identity: founder, kind: .group)
        _ = try group.addMemberFromWire(identity: founder, keyPackage: try peer.createKeyPackage())
        let groupIdHex = DsClient.pseudonymHex(try group.groupId())
        return try bootstrapped(
            identity: founder, seed: seed, group: group,
            restoredBeforeBootstrap: restoredBeforeBootstrap(groupIdHex))
    }

    func testAGroupTheDeviceAlreadyHoldsLeavesTheRestoredSetWhenTheChatLayerLoads() throws {
        let f = try heldGroup { held in [held, self.other1] }
        XCTAssertEqual(restored(f), [other1], "the held group is no longer 'restored, not joined'")
    }

    func testJoiningAGroupDropsItFromTheRestoredSet() throws {
        let f = try heldGroup { _ in [] }
        RestoredKnownGroupsStore.add(
            accountIdB64: f.accountIdB64, groupIdsHex: [other1, other2], defaults: defaults)

        // What every found / join path runs once the group is in hand.
        f.repo.persistKnownGroupBestEffort(groupIdHex: other1)

        XCTAssertEqual(restored(f), [other2])
    }

    /// A member the owner removed: the local group has ended, and the user
    /// deletes the chat.
    func testDeletingAnEndedChatDropsItFromTheRestoredSet() throws {
        let ownerSeed = Data(repeating: 0x11, count: 32)
        let memberSeed = Data(repeating: 0x55, count: 32)
        let owner = try Identity.fromSeed(seed: ownerSeed)
        let member = try Identity.fromSeed(seed: memberSeed)
        let ownerGroup = try Group.create(identity: owner, kind: .group)
        let added = try ownerGroup.addMemberFromWire(
            identity: owner, keyPackage: try member.createKeyPackage())
        let memberGroup = try Group.joinFromWelcome(welcome: added.welcome, identity: member)
        let memberKey = member.signaturePublicKey()
        let memberLeaf = try XCTUnwrap(
            try ownerGroup.members().first { $0.signaturePublicKey == memberKey }?.leafIndex)
        let proposal = try ownerGroup.proposeRemoveMember(identity: owner, leafIndex: memberLeaf)
        _ = try memberGroup.processIncomingVerifyingRoles(identity: member, bytes: proposal)
        let swept = try XCTUnwrap(try ownerGroup.commitPendingProposals(identity: owner))
        _ = try memberGroup.processIncomingVerifyingRoles(identity: member, bytes: swept.commit)
        XCTAssertFalse(try memberGroup.isActive(), "the fixture's membership has ended")

        let f = try bootstrapped(
            identity: member, seed: memberSeed, group: memberGroup, restoredBeforeBootstrap: [])
        // A restore that ran while the ended chat was still on the device
        // named it again.
        RestoredKnownGroupsStore.add(
            accountIdB64: f.accountIdB64, groupIdsHex: [f.groupIdHex, other1], defaults: defaults)

        XCTAssertTrue(f.repo.deleteChat(groupIdHex: f.groupIdHex))

        XCTAssertEqual(restored(f), [other1], "a deleted chat is not carried in this device's backups")
    }

    func testTheWipeClearsBothStoresAndLogOutKeepsThem() {
        let accountIdB64 = base64Encode(randomBytes(16))
        accounts.append(accountIdB64)
        let group = String(repeating: "f6", count: 32)
        RestoredKnownGroupsStore.add(accountIdB64: accountIdB64, groupIdsHex: [group])
        BackupRestoreRetryStore.mark(accountIdB64: accountIdB64)

        // Log out: a repository holding this account's session signs out.
        let sessionStore = SessionStore(defaults: defaults)
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: "erin"))
        let persistence = FlippablePersistence()
        persistence.deviceKey = true
        persistence.seed = Data(repeating: 0x33, count: AccountIdentitySeedStore.seedLength)
        persistence.tokens[accountIdB64] = PersistedSessionToken(
            sessionToken: "tok", sessionExpiresAtMs: 9_000_000)
        // The landing must not consume the retry mark this test is about, so
        // the repository's own bookkeeping lives in the transient suite.
        let host = NameRouteStub.scope { _, _, _ in (503, "") }
        let repo = AuthRepository(
            sessionStore: sessionStore,
            authClientFactory: { NameRouteStub.client(host) },
            persistence: persistence,
            nowMs: { 1_000_000 },
            accountDirectory: QuietAccountDirectory(),
            loginHandleBinds: LoginHandleBindLedger(),
            // A silent restore checks the published identity (ADR-0073
            // section 4.2); the directory of this fixture is unreachable.
            restoreDirectory: OfflineRestoreDirectory(),
            pushBinding: PushAccountBindingStore(defaults: defaults),
            removeDeliveredNotifications: {},
            backupRestoreSource: ScriptedBackupSource(),
            backupDefaults: defaults,
            earlierKeysInputs: skipEarlierKeysGate
        )
        XCTAssertNotNil(repo.identitySecrets, "the fixture restores a session")
        repo.logout()
        XCTAssertNil(repo.identitySecrets)

        XCTAssertEqual(
            RestoredKnownGroupsStore.load(accountIdB64: accountIdB64), [group], "log out keeps the list")
        XCTAssertTrue(BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64), "and the retry mark")

        // Forget device / Delete account.
        let report = AccountLocalDataWipe.wipe(
            accountIdB64: accountIdB64, removeDeliveredNotifications: {})
        XCTAssertEqual(report.failedSteps, [])
        XCTAssertTrue(RestoredKnownGroupsStore.load(accountIdB64: accountIdB64).isEmpty)
        XCTAssertFalse(BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64))
        AcceptedContactsStore.bind(accountIdB64: nil)
    }
}

// MARK: - E. The "Synchronizing..." label

final class SynchronizingLabelRuleTests: XCTestCase {

    private var suiteName = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suiteName = "sync-label-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)!
    }

    override func tearDown() {
        UserDefaults().removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    @MainActor
    func testARestoredAccountWithAnEmptyChatListShowsNoLabel() throws {
        let accountId = randomBytes(16)
        let accountIdB64 = base64Encode(accountId)
        defer { cleanAccount(accountIdB64) }
        PhraseRestoreBackup.apply(
            try parsedBackup(
                accountId: accountId,
                groupIds: [Data(repeating: 0x41, count: 32), Data(repeating: 0x42, count: 32)]),
            accountIdB64: accountIdB64, context: "test", defaults: defaults)
        XCTAssertEqual(
            RestoredKnownGroupsStore.load(accountIdB64: accountIdB64, defaults: defaults).count, 2)

        // The label's whole rule is `pendingKnownGroups > 0`, read with no
        // chats joined: the conversations a backup named do not count.
        XCTAssertEqual(
            StateBundle.pendingKnownGroups(
                joinedGroupIdHexes: [], accountIdB64: accountIdB64, defaults: defaults),
            0)
        XCTAssertTrue(StateBundle.loadKnownGroupIds(accountIdB64: accountIdB64, defaults: defaults).isEmpty)
    }

    func testAGroupThePairingBundleNamedStillShowsTheLabelUntilItIsJoined() {
        let accountIdB64 = base64Encode(randomBytes(16))
        let group = Data(repeating: 0x43, count: 32)
        StateBundle.saveKnownGroupIds([base64Encode(group)], accountIdB64: accountIdB64, defaults: defaults)

        XCTAssertEqual(
            StateBundle.pendingKnownGroups(
                joinedGroupIdHexes: [], accountIdB64: accountIdB64, defaults: defaults),
            1)
        XCTAssertEqual(
            StateBundle.pendingKnownGroups(
                joinedGroupIdHexes: [hex(group)], accountIdB64: accountIdB64, defaults: defaults),
            0)
    }
}
