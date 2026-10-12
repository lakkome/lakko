// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0065 R.7 case 1 - the re-send marker. The cases are those of
/// `apps/web/src/chat/resend-marker.test.ts` and of `heldResendPredicate` in
/// `resend-receive.test.ts` (that file's two envelope cases test the wire
/// codec, which is not this module's).
final class ResendMarkerTests: XCTestCase {

    private typealias Row = ResendMarker.HeldRow

    // MARK: - resendVerdict

    private let marker = ResendMarker.Origin(groupIdB64: "old-group", messageId: 7)

    private func verdict(
        marker: ResendMarker.Origin?? = .none,
        originConversation: String? = "conv",
        originIsOlderSegment: Bool = true,
        heldAuthor: String? = "founder",
        senderAuthor: String? = "founder"
    ) -> ResendMarker.Verdict {
        let used: ResendMarker.Origin?
        switch marker {
        case .none: used = self.marker
        case .some(let given): used = given
        }
        return ResendMarker.resendVerdict(
            marker: used, receivedConversation: "conv", originConversation: originConversation,
            originIsOlderSegment: originIsOlderSegment, heldAuthor: heldAuthor, senderAuthor: senderAuthor)
    }

    func testMatchTheOriginalIsHeldInAnOlderSegmentByTheSameAuthor() {
        XCTAssertEqual(verdict(), .skip)
    }

    func testNoMatchTheNamedRowIsNotHeld() {
        XCTAssertEqual(verdict(heldAuthor: nil), .draw)
    }

    func testOtherAuthorAMarkerNamingSomebodyElsesRowHidesNothing() {
        XCTAssertEqual(verdict(senderAuthor: "someone-else"), .draw)
        XCTAssertEqual(verdict(senderAuthor: nil), .draw)
    }

    func testOtherConversationHidesNothing() {
        XCTAssertEqual(verdict(originConversation: "another"), .draw)
        XCTAssertEqual(verdict(originConversation: nil), .draw)
    }

    func testAMarkerNamingTheReceivingGroupItselfOrALaterOneHidesNothing() {
        XCTAssertEqual(verdict(originIsOlderSegment: false), .draw)
    }

    func testNoMarkerIsAnOrdinaryMessage() {
        XCTAssertEqual(verdict(marker: .some(nil)), .draw)
    }

    // MARK: - heldRowAuthor

    private let same: (String) -> String = { $0 }

    func testARowThisDeviceSentIsByItsOwnAccount() {
        XCTAssertEqual(
            ResendMarker.heldRowAuthor(Row(messageId: 1, direction: .outgoing), ownPersonKeyB64: "own", canonical: same),
            "own")
        XCTAssertNil(
            ResendMarker.heldRowAuthor(Row(messageId: 1, direction: .outgoing), ownPersonKeyB64: nil, canonical: same))
    }

    func testAReceivedRowIsByItsStoredAuthorThroughTheAliases() {
        XCTAssertEqual(
            ResendMarker.heldRowAuthor(
                Row(messageId: 1, direction: .incoming, authorKeyB64: "old"),
                ownPersonKeyB64: "own", canonical: { _ in "row" }),
            "row")
    }

    func testASystemRowAMissingRowAndARowWithNoAuthorHaveNone() {
        XCTAssertNil(
            ResendMarker.heldRowAuthor(Row(messageId: 1, direction: .system), ownPersonKeyB64: "own", canonical: same))
        XCTAssertNil(ResendMarker.heldRowAuthor(nil, ownPersonKeyB64: "own", canonical: same))
        XCTAssertNil(
            ResendMarker.heldRowAuthor(
                Row(messageId: 1, direction: .incoming, authorKeyB64: nil), ownPersonKeyB64: "own", canonical: same))
        XCTAssertNil(
            ResendMarker.heldRowAuthor(
                Row(messageId: 1, direction: .incoming, authorKeyB64: ""), ownPersonKeyB64: "own", canonical: same))
    }

    // MARK: - heldResendPredicate

    private static let old = Data(repeating: 0x01, count: 32)
    private static let new = Data(repeating: 0x02, count: 32)
    private static let other = Data(repeating: 0x03, count: 32)

    private static func hex(_ data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined()
    }

    private let oldHex = ResendMarkerTests.hex(ResendMarkerTests.old)
    private let newHex = ResendMarkerTests.hex(ResendMarkerTests.new)
    private let otherHex = ResendMarkerTests.hex(ResendMarkerTests.other)
    private let oldB64 = ResendMarkerTests.old.base64EncodedString()

    private struct ReadFailed: Error {}

    private final class Counter {
        var loads = 0
    }

    /// OLD -> NEW is one conversation (rooted at OLD); OTHER is its own.
    private func port(
        _ log: ResendMarker.HeldLog?,
        counter: Counter = Counter(),
        loadThrows: Bool = false,
        lineageThrows: Bool = false
    ) -> ResendMarker.ReceivePort {
        let oldHex = self.oldHex
        let newHex = self.newHex
        return ResendMarker.ReceivePort(
            receivedGroupIdHex: newHex,
            conversationOf: { hex in
                if lineageThrows { throw ReadFailed() }
                return hex == newHex ? oldHex : hex
            },
            segmentIndexOf: { hex in
                if lineageThrows { throw ReadFailed() }
                return hex == newHex ? 1 : 0
            },
            ownPersonKeyB64: "own",
            canonical: { $0 },
            loadLog: {
                counter.loads += 1
                if loadThrows { throw ReadFailed() }
                return log
            })
    }

    private func log(_ groupHex: String, _ rows: [Row]) -> ResendMarker.HeldLog {
        ResendMarker.HeldLog(rowsByGroupHex: [groupHex: rows])
    }

    private func received(_ senderKey: String?, _ marker: ResendMarker.Origin? = nil) -> ResendMarker.Received {
        ResendMarker.Received(resendOrigin: marker, senderKeyB64: senderKey)
    }

    private var origin: ResendMarker.Origin { ResendMarker.Origin(groupIdB64: oldB64, messageId: 41) }

    func testTheLineageFixtureReadsOldAndNewAsOneConversation() throws {
        let p = port(nil)
        XCTAssertEqual(try p.conversationOf(newHex), try p.conversationOf(oldHex))
        XCTAssertLessThan(try p.segmentIndexOf(oldHex), try p.segmentIndexOf(newHex))
        XCTAssertNotEqual(try p.conversationOf(otherHex), try p.conversationOf(newHex))
    }

    func testTheMarkersGroupIdIsReadAsLowercaseHex() {
        XCTAssertEqual(ResendMarker.groupIdHex(fromB64: oldB64), oldHex)
        XCTAssertEqual(ResendMarker.groupIdHex(fromB64: Data([0xAB, 0xCD]).base64EncodedString()), "abcd")
        XCTAssertNil(ResendMarker.groupIdHex(fromB64: "%%%"))
        XCTAssertNil(ResendMarker.groupIdHex(fromB64: ""))
    }

    func testMatchAnotherDeviceOfThisAccountHoldsItsOwnOriginal() {
        let e = received("own", origin)
        let held = ResendMarker.heldResendPredicate(
            batch: [e], port: port(log(oldHex, [Row(messageId: 41, direction: .outgoing)])))
        XCTAssertTrue(held(e))
    }

    func testMatchThroughAReceivedRowTheStoredAuthorIsTheSender() {
        let e = received("peer", origin)
        let held = ResendMarker.heldResendPredicate(
            batch: [e], port: port(log(oldHex, [Row(messageId: 41, direction: .incoming, authorKeyB64: "peer")])))
        XCTAssertTrue(held(e))
    }

    func testNoMatchTheNamedRowIsNotHeldSoTheMessageIsDrawn() {
        let e = received("own", origin)
        let held = ResendMarker.heldResendPredicate(
            batch: [e], port: port(log(oldHex, [Row(messageId: 40, direction: .outgoing)])))
        XCTAssertFalse(held(e))
    }

    func testOtherAuthorThroughThePredicateHidesNothing() {
        let e = received("attacker", origin)
        let held = ResendMarker.heldResendPredicate(
            batch: [e], port: port(log(oldHex, [Row(messageId: 41, direction: .incoming, authorKeyB64: "victim")])))
        XCTAssertFalse(held(e))
    }

    func testOtherConversationThroughThePredicateHidesNothing() {
        let e = received(
            "own", ResendMarker.Origin(groupIdB64: Self.other.base64EncodedString(), messageId: 41))
        let held = ResendMarker.heldResendPredicate(
            batch: [e], port: port(log(otherHex, [Row(messageId: 41, direction: .outgoing)])))
        XCTAssertFalse(held(e))
    }

    func testAMarkerNamingTheReceivingGroupItselfHidesNothing() {
        let e = received(
            "own", ResendMarker.Origin(groupIdB64: Self.new.base64EncodedString(), messageId: 41))
        let held = ResendMarker.heldResendPredicate(
            batch: [e], port: port(log(newHex, [Row(messageId: 41, direction: .outgoing)])))
        XCTAssertFalse(held(e))
    }

    func testAGroupWithNoStoredSlotHidesNothing() {
        // The lineage knows OLD, the log holds no slot for it.
        let e = received("own", origin)
        let held = ResendMarker.heldResendPredicate(
            batch: [e], port: port(log(otherHex, [Row(messageId: 41, direction: .outgoing)])))
        XCTAssertFalse(held(e))
    }

    func testAnUnattributableSenderAndAJunkGroupIdHideNothing() {
        let a = received(nil, origin)
        let b = received("own", ResendMarker.Origin(groupIdB64: "%%%", messageId: 41))
        let held = ResendMarker.heldResendPredicate(
            batch: [a, b], port: port(log(oldHex, [Row(messageId: 41, direction: .outgoing)])))
        XCTAssertFalse(held(a))
        XCTAssertFalse(held(b))
    }

    func testABatchWithNoMarkerNeverReadsTheLog() {
        let counter = Counter()
        let e = received("own")
        let held = ResendMarker.heldResendPredicate(
            batch: [e], port: port(log(oldHex, [Row(messageId: 41, direction: .outgoing)]), counter: counter))
        XCTAssertFalse(held(e))
        XCTAssertEqual(counter.loads, 0)
    }

    func testTheLogIsReadOncePerBatch() {
        let counter = Counter()
        let a = received("own", origin)
        let b = received("own", ResendMarker.Origin(groupIdB64: oldB64, messageId: 40))
        let held = ResendMarker.heldResendPredicate(
            batch: [a, b], port: port(log(oldHex, [Row(messageId: 41, direction: .outgoing)]), counter: counter))
        XCTAssertTrue(held(a))
        XCTAssertFalse(held(b))
        XCTAssertFalse(held(received("own")))
        XCTAssertEqual(counter.loads, 1)
    }

    func testALogThatCannotBeReadDropsNothing() {
        let e = received("own", origin)
        let rows = log(oldHex, [Row(messageId: 41, direction: .outgoing)])
        XCTAssertFalse(ResendMarker.heldResendPredicate(batch: [e], port: port(rows, loadThrows: true))(e))
        XCTAssertFalse(ResendMarker.heldResendPredicate(batch: [e], port: port(nil))(e))
        XCTAssertFalse(ResendMarker.heldResendPredicate(batch: [e], port: port(rows, lineageThrows: true))(e))
    }
}
