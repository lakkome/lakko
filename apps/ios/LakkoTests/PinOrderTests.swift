// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// `docs/ux/navigation.md` section 2.2.1 — the order of pinned conversations,
/// the client half.
///
/// The rule (where a pin is drawn, what each action writes, when Move up /
/// Move down applies) is `crates/lakko-ui-rules` and is tested there. What is
/// tested here is that iOS reaches it through the FFI with the right inputs:
/// the register mapped to row ids and back, the stored list handed over
/// unfiltered, the `.onMove` arguments turned into the index the helper
/// takes, and a folder's pins written as the folder's own references.
final class PinOrderTests: XCTestCase {

    private let a = String(repeating: "aa", count: 32)
    private let b = String(repeating: "bb", count: 32)
    private let c = String(repeating: "cc", count: 32)
    private let d = String(repeating: "dd", count: 32)
    private let channel = String(repeating: "0f", count: 16)
    private let folderId = Data(repeating: 7, count: 16)

    private func row(
        _ id: String, kind: ConversationListKind = .chat, at ms: Int64? = nil,
        pinned: Bool = false, archived: Bool = false
    ) -> ConversationListRow {
        ConversationListRow(
            id: id, kind: kind, title: String(id.prefix(4)), preview: "",
            lastActivityMs: ms, pinned: pinned, archived: archived)
    }

    private func ref(_ hex: String, _ kind: FolderConversationKind = .oneToOne) -> FolderConversationRef {
        FolderConversationRef(kind: kind, id: ChatFolders.bytes(fromHex: hex)!)
    }

    private func folder(pinned: [FolderConversationRef]) -> FolderView {
        FolderView(
            folderId: folderId,
            meta: FolderMetaRegister(
                hlc: 1, folderId: folderId, name: "Work",
                includeKinds: ChatFolders.Include.all, excludeFlags: 0),
            included: [], excluded: [], pinned: pinned)
    }

    // MARK: - Register <-> row ids

    func testStoredIdsAreTheFlagsKeys() {
        let bytes = [ChatFolders.bytes(fromHex: a)!, ChatFolders.bytes(fromHex: channel)!]
        XCTAssertEqual(PinOrder.storedIds(bytes), [a, channel])
        XCTAssertEqual(PinOrder.storedIds(OrderState(groupIds: bytes, updatedAtHlc: 3)), [a, channel])
        XCTAssertEqual(PinOrder.storedIds(nil as OrderState?), [])
        XCTAssertEqual(PinOrder.bytes([a, channel]), bytes)
    }

    func testBytesDropWhatIsNotHex() {
        XCTAssertEqual(PinOrder.bytes(["zz", a]), [ChatFolders.bytes(fromHex: a)!])
    }

    func testStoredIdsMatchRowsWhateverTheCase() {
        let upper = a.uppercased()
        XCTAssertEqual(PinOrder.spelledAsRows([a, b], rows: [row(upper)]), [upper, b])
    }

    // MARK: - The list draws the stored order

    func testPinsFollowTheStoredOrderNotActivity() {
        let rows = [
            row(a, at: 300, pinned: true),
            row(b, at: 100, pinned: true),
            row(c, at: 200, pinned: true),
            row(d, at: 400),
        ]
        XCTAssertEqual(ConversationList.sorted(rows).map(\.id), [a, c, b, d])
        XCTAssertEqual(ConversationList.sorted(rows, pinOrder: [b, a, c]).map(\.id), [b, a, c, d])
    }

    func testAPinTheOrderDoesNotNameSitsAboveTheNamedOnes() {
        let rows = [row(a, at: 100, pinned: true), row(b, at: 50, pinned: true), row(c, at: 900)]
        XCTAssertEqual(ConversationList.sorted(rows, pinOrder: [b]).map(\.id), [a, b, c])
    }

    func testAnUnpinnedRowTheOrderNamesStaysByActivity() {
        let rows = [row(a, at: 100, pinned: true), row(b, at: 50), row(c, at: 900)]
        XCTAssertEqual(ConversationList.sorted(rows, pinOrder: [b, a]).map(\.id), [a, c, b])
    }

    func testFolderRowsTakeAPendingPinOrder() {
        let f = folder(pinned: [ref(a), ref(b)])
        let all = [row(a, at: 1), row(b, at: 2), row(c, at: 3)]
        XCTAssertEqual(ChatFolders.rows(in: f, from: all).map(\.id), [a, b, c])
        XCTAssertEqual(ChatFolders.rows(in: f, from: all, pins: [b, a]).map(\.id), [b, a, c])
    }

    // MARK: - What each action writes

    func testPinGoesOnTopAndKeepsWhatThisDeviceDoesNotShow() {
        // `d` is stored but has no row here: it must survive the write.
        let next = PinOrder.next(.pin, stored: [b, d, c], shown: [b, c], id: a)
        XCTAssertEqual(next.first, a)
        XCTAssertTrue(next.contains(d))
        XCTAssertEqual(Set(next), Set([a, b, c, d]))
    }

    func testUnpinLeavesTheOthersInOrder() {
        let next = PinOrder.next(.unpin, stored: [a, b, c], shown: [a, b, c], id: b)
        XCTAssertEqual(next, [a, c])
    }

    func testMoveWritesTheShownOrder() {
        XCTAssertEqual(PinOrder.next(.move(toIndex: 0), stored: [a, b, c], shown: [a, b, c], id: c), [c, a, b])
        XCTAssertEqual(PinOrder.next(.move(toIndex: 99), stored: [a, b, c], shown: [a, b, c], id: a), [b, c, a])
    }

    func testPinsMadeBeforeTheOrderAreWrittenInByTheFirstMove() {
        // Nothing stored yet: the move writes the whole list as shown.
        let next = PinOrder.next(.move(toIndex: 2), stored: [], shown: [a, b, c], id: a)
        XCTAssertEqual(next, [b, c, a])
    }

    func testStepTargetFollowsTheSharedRule() {
        let shown = [a, b, c]
        XCTAssertNil(PinOrder.stepTarget(shown: shown, id: a, up: true))
        XCTAssertEqual(PinOrder.stepTarget(shown: shown, id: a, up: false), 1)
        XCTAssertEqual(PinOrder.stepTarget(shown: shown, id: b, up: true), 0)
        XCTAssertNil(PinOrder.stepTarget(shown: shown, id: c, up: false))
        XCTAssertNil(PinOrder.stepTarget(shown: [a], id: a, up: false))
        XCTAssertNil(PinOrder.stepTarget(shown: shown, id: d, up: true))
    }

    // MARK: - Drag

    func testDragNeedsTwoPinsNoSearchAndNotTheArchive() {
        XCTAssertTrue(PinOrder.dragEnabled(pinCount: 2, searching: false, inArchive: false))
        XCTAssertFalse(PinOrder.dragEnabled(pinCount: 1, searching: false, inArchive: false))
        XCTAssertFalse(PinOrder.dragEnabled(pinCount: 3, searching: true, inArchive: false))
        XCTAssertFalse(PinOrder.dragEnabled(pinCount: 3, searching: false, inArchive: true))
    }

    func testDropTargetFollowsOnMoveSemantics() {
        let pins = [a, b, c]
        // Down: `destination` counts positions before the move.
        XCTAssertEqual(PinOrder.dropTarget(pins: pins, source: [0], destination: 3)?.toIndex, 2)
        XCTAssertEqual(PinOrder.dropTarget(pins: pins, source: [0], destination: 2)?.toIndex, 1)
        // Up.
        let up = PinOrder.dropTarget(pins: pins, source: [2], destination: 0)
        XCTAssertEqual(up?.id, c)
        XCTAssertEqual(up?.toIndex, 0)
        // Dropped where it was.
        XCTAssertNil(PinOrder.dropTarget(pins: pins, source: [1], destination: 1))
        XCTAssertNil(PinOrder.dropTarget(pins: pins, source: [1], destination: 2))
        // Out of range never moves anything below the pins.
        XCTAssertEqual(PinOrder.dropTarget(pins: pins, source: [0], destination: 40)?.toIndex, 2)
        XCTAssertNil(PinOrder.dropTarget(pins: pins, source: [5], destination: 0))
    }

    func testDropThenMoveMatchesWhatTheListShowedOnLift() {
        // The index a drop yields, written through the helper, is the order
        // SwiftUI drew at the moment of the drop.
        let pins = [a, b, c, d]
        for from in 0..<pins.count {
            for destination in 0...pins.count {
                var drawn = pins
                drawn.move(fromOffsets: [from], toOffset: destination)
                guard let drop = PinOrder.dropTarget(pins: pins, source: [from], destination: destination)
                else {
                    XCTAssertEqual(drawn, pins)
                    continue
                }
                let written = PinOrder.next(
                    .move(toIndex: drop.toIndex), stored: pins, shown: pins, id: drop.id)
                XCTAssertEqual(written, drawn, "from \(from) to \(destination)")
            }
        }
    }

    func testPinnedRunIsTheLeadingPins() {
        let rows = [row(a, pinned: true), row(b, pinned: true, archived: true), row(c), row(d, pinned: true)]
        let run = PinnedRun.split(rows, isPinned: { $0.pinned })
        XCTAssertEqual(run.pinned.map(\.id), [a, b])
        XCTAssertEqual(run.rest.map(\.id), [c, d])
    }

    // MARK: - Hold and lift on a draggable row

    @MainActor
    func testOnlyAHoldWithoutMovingCounts() {
        let start = CGPoint(x: 200, y: 130)
        XCTAssertTrue(HoldReleaseObserver.isHoldWithoutMove(
            start: start, end: CGPoint(x: 205, y: 136), held: 0.8))
        // Moved: a drag the system did not cancel opens nothing.
        XCTAssertFalse(HoldReleaseObserver.isHoldWithoutMove(
            start: start, end: CGPoint(x: 200, y: 150), held: 0.8))
        // A tap is not a hold.
        XCTAssertFalse(HoldReleaseObserver.isHoldWithoutMove(start: start, end: start, held: 0.2))
    }

    @MainActor
    func testTheHoldOpensTheRowUnderTheFinger() {
        let a = UUID(), b = UUID()
        let frames = [
            (a, CGRect(x: 0, y: 100, width: 390, height: 64)),
            (b, CGRect(x: 0, y: 164, width: 390, height: 64)),
        ]
        XCTAssertEqual(HoldReleaseObserver.target(at: CGPoint(x: 10, y: 120), frames: frames), a)
        XCTAssertEqual(HoldReleaseObserver.target(at: CGPoint(x: 10, y: 200), frames: frames), b)
        XCTAssertNil(HoldReleaseObserver.target(at: CGPoint(x: 10, y: 40), frames: frames))
    }

    // MARK: - A folder's own pins

    func testFolderPinGoesOnTopAndKeepsAnUnheldPin() {
        // `d` is pinned in the folder but this device has no row for it.
        let f = folder(pinned: [ref(b), ref(d)])
        let change = ChatFolders.togglePin(folder: f, row: row(a), shown: [b], hlc: 9)
        let written = change?.pins.first?.pinned.map(ChatFolders.rowId)
        XCTAssertEqual(written?.first, a)
        XCTAssertEqual(Set(written ?? []), Set([a, b, d]))
    }

    func testFolderUnpinKeepsTheOthersAndTheirKinds() {
        let f = folder(pinned: [ref(a), ref(channel, .channel), ref(b)])
        let change = ChatFolders.togglePin(
            folder: f, row: row(a), shown: [a, channel, b], hlc: 9)
        XCTAssertEqual(change?.pins.first?.pinned, [ref(channel, .channel), ref(b)])
    }

    func testFolderMoveWritesTheFolderPinsOnly() {
        let f = folder(pinned: [ref(a), ref(b), ref(c)])
        let change = ChatFolders.movePin(folder: f, shown: [a, b, c], id: c, toIndex: 0, hlc: 9)
        XCTAssertEqual(change?.pins.first?.pinned.map(ChatFolders.rowId), [c, a, b])
        XCTAssertEqual(change?.pins.first?.folderId, folderId)
        XCTAssertTrue(change?.metas.isEmpty ?? false)
        XCTAssertTrue(change?.members.isEmpty ?? false)
        XCTAssertNil(change?.order)
    }

    func testFolderMoveOfANonPinOrAStayWritesNothing() {
        let f = folder(pinned: [ref(a), ref(b)])
        XCTAssertNil(ChatFolders.movePin(folder: f, shown: [a, b], id: c, toIndex: 0, hlc: 9))
        XCTAssertNil(ChatFolders.movePin(folder: f, shown: [a, b], id: a, toIndex: 0, hlc: 9))
    }

    func testFolderPinLimitStays() {
        let full = folder(pinned: (0..<10).map { ref(String(format: "%064x", $0)) })
        XCTAssertNil(ChatFolders.togglePin(folder: full, row: row(a), hlc: 9))
    }
}
