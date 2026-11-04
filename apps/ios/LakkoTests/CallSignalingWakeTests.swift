// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// Which outbound call-signaling frames ask for a push wake.
///
/// `docs/ux/calls.md` §2.1 requires a call to arrive on a device whose app is
/// backgrounded or closed, and §7 names the missing piece as a call-kind wake.
/// The rule is deliberately narrow: an offer starts the ring, an end stops it,
/// and nothing else wakes anybody — an answer and an ICE candidate belong to a
/// call whose peer is already awake, and the group-call bodies are not a 1x1
/// ring at all.
///
/// Every fixture is composed with the real `composeCallSignaling`, so the tag
/// table is checked against the bytes prost actually emits rather than against
/// a hand-written copy of them. A hand-written tag would pass a test and fail
/// on the wire, which is the whole failure mode a byte peek has.
final class CallSignalingWakeTests: XCTestCase {

    private let callId = Data(repeating: 0x0A, count: 16)

    private func offer(video: Bool = false) throws -> Data {
        try composeCallSignaling(body: .offer(
            callId: callId, sdp: "v=0", createdAtMs: 1,
            media: FfiCallMediaFlags(audio: true, video: video)))
    }

    private func end(_ reason: FfiCallEndReason = .hungUp) throws -> Data {
        try composeCallSignaling(body: .end(callId: callId, reason: reason))
    }

    // MARK: - The two that wake

    func testAnOfferWakes() throws {
        XCTAssertEqual(CallSignalingWake.bodyKind(try offer()), .offer)
        XCTAssertTrue(CallSignalingWake.shouldWake(try offer()))
    }

    /// A video offer is the same body with a flag set — nothing about the
    /// wake decision may depend on the media kind, which is not on the wake
    /// payload at all.
    func testAVideoOfferWakesToo() throws {
        XCTAssertTrue(CallSignalingWake.shouldWake(try offer(video: true)))
    }

    /// Without this the ring outlives the call: a cancelled or declined call
    /// keeps ringing a closed app's notification until the user dismisses it
    /// by hand. Every end reason must stop it, not just the ordinary hang-up.
    func testEveryEndReasonWakes() throws {
        let reasons: [FfiCallEndReason] = [
            .hungUp, .declined, .busy, .timedOut, .error,
        ]
        for reason in reasons {
            let bytes = try end(reason)
            XCTAssertEqual(CallSignalingWake.bodyKind(bytes), .end, "reason \(reason)")
            XCTAssertTrue(CallSignalingWake.shouldWake(bytes), "reason \(reason)")
        }
    }

    // MARK: - The five that stay silent

    /// The peer answering is by construction awake and in the call.
    func testAnAnswerDoesNotWake() throws {
        let bytes = try composeCallSignaling(body: .answer(callId: callId, sdp: "v=0"))
        XCTAssertEqual(CallSignalingWake.bodyKind(bytes), .answer)
        XCTAssertFalse(CallSignalingWake.shouldWake(bytes))
    }

    /// A call trickles a burst of these; one push each would be the loudest
    /// thing in the system for no gain.
    func testAnIceCandidateDoesNotWake() throws {
        let bytes = try composeCallSignaling(body: .iceCandidate(
            callId: callId, candidate: "candidate:1 1 udp 2130706431 10.0.0.1 54321 typ host",
            sdpMid: "0", sdpMLineIndex: 0))
        XCTAssertEqual(CallSignalingWake.bodyKind(bytes), .candidate)
        XCTAssertFalse(CallSignalingWake.shouldWake(bytes))
    }

    /// Group-call control frames are announcements, not a ring
    /// (`docs/ux/calls.md` §9 puts the group call out of scope). Waking for a
    /// join or a leave would push once per participant churn.
    func testGroupControlBodiesDoNotWake() throws {
        let start = try composeCallSignaling(body: .groupStart(
            callId: callId, initiatorLeaf: 2, createdAtMs: 1,
            media: FfiCallMediaFlags(audio: true, video: false)))
        let join = try composeCallSignaling(body: .groupJoin(
            callId: callId, leafIndex: 2, joinedAtMs: 1))
        let leave = try composeCallSignaling(body: .groupLeave(
            callId: callId, leafIndex: 2, reason: .hungUp))

        XCTAssertEqual(CallSignalingWake.bodyKind(start), .groupStart)
        XCTAssertEqual(CallSignalingWake.bodyKind(join), .groupJoin)
        // Field 16's tag no longer fits one varint byte (0x82 0x01); a rule
        // that read the first byte alone would call this "not signaling".
        XCTAssertEqual(CallSignalingWake.bodyKind(leave), .groupLeave)

        for bytes in [start, join, leave] {
            XCTAssertFalse(CallSignalingWake.shouldWake(bytes))
        }
    }

    // MARK: - Anything that is not signaling fails toward silence

    func testNonSignalingBytesNeitherClassifyNorWake() {
        let cases: [(String, Data)] = [
            ("empty", Data()),
            ("chat envelope", Data(#"{"v":1,"text":"hi"}"#.utf8)),
            ("service envelope", Data("lakko/committer-beacon/v1\0\u{01}".utf8)),
            // ASCII `r` / `z` / the two-byte tag's first byte, with nothing
            // behind them that frames a group body. A first-byte rule would
            // read all three as call signaling.
            ("bare r", Data("read you later".utf8)),
            ("bare z", Data("zebra".utf8)),
            ("bare 0x82", Data([0x82])),
            ("0x82 without its continuation", Data([0x82, 0x02, 0x00])),
        ]
        for (name, bytes) in cases {
            XCTAssertNil(CallSignalingWake.bodyKind(bytes), name)
            XCTAssertFalse(CallSignalingWake.shouldWake(bytes), name)
        }
    }

    /// The decision is taken from the bytes, so it must survive being handed
    /// a truncated frame: a prefix of an offer still names an offer (the tag
    /// is the first byte), and a prefix of a group body no longer frames one.
    /// Neither may crash — this runs on the publish path of every frame.
    func testTruncatedFramesAreClassifiedWithoutCrashing() throws {
        let offerBytes = try offer()
        for length in 1...offerBytes.count {
            let prefix = offerBytes.prefix(length)
            XCTAssertTrue(CallSignalingWake.shouldWake(Data(prefix)), "offer prefix \(length)")
        }
        let leave = try composeCallSignaling(body: .groupLeave(
            callId: callId, leafIndex: 2, reason: .hungUp))
        for length in 1..<leave.count {
            XCTAssertFalse(CallSignalingWake.shouldWake(Data(leave.prefix(length))))
        }
    }

    // MARK: - The wake the publisher asks for

    /// The port in `CallHost.live` asks for `.call` only for an offer or an
    /// end, and only when the session asks. Restated here against the bodies
    /// that matter so a change to either side has to change this test.
    func testThePublisherAsksForACallWakeOnlyForOfferAndEnd() throws {
        func requested(_ bytes: Data) -> DsWake.Kind? {
            CallSignalingWake.wakeKind(for: bytes, sessionAsks: true)
        }
        XCTAssertEqual(requested(try offer()), .call)
        XCTAssertEqual(requested(try end()), .call)
        XCTAssertNil(requested(try composeCallSignaling(
            body: .answer(callId: callId, sdp: "v=0"))))
        XCTAssertNil(requested(try composeCallSignaling(body: .groupJoin(
            callId: callId, leafIndex: 2, joinedAtMs: 1))))
    }

    // MARK: - The session's ask: the dial, and the end of a call that never connected

    /// The dial rings the other person; a renegotiation offer (camera on or
    /// off, the peer's screen share) is the same body for a call both sides
    /// are in, and must not: on iOS a call wake is a VoIP push, reported to
    /// CallKit as a new incoming call.
    func testTheDialOfferAsksForAWakeAndARenegotiationOfferDoesNot() throws {
        XCTAssertTrue(CallSignalingWake.sessionAsks(.dialOffer, everConnected: false))
        XCTAssertFalse(CallSignalingWake.sessionAsks(.renegotiationOffer, everConnected: true))
        XCTAssertFalse(CallSignalingWake.sessionAsks(.renegotiationOffer, everConnected: false))

        XCTAssertEqual(CallSignalingWake.wakeKind(for: try offer(), sessionAsks: true), .call)
        XCTAssertNil(
            CallSignalingWake.wakeKind(for: try offer(video: true), sessionAsks: false),
            "a renegotiation offer carries no wake")
    }

    /// An end stops a ring: the caller cancelling, the callee declining or
    /// missing it. After the call connected nobody is ringing, and a wake rang
    /// the other phone for a call that is over.
    func testAnEndAsksForAWakeOnlyWhileTheCallNeverConnected() throws {
        XCTAssertTrue(CallSignalingWake.sessionAsks(.end, everConnected: false))
        XCTAssertFalse(CallSignalingWake.sessionAsks(.end, everConnected: true))

        XCTAssertEqual(
            CallSignalingWake.wakeKind(
                for: try end(.declined),
                sessionAsks: CallSignalingWake.sessionAsks(.end, everConnected: false)),
            .call)
        XCTAssertNil(CallSignalingWake.wakeKind(
            for: try end(.hungUp),
            sessionAsks: CallSignalingWake.sessionAsks(.end, everConnected: true)))
    }

    func testAnswersAndCandidatesNeverAsk() {
        for connected in [false, true] {
            XCTAssertFalse(CallSignalingWake.sessionAsks(.answer, everConnected: connected))
            XCTAssertFalse(CallSignalingWake.sessionAsks(.candidate, everConnected: connected))
        }
    }

    /// The bytes bound the session's ask: a frame that is not an offer or an
    /// end carries no wake whatever the session says.
    func testTheBytesBoundTheSessionsAsk() throws {
        XCTAssertNil(CallSignalingWake.wakeKind(
            for: try composeCallSignaling(body: .answer(callId: callId, sdp: "v=0")),
            sessionAsks: true))
        XCTAssertNil(CallSignalingWake.wakeKind(
            for: Data(#"{"v":1,"text":"hi"}"#.utf8), sessionAsks: true))
    }

    /// Through a real session: a call cancelled before it connected, and a
    /// ring declined, each send an end that asks for a wake.
    @MainActor
    func testARealSessionsCancelAndDeclineAskForAWake() async throws {
        var sent: [(FfiCallSignalingBody, Bool)] = []

        let caller = CallSession(callId: callId) { bytes, wake in
            if let body = try? parseCallSignaling(plaintext: bytes) { sent.append((body, wake)) }
        }
        caller.setCallKey(seed: Data(repeating: 0x07, count: 32), localLeaf: 0)
        caller.dial(audio: true, video: false)
        caller.hangUp()
        try await Task.sleep(nanoseconds: 300_000_000)
        XCTAssertEqual(sent.count, 1)
        guard case .end? = sent.first?.0 else { return XCTFail("the cancel sends the End") }
        XCTAssertEqual(sent.first?.1, true, "a cancelled call stops the other phone ringing")

        sent.removeAll()
        let callee = CallSession(callId: callId) { bytes, wake in
            if let body = try? parseCallSignaling(plaintext: bytes) { sent.append((body, wake)) }
        }
        callee.applyIncomingSignaling(.offer(
            callId: callId, sdp: "v=0\r\n", createdAtMs: 1,
            media: FfiCallMediaFlags(audio: true, video: false)))
        callee.declineCall()
        XCTAssertEqual(sent.count, 1)
        guard case .end? = sent.first?.0 else { return XCTFail("the decline sends the End") }
        XCTAssertEqual(sent.first?.1, true, "a declined ring stops the caller's other devices")
    }

    // MARK: - Receive: a wake for a call already on this device

    func testAWakeForTheConversationOfACallOnThisDeviceIsSuppressed() {
        let rule = CallWakeSuppression()
        for state: CallSession.WebCallState in [.ringing, .outgoing, .connecting, .connected, .ending] {
            XCTAssertTrue(
                rule.suppresses(groupIdHex: "AA11", activeGroupIdHex: "aa11", activeState: state, nowMs: 0),
                "state \(state)")
            XCTAssertFalse(
                rule.suppresses(groupIdHex: "bb22", activeGroupIdHex: "aa11", activeState: state, nowMs: 0),
                "another conversation still rings, state \(state)")
        }
        // A pre-join check has sent nothing; a new call in it still rings.
        XCTAssertFalse(rule.suppresses(
            groupIdHex: "aa11", activeGroupIdHex: "aa11", activeState: .idle, nowMs: 0))
        XCTAssertFalse(rule.suppresses(
            groupIdHex: "aa11", activeGroupIdHex: nil, activeState: nil, nowMs: 0))
    }

    func testAWakeShortlyAfterACallEndedIsSuppressedThenRingsAgain() {
        var rule = CallWakeSuppression()
        let window = CallWakeSuppression.recentEndWindowMs
        rule.noteEnded(groupIdHex: "AA11", atMs: 1_000)
        func suppressed(_ group: String, at now: Int64) -> Bool {
            rule.suppresses(groupIdHex: group, activeGroupIdHex: nil, activeState: nil, nowMs: now)
        }
        XCTAssertTrue(suppressed("aa11", at: 1_000))
        XCTAssertTrue(suppressed("aa11", at: 1_000 + window - 1))
        XCTAssertFalse(suppressed("aa11", at: 1_000 + window))
        XCTAssertFalse(suppressed("bb22", at: 1_000))

        rule.reset()
        XCTAssertFalse(suppressed("aa11", at: 1_000))
    }

    /// The wire value the DS matches on in `x-lakko-wake`. A malformed header
    /// is refused with a 400, so a drifted spelling here would fail every
    /// call offer's publish, not only its wake.
    func testTheCallWakeKindSerialisesAsTheServerSpellsIt() {
        XCTAssertEqual(DsWake.Kind.call.rawValue, "call")
    }
}
