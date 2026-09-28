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

    /// The port in `CallHost.live` asks for `.call` exactly when the decision
    /// says wake, and for nothing otherwise. Restated here against the two
    /// bodies that matter so a change to either side has to change this test.
    func testThePublisherAsksForACallWakeOnlyForOfferAndEnd() throws {
        func requested(_ bytes: Data) -> DsWake.Kind? {
            CallSignalingWake.shouldWake(bytes) ? .call : nil
        }
        XCTAssertEqual(requested(try offer()), .call)
        XCTAssertEqual(requested(try end()), .call)
        XCTAssertNil(requested(try composeCallSignaling(
            body: .answer(callId: callId, sdp: "v=0"))))
        XCTAssertNil(requested(try composeCallSignaling(body: .groupJoin(
            callId: callId, leafIndex: 2, joinedAtMs: 1))))
    }

    /// The wire value the DS matches on in `x-lakko-wake`. A malformed header
    /// is refused with a 400, so a drifted spelling here would fail every
    /// call offer's publish, not only its wake.
    func testTheCallWakeKindSerialisesAsTheServerSpellsIt() {
        XCTAssertEqual(DsWake.Kind.call.rawValue, "call")
    }
}
