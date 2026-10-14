// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// The per-account stores that had no ``AccountStoreGate`` before: a write
/// for an account whose session ended (signed out, or wiped by Forget device)
/// is not made, and the stores still write for an open account. None of these
/// touches the Keychain. (The gate is reset before every test by
/// `StoreGateTestReset`.)
@MainActor
final class AccountStoreGateCoverageTests: XCTestCase {

    private let account = "coverage-account"
    private var suite = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suite = "lakko.tests.gate-coverage.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suite)
        super.tearDown()
    }

    // MARK: - The wipe mark

    /// What a sign-in writes before its session lands: refused only by a wipe
    /// AFTER the work started; a sign-out, an earlier wipe, or an account not
    /// reopened yet refuse nothing.
    func testAWipeMarkRefusesOnlyAWipeThatCameAfterIt() throws {
        let mark = AccountStoreGate.wipeMark(account)
        AccountStoreGate.seal(account)
        XCTAssertNotNil(
            AccountStoreGate.enter(unlessWipedSince: mark).map { $0.leave() },
            "a plain sign-out refuses nothing")

        AccountStoreGate.seal(account, wiping: true) {}
        XCTAssertNil(AccountStoreGate.enter(unlessWipedSince: mark))
        XCTAssertThrowsError(try AccountStoreGate.enterOrEnd(unlessWipedSince: mark)) { error in
            XCTAssertTrue(Cancellation.isCancellation(error))
        }
        AccountStoreGate.open(account)
        XCTAssertNil(
            AccountStoreGate.enter(unlessWipedSince: mark),
            "still refused after a landing: the work began before the wipe")

        let afterTheWipe = AccountStoreGate.wipeMark(account)
        AccountStoreGate.seal(account, wiping: true) {}
        let signInAfterForget = AccountStoreGate.wipeMark(account)
        XCTAssertNotNil(
            AccountStoreGate.enter(unlessWipedSince: signInAfterForget).map { $0.leave() },
            "a sign-in that starts after the wipe writes, sealed or not")
        XCTAssertNil(AccountStoreGate.enter(unlessWipedSince: afterTheWipe))
    }

    func testAStagedWipeCountsAsAWipe() {
        let mark = AccountStoreGate.wipeMark(account)
        AccountStoreGate.seal(account, wiping: true, staged: true) {}
        XCTAssertNil(AccountStoreGate.enter(unlessWipedSince: mark))
    }

    func testAWipeOfAnotherAccountRefusesNothing() {
        let mark = AccountStoreGate.wipeMark(account)
        AccountStoreGate.seal("another-account", wiping: true) {}
        XCTAssertNotNil(AccountStoreGate.enter(unlessWipedSince: mark).map { $0.leave() })
    }

    // MARK: - The founder's row (startGroupWith)

    func testTheFoundingRowGoesOnlyWhenNoWelcomeWentOut() {
        XCTAssertTrue(FoundingRollback.forgetsRow(welcomesDelivered: 0))
        XCTAssertFalse(FoundingRollback.forgetsRow(welcomesDelivered: 1))
        XCTAssertFalse(FoundingRollback.forgetsRow(welcomesDelivered: 2))
    }

    func testAFounderRowIsRefusedForASealedAccountAndWrittenForAnOpenOne() throws {
        let store = KnownGroupsStore(defaults: defaults)
        try store.addOrEnd(accountIdB64: account, groupIdHex: "aa")
        XCTAssertEqual(store.load(accountIdB64: account), ["aa"])

        AccountStoreGate.seal(account)
        XCTAssertThrowsError(try store.addOrEnd(accountIdB64: account, groupIdHex: "bb")) { error in
            XCTAssertTrue(error is AccountStoreGate.Sealed)
        }
        XCTAssertEqual(store.load(accountIdB64: account), ["aa"], "nothing was written")
    }

    // MARK: - Contacts and profiles

    func testAcceptedContactsAreNotSavedForASealedAccount() {
        let saved = AcceptedContactsStore.defaults
        AcceptedContactsStore.defaults = defaults
        defer { AcceptedContactsStore.defaults = saved }

        AccountStoreGate.seal(account, wiping: true) {}
        AcceptedContactsStore.save([], accountIdB64: account)
        XCTAssertNil(defaults.data(forKey: AcceptedContactsStore.storageKey(accountIdB64: account)))

        AccountStoreGate.open(account)
        AcceptedContactsStore.save([], accountIdB64: account)
        XCTAssertNotNil(defaults.data(forKey: AcceptedContactsStore.storageKey(accountIdB64: account)))
    }

    func testTheOwnProfileAndReceivedProfilesAreNotWrittenForASealedAccount() {
        let own = OwnProfileStore(defaults: defaults)
        let received = UserDefaultsProfilePersistence(defaults: defaults)
        AccountStoreGate.seal(account)

        own.save(displayName: "Mira", accountIdB64: account, nowMs: 10)
        own.store(OwnProfile(displayName: "Mira", updatedAtMs: 11), accountIdB64: account)
        received.save(Data([1]), accountIdB64: account)
        XCTAssertNil(own.load(accountIdB64: account))
        XCTAssertNil(received.load(accountIdB64: account))

        AccountStoreGate.open(account)
        own.save(displayName: "Mira", accountIdB64: account, nowMs: 12)
        XCTAssertEqual(own.load(accountIdB64: account)?.displayName, "Mira")
    }

    func testAnInviteLinkSecretIsNotKeptForASealedAccount() {
        let store = InviteLinkSecretStore(backend: InMemoryInviteLinkSecretBackend())
        let secret = Data(repeating: 7, count: 12)
        store.put(accountIdB64: account, slug: "kept", secret: secret)

        AccountStoreGate.seal(account, wiping: true) {}
        store.put(accountIdB64: account, slug: "late", secret: secret)
        XCTAssertNil(store.secret(accountIdB64: account, slug: "late"))
        store.remove(accountIdB64: account, slug: "kept")
        XCTAssertNotNil(
            store.secret(accountIdB64: account, slug: "kept"),
            "a sealed account's map is not rewritten either")
    }

    // MARK: - Calls and media

    func testAFinishedCallIsNotFiledForASealedAccount() {
        let backing = InMemoryCallHistoryBacking()
        let store = CallHistoryStore(backing: backing)
        store.bind(accountIdB64: account)
        AccountStoreGate.seal(account, wiping: true) {}

        store.record(CallHistoryEntry(
            id: "c1", conversationId: "aa", kind: .chat, direction: .outgoing,
            outcome: .answered, video: false, startedAtMs: 1, durationMs: 0))
        store.backfillOnce { [] }
        XCTAssertNil(backing.load(accountIdB64: account))
        XCTAssertNil(backing.load(accountIdB64: CallHistoryStore.backfillMarkerKey(account)))
    }

    func testTheCallMirrorsAreNotWrittenForASealedAccount() {
        AccountStoreGate.seal(account)
        CallPeerNameMirror.mirror(accountIdB64: account, groupIdHex: "aa", name: "Mira", defaults: defaults)
        ConversationMuteMirror.replaceAll(accountIdB64: account, mutedGroupIdsHex: ["aa"], defaults: defaults)
        XCTAssertNil(CallPeerNameMirror.name(accountIdB64: account, groupIdHex: "aa", defaults: defaults))
        XCTAssertFalse(ConversationMuteMirror.isMuted(accountIdB64: account, groupIdHex: "aa", defaults: defaults))

        AccountStoreGate.open(account)
        CallPeerNameMirror.mirror(accountIdB64: account, groupIdHex: "aa", name: "Mira", defaults: defaults)
        XCTAssertEqual(CallPeerNameMirror.name(accountIdB64: account, groupIdHex: "aa", defaults: defaults), "Mira")
    }

    func testAPlayedMarkIsNotWrittenForASealedAccount() {
        let prefs = MediaPrefs(defaults: defaults, accountIdB64: { [account] in account })
        AccountStoreGate.seal(account)
        prefs.markPlayed("m1")
        XCTAssertEqual(prefs.playedKeys(), [])

        AccountStoreGate.open(account)
        prefs.markPlayed("m1")
        XCTAssertEqual(prefs.playedKeys(), ["m1"])
    }

    // MARK: - Backup and sync bookkeeping

    func testTheAutomaticBackupStateIsNotWrittenForASealedAccount() {
        AccountStoreGate.seal(account, wiping: true) {}
        AutoBackupStore.markDirty(accountIdB64: account, nowMs: 1, defaults: defaults)
        XCTAssertFalse(
            AutoBackupStore.knownGroupsChanged(accountIdB64: account, groupIds: ["aa"], defaults: defaults),
            "nothing recorded, no change reported")
        AutoBackupStore.save(accountIdB64: account, state: .clean, defaults: defaults)
        XCTAssertTrue(defaults.dictionaryRepresentation().keys.filter { $0.contains(account) }.isEmpty)
    }

    func testAFailedSyncPublishIsNotFlaggedForASealedAccount() {
        let tracker = PendingSyncTracker(accountIdB64: account, defaults: defaults)
        AccountStoreGate.seal(account, wiping: true) {}
        tracker.markFailed()
        XCTAssertFalse(tracker.isPending)

        AccountStoreGate.open(account)
        tracker.markFailed()
        XCTAssertTrue(tracker.isPending)
    }

    // MARK: - Channels

    func testARotationStateIsNotFiledForASealedAccount() {
        let state = ChannelRotationState(lastRotationMs: 5, postsSinceRotation: 1)
        AccountStoreGate.seal(account, wiping: true) {}
        ChannelRotationStateStore.save(accountIdB64: account, channelId: "c1", state: state, defaults: defaults)
        XCTAssertNil(ChannelRotationStateStore.load(accountIdB64: account, channelId: "c1", defaults: defaults))

        AccountStoreGate.open(account)
        ChannelRotationStateStore.save(accountIdB64: account, channelId: "c1", state: state, defaults: defaults)
        XCTAssertNotNil(ChannelRotationStateStore.load(accountIdB64: account, channelId: "c1", defaults: defaults))
    }

    func testACoPublishRequestIsNotSentWithoutItsMark() throws {
        let marks = CoPublishedJournals(defaults: defaults, accountIdB64: { [account] in account })
        AccountStoreGate.seal(account)
        XCTAssertThrowsError(try marks.markOrEnd(journalIdHex: "AB")) { error in
            XCTAssertTrue(error is AccountStoreGate.Sealed)
        }
        marks.mark(journalIdHex: "AB")
        XCTAssertFalse(marks.contains(journalIdHex: "ab"))

        let unbound = CoPublishedJournals(defaults: defaults, accountIdB64: { nil })
        XCTAssertThrowsError(try unbound.markOrEnd(journalIdHex: "ab")) { error in
            XCTAssertTrue(error is StaleBindingError)
        }

        AccountStoreGate.open(account)
        try marks.markOrEnd(journalIdHex: "AB")
        XCTAssertTrue(marks.contains(journalIdHex: "ab"))
    }

    func testAFoundingMarkNamesItsAccount() {
        let marks = JournalFoundingMarks(defaults: defaults)
        AccountStoreGate.seal(account, wiping: true) {}
        marks.mark(journalIdHex: "j1", accountIdB64: account)
        marks.mark(journalIdHex: "j2", accountIdB64: nil)
        XCTAssertEqual(marks.all(), [])

        AccountStoreGate.open(account)
        marks.mark(journalIdHex: "j1", accountIdB64: account)
        XCTAssertEqual(marks.all(), ["j1"])
    }

    /// The publish counter must be on disk before a post goes out; with no
    /// account bound there is nowhere to put it, so no counter is handed out.
    func testNoPublishCounterIsHandedOutWithNoAccountBound() {
        let state = ChannelsUiState()
        XCTAssertThrowsError(try state.nextCounter(channelId: "c1")) { error in
            XCTAssertTrue(error is StaleBindingError)
        }
    }
}
