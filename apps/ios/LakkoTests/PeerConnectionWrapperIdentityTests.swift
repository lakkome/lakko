// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import WebRTC
import XCTest

/// The premise of ``WebRTCPeerConnection``'s `sframeSenders` /
/// `sframeReceivers`: `RTCPeerConnection.senders` and `.receivers` build new
/// wrapper objects on every read, so an object association made on one read
/// is not on the next, and a rekey over them reached no transform.
final class PeerConnectionWrapperIdentityTests: XCTestCase {

    func testSendersAndReceiversAreNewWrappersOnEveryRead() throws {
        let factory = RTCPeerConnectionFactory()
        let config = RTCConfiguration()
        config.sdpSemantics = .unifiedPlan
        let constraints = RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)
        let peer = try XCTUnwrap(
            factory.peerConnection(with: config, constraints: constraints, delegate: nil))
        defer { peer.close() }
        let track = factory.audioTrack(withTrackId: "a0")
        let added = try XCTUnwrap(peer.add(track, streamIds: ["s"]))
        let first = try XCTUnwrap(peer.senders.first)
        let second = try XCTUnwrap(peer.senders.first)
        XCTAssertEqual(first.senderId, added.senderId)
        XCTAssertFalse(first === second, "senders returned the same wrapper twice")
        XCTAssertFalse(first === added, "senders returned the wrapper add() gave")
        let r1 = try XCTUnwrap(peer.receivers.first)
        let r2 = try XCTUnwrap(peer.receivers.first)
        XCTAssertFalse(r1 === r2, "receivers returned the same wrapper twice")
    }
}
