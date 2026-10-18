// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0079 (reworked 2026-10-17) — the claim-then-build rule and its record,
/// the mirror of `apps/web/src/contacts/link-claims.test.ts`.
@MainActor
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

    // MARK: - The build of a won request (ADR-0079 ordering, 2026-10-17)

    func testTheWelcomeGoesToTheOwnMailboxBeforeTheRequesters() {
        let targets = [
            LinkClaims.FoundedTarget(accountKeyB64: "requester", isRecipient: true),
            LinkClaims.FoundedTarget(accountKeyB64: "own", isRecipient: false),
        ]
        XCTAssertEqual(
            LinkClaims.welcomeOrder(targets, isRecipient: \.isRecipient).map(\.accountKeyB64),
            ["own", "requester"])
        XCTAssertEqual(LinkClaims.welcomeOrder([targets[0]], isRecipient: \.isRecipient).count, 1)
    }

    func testATakeoverDeviceNotInTheFoundedGroupThatSeesANoteForwardsAndNeverFounds() async {
        let note = LinkClaims.HeldNote(id: 7, groupIdB64: "theirs", welcomeB64: "w")
        let device = BuildFake(note: note, holdsNotedGroup: false)
        let built = await LinkClaims.runBuild(device.deps())
        XCTAssertTrue(built)
        XCTAssertEqual(device.log, ["forward:theirs", "finish:none"])
        XCTAssertEqual(LinkClaims.buildStep(hasFounded: false, hasNote: true), .forward)
    }

    func testTheNoteIsPostedBeforeAnyWelcomeAndEachStepIsRecordedFirst() async {
        let device = BuildFake()
        let built = await LinkClaims.runBuild(device.deps())
        XCTAssertTrue(built)
        XCTAssertEqual(device.log, [
            "prepare", "record:-:-", "postNote", "record:note:-", "hold", "deliver",
            "record:note:delivered", "finish:g",
        ])
    }

    func testNoWelcomeLeavesWhenTheNotesPublishFails() async {
        let device = BuildFake(failNote: true)
        let built = await LinkClaims.runBuild(device.deps())
        XCTAssertFalse(built)
        XCTAssertFalse(device.log.contains("deliver"))
        XCTAssertEqual(device.founded?.notePosted, false)
    }

    func testARecordedFoundingIsResumedNeverFoundedAgainEvenWithANoteHeld() async {
        var founded = BuildFake.founding
        founded.notePosted = true
        let device = BuildFake(
            founded: founded, note: LinkClaims.HeldNote(id: 7, groupIdB64: "theirs", welcomeB64: "w"))
        let built = await LinkClaims.runBuild(device.deps())
        XCTAssertTrue(built)
        XCTAssertFalse(device.log.contains("prepare"))
        XCTAssertFalse(device.log.contains("postNote"))
        XCTAssertFalse(device.log.contains("forward:theirs"))
        XCTAssertEqual(LinkClaims.buildStep(hasFounded: true, hasNote: true), .resume)
        XCTAssertEqual(LinkClaims.buildStep(hasFounded: false, hasNote: false), .found)
    }

    func testAStraddledOwnClaimDoesNotCountForItsAuthorEither() {
        let record = LinkClaims.Record(accountIdB64: "acct", defaults: suite())
        record.addOwnWire("k", wireHex: "c0ffee")
        XCTAssertEqual(record.judgeOwnRow(wireHex: "c0ffee", messageId: 10, counts: false), "claim")
        XCTAssertEqual(record.own("k")?.ids, [])
        XCTAssertEqual(record.own("k")?.wires, [])
        XCTAssertEqual(LinkClaims.step(own: record.own("k"), answeredElsewhere: false, verdict: .accept), .postClaim)
    }

    func testANoteIsConfirmedOnlyWhenItsRowCounts() {
        let record = LinkClaims.Record(accountIdB64: "acct", defaults: suite())
        record.setNoteWire("k", wireHex: "abcd")
        XCTAssertTrue(record.hasUnmatchedWires())
        XCTAssertEqual(record.judgeOwnRow(wireHex: "abcd", messageId: 20, counts: false), "note")
        XCTAssertEqual(record.own("k")?.noteConfirmed, false)
        XCTAssertNil(record.own("k")?.noteWire)
        record.setNoteWire("k", wireHex: "ef01")
        XCTAssertEqual(record.judgeOwnRow(wireHex: "ef01", messageId: 22, counts: true), "note")
        XCTAssertEqual(record.own("k")?.noteConfirmed, true)
        record.setNoteWire("k", wireHex: "aa")
        record.dropUnlandedNote("k")
        XCTAssertNil(record.own("k")?.noteWire)
    }

    func testNoWelcomeIsDeliveredOnAStraddledNote() async {
        let device = BuildFake(noteCounts: false)
        let built = await LinkClaims.runBuild(device.deps())
        XCTAssertFalse(built)
        XCTAssertEqual(device.log, ["prepare", "record:-:-", "postNote"])
        XCTAssertEqual(device.founded?.notePosted, false)
    }

    /// Through the core: a claim straddled by another device's commit does
    /// not count, one sealed at the journal's epoch does, and an own commit
    /// merged before the loop reaches the claim does not straddle it.
    func testTheReadabilityRuleThroughTheCore() {
        var j = journalEpochsStartingAt(base: 5)
        XCTAssertTrue(journalRowCounts(epochs: j, rowId: 20, sealedEpoch: 5))
        j = journalEpochsObserve(epochs: j, rowId: 21, sealedEpoch: 5, epochAfter: 6, groupEpochAfter: 6)
        XCTAssertFalse(journalRowCounts(epochs: j, rowId: 22, sealedEpoch: 5))
        XCTAssertTrue(journalRowCounts(epochs: j, rowId: 22, sealedEpoch: 6))
        XCTAssertTrue(journalRowCounts(epochs: j, rowId: 20, sealedEpoch: 5))
        // A refused commit leaves it unchanged.
        XCTAssertEqual(
            journalEpochsObserve(epochs: j, rowId: 23, sealedEpoch: 6, epochAfter: 7, groupEpochAfter: 6), j)
        let record = LinkClaims.Record(accountIdB64: "acct", defaults: suite())
        record.setJournal(.init(groupIdHex: "aa", base: j.base, commitRowIds: j.commitRowIds,
                                commitEpochsAfter: j.commitEpochsAfter))
        XCTAssertEqual(record.journal(groupIdHex: "aa")?.commitRowIds, [21])
        XCTAssertNil(record.journal(groupIdHex: "bb"))
    }

    func testTheRecordKeepsTheEarliestNotePerRequestBoundedAcrossInstances() {
        let defaults = suite()
        let record = LinkClaims.Record(accountIdB64: "acct", defaults: defaults)
        XCTAssertNil(record.note("k"))
        XCTAssertTrue(record.recordNote("k", note: .init(id: 9, groupIdB64: "a", welcomeB64: "w"), nowMs: 1))
        XCTAssertFalse(record.recordNote("k", note: .init(id: 12, groupIdB64: "b", welcomeB64: "w"), nowMs: 2))
        XCTAssertTrue(record.recordNote("k", note: .init(id: 4, groupIdB64: "c", welcomeB64: "w"), nowMs: 3))
        XCTAssertEqual(LinkClaims.Record(accountIdB64: "acct", defaults: defaults).note("k")?.groupIdB64, "c")
        for i in 0...LinkClaims.Record.maxNotes {
            record.recordNote("k\(i)", note: .init(id: 1, groupIdB64: "g", welcomeB64: "w"), nowMs: Int64(i + 10))
        }
        XCTAssertNil(record.note("k"))
        LinkClaims.Record.clear(accountIdB64: "acct", defaults: defaults)
        XCTAssertNil(record.note("k\(LinkClaims.Record.maxNotes)"))
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
        XCTAssertNil(later.judgeOwnRow(wireHex: "beef", messageId: 10, counts: true))
        XCTAssertEqual(later.judgeOwnRow(wireHex: "c0ffee", messageId: 10, counts: true), "claim")
        XCTAssertEqual(record.own("k"), LinkClaims.Own(ids: [10]))
        XCTAssertFalse(record.hasUnmatchedWires())
        record.addOwnWire("k", wireHex: "d00d")
        record.judgeOwnRow(wireHex: "d00d", messageId: 14, counts: true)
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

/// A device for ``LinkClaims/runBuild(_:)``: recorded state, and every step
/// logs its name in order.
@MainActor
private final class BuildFake {
    static let founding = LinkClaims.Founded(
        groupIdB64: "g", welcomeB64: "w",
        targets: [
            LinkClaims.FoundedTarget(accountKeyB64: "own", isRecipient: false),
            LinkClaims.FoundedTarget(accountKeyB64: "requester", isRecipient: true),
        ],
        notePosted: false, delivered: false)

    var log: [String] = []
    var founded: LinkClaims.Founded?
    let note: LinkClaims.HeldNote?
    let holdsNotedGroup: Bool
    let failNote: Bool
    let noteCounts: Bool

    init(
        founded: LinkClaims.Founded? = nil, note: LinkClaims.HeldNote? = nil,
        holdsNotedGroup: Bool = false, failNote: Bool = false, noteCounts: Bool = true
    ) {
        self.founded = founded
        self.note = note
        self.holdsNotedGroup = holdsNotedGroup
        self.failNote = failNote
        self.noteCounts = noteCounts
    }

    func deps() -> LinkClaims.BuildDeps {
        LinkClaims.BuildDeps(
            founded: { self.founded },
            note: { self.note },
            prepare: {
                self.log.append("prepare")
                return BuildFake.founding
            },
            record: { f in
                self.founded = f
                self.log.append("record:\(f.notePosted ? "note" : "-"):\(f.delivered ? "delivered" : "-")")
                return true
            },
            postNote: { _ in
                if self.failNote { throw NSError(domain: "test", code: 1) }
                self.log.append("postNote")
                return self.noteCounts
            },
            hold: { f in
                self.log.append("hold")
                return f.groupIdB64
            },
            deliver: { _ in self.log.append("deliver") },
            forward: { n in
                self.log.append("forward:\(n.groupIdB64)")
                return self.holdsNotedGroup ? n.groupIdB64 : nil
            },
            finish: { g in self.log.append("finish:\(g ?? "none")") })
    }
}
