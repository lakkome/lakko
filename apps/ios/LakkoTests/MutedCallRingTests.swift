// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// `docs/ux/calls.md` section 2.1 — "a muted chat never rings", on the three
/// iOS surfaces that can ring.
///
/// Section 2.1.1 says what "never rings" does NOT mean: the call is still
/// OFFERED and still answerable, quietly, for the whole window. Everything here
/// is about the verdict and the flag that carries it, both unchanged by that;
/// what the app then DOES with a `.silent` verdict is `CallHostTests`.
///
/// What is tested here is the WIRING, never the rule: the rule is
/// `lakko-ui-rules`, which pins its own boundary, its own ordering and its own
/// `Silent`-is-not-`TooLate` distinction in Rust. What Rust cannot see is
/// whether this platform ever asks it, whether the answer reaches the three
/// paths, and whether the mirror that carries the mute flag across a process
/// boundary fails in the direction that rings.
@MainActor
final class MutedCallRingTests: XCTestCase {

    private var defaults: UserDefaults!
    private let suite = "me.lakko.tests.muted-call-ring"

    override func setUp() {
        super.setUp()
        UserDefaults().removePersistentDomain(forName: suite)
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() {
        UserDefaults().removePersistentDomain(forName: suite)
        defaults = nil
        super.tearDown()
    }

    // MARK: - The mirror

    /// The one that matters most. A missing mirror must ring: an unwanted ring
    /// is an annoyance, a swallowed call is a call the person never finds out
    /// about, and "the file is not there yet" is an ordinary state on a fresh
    /// install and in an unprovisioned App Group.
    func testAMissingMirrorRings() {
        XCTAssertFalse(ConversationMuteMirror.isMuted(groupIdHex: "aa11", defaults: defaults))
        XCTAssertFalse(ConversationMuteMirror.isMuted(groupIdHex: "aa11", defaults: nil))
        ConversationMuteMirror.replaceAll(mutedGroupIdsHex: ["aa11"], defaults: nil)
        XCTAssertFalse(ConversationMuteMirror.isMuted(groupIdHex: "aa11", defaults: nil))
    }

    func testAMirroredConversationReadsAsMuted() {
        ConversationMuteMirror.replaceAll(mutedGroupIdsHex: ["aa11"], defaults: defaults)
        XCTAssertTrue(ConversationMuteMirror.isMuted(groupIdHex: "aa11", defaults: defaults))
        XCTAssertFalse(ConversationMuteMirror.isMuted(groupIdHex: "bb22", defaults: defaults))
    }

    /// The reason the write is whole-set and not per-row: an unmute, including
    /// one done on a sibling device and folded in by the sync poll, has to
    /// REMOVE the row. A per-row write would leave the old one behind and go
    /// on silencing a conversation the person un-silenced.
    func testUnmutingRemovesTheRowRatherThanLeavingItBehind() {
        ConversationMuteMirror.replaceAll(mutedGroupIdsHex: ["aa11", "bb22"], defaults: defaults)
        ConversationMuteMirror.replaceAll(mutedGroupIdsHex: ["bb22"], defaults: defaults)
        XCTAssertFalse(ConversationMuteMirror.isMuted(groupIdHex: "aa11", defaults: defaults))
        XCTAssertTrue(ConversationMuteMirror.isMuted(groupIdHex: "bb22", defaults: defaults))

        ConversationMuteMirror.replaceAll(mutedGroupIdsHex: [], defaults: defaults)
        XCTAssertFalse(ConversationMuteMirror.isMuted(groupIdHex: "bb22", defaults: defaults))
    }

    /// The writer and the readers reach the key from different places — the
    /// sync repository's flag map, a DS row's group hex, a push route — and
    /// nothing guarantees they agree on case.
    func testTheKeyIsCaseInsensitive() {
        ConversationMuteMirror.replaceAll(mutedGroupIdsHex: ["AA11"], defaults: defaults)
        XCTAssertTrue(ConversationMuteMirror.isMuted(groupIdHex: "aa11", defaults: defaults))
    }

    /// A row left behind after a logout would silence the next account's calls
    /// in a conversation it has never heard of.
    func testClearAllLeavesNothingBehind() {
        ConversationMuteMirror.replaceAll(mutedGroupIdsHex: ["aa11", "bb22"], defaults: defaults)
        ConversationMuteMirror.clearAll(defaults: defaults)
        XCTAssertFalse(ConversationMuteMirror.isMuted(groupIdHex: "aa11", defaults: defaults))
        XCTAssertFalse(ConversationMuteMirror.isMuted(groupIdHex: "bb22", defaults: defaults))
    }

    /// It carries the muted set and nothing else — not the conversation list.
    /// An unmuted conversation leaves no row, so the shared container learns
    /// nothing about it.
    func testOnlyMutedConversationsLeaveARow() {
        ConversationMuteMirror.replaceAll(mutedGroupIdsHex: ["aa11"], defaults: defaults)
        let keys = defaults.dictionaryRepresentation().keys
            .filter { $0.hasPrefix("me.lakko.conversation-muted.") }
        XCTAssertEqual(keys.count, 1)
        XCTAssertTrue(keys.allSatisfy { $0.hasSuffix("aa11") })
    }

    // MARK: - The routing decision

    private let callA = Data(repeating: 0x0A, count: 16)
    private let groupA = "aa11"
    private let t0: Int64 = 1_800_000_000_000

    private func dsTime(_ ms: Int64) -> String {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
    }

    private func offerRow(ageMs: Int64) -> InboundCallRow {
        InboundCallRow(
            groupIdHex: groupA,
            messageId: 1,
            createdAt: dsTime(t0 - ageMs),
            body: .offer(
                callId: callA,
                sdp: "v=0",
                createdAtMs: 1,
                media: FfiCallMediaFlags(audio: true, video: false))
        )
    }

    private func decide(
        ageMs: Int64 = 1_000,
        muted: Bool,
        active: CallHostRouting.ActiveCall? = nil,
        ended: Set<String> = [],
        otherCallBusy: Bool = false
    ) -> CallHostRouting.Action {
        CallHostRouting.decide(
            row: offerRow(ageMs: ageMs),
            nowMs: t0,
            active: active,
            endedCallIds: ended,
            otherCallBusy: otherCallBusy,
            muted: muted)
    }

    func testAFreshOfferRingsWhenTheConversationIsNotMuted() {
        XCTAssertEqual(decide(muted: false), .ring)
    }

    /// The rule that was implemented nowhere on this platform.
    func testAMutedConversationDoesNotRing() {
        XCTAssertEqual(decide(muted: true), .silent)
    }

    /// And `silent` is not `drop`. The distinction is the whole point: a
    /// person who muted a conversation asked not to be interrupted by it, not
    /// to stop being reachable by the person in it — so `silent` opens the same
    /// answerable call `ring` does and withholds only the interruption
    /// (section 2.1.1).
    func testSilentIsNotTheSameAnswerAsDropping() {
        XCTAssertNotEqual(decide(muted: true), CallHostRouting.Action.drop)
        XCTAssertNotEqual(
            decide(muted: true),
            decide(ageMs: Int64(CallHostRouting.ringWindowMs), muted: false))
    }

    /// Past the window nothing rings and nothing is recorded as still
    /// answerable, muted or not: the caller has already given up.
    func testPastTheWindowNothingRingsEitherWay() {
        let window = Int64(CallHostRouting.ringWindowMs)
        XCTAssertEqual(decide(ageMs: window, muted: false), .drop)
        XCTAssertEqual(decide(ageMs: window, muted: true), .drop)
    }

    /// `End{busy}` is a message to the CALLER, not an interruption of the
    /// person here — it stops a phone ringing in someone else's hand.
    /// Withholding it because this conversation is muted would make the caller
    /// wait out the whole window for nothing.
    func testBusyStillAnswersTheCallerFromAMutedConversation() {
        XCTAssertEqual(decide(muted: true, otherCallBusy: true), .busy)
        XCTAssertEqual(
            decide(
                muted: true,
                active: .init(callIdHex: "ff", groupIdHex: "cc33")),
            .busy)
    }

    /// A call already dealt with never comes back, muted or not.
    func testAnEndedCallNeverRingsAgainNorRecordsTwice() {
        let hex = CallHostRouting.hex(callA)
        XCTAssertEqual(decide(muted: true, ended: [hex]), .drop)
        XCTAssertEqual(decide(muted: false, ended: [hex]), .drop)
    }

    // MARK: - The CallKit report

    /// The VoIP path's only new decision. It is made from the wake alone, at
    /// report time, because that is all iOS gives this path before it demands
    /// a call be reported.
    func testTheVoipWakeIsSilentOnlyForAMutedConversation() {
        let group = String(repeating: "a", count: 64)
        ConversationMuteMirror.replaceAll(mutedGroupIdsHex: [group], defaults: defaults)

        XCTAssertTrue(VoipPushRegistrar.wakeIsMuted(
            VoipWakePayload(groupHex: group, pointer: 7, isCallWake: true),
            defaults: defaults))
        XCTAssertFalse(VoipPushRegistrar.wakeIsMuted(
            VoipWakePayload(groupHex: String(repeating: "b", count: 64), pointer: 7, isCallWake: true),
            defaults: defaults))
    }

    /// A wake that names no conversation rings. There is nothing to look up,
    /// and a VoIP push that quietly goes nowhere is a call the person never
    /// hears about — worse than an unwanted ring in every case.
    func testAWakeWithNoGroupRings() {
        XCTAssertFalse(VoipPushRegistrar.wakeIsMuted(
            VoipWakePayload(groupHex: nil, pointer: nil, isCallWake: true),
            defaults: defaults))
    }

    /// And so does one whose conversation this device has no mirror row for —
    /// the fresh-install and unprovisioned-suite case, on the path where being
    /// wrong is most expensive.
    func testAWakeForAnUnmirroredConversationRings() {
        XCTAssertFalse(VoipPushRegistrar.wakeIsMuted(
            VoipWakePayload(groupHex: "cc33", pointer: 1, isCallWake: true),
            defaults: defaults))
        XCTAssertFalse(VoipPushRegistrar.wakeIsMuted(
            VoipWakePayload(groupHex: "cc33", pointer: 1, isCallWake: true),
            defaults: nil))
    }

    // MARK: - The extension's copy of the rule

    /// The drift guard for the one process that cannot call the shared rule.
    ///
    /// The notification-service extension links the slim `LakkoCoreExtension`
    /// FFI (ADR-0041 Phase 2), which does not carry `ringDecision`, so
    /// ``nseRingVerdict(muted:createdAt:nowMs:)`` restates it in Swift. This
    /// test target links the FULL framework, so it can run both side by side:
    /// a change to the Rust rule that the copy does not follow fails here
    /// rather than on a phone.
    func testTheExtensionsCopyAgreesWithTheSharedRule() {
        let window = Int64(LakkoCore.callRingWindowMs())
        XCTAssertEqual(nseCallRingWindowMs, LakkoCore.callRingWindowMs())

        for muted in [false, true] {
            for ageMs: Int64 in [0, 1, 1_000, window - 1, window, window + 1, window * 10] {
                let expected: NseRingVerdict
                switch LakkoCore.ringDecision(muted: muted, offerAgeMs: UInt64(ageMs)) {
                case .ring: expected = .ring
                case .silent: expected = .silent
                case .tooLate: expected = .tooLate
                }
                XCTAssertEqual(
                    nseRingVerdict(muted: muted, createdAt: dsTime(t0 - ageMs), nowMs: t0),
                    expected,
                    "muted=\(muted) age=\(ageMs)")
            }
        }
    }

    /// An age that cannot be established is not an age the window can be
    /// measured against, so it is treated as past it — the same answer the
    /// app's own routing gives an unreadable row time.
    func testAnUnreadableRowTimeIsTreatedAsPastTheWindow() {
        XCTAssertEqual(nseRingVerdict(muted: false, createdAt: "", nowMs: t0), .tooLate)
        XCTAssertEqual(nseRingVerdict(muted: false, createdAt: "not a time", nowMs: t0), .tooLate)
    }

    /// The server's clock may read ahead of this device's. That is a zero-age
    /// offer, not a negative one that would ring past the window.
    func testAnOfferStampedInTheFutureIsSimplyFresh() {
        XCTAssertEqual(nseRingVerdict(muted: false, createdAt: dsTime(t0 + 9_000), nowMs: t0), .ring)
        XCTAssertEqual(nseRingVerdict(muted: true, createdAt: dsTime(t0 + 9_000), nowMs: t0), .silent)
    }
}
