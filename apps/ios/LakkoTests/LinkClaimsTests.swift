// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0079 (reworked 2026-10-17) — the claim-then-build rule and its record,
/// the mirror of `apps/web/src/contacts/link-claims.test.ts`.
final class LinkClaimsTests: XCTestCase {

    private func suite() -> UserDefaults {
        let name = "link-claims-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        addTeardownBlock { defaults.removePersistentDomain(forName: name) }
        return defaults
    }

    private let posted = LinkClaims.Own(ids: [9])
    private let posting = LinkClaims.Own(wires: ["aa"])

    func testWaitsWithNoTimerWhileTheSyncedStateIsNotLoaded() {
        XCTAssertEqual(LinkClaims.step(own: nil, answeredElsewhere: false, verdict: nil), .wait)
    }

    func testFollowsTheRegisterOnceLoaded() {
        XCTAssertEqual(LinkClaims.step(own: nil, answeredElsewhere: false, verdict: .accept), .postClaim)
        XCTAssertEqual(LinkClaims.step(own: nil, answeredElsewhere: false, verdict: .answered), .drop)
        XCTAssertEqual(LinkClaims.step(own: nil, answeredElsewhere: false, verdict: .dead), .cardDead)
    }

    func testWaitsUnshownOnARosterDevicesClaimUntilTheRegisterSaysAnswered() {
        XCTAssertEqual(LinkClaims.step(own: nil, answeredElsewhere: true, verdict: .accept), .wait)
        XCTAssertEqual(LinkClaims.step(own: nil, answeredElsewhere: true, verdict: nil), .wait)
        XCTAssertEqual(LinkClaims.step(own: nil, answeredElsewhere: true, verdict: .answered), .drop)
        XCTAssertEqual(LinkClaims.step(own: nil, answeredElsewhere: true, verdict: .dead), .drop)
        XCTAssertEqual(LinkClaims.step(own: LinkClaims.Own(), answeredElsewhere: true, verdict: .accept), .wait)
    }

    func testReChecksALostClaimUntilTheRequestIsAnswered() {
        let lost = LinkClaims.Own(ids: [9], settled: true)
        XCTAssertEqual(LinkClaims.step(own: lost, answeredElsewhere: true, verdict: .accept), .recheck)
        XCTAssertEqual(LinkClaims.step(own: lost, answeredElsewhere: false, verdict: nil), .recheck)
        XCTAssertEqual(LinkClaims.step(own: lost, answeredElsewhere: true, verdict: .answered), .drop)
        XCTAssertEqual(LinkClaims.step(own: lost, answeredElsewhere: true, verdict: .dead), .drop)
    }

    func testDecidesByTheJournalOnceThisDeviceHasPostedOrIsPosting() {
        XCTAssertEqual(LinkClaims.step(own: posted, answeredElsewhere: true, verdict: .accept), .decide)
        XCTAssertEqual(LinkClaims.step(own: posting, answeredElsewhere: true, verdict: nil), .decide)
    }

    func testBuildsAWonClaimEvenAfterTheLinkWasUsedUp() {
        XCTAssertEqual(
            LinkClaims.step(own: LinkClaims.Own(ids: [9], won: 9), answeredElsewhere: true, verdict: .dead), .build)
        XCTAssertEqual(
            LinkClaims.step(own: LinkClaims.Own(ids: [9], won: 9), answeredElsewhere: true, verdict: .answered), .build)
    }

    func testTakesDownADeadLinkCardOnlyForAnotherDevicesClaim() {
        XCTAssertTrue(LinkClaims.shownCardAnswered(own: nil, answeredElsewhere: true))
        XCTAssertFalse(LinkClaims.shownCardAnswered(own: nil, answeredElsewhere: false))
        XCTAssertFalse(LinkClaims.shownCardAnswered(own: posted, answeredElsewhere: true))
    }

    /// Through the core: the earliest roster claim wins and its author
    /// builds; an own earlier claim recognised only by its bytes still wins;
    /// a removed device's earlier claim is passed over.
    func testTheCoreDecidesOverAuthoredClaimsAndTheRoster() throws {
        let a = Data(repeating: 0xA, count: 32)
        let b = Data(repeating: 0xB, count: 32)
        let removed = Data(repeating: 0xC, count: 32)
        let own = LinkClaims.Own(ids: [10, 12])
        let seen = [LinkClaims.Observed(id: 11, author: LinkClaims.hex(b))]
        let claims = ContactsRepository.ffiClaims(
            LinkClaims.claimsToDecide(own: own, seen: seen, ownKeyHex: LinkClaims.hex(a)))
        XCTAssertEqual(try linkClaimOutcome(ownKey: a, claims: claims, rosterKeys: [a, b]), .won(messageId: 10))
        XCTAssertEqual(try linkClaimOutcome(ownKey: b, claims: claims, rosterKeys: [a, b]), .lost(winnerMessageId: 10))
        let withRemoved = claims + [FfiObservedLinkClaim(messageId: 3, authorKey: removed)]
        XCTAssertEqual(try linkClaimOutcome(ownKey: a, claims: withRemoved, rosterKeys: [a, b]), .won(messageId: 10))
        XCTAssertEqual(try linkClaimOutcome(ownKey: a, claims: [], rosterKeys: [a, b]), .noWinner)
        XCTAssertFalse(try linkClaimAnsweredElsewhere(
            claims: [FfiObservedLinkClaim(messageId: 3, authorKey: removed)], rosterKeys: [a, b]))
    }

    func testTheRequestKeyIsTheLinkIdThenTheRequesterKey() {
        let key = LinkClaims.requestKeyHex(
            linkId: Data(repeating: 1, count: 16), requesterKey: Data(repeating: 2, count: 32))
        XCTAssertEqual(key, String(repeating: "01", count: 16) + String(repeating: "02", count: 32))
    }

    func testTheRecordKeepsEverySeenClaimWithItsAuthorAcrossInstances() {
        let defaults = suite()
        let author = String(repeating: "bb", count: 32)
        let record = LinkClaims.Record(accountIdB64: "acct", defaults: defaults)
        XCTAssertTrue(record.seen("k").isEmpty)
        XCTAssertTrue(record.recordSeen("k", messageId: 12, authorHex: author, nowMs: 1))
        XCTAssertFalse(record.recordSeen("k", messageId: 12, authorHex: author, nowMs: 2))
        XCTAssertTrue(record.recordSeen("k", messageId: 9, authorHex: author, nowMs: 3))
        XCTAssertEqual(
            LinkClaims.Record(accountIdB64: "acct", defaults: defaults).seen("k").map(\.id), [9, 12])
        XCTAssertTrue(LinkClaims.Record(accountIdB64: "other", defaults: defaults).seen("k").isEmpty)
        LinkClaims.Record.clear(accountIdB64: "acct", defaults: defaults)
        XCTAssertTrue(record.seen("k").isEmpty)
    }

    func testTheOwnClaimRowIsRecognisedByItsBytesAfterALostPublishAnswer() {
        let defaults = suite()
        let record = LinkClaims.Record(accountIdB64: "acct", defaults: defaults)
        XCTAssertTrue(record.addOwnWire("k", wireHex: "c0ffee"))
        XCTAssertTrue(record.hasUnmatchedWires())
        let later = LinkClaims.Record(accountIdB64: "acct", defaults: defaults)
        XCTAssertFalse(later.recordOwnClaimRow(wireHex: "beef", messageId: 10))
        XCTAssertTrue(later.recordOwnClaimRow(wireHex: "c0ffee", messageId: 10))
        XCTAssertEqual(record.own("k"), LinkClaims.Own(ids: [10]))
        XCTAssertFalse(record.hasUnmatchedWires())
        record.addOwnWire("k", wireHex: "d00d")
        record.recordOwnClaimRow(wireHex: "d00d", messageId: 14)
        XCTAssertEqual(record.own("k")?.ids, [10, 14])
        record.addOwnWire("k", wireHex: "aa")
        record.dropUnlanded("k")
        XCTAssertEqual(record.own("k")?.wires, [])
        record.setSettled("k")
        XCTAssertEqual(record.own("k")?.settled, true)
        record.setWon("k", messageId: 10)
        XCTAssertEqual(LinkClaims.Record(accountIdB64: "acct", defaults: defaults).own("k")?.won, 10)
        record.forgetOwn("k")
        XCTAssertNil(record.own("k"))
    }

    func testTheUndecidedInboxSurvivesAndIsSealed() {
        let defaults = suite()
        let seal: (Data) throws -> Data = { Data($0.reversed()) }
        let open: (Data) throws -> Data = { Data($0.reversed()) }
        let inbox = LinkRequestInbox(accountIdB64: "acct", defaults: defaults, seal: seal, open: open)
        let request = IncomingContactRequest(
            messageId: 5, fromPubkey: Data(repeating: 3, count: 32), fromAccountId: nil,
            fromPseudonym: Data(repeating: 0, count: 32), validAfterMs: 0, intro: nil, createdAt: "t")
        XCTAssertTrue(inbox.add(.init(
            request: request, linkId: Data(repeating: 1, count: 16), linkSecret: Data(repeating: 2, count: 12))))
        let again = LinkRequestInbox(accountIdB64: "acct", defaults: defaults, seal: seal, open: open)
        XCTAssertEqual(again.rows().map(\.request.messageId), [5])
        // The stored bytes are the sealed form, not the JSON.
        let stored = defaults.data(forKey: "me.lakko.contacts.link-requests.v1.acct")
        XCTAssertNotNil(stored)
        XCTAssertNil(try? JSONSerialization.jsonObject(with: stored ?? Data()))
        again.remove(messageId: 5)
        XCTAssertTrue(inbox.rows().isEmpty)
    }

    func testTheDeadLinkNoteSurvivesTheStoredRequestList() throws {
        let request = IncomingContactRequest(
            messageId: 6, fromPubkey: Data(repeating: 3, count: 32), fromAccountId: nil,
            fromPseudonym: Data(repeating: 0, count: 32), validAfterMs: 0, intro: nil, createdAt: "t")
            .sentThroughDeadLink(linkId: Data(repeating: 1, count: 16))
        let back = try JSONDecoder().decode(
            PersistedContactsInbox.self,
            from: try JSONEncoder().encode(PersistedContactsInbox(cursor: 3, pending: [request])))
        XCTAssertEqual(back.pending.first?.viaDeadLink, true)
        XCTAssertEqual(back.pending.first?.deadLinkId, Data(repeating: 1, count: 16))
    }
}
