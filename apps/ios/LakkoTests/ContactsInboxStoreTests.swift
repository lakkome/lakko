// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// Unit tests for the persisted stranger inbox (``ContactsInboxStore``) and
/// the pure batch rules ``ContactsRepository/refreshInbox(waitSecs:)`` applies
/// (``ContactsInboxMerge``). The store runs against an isolated `UserDefaults`
/// suite; the merge rules are plain value functions.
final class ContactsInboxStoreTests: XCTestCase {
    private var suite = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suite = "contacts-inbox-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suite)
        defaults = nil
        super.tearDown()
    }

    private func request(
        _ messageId: Int64, from keyByte: UInt8, intro: String? = "hi",
        profile: ProfileClaim? = nil
    ) -> IncomingContactRequest {
        IncomingContactRequest(
            messageId: messageId,
            fromPubkey: Data(repeating: keyByte, count: 32),
            fromAccountId: Data(repeating: keyByte, count: 16),
            fromPseudonym: Data(repeating: 0xEE, count: 32),
            validAfterMs: 1_700_000_000_000,
            intro: intro,
            profile: profile,
            createdAt: "2026-09-16T10:00:00Z"
        )
    }

    // MARK: - Store round-trip

    func testAbsentRowReadsAsEmpty() {
        let store = ContactsInboxStore(defaults: defaults)
        XCTAssertEqual(store.load(accountIdB64: "acct-a"), .empty)
        XCTAssertEqual(store.load(accountIdB64: "acct-a").cursor, 0)
        XCTAssertTrue(store.load(accountIdB64: "acct-a").pending.isEmpty)
    }

    func testRoundTripPreservesCursorAndPendingIncludingProfile() {
        let store = ContactsInboxStore(defaults: defaults)
        let claim = ProfileClaim(displayName: "Wanda", username: "wanda", updatedAtMs: 42)
        let inbox = PersistedContactsInbox(
            cursor: 17,
            pending: [request(5, from: 0x01), request(9, from: 0x02, intro: nil, profile: claim)]
        )
        store.save(inbox, accountIdB64: "acct-a")

        let reloaded = ContactsInboxStore(defaults: defaults).load(accountIdB64: "acct-a")
        XCTAssertEqual(reloaded, inbox)
        XCTAssertEqual(reloaded.pending[1].profile, claim)
        XCTAssertNil(reloaded.pending[1].intro)
    }

    func testRowsAreKeyedPerAccount() {
        let store = ContactsInboxStore(defaults: defaults)
        store.save(PersistedContactsInbox(cursor: 3, pending: [request(1, from: 0x01)]), accountIdB64: "acct-a")
        store.save(PersistedContactsInbox(cursor: 8, pending: []), accountIdB64: "acct-b")

        XCTAssertEqual(store.load(accountIdB64: "acct-a").cursor, 3)
        XCTAssertEqual(store.load(accountIdB64: "acct-a").pending.count, 1)
        XCTAssertEqual(store.load(accountIdB64: "acct-b").cursor, 8)
        XCTAssertTrue(store.load(accountIdB64: "acct-b").pending.isEmpty)
        XCTAssertEqual(
            ContactsInboxStore.storageKey(accountIdB64: "acct-a"),
            "me.lakko.contacts.inbox.v1.acct-a"
        )
    }

    func testClearDropsOnlyThatAccount() {
        let store = ContactsInboxStore(defaults: defaults)
        store.save(PersistedContactsInbox(cursor: 3, pending: [request(1, from: 0x01)]), accountIdB64: "acct-a")
        store.save(PersistedContactsInbox(cursor: 8, pending: []), accountIdB64: "acct-b")

        store.clear(accountIdB64: "acct-a")

        XCTAssertEqual(store.load(accountIdB64: "acct-a"), .empty)
        XCTAssertEqual(store.load(accountIdB64: "acct-b").cursor, 8)
    }

    func testCorruptRowReadsAsEmptyRatherThanFailing() {
        defaults.set(Data("not json".utf8), forKey: ContactsInboxStore.storageKey(accountIdB64: "acct-a"))
        XCTAssertEqual(ContactsInboxStore(defaults: defaults).load(accountIdB64: "acct-a"), .empty)
    }

    // MARK: - Dedupe rule

    func testFetchedRequestAlreadyPendingIsDropped() {
        let pending = [request(5, from: 0x01)]
        let result = ContactsInboxMerge.merge(
            fetched: [request(5, from: 0x01), request(6, from: 0x03)],
            pending: pending,
            accepted: []
        )
        XCTAssertEqual(result.kept.map(\.messageId), [6])
        XCTAssertEqual(result.dropped.map(\.request.messageId), [5])
        XCTAssertEqual(result.dropped.map(\.reason), [.alreadyPending])
    }

    func testFetchedRequestFromAcceptedContactIsDropped() {
        // The live defect: the request Wanda accepted was re-fetched after a
        // relaunch and re-listed under "Requests" beside her contact row.
        let accepted = [AcceptedContact(request: request(5, from: 0x01), groupIdHex: "ab")]
        let result = ContactsInboxMerge.merge(
            fetched: [request(5, from: 0x01), request(7, from: 0x01), request(8, from: 0x02)],
            pending: [],
            accepted: accepted
        )
        XCTAssertEqual(result.kept.map(\.messageId), [8])
        XCTAssertEqual(result.dropped.map(\.reason), [.alreadyAccepted, .alreadyAccepted])
    }

    func testDuplicateMessageIdWithinOneBatchIsKeptOnce() {
        let result = ContactsInboxMerge.merge(
            fetched: [request(5, from: 0x01), request(5, from: 0x01)],
            pending: [],
            accepted: []
        )
        XCTAssertEqual(result.kept.count, 1)
        XCTAssertEqual(result.dropped.map(\.reason), [.alreadyPending])
    }

    /// One person is one row: a person who asked again (search offers that)
    /// replaces their earlier request, in one batch or across two.
    func testAPersonWhoAskedAgainIsOneRow() {
        let later = ContactsInboxMerge.merge(
            fetched: [request(7, from: 0x01)], pending: [request(5, from: 0x01)], accepted: [])
        XCTAssertEqual(later.applied(to: [request(5, from: 0x01)]).map(\.messageId), [7])

        let sameBatch = ContactsInboxMerge.merge(
            fetched: [request(5, from: 0x01), request(7, from: 0x01)], pending: [], accepted: [])
        XCTAssertEqual(sameBatch.applied(to: []).map(\.messageId), [7])
    }

    func testFreshRequestsKeepFetchOrder() {
        let result = ContactsInboxMerge.merge(
            fetched: [request(9, from: 0x01), request(4, from: 0x02), request(6, from: 0x03)],
            pending: [request(1, from: 0x09)],
            accepted: [AcceptedContact(request: request(2, from: 0x0A))]
        )
        XCTAssertEqual(result.kept.map(\.messageId), [9, 4, 6])
        XCTAssertTrue(result.dropped.isEmpty)
    }

    // MARK: - Cursor never moves backwards

    func testCursorAdvancesOnNewerPoll() {
        XCTAssertEqual(ContactsInboxMerge.advancedCursor(current: 10, polled: 15), 15)
    }

    func testCursorHoldsOnStalePoll() {
        // A batch answered from an older cursor must not rewind the inbox.
        XCTAssertEqual(ContactsInboxMerge.advancedCursor(current: 15, polled: 10), 15)
        XCTAssertEqual(ContactsInboxMerge.advancedCursor(current: 15, polled: 0), 15)
    }

    func testCursorIsIdempotentOnEqualPoll() {
        XCTAssertEqual(ContactsInboxMerge.advancedCursor(current: 15, polled: 15), 15)
    }
}
