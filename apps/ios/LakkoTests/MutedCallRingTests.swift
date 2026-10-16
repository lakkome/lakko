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

    private let acct = "acct-A"
    private let other = "acct-B"

    /// The one that matters most. A missing mirror must ring: an unwanted ring
    /// is an annoyance, a swallowed call is a call the person never finds out
    /// about, and "the file is not there yet" is an ordinary state on a fresh
    /// install and in an unprovisioned App Group.
    func testAMissingMirrorRings() {
        XCTAssertFalse(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "aa11", defaults: defaults))
        XCTAssertFalse(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "aa11", defaults: nil))
        ConversationMuteMirror.replaceAll(accountIdB64: acct, mutedGroupIdsHex: ["aa11"], defaults: nil)
        XCTAssertFalse(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "aa11", defaults: nil))
    }

    func testAMirroredConversationReadsAsMuted() {
        ConversationMuteMirror.replaceAll(accountIdB64: acct, mutedGroupIdsHex: ["aa11"], defaults: defaults)
        XCTAssertTrue(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "aa11", defaults: defaults))
        XCTAssertFalse(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "bb22", defaults: defaults))
    }

    /// The reason the write is whole-set and not per-row: an unmute, including
    /// one done on a sibling device and folded in by the sync poll, has to
    /// REMOVE the row. A per-row write would leave the old one behind and go
    /// on silencing a conversation the person un-silenced.
    func testUnmutingRemovesTheRowRatherThanLeavingItBehind() {
        ConversationMuteMirror.replaceAll(accountIdB64: acct, mutedGroupIdsHex: ["aa11", "bb22"], defaults: defaults)
        ConversationMuteMirror.replaceAll(accountIdB64: acct, mutedGroupIdsHex: ["bb22"], defaults: defaults)
        XCTAssertFalse(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "aa11", defaults: defaults))
        XCTAssertTrue(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "bb22", defaults: defaults))

        ConversationMuteMirror.replaceAll(accountIdB64: acct, mutedGroupIdsHex: [], defaults: defaults)
        XCTAssertFalse(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "bb22", defaults: defaults))
    }

    /// The writer and the readers reach the key from different places — the
    /// sync repository's flag map, a DS row's group hex, a push route — and
    /// nothing guarantees they agree on case.
    func testTheKeyIsCaseInsensitive() {
        ConversationMuteMirror.replaceAll(accountIdB64: acct, mutedGroupIdsHex: ["AA11"], defaults: defaults)
        XCTAssertTrue(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "aa11", defaults: defaults))
    }

    /// Two accounts on one phone never see each other's mutes: a mute one
    /// account set does not silence the other's call in the same group, and
    /// one account republishing its set leaves the other's rows alone.
    func testTwoAccountsNeverSeeEachOthersMutes() {
        ConversationMuteMirror.replaceAll(accountIdB64: acct, mutedGroupIdsHex: ["aa11"], defaults: defaults)
        XCTAssertFalse(ConversationMuteMirror.isMuted(accountIdB64: other, groupIdHex: "aa11", defaults: defaults))

        ConversationMuteMirror.replaceAll(accountIdB64: other, mutedGroupIdsHex: ["bb22"], defaults: defaults)
        XCTAssertTrue(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "aa11", defaults: defaults))
        XCTAssertFalse(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "bb22", defaults: defaults))
        XCTAssertTrue(ConversationMuteMirror.isMuted(accountIdB64: other, groupIdHex: "bb22", defaults: defaults))
    }

    /// No account: nothing is muted and nothing is written.
    func testNoAccountReadsNothingAndWritesNothing() {
        ConversationMuteMirror.replaceAll(accountIdB64: nil, mutedGroupIdsHex: ["aa11"], defaults: defaults)
        XCTAssertFalse(ConversationMuteMirror.isMuted(accountIdB64: nil, groupIdHex: "aa11", defaults: defaults))
        XCTAssertTrue(defaults.dictionaryRepresentation().keys
            .filter { $0.hasPrefix("me.lakko.conversation-muted.") }.isEmpty)
    }

    /// Forget-device removes the forgotten account's rows (and the old
    /// device-wide rows) and nothing else.
    func testForgetClearsOnlyThatAccount() {
        ConversationMuteMirror.replaceAll(accountIdB64: acct, mutedGroupIdsHex: ["aa11", "bb22"], defaults: defaults)
        ConversationMuteMirror.replaceAll(accountIdB64: other, mutedGroupIdsHex: ["aa11"], defaults: defaults)
        defaults.set(true, forKey: ConversationMuteMirror.legacyKeyPrefix + "aa11")
        ConversationMuteMirror.clear(accountIdB64: acct, defaults: defaults)
        XCTAssertFalse(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "aa11", defaults: defaults))
        XCTAssertFalse(ConversationMuteMirror.isMuted(accountIdB64: acct, groupIdHex: "bb22", defaults: defaults))
        XCTAssertTrue(ConversationMuteMirror.isMuted(accountIdB64: other, groupIdHex: "aa11", defaults: defaults))
        XCTAssertNil(defaults.object(forKey: ConversationMuteMirror.legacyKeyPrefix + "aa11"))
    }

    /// It carries the muted set and nothing else — not the conversation list.
    /// An unmuted conversation leaves no row, so the shared container learns
    /// nothing about it.
    func testOnlyMutedConversationsLeaveARow() {
        ConversationMuteMirror.replaceAll(accountIdB64: acct, mutedGroupIdsHex: ["aa11"], defaults: defaults)
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

    /// The binding that names the signed-in account (ADR-0042, amended
    /// 2026-09-30, with ADR-0078).
    private var binding: PushAccountBinding { PushAccountBinding(accountIdB64: acct) }

    /// A wake for `pushGid`, routed for the bound account to `group` (the
    /// same string unless given), carrying the subscription id this device
    /// holds for that row. `subId` overrides what the wake carries.
    private func wake(_ pushGid: String?, group: String? = nil, subId: String?? = .none) -> VoipWakePayload {
        var held: String?
        if let pushGid {
            let routes = PushRoutingStore(defaults: defaults)
            routes.putRouting(
                accountIdB64: acct, groupIdHex: group ?? pushGid, secret: Data(repeating: 0x33, count: 32),
                pushGidHex: pushGid, distributed: true, mintEpoch: 0)
            held = PushAccountBinding.hex(routes.ensureSubscriptionId(accountIdB64: acct, pushGidHex: pushGid))
        }
        let carried: String?
        switch subId {
        case .none: carried = held ?? String(repeating: "5", count: 32)
        case .some(let value): carried = value
        }
        return VoipWakePayload(groupHex: pushGid, pointer: 7, isCallWake: true, subscriptionIdHex: carried)
    }

    private func decide(_ wake: VoipWakePayload, binding: PushAccountBinding?) -> VoipWakeDecision {
        VoipPushRegistrar.decide(
            wake, binding: binding, routes: PushRoutingStore(defaults: defaults), defaults: defaults)
    }

    /// The VoIP path's mute decision. It is made from the wake alone, at
    /// report time, because that is all iOS gives this path before it demands
    /// a call be reported.
    func testTheVoipWakeIsSilentOnlyForAMutedConversation() {
        let group = String(repeating: "a", count: 64)
        let otherGroup = String(repeating: "b", count: 64)
        ConversationMuteMirror.replaceAll(accountIdB64: acct, mutedGroupIdsHex: [group], defaults: defaults)

        XCTAssertEqual(
            decide(wake(group), binding: binding),
            .silent(VoipWakeTarget(accountIdB64: acct, groupIdHex: group)))
        XCTAssertEqual(
            decide(wake(otherGroup), binding: binding),
            .ring(VoipWakeTarget(accountIdB64: acct, groupIdHex: otherGroup)))
    }

    /// Another account's mute never silences this one's call: the mute flag
    /// read is the BOUND account's.
    func testAnotherAccountsMuteNeverSilencesTheBoundAccount() {
        let group = String(repeating: "a", count: 64)
        ConversationMuteMirror.replaceAll(accountIdB64: other, mutedGroupIdsHex: [group], defaults: defaults)
        XCTAssertEqual(
            decide(wake(group), binding: binding),
            .ring(VoipWakeTarget(accountIdB64: acct, groupIdHex: group)))
    }

    /// A wake whose conversation this device has no mute mirror row for —
    /// the fresh-install and unprovisioned-suite case — rings.
    func testAWakeForAnUnmirroredConversationRings() {
        XCTAssertEqual(
            decide(wake("cc33"), binding: binding),
            .ring(VoipWakeTarget(accountIdB64: acct, groupIdHex: "cc33")))
        XCTAssertEqual(
            VoipPushRegistrar.decide(
                wake("cc33"), binding: binding, routes: PushRoutingStore(defaults: defaults), defaults: nil),
            .ring(VoipWakeTarget(accountIdB64: acct, groupIdHex: "cc33")))
    }

    // MARK: - Only the signed-in account (ADR-0042, amended 2026-09-30)

    /// Signed out: no binding. The call is reported (Apple's rule) and ended
    /// at once, nameless, and nothing is fetched.
    func testWithNoBindingTheWakeIsRefused() {
        XCTAssertEqual(decide(wake(String(repeating: "a", count: 64)), binding: nil), .refuse)
        XCTAssertEqual(decide(wake(nil), binding: nil), .refuse)
    }

    /// ADR-0078 — a wake for a subscription this account does not hold, with
    /// no `s`, or naming no conversation, is refused.
    func testAWakeForASubscriptionNotHeldIsRefused() {
        let group = String(repeating: "a", count: 64)
        XCTAssertEqual(decide(wake(group, subId: String(repeating: "e", count: 32)), binding: binding), .refuse)
        XCTAssertEqual(decide(wake(group, subId: .some(nil)), binding: binding), .refuse)
        XCTAssertEqual(decide(wake(nil), binding: binding), .refuse)
    }

    /// The reverse index is read for the bound account: a row another account
    /// filed under the same `push_gid` (with its own subscription) is a miss.
    func testTheRouteIsReadForTheBoundAccountOnly() {
        let pushGid = String(repeating: "f", count: 64)
        let mine = String(repeating: "1", count: 64)
        let theirs = String(repeating: "2", count: 64)
        let routes = PushRoutingStore(defaults: defaults)
        routes.putRouting(
            accountIdB64: other, groupIdHex: theirs, secret: Data(repeating: 0x33, count: 32),
            pushGidHex: pushGid, distributed: true, mintEpoch: 0)
        let theirSub = PushAccountBinding.hex(routes.ensureSubscriptionId(accountIdB64: other, pushGidHex: pushGid))
        XCTAssertEqual(
            decide(VoipWakePayload(groupHex: pushGid, pointer: 7, isCallWake: true, subscriptionIdHex: theirSub),
                   binding: binding),
            .refuse,
            "another account's row is a miss")

        ConversationMuteMirror.replaceAll(accountIdB64: acct, mutedGroupIdsHex: [mine], defaults: defaults)
        XCTAssertEqual(
            decide(wake(pushGid, group: mine), binding: binding),
            .silent(VoipWakeTarget(accountIdB64: acct, groupIdHex: mine)))
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
