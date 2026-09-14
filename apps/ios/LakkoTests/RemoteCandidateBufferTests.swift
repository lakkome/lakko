// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

final class RemoteCandidateBufferTests: XCTestCase {
    private func candidate(_ n: Int) -> RemoteIceCandidate {
        RemoteIceCandidate(sdpMid: "0", sdpMLineIndex: 0, candidate: "candidate:\(n)")
    }

    func testKeepsCandidatesUntilTheRemoteDescriptionAndReleasesThemInOrder() {
        var buffer = RemoteCandidateBuffer()
        XCTAssertTrue(buffer.offer(candidate(1)))
        XCTAssertTrue(buffer.offer(candidate(2)))
        XCTAssertEqual(buffer.markRemoteDescriptionApplied(), [candidate(1), candidate(2)])
    }

    func testAfterTheRemoteDescriptionCandidatesAreAppliedDirectly() {
        var buffer = RemoteCandidateBuffer()
        _ = buffer.markRemoteDescriptionApplied()
        XCTAssertFalse(buffer.offer(candidate(3)))
        XCTAssertEqual(buffer.markRemoteDescriptionApplied(), [])
    }

    func testDrainsExactlyOnce() {
        var buffer = RemoteCandidateBuffer()
        _ = buffer.offer(candidate(1))
        XCTAssertEqual(buffer.markRemoteDescriptionApplied().count, 1)
        XCTAssertEqual(buffer.markRemoteDescriptionApplied(), [])
    }

    func testCapacityBoundsWhatARingingCallCanAccumulate() {
        var buffer = RemoteCandidateBuffer(capacity: 2)
        for n in 1...5 { XCTAssertTrue(buffer.offer(candidate(n))) }
        XCTAssertEqual(buffer.dropped, 3)
        XCTAssertEqual(buffer.markRemoteDescriptionApplied(), [candidate(1), candidate(2)])
    }

    func testResetBuffersAgain() {
        var buffer = RemoteCandidateBuffer()
        _ = buffer.markRemoteDescriptionApplied()
        buffer.reset()
        XCTAssertTrue(buffer.offer(candidate(9)))
        XCTAssertEqual(buffer.dropped, 0)
        XCTAssertEqual(buffer.markRemoteDescriptionApplied(), [candidate(9)])
    }
}
