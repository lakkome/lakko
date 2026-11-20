// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0089 step 4 — the suspension marks service (cache until
/// `reuseUntilMs`, one request per prefix, a failure back-off, `unknown` on
/// any check error, nothing asked when unpinned, never this account) and
/// each mark site's decision.
@MainActor
final class SuspensionMarksTests: XCTestCase {

    // MARK: - Fakes

    private struct Refused: Error {}

    @MainActor
    private final class Sent { var value = false }

    @MainActor
    private final class Fake {
        var logKey: Data? = Data(repeating: 7, count: 32)
        var now: Int64 = 1_000
        var fetches: [String] = []
        /// The answer a fetch returns; `nil` is a failed fetch.
        var answer: String? = SuspensionMarksTests.answerJSON
        // F2 — the proof behind a `suspended`.
        var rootKey: Data? = Data(repeating: 9, count: 32)
        var moderatorList: Data? = Data([0x4c])
        var entry: String? = "{\"entry\":1}"
        var proveError: Error?
        var entryFetches: [(UInt64, UInt64)] = []
        var listFetches = 0
        var proven: [(target: Data, list: Data, root: Data, entry: String)] = []
        /// A list's version is its first byte; the floor is the highest
        /// version admitted, as ``ModeratorListGate`` keeps it.
        var listFloor: UInt8?
        /// While set, a fetch waits for `release()`.
        var hold = false
        var held: [CheckedContinuation<Void, Never>] = []
        /// What the check says per account; absent throws.
        var standings: [Data: ModerationStanding] = [:]
        var verifies = 0
        var own: Set<Data> = []
        /// Maps a key to its current key (ADR-0074).
        var successor: [Data: Data] = [:]
        /// While set, the bound's sleep never ends until `wake()`.
        var holdSleep = false
        var sleepers: [CheckedContinuation<Void, Never>] = []

        func release() {
            hold = false
            let waiting = held
            held = []
            waiting.forEach { $0.resume() }
        }

        func wake() {
            holdSleep = false
            let waiting = sleepers
            sleepers = []
            waiting.forEach { $0.resume() }
        }
    }

    private func key(_ byte: UInt8) -> Data { Data(repeating: byte, count: 32) }

    /// A bucket answer whose head holds 9 leaves.
    nonisolated static let answerJSON = #"{"period":1,"root":{"log_size":9}}"#

    private func standing(
        suspended: Bool, until: Int64? = nil, reuseUntil: Int64 = 5_000, logIndex: UInt64? = 4
    ) -> ModerationStanding {
        ModerationStanding(
            suspended: suspended, untilMs: until, period: 1, reuseUntilMs: reuseUntil,
            logIndex: suspended ? logIndex : nil)
    }

    private func proofPorts(_ fake: Fake) -> SuspensionMarks.ProofPorts {
        SuspensionMarks.ProofPorts(
            rootKey: { fake.rootKey },
            moderatorList: {
                fake.listFetches += 1
                return fake.moderatorList
            },
            admitList: { list, _ in
                guard let version = list.first else { return false }
                guard ModeratorListGate.accepts(
                    version: UInt64(version), highestSeen: fake.listFloor.map(UInt64.init)) else { return false }
                fake.listFloor = max(fake.listFloor ?? 0, version)
                return true
            },
            fetchEntry: { index, size in
                fake.entryFetches.append((index, size))
                return fake.entry
            },
            proveAccount: { _, entry, list, root, key, _ in
                if let error = fake.proveError { throw error }
                fake.proven.append((key, list, root, entry))
            },
            proveChannel: { _, entry, list, root, channel, _ in
                if let error = fake.proveError { throw error }
                fake.proven.append((channel, list, root, entry))
            })
    }

    /// Every key falls in one prefix unless its first byte is 0xff.
    private func marks(_ fake: Fake) -> SuspensionMarks {
        SuspensionMarks(ports: SuspensionMarks.Ports(
            logKey: { fake.logKey },
            witnessKeys: { [] },
            witnessThreshold: { 1 },
            prefix: { $0.first == 0xff ? "ffff" : "abcd" },
            fetch: { prefix in
                fake.fetches.append(prefix)
                if fake.hold { await withCheckedContinuation { fake.held.append($0) } }
                return fake.answer
            },
            verify: { _, key, _, _, _, _ in
                fake.verifies += 1
                guard let standing = fake.standings[key] else { throw Refused() }
                return standing
            },
            nowMs: { fake.now },
            currentKey: { fake.successor[$0] ?? $0 },
            isOwn: { fake.own.contains($0) },
            sleepMs: { _ in
                if fake.holdSleep { await withCheckedContinuation { fake.sleepers.append($0) } }
            },
            proof: proofPorts(fake)))
    }

    private func drain() async {
        for _ in 0..<30 { await Task.yield() }
    }

    // MARK: - The service

    func testNothingIsAskedWhenNoLogKeyIsPinned() async {
        let fake = Fake()
        fake.logKey = nil
        fake.standings[key(1)] = standing(suspended: true)
        let service = marks(fake)
        let result = await service.standing(key(1))
        XCTAssertEqual(result, .unknown)
        service.check([key(1)])
        let dropped = await service.verifiedSuspended(key(1))
        await drain()
        XCTAssertFalse(dropped)
        XCTAssertEqual(fake.fetches, [])
        XCTAssertFalse(service.isSuspended(key(1)))
    }

    func testAMalformedPinReadsAsNoPin() {
        XCTAssertNil(ModerationClient.rootKey(fromInfoValue: ""))
        XCTAssertNil(ModerationClient.rootKey(fromInfoValue: "not-base64!"))
        XCTAssertNil(ModerationClient.rootKey(fromInfoValue: "AAAA"), "not 32 bytes")
    }

    func testAnAnswerIsReusedForThePrefixUntilReuseUntilMs() async {
        let fake = Fake()
        fake.standings[key(1)] = standing(suspended: false, reuseUntil: 5_000)
        fake.standings[key(2)] = standing(suspended: true, reuseUntil: 5_000)
        let service = marks(fake)

        let first = await service.standing(key(1))
        XCTAssertEqual(first, .notSuspended)
        let second = await service.standing(key(2))
        XCTAssertEqual(second, .suspended(untilMs: nil))
        XCTAssertEqual(fake.fetches, ["abcd"], "one bucket answers both accounts")
        XCTAssertTrue(service.isSuspended(key(2)))

        fake.now = 4_999
        _ = await service.standing(key(1))
        XCTAssertEqual(fake.fetches.count, 1)

        fake.now = 5_000
        _ = await service.standing(key(1))
        XCTAssertEqual(fake.fetches.count, 2, "asked again once the snapshot's reuse ends")
    }

    func testOnlyOneRequestPerPrefixIsInFlight() async {
        let fake = Fake()
        fake.hold = true
        fake.standings[key(1)] = standing(suspended: true)
        fake.standings[key(2)] = standing(suspended: false)
        let service = marks(fake)

        async let a = service.standing(key(1))
        async let b = service.standing(key(2))
        await drain()
        XCTAssertEqual(fake.fetches, ["abcd"])
        fake.release()
        let (ra, rb) = await (a, b)
        XCTAssertEqual(ra, .suspended(untilMs: nil))
        XCTAssertEqual(rb, .notSuspended)
        XCTAssertEqual(fake.fetches, ["abcd"])
    }

    func testAnotherPrefixIsItsOwnRequest() async {
        let fake = Fake()
        var other = key(0xff)
        other[1] = 1
        fake.standings[key(1)] = standing(suspended: false)
        fake.standings[other] = standing(suspended: false)
        let service = marks(fake)
        _ = await service.standing(key(1))
        _ = await service.standing(other)
        XCTAssertEqual(fake.fetches, ["abcd", "ffff"])
    }

    func testAFailedFetchIsNotRetriedForTenMinutes() async {
        let fake = Fake()
        fake.answer = nil
        fake.standings[key(1)] = standing(suspended: true)
        let service = marks(fake)

        let failed = await service.standing(key(1))
        XCTAssertEqual(failed, .unknown)
        fake.answer = Self.answerJSON
        fake.now += SuspensionMarks.failureBackoffMs - 1
        let held = await service.standing(key(1))
        XCTAssertEqual(held, .unknown)
        XCTAssertEqual(fake.fetches.count, 1)

        fake.now += 1
        let retried = await service.standing(key(1))
        XCTAssertEqual(retried, .suspended(untilMs: nil))
        XCTAssertEqual(fake.fetches.count, 2)
    }

    func testAnyCheckErrorIsUnknownAndShowsNoMark() async {
        let fake = Fake()
        let service = marks(fake)
        // `standings` holds nothing: every check throws.
        let result = await service.standing(key(1))
        XCTAssertEqual(result, .unknown)
        XCTAssertFalse(service.isSuspended(key(1)))
        XCTAssertTrue(service.marks.isEmpty)
        // An answer that does not verify counts as a failed fetch.
        _ = await service.standing(key(1))
        XCTAssertEqual(fake.fetches.count, 1)
    }

    func testTheRealCheckRefusesAGarbageAnswerAsUnknown() async {
        let fake = Fake()
        fake.answer = "{\"period\":1}"
        var ports = SuspensionMarks.Ports.live
        ports.logKey = { fake.logKey }
        ports.fetch = { _ in fake.answer }
        ports.currentKey = { $0 }
        ports.isOwn = { _ in false }
        ports.witnessKeys = { [] }
        let service = SuspensionMarks(ports: ports)
        let result = await service.standing(key(1))
        XCTAssertEqual(result, .unknown)
        XCTAssertFalse(service.isSuspended(key(1)))
    }

    func testTheRealPrefixIsFourLowerCaseHex() throws {
        let prefix = try moderationBucketPrefix(accountKey: key(1))
        XCTAssertTrue(SuspensionMarks.isPrefix(prefix), prefix)
        XCTAssertFalse(SuspensionMarks.isPrefix("ABCD"))
        XCTAssertFalse(SuspensionMarks.isPrefix("abc"))
        XCTAssertFalse(SuspensionMarks.isPrefix("../a"))
    }

    func testAMarkGoesWhenTheSuspensionEndsOrIsLifted() async {
        let fake = Fake()
        fake.standings[key(1)] = standing(suspended: true, until: 3_000, reuseUntil: 2_000)
        let service = marks(fake)
        _ = await service.standing(key(1))
        XCTAssertTrue(service.isSuspended(key(1)))
        fake.now = 3_000
        XCTAssertFalse(service.isSuspended(key(1)), "past its end, with nothing asked")

        fake.now = 2_500
        fake.standings[key(1)] = standing(suspended: false, reuseUntil: 9_000)
        _ = await service.standing(key(1))
        XCTAssertFalse(service.isSuspended(key(1)))
        XCTAssertTrue(service.marks.isEmpty, "nothing about it is kept")
    }

    func testThisAccountIsNeverAskedAbout() async {
        let fake = Fake()
        fake.own = [key(1)]
        fake.standings[key(1)] = standing(suspended: true)
        let service = marks(fake)
        let result = await service.standing(key(1))
        XCTAssertEqual(result, .unknown)
        XCTAssertFalse(service.isSuspended(key(1)))
        XCTAssertEqual(fake.fetches, [])
    }

    func testAnOldKeyIsAskedAboutAsItsCurrentKey() async {
        let fake = Fake()
        fake.successor[key(1)] = key(2)
        fake.standings[key(2)] = standing(suspended: true)
        let service = marks(fake)
        let result = await service.standing(key(1))
        XCTAssertEqual(result, .suspended(untilMs: nil))
        XCTAssertTrue(service.isSuspended(key(1)))
        XCTAssertTrue(service.isSuspended(key(2)))
    }

    // MARK: - Contact requests

    func testARequestIsDroppedOnlyOnAVerifiedSuspension() async {
        let fake = Fake()
        fake.standings[key(1)] = standing(suspended: true)
        fake.standings[key(2)] = standing(suspended: false)
        let service = marks(fake)
        fake.holdSleep = true
        let suspended = await service.verifiedSuspended(key(1))
        let notSuspended = await service.verifiedSuspended(key(2))
        let unknown = await service.verifiedSuspended(key(3))
        XCTAssertTrue(suspended)
        XCTAssertFalse(notSuspended)
        XCTAssertFalse(unknown, "a check that fails shows the request")
        fake.wake()
    }

    func testARequestIsShownWhenTheCheckOutlastsItsBound() async {
        let fake = Fake()
        fake.hold = true
        fake.standings[key(1)] = standing(suspended: true)
        let service = marks(fake)
        let dropped = await service.verifiedSuspended(key(1), boundMs: 3_000)
        XCTAssertFalse(dropped, "the bound ended first")
        fake.release()
        await drain()
        XCTAssertTrue(service.isSuspended(key(1)), "the late answer still marks them")
    }

    // MARK: - Mark sites

    func testTheOneToOneBannerYieldsToTheBlockBanner() {
        XCTAssertTrue(SuspensionMarkRules.showsOneToOneBanner(blocked: false, suspended: true))
        XCTAssertFalse(SuspensionMarkRules.showsOneToOneBanner(blocked: true, suspended: true))
        XCTAssertFalse(SuspensionMarkRules.showsOneToOneBanner(blocked: false, suspended: false))
    }

    func testTheCardShowsBlockedOverSuspended() {
        XCTAssertEqual(SuspensionMarkRules.cardLine(blocked: true, suspended: true), .blocked)
        XCTAssertEqual(SuspensionMarkRules.cardLine(blocked: false, suspended: true), .suspended)
        XCTAssertEqual(SuspensionMarkRules.cardLine(blocked: true, suspended: false), .blocked)
        XCTAssertEqual(SuspensionMarkRules.cardLine(blocked: false, suspended: false), .none)
    }

    func testTheMemberMarkerIsNeverOnThisDevicesRow() {
        XCTAssertTrue(SuspensionMarkRules.showsMemberMarker(isOwnRow: false, suspended: true))
        XCTAssertFalse(SuspensionMarkRules.showsMemberMarker(isOwnRow: true, suspended: true))
        XCTAssertFalse(SuspensionMarkRules.showsMemberMarker(isOwnRow: false, suspended: false))
    }

    func testSearchKeepsOrderAndDropsOnlyTheSuspended() {
        let people = [key(1), key(2), key(3)]
        let visible = SuspensionMarkRules.searchVisible(people, key: { $0 }, isSuspended: { $0 == key(2) })
        XCTAssertEqual(visible, [key(1), key(3)])
    }

    func testAnInviteFromASuspendedAccountOffersNoAdd() {
        XCTAssertFalse(SuspensionMarkRules.inviteOffersAdd(inviterSuspended: true))
        XCTAssertTrue(SuspensionMarkRules.inviteOffersAdd(inviterSuspended: false))
    }

    // MARK: - Review fixes: one bound for a batch, Add, routes, reset

    func testABatchIsAskedAboutAllAtOnceUnderOneBound() async {
        let fake = Fake()
        fake.hold = true
        var other = key(0xff)
        other[1] = 2
        fake.standings[key(1)] = standing(suspended: true)
        fake.standings[other] = standing(suspended: true)
        let service = marks(fake)
        // The bound ends at once: both fetches were already started, so the
        // two requests cost one bound together, not one each.
        let dropped = await service.verifiedSuspended([key(1), other], boundMs: 3_000)
        XCTAssertEqual(dropped, [], "nothing had answered when the bound ended")
        await drain()
        XCTAssertEqual(Set(fake.fetches), ["abcd", "ffff"])
        fake.release()
        await drain()
        XCTAssertTrue(service.isSuspended(key(1)))
        XCTAssertTrue(service.isSuspended(other))
    }

    func testABatchReturnsAsSoonAsEveryAnswerIsIn() async {
        let fake = Fake()
        fake.holdSleep = true
        fake.standings[key(1)] = standing(suspended: true)
        fake.standings[key(2)] = standing(suspended: false)
        let service = marks(fake)
        let dropped = await service.verifiedSuspended([key(1), key(2), key(3), key(1)])
        XCTAssertEqual(dropped, [key(1)], "only a verified suspension; key(3) failed its check")
        fake.wake()
    }

    func testTheInboxDropsOnlyRequestsFromTheVerifiedSet() {
        let request = { (byte: UInt8) in
            IncomingContactRequest(
                messageId: Int64(byte), fromPubkey: Data(repeating: byte, count: 32),
                fromAccountId: Data(repeating: byte, count: 16), fromPseudonym: Data(repeating: byte, count: 32),
                validAfterMs: 0, intro: nil, createdAt: "2026-11-20T00:00:00Z")
        }
        let events: [IncomingMailboxEvent] = [
            .contactRequest(request(1)),
            .welcome(messageId: 9, welcomeBytes: Data([1]), createdAt: ""),
            .contactRequest(request(2)),
        ]
        XCTAssertEqual(ContactsRepository.requestSenders(events), [key(1), key(2)])
        let suspended: Set<Data> = [key(2)]
        XCTAssertEqual(
            events.map { ContactsRepository.droppedAsSuspended($0, suspendedSenders: suspended) },
            [false, false, true])
    }

    func testHeldRequestsOfASuspendedSenderAreHiddenNotDeleted() {
        let requests = [1, 2, 3].map { (byte: UInt8) in
            IncomingContactRequest(
                messageId: Int64(byte), fromPubkey: Data(repeating: byte, count: 32),
                fromAccountId: nil, fromPseudonym: Data(repeating: byte, count: 32),
                validAfterMs: 0, intro: nil, createdAt: "")
        }
        let state = ContactsState(pending: requests, lastError: "kept")
        let hidden = ContactsViewModel.hidingSuspended(state, isSuspended: { $0 == self.key(2) })
        XCTAssertEqual(hidden.pending.map(\.messageId), [1, 3])
        XCTAssertEqual(hidden.lastError, "kept")
        XCTAssertEqual(state.pending.count, 3, "the held list itself is untouched")
        XCTAssertEqual(ContactsViewModel.hidingSuspended(state, isSuspended: { _ in false }), state)
    }

    func testAddBeforeTheStandingAnsweredSendsNothingOnAVerifiedSuspension() async {
        var sent = 0
        let refused: Int? = await SuspensionMarkRules.addThroughInvite(
            inviter: key(1), inviterSuspended: { _ in true }, send: { sent += 1; return 1 })
        XCTAssertNil(refused)
        XCTAssertEqual(sent, 0)
        let added: Int? = await SuspensionMarkRules.addThroughInvite(
            inviter: key(1), inviterSuspended: { _ in false }, send: { sent += 1; return 1 })
        XCTAssertEqual(added, 1)
        XCTAssertEqual(sent, 1)
    }

    func testAddWaitsForTheInvitersStandingThroughTheService() async {
        let fake = Fake()
        fake.hold = true
        fake.holdSleep = true
        fake.standings[key(1)] = standing(suspended: true)
        let service = marks(fake)
        let sent = Sent()
        let inviter = key(1)
        let tap = Task { @MainActor in
            await SuspensionMarkRules.addThroughInvite(
                inviter: inviter, inviterSuspended: { await service.verifiedSuspended($0) },
                send: { sent.value = true; return true })
        }
        await drain()
        XCTAssertFalse(sent.value, "nothing goes while the standing is unanswered")
        fake.release()
        let result = await tap.value
        XCTAssertNil(result)
        XCTAssertFalse(sent.value)
        fake.wake()
    }

    func testAddProceedsWhenTheStandingOutlastsItsBound() async {
        let fake = Fake()
        fake.hold = true
        fake.standings[key(1)] = standing(suspended: true)
        let service = marks(fake)
        let result: Bool? = await SuspensionMarkRules.addThroughInvite(
            inviter: key(1), inviterSuspended: { await service.verifiedSuspended($0) }, send: { true })
        XCTAssertEqual(result, true)
        fake.release()
        await drain()
    }

    func testResetClearsTheMarksAndALateAnswerWritesNothing() async {
        let fake = Fake()
        fake.standings[key(1)] = standing(suspended: true)
        let service = marks(fake)
        _ = await service.standing(key(1))
        XCTAssertTrue(service.isSuspended(key(1)))
        service.reset()
        XCTAssertFalse(service.isSuspended(key(1)))
        XCTAssertTrue(service.marks.isEmpty)

        fake.hold = true
        fake.standings[key(2)] = standing(suspended: true)
        let late = Task { @MainActor in await service.standing(key(2)) }
        await drain()
        service.reset()
        fake.release()
        let result = await late.value
        XCTAssertEqual(result, .unknown)
        XCTAssertFalse(service.isSuspended(key(2)))
        XCTAssertEqual(fake.fetches.count, 2, "the reset dropped the cached answer")
    }

    // MARK: - F2: a suspension is acted on only once proven

    private struct NotProven: Error {}

    func testAProvenSuspensionMarksAndTheProofIsAskedOfTheRightEntry() async {
        let fake = Fake()
        fake.standings[key(1)] = standing(suspended: true, logIndex: 6)
        let service = marks(fake)
        let result = await service.standing(key(1))
        XCTAssertEqual(result, .suspended(untilMs: nil))
        XCTAssertTrue(service.isSuspended(key(1)))
        XCTAssertEqual(fake.entryFetches.map(\.0), [6], "the bucket entry's log index")
        XCTAssertEqual(fake.entryFetches.map(\.1), [9], "the answer head's log_size")
        XCTAssertEqual(fake.proven.count, 1)
        XCTAssertEqual(fake.proven.first?.target, key(1))
        XCTAssertEqual(fake.proven.first?.root, fake.rootKey)
        XCTAssertEqual(fake.proven.first?.list, fake.moderatorList)
        XCTAssertEqual(fake.proven.first?.entry, fake.entry)

        // Held with the answer: no second proof until the reuse ends.
        _ = await service.standing(key(1))
        XCTAssertEqual(fake.proven.count, 1)
    }

    func testANotSuspendedAnswerAsksForNoProof() async {
        let fake = Fake()
        fake.standings[key(1)] = standing(suspended: false)
        let service = marks(fake)
        let result = await service.standing(key(1))
        XCTAssertEqual(result, .notSuspended)
        XCTAssertTrue(fake.entryFetches.isEmpty)
        XCTAssertEqual(fake.listFetches, 0)
    }

    /// Every way the proof can fall short leaves the standing unknown, and
    /// so every mark site (banner, card, member marker, search, invite Add,
    /// request drop, held-request hiding) treats the person as not suspended.
    func testAnUnprovenSuspensionIsUnknownAtEverySite() async {
        let cases: [(String, (Fake) -> Void)] = [
            ("the proof does not hold", { $0.proveError = NotProven() }),
            ("no root key pinned", { $0.rootKey = nil }),
            ("no moderator list", { $0.moderatorList = nil }),
            ("no log entry", { $0.entry = nil }),
            ("no log index", { $0.standings[Data(repeating: 1, count: 32)] = ModerationStanding(
                suspended: true, untilMs: nil, period: 1, reuseUntilMs: 5_000, logIndex: nil) }),
            ("no log size in the answer", { $0.answer = #"{"period":1}"# }),
        ]
        for (name, setUp) in cases {
            let fake = Fake()
            fake.standings[key(1)] = standing(suspended: true)
            setUp(fake)
            let service = marks(fake)
            let result = await service.standing(key(1))
            XCTAssertEqual(result, .unknown, name)
            XCTAssertFalse(service.isSuspended(key(1)), name)
            XCTAssertTrue(service.marks.isEmpty, name)
            let dropped = await service.verifiedSuspended(key(1))
            XCTAssertFalse(dropped, "the contact request is not dropped: \(name)")

            // The sites, fed by the service.
            let suspended = service.isSuspended(key(1))
            XCTAssertFalse(SuspensionMarkRules.showsOneToOneBanner(blocked: false, suspended: suspended), name)
            XCTAssertEqual(SuspensionMarkRules.cardLine(blocked: false, suspended: suspended), .none, name)
            XCTAssertFalse(SuspensionMarkRules.showsMemberMarker(isOwnRow: false, suspended: suspended), name)
            XCTAssertEqual(
                SuspensionMarkRules.searchVisible([key(1)], key: { $0 }, isSuspended: service.isSuspended),
                [key(1)], name)
            XCTAssertTrue(SuspensionMarkRules.inviteOffersAdd(inviterSuspended: suspended), name)
            let added: Bool? = await SuspensionMarkRules.addThroughInvite(
                inviter: key(1), inviterSuspended: { await service.verifiedSuspended($0) }, send: { true })
            XCTAssertEqual(added, true, "Add is not refused: \(name)")
        }
    }

    func testAFailedProofIsAskedAgainAfterTheBackoffAndThenMarks() async {
        let fake = Fake()
        fake.standings[key(1)] = standing(suspended: true, reuseUntil: 10_000_000)
        fake.proveError = NotProven()
        let service = marks(fake)
        let first = await service.standing(key(1))
        XCTAssertEqual(first, .unknown)
        XCTAssertEqual(fake.listFetches, 1)

        fake.proveError = nil
        fake.now += SuspensionMarks.failureBackoffMs - 1
        let held = await service.standing(key(1))
        XCTAssertEqual(held, .unknown, "held for the backoff")
        XCTAssertEqual(fake.entryFetches.count, 1)

        fake.now += 1
        let proven = await service.standing(key(1))
        XCTAssertEqual(proven, .suspended(untilMs: nil))
        XCTAssertEqual(fake.listFetches, 2, "a failed proof drops the held moderator list")
    }

    func testTheModeratorListIsFetchedOnceAndHeld() async {
        let fake = Fake()
        fake.standings[key(1)] = standing(suspended: true)
        fake.standings[key(2)] = standing(suspended: true)
        let service = marks(fake)
        _ = await service.standing(key(1))
        _ = await service.standing(key(2))
        XCTAssertEqual(fake.listFetches, 1)
        XCTAssertEqual(fake.proven.count, 2)
        service.reset()
        _ = await service.standing(key(1))
        XCTAssertEqual(fake.listFetches, 2, "a reset drops the held list")
    }

    func testTheAnswerLogSizeIsReadFromTheHead() {
        XCTAssertEqual(SuspensionMarks.answerLogSize(Self.answerJSON), 9)
        XCTAssertEqual(SuspensionMarks.answerLogSize(#"{"root":{"log_size":18446744073709551615}}"#), UInt64.max)
        XCTAssertNil(SuspensionMarks.answerLogSize("{}"))
        XCTAssertNil(SuspensionMarks.answerLogSize(#"{"root":{"log_size":-1}}"#))
        XCTAssertNil(SuspensionMarks.answerLogSize(#"{"root":{"log_size":1.5}}"#))
        XCTAssertNil(SuspensionMarks.answerLogSize("not json"))
    }

    func testTheRealProofRefusesGarbageSoTheStandingIsUnknown() async {
        let fake = Fake()
        fake.standings[key(1)] = standing(suspended: true)
        var proof = proofPorts(fake)
        proof.proveAccount = SuspensionMarks.ProofPorts.live.proveAccount
        var ports = SuspensionMarks.Ports(
            logKey: { fake.logKey }, witnessKeys: { [] }, witnessThreshold: { 1 },
            prefix: { _ in "abcd" }, fetch: { _ in fake.answer },
            verify: { _, key, _, _, _, _ in fake.standings[key]! },
            nowMs: { fake.now }, currentKey: { $0 }, isOwn: { _ in false }, sleepMs: { _ in })
        ports.proof = proof
        let service = SuspensionMarks(ports: ports)
        let result = await service.standing(key(1))
        XCTAssertEqual(result, .unknown)
        XCTAssertFalse(service.isSuspended(key(1)))
    }

    func testUnprovableByDefault() async {
        let fake = Fake()
        fake.standings[key(1)] = standing(suspended: true)
        let ports = SuspensionMarks.Ports(
            logKey: { fake.logKey }, witnessKeys: { [] }, witnessThreshold: { 1 },
            prefix: { _ in "abcd" }, fetch: { _ in fake.answer },
            verify: { _, key, _, _, _, _ in fake.standings[key]! },
            nowMs: { fake.now }, currentKey: { $0 }, isOwn: { _ in false }, sleepMs: { _ in })
        let service = SuspensionMarks(ports: ports)
        let result = await service.standing(key(1))
        XCTAssertEqual(result, .unknown, "a Ports built without a proof proves nothing")
    }

    func testAModeratorListOlderThanTheFloorIsNotUsed() async {
        let fake = Fake()
        fake.standings[key(1)] = standing(suspended: true)
        fake.listFloor = 0x50
        fake.moderatorList = Data([0x4c])   // older than the floor
        let service = marks(fake)
        let result = await service.standing(key(1))
        XCTAssertEqual(result, .unknown, "an older list proves nothing")
        XCTAssertFalse(service.isSuspended(key(1)))
        XCTAssertTrue(fake.proven.isEmpty)
        XCTAssertEqual(fake.listFloor, 0x50, "the floor is not lowered")
    }

    func testANewerListRaisesTheFloorAndAHeldOlderOneIsFetchedAgain() async {
        let fake = Fake()
        fake.standings[key(1)] = standing(suspended: true)
        fake.standings[key(2)] = standing(suspended: true)
        fake.moderatorList = Data([0x4c])
        let service = marks(fake)
        _ = await service.standing(key(1))
        XCTAssertEqual(fake.listFloor, 0x4c)
        XCTAssertEqual(fake.listFetches, 1)

        // The report sheet saw a newer list meanwhile: the held one is below
        // the floor now and is not used; the server still serves the old one.
        fake.listFloor = 0x60
        let stale = await service.standing(key(2))
        XCTAssertEqual(stale, .unknown)
        XCTAssertEqual(fake.listFetches, 2, "the held list was not re-admitted, so it was fetched again")

        // A list at or above the floor proves, and raises it.
        fake.moderatorList = Data([0x61])
        fake.now += SuspensionMarks.failureBackoffMs
        let proven = await service.standing(key(2))
        XCTAssertEqual(proven, .suspended(untilMs: nil))
        XCTAssertEqual(fake.listFloor, 0x61)
        XCTAssertEqual(fake.proven.last?.list, Data([0x61]))
    }
}
