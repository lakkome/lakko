// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0048 §2 trigger 4 on iOS — the profile request ("introduce
/// yourselves"): when this device asks, what it remembers, and how the
/// receive loop classifies a request so it never becomes a bubble.
///
/// The orchestration (`ProfileRequester`) is driven with an injected rule and
/// recording ports, so no live group is needed; one test then pins the REAL
/// core rule (`shouldRequestProfiles`) against the same inputs, so the Swift
/// side cannot silently drift from what the three clients share. The
/// classification tests drive the REAL `ChatClient.toEvent` over the REAL
/// `composeProfileRequest()`, for the reason `ChatServiceEnvelopeTests` gives:
/// a double whose parse throws where the real one degrades would pass with
/// the arm deleted.
@MainActor
final class ProfileRequestPolicyTests: XCTestCase {

    // MARK: - Harness

    /// Everything the requester touched, so each test asserts on effects
    /// rather than on the returned outcome alone.
    @MainActor
    private final class Harness {
        var sent: [String] = []
        var recorded: [(group: String, atMs: UInt64)] = []
        var ruleCalls: [(joined: Bool, unnamed: UInt32, last: UInt64?, now: UInt64)] = []
        var unnamed: Int? = 0
        var canSend = true
        var last: UInt64?
        var now: UInt64 = 1_000_000
        var sendFails = false
        /// The injected rule. Defaults to the REAL core rule so most tests
        /// exercise the production wiring; a test that wants a fixed answer
        /// overrides it.
        var rule: (Bool, UInt32, UInt64?, UInt64) -> Bool = { joined, unnamed, last, now in
            shouldRequestProfiles(
                joinedJustNow: joined, unnamedMembers: unnamed, lastRequestedAtMs: last, nowMs: now)
        }

        struct SendFailed: Error {}

        func requester() -> ProfileRequester {
            ProfileRequester(
                ports: ProfileRequester.Ports(
                    send: { [unowned self] group in
                        if self.sendFails { throw SendFailed() }
                        self.sent.append(group)
                    },
                    unnamedMembers: { [unowned self] _ in self.unnamed },
                    canSend: { [unowned self] _ in self.canSend },
                    lastRequestedAtMs: { [unowned self] _ in self.last },
                    recordRequestedAt: { [unowned self] group, atMs in
                        self.recorded.append((group, atMs))
                        self.last = atMs
                    },
                    nowMs: { [unowned self] in self.now },
                    rule: { [unowned self] joined, unnamed, last, now in
                        self.ruleCalls.append((joined, unnamed, last, now))
                        return self.rule(joined, unnamed, last, now)
                    }
                ))
        }
    }

    private let group = "ab12"
    private var interval: UInt64 { profileRequestMinIntervalMs() }

    /// `XCTAssertEqual` cannot await inside its autoclosure, so the outcome is
    /// taken first and compared second.
    private func assertOutcome(
        _ expected: ProfileRequester.Outcome,
        file: StaticString = #filePath, line: UInt = #line,
        _ run: @MainActor () async -> ProfileRequester.Outcome
    ) async {
        let got = await run()
        XCTAssertEqual(got, expected, file: file, line: line)
    }

    // MARK: - The join trigger

    /// A join asks regardless of the count and regardless of when this device
    /// last asked: a new membership is a new audience.
    func testJoinAlwaysAsksAndStamps() async {
        let h = Harness()
        h.unnamed = 0
        h.last = h.now - 1  // asked a millisecond ago
        let outcome = await h.requester().noteJoined(groupIdHex: group)
        XCTAssertEqual(outcome, .requested)
        XCTAssertEqual(h.sent, [group])
        XCTAssertEqual(h.recorded.count, 1)
        XCTAssertEqual(h.recorded.first?.atMs, h.now)
        XCTAssertEqual(h.ruleCalls.first?.joined, true)
    }

    /// A torn roster read must not stop the join request: the count is
    /// irrelevant to that trigger, so it is fed to the rule as zero.
    func testJoinAsksEvenWhenTheRosterCannotBeRead() async {
        let h = Harness()
        h.unnamed = nil
        await assertOutcome(.requested) { await h.requester().noteJoined(groupIdHex: self.group) }
        XCTAssertEqual(h.ruleCalls.first?.unnamed, 0)
        XCTAssertEqual(h.sent, [group])
    }

    // MARK: - The on-open trigger

    func testOpenWithEveryoneNamedAsksNothing() async {
        let h = Harness()
        h.unnamed = 0
        await assertOutcome(.notDue) { await h.requester().noteOpened(groupIdHex: self.group) }
        XCTAssertTrue(h.sent.isEmpty)
        XCTAssertTrue(h.recorded.isEmpty)
    }

    func testOpenWithAPlaceholderAndNoPriorRequestAsks() async {
        let h = Harness()
        h.unnamed = 3
        await assertOutcome(.requested) { await h.requester().noteOpened(groupIdHex: self.group) }
        XCTAssertEqual(h.sent, [group])
        XCTAssertEqual(h.recorded.first?.atMs, h.now)
        XCTAssertEqual(h.ruleCalls.first?.unnamed, 3)
        XCTAssertNil(h.ruleCalls.first?.last)
    }

    /// The whole point of the stamp: a request repeats no sooner than the
    /// core's interval, so the answers get a day to arrive before the group is
    /// asked again.
    func testOpenAskedRecentlyIsThrottledUntilTheInterval() async {
        let h = Harness()
        h.unnamed = 1
        // Far enough from zero that subtracting the 24 h interval cannot
        // underflow the unsigned clock.
        h.now = 10 * interval
        h.last = h.now - (interval - 1)
        await assertOutcome(.notDue) { await h.requester().noteOpened(groupIdHex: self.group) }
        XCTAssertTrue(h.sent.isEmpty)

        h.last = h.now - interval
        await assertOutcome(.requested) { await h.requester().noteOpened(groupIdHex: self.group) }
        XCTAssertEqual(h.sent, [group])
    }

    /// Two opens in one session: the first asks and stamps, the second reads
    /// the stamp the first wrote and declines.
    func testASecondOpenInTheSameSessionReadsTheFirstStamp() async {
        let h = Harness()
        h.unnamed = 2
        let requester = h.requester()
        await assertOutcome(.requested) { await requester.noteOpened(groupIdHex: self.group) }
        h.now += 5_000
        await assertOutcome(.notDue) { await requester.noteOpened(groupIdHex: self.group) }
        XCTAssertEqual(h.sent.count, 1)
    }

    /// An unreadable roster is not evidence that anyone is unnamed, so an open
    /// asks nothing on it (the join trigger differs, above).
    func testOpenWithAnUnreadableRosterAsksNothing() async {
        let h = Harness()
        h.unnamed = nil
        await assertOutcome(.cannotAsk) { await h.requester().noteOpened(groupIdHex: self.group) }
        XCTAssertTrue(h.ruleCalls.isEmpty)
        XCTAssertTrue(h.sent.isEmpty)
    }

    // MARK: - Guards shared by both triggers

    /// An evicted device (or one with no handle) asks nothing and consults no
    /// rule — the send would be refused anyway.
    func testCannotSendAsksNothing() async {
        let h = Harness()
        h.canSend = false
        h.unnamed = 4
        await assertOutcome(.cannotAsk) { await h.requester().noteOpened(groupIdHex: self.group) }
        await assertOutcome(.cannotAsk) { await h.requester().noteJoined(groupIdHex: self.group) }
        XCTAssertTrue(h.ruleCalls.isEmpty)
        XCTAssertTrue(h.sent.isEmpty)
        XCTAssertTrue(h.recorded.isEmpty)
    }

    /// A publish that threw asked nobody, so nothing is recorded and the next
    /// trigger asks again rather than staying silent for a day.
    func testAFailedSendIsNotStamped() async {
        let h = Harness()
        h.unnamed = 1
        h.sendFails = true
        await assertOutcome(.sendFailed) { await h.requester().noteOpened(groupIdHex: self.group) }
        XCTAssertTrue(h.recorded.isEmpty)
        XCTAssertNil(h.last)

        h.sendFails = false
        await assertOutcome(.requested) { await h.requester().noteOpened(groupIdHex: self.group) }
        XCTAssertEqual(h.recorded.count, 1)
    }

    /// The rule is the ONLY decision-maker: with an injected rule that always
    /// says yes, an open with nobody unnamed still asks; with one that always
    /// says no, a join does not. (The production rule is the core's; this pins
    /// that the orchestration adds no rule of its own.)
    func testTheInjectedRuleIsTheWholeDecision() async {
        let yes = Harness()
        yes.unnamed = 0
        yes.rule = { _, _, _, _ in true }
        await assertOutcome(.requested) { await yes.requester().noteOpened(groupIdHex: self.group) }

        let no = Harness()
        no.unnamed = 5
        no.rule = { _, _, _, _ in false }
        await assertOutcome(.notDue) { await no.requester().noteJoined(groupIdHex: self.group) }
        XCTAssertTrue(no.sent.isEmpty)
    }

    // MARK: - The real core rule and wire

    /// Pins the FFI rule the production ports call, on the inputs the tests
    /// above feed it, so the Swift orchestration and the shared rule agree.
    func testTheRealCoreRule() {
        let now: UInt64 = 5_000_000_000
        XCTAssertTrue(shouldRequestProfiles(
            joinedJustNow: true, unnamedMembers: 0, lastRequestedAtMs: now, nowMs: now))
        XCTAssertFalse(shouldRequestProfiles(
            joinedJustNow: false, unnamedMembers: 0, lastRequestedAtMs: nil, nowMs: now))
        XCTAssertTrue(shouldRequestProfiles(
            joinedJustNow: false, unnamedMembers: 1, lastRequestedAtMs: nil, nowMs: now))
        XCTAssertFalse(shouldRequestProfiles(
            joinedJustNow: false, unnamedMembers: 1, lastRequestedAtMs: now - interval + 1, nowMs: now))
        XCTAssertTrue(shouldRequestProfiles(
            joinedJustNow: false, unnamedMembers: 1, lastRequestedAtMs: now - interval, nowMs: now))
        // A clock that went backwards reads as "recently asked", never as a
        // reason to ask again.
        XCTAssertFalse(shouldRequestProfiles(
            joinedJustNow: false, unnamedMembers: 1, lastRequestedAtMs: now + 10, nowMs: now))
        XCTAssertEqual(interval, 24 * 60 * 60 * 1_000)
    }

    /// The length the foreground arm and the NSE peek read off the FFI is the
    /// length of the composed wire, and every request is byte-identical.
    func testTheRequestWireMatchesTheAdvertisedLength() {
        let wire = composeProfileRequest()
        XCTAssertEqual(wire.count, Int(profileRequestEnvelopeLen()))
        XCTAssertEqual(wire.count, 26)
        XCTAssertEqual(wire.first, 0x6C)
        XCTAssertEqual(wire, composeProfileRequest())
        XCTAssertNoThrow(try decodeProfileRequest(plaintext: wire))
    }

    // MARK: - Foreground classification

    private func classify(_ plaintext: Data, senderLeaf: UInt32? = 3) -> IncomingChatEvent {
        ChatClient.toEvent(
            messageId: 1,
            createdAt: "2026-09-19T00:00:00Z",
            processed: .application(data: plaintext, senderLeaf: senderLeaf, senderAccount: nil),
            resolveSender: { "member-\($0)" }
        )
    }

    func testARequestClassifiesAsARequestWithItsSender() {
        guard case .profileRequest(let messageId, let senderKey, _) = classify(composeProfileRequest())
        else {
            return XCTFail("a profile request must classify as .profileRequest")
        }
        XCTAssertEqual(messageId, 1)
        XCTAssertEqual(senderKey, "member-3")
    }

    /// Whatever the classification, no event may carry the domain as visible
    /// text — the defect the arm exists to close.
    func testARequestNeverReachesTheTimelineAsText() {
        if case .application(_, let text, _, _, _) = classify(composeProfileRequest()) {
            XCTFail("the request rendered as a chat bubble reading '\(text)'")
        }
        XCTAssertTrue(ChatClient.looksLikeProfileRequest(composeProfileRequest()))
    }

    /// An unattributable sender still never becomes a bubble; the receive
    /// side is what declines to ANSWER it.
    func testAnUnattributableRequestIsStillNotABubble() {
        guard case .profileRequest(_, let senderKey, _) = classify(composeProfileRequest(), senderLeaf: nil)
        else {
            return XCTFail("expected .profileRequest")
        }
        XCTAssertNil(senderKey)
    }

    /// A schema version this build does not know is NOT swallowed by the arm:
    /// it falls through like any other unrecognised plaintext, so a future v2
    /// is visible rather than silently dropped. The beacon (one byte longer)
    /// and a chat envelope are not requests either.
    func testTheArmIsFailClosedOnVersionAndExactOnLength() throws {
        var v2 = composeProfileRequest()
        v2[v2.count - 1] = 0x02
        XCTAssertFalse(ChatClient.looksLikeProfileRequest(v2))
        if case .profileRequest = classify(v2) { XCTFail("an unknown version must not classify as a request") }
        XCTAssertFalse(ChatClient.looksLikeProfileRequest(composeCommitterBeacon()))
        XCTAssertFalse(ChatClient.looksLikeProfileRequest(try composeEnvelope(text: "hi", attachments: [])))
        XCTAssertFalse(ChatClient.looksLikeProfileRequest(Data(repeating: 0x6C, count: 26)))
    }

    // MARK: - The stamp store

    func testStoreRoundTripsPerAccountAndGroup() throws {
        let suite = "ProfileRequestPolicyTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = ProfileRequestStore(defaults: defaults)

        XCTAssertNil(store.lastRequestedAtMs(accountIdB64: "A", groupIdHex: group))
        store.record(accountIdB64: "A", groupIdHex: group, atMs: 42)
        XCTAssertEqual(store.lastRequestedAtMs(accountIdB64: "A", groupIdHex: group), 42)
        // Another account and another group do not see it.
        XCTAssertNil(store.lastRequestedAtMs(accountIdB64: "B", groupIdHex: group))
        XCTAssertNil(store.lastRequestedAtMs(accountIdB64: "A", groupIdHex: "cd34"))
        // A value past Int64.max survives the UserDefaults round trip.
        store.record(accountIdB64: "A", groupIdHex: "cd34", atMs: UInt64.max)
        XCTAssertEqual(store.lastRequestedAtMs(accountIdB64: "A", groupIdHex: "cd34"), UInt64.max)
        // Overwrite is the newest write.
        store.record(accountIdB64: "A", groupIdHex: group, atMs: 43)
        XCTAssertEqual(store.lastRequestedAtMs(accountIdB64: "A", groupIdHex: group), 43)
        store.clear(accountIdB64: "A")
        XCTAssertNil(store.lastRequestedAtMs(accountIdB64: "A", groupIdHex: group))
    }
}
