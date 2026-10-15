// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Security
import XCTest
@testable import Lakko

// MARK: - ADR-0077 decision 3: the new phrase is kept before the server step

final class FreshPhraseKeepTests: XCTestCase {

    private final class Row {
        var value: Data?
        var failReads = false
        var failWrites = false
        var writes = 0
        var ports: FreshPhraseKeep.Ports {
            FreshPhraseKeep.Ports(
                read: {
                    if self.failReads { throw AuthClientError.argument("keychain read") }
                    return self.value
                },
                write: {
                    if self.failWrites { throw AuthClientError.argument("keychain write") }
                    self.writes += 1
                    self.value = $0
                },
                delete: { self.value = nil })
        }
    }

    func testThePhraseIsInTheRowOnceKept() throws {
        let row = Row()
        let kept = try FreshPhraseKeep.keep(phrase: "fresh words", ports: row.ports)
        XCTAssertEqual(row.value, Data("fresh words".utf8))
        XCTAssertTrue(kept.isHeld)
    }

    /// Nothing is sent for a phrase that could not be kept, and the row is as
    /// it was.
    func testAPhraseThatCannotBeWrittenIsRefusedAndTheRowIsUntouched() {
        let row = Row()
        row.value = Data("older words".utf8)
        row.failWrites = true
        XCTAssertThrowsError(try FreshPhraseKeep.keep(phrase: "fresh words", ports: row.ports)) {
            XCTAssertEqual($0 as? FreshPhraseKeep.NotStored, FreshPhraseKeep.NotStored())
        }
        XCTAssertEqual(row.value, Data("older words".utf8))
    }

    /// A row that cannot be read could not be put back either: refused
    /// before anything is written over it.
    func testARowThatCannotBeReadIsRefusedBeforeAnyWrite() {
        let row = Row()
        row.value = Data("older words".utf8)
        row.failReads = true
        XCTAssertThrowsError(try FreshPhraseKeep.keep(phrase: "fresh words", ports: row.ports))
        XCTAssertEqual(row.writes, 0)
        XCTAssertEqual(row.value, Data("older words".utf8))
    }

    func testAStepThatDidNotHappenPutsThePreviousRowBack() throws {
        let row = Row()
        row.value = Data("older words".utf8)
        let kept = try FreshPhraseKeep.keep(phrase: "fresh words", ports: row.ports)
        XCTAssertTrue(kept.undo())
        XCTAssertEqual(row.value, Data("older words".utf8))
    }

    func testAStepThatDidNotHappenLeavesNoRowWhereThereWasNone() throws {
        let row = Row()
        let kept = try FreshPhraseKeep.keep(phrase: "fresh words", ports: row.ports)
        XCTAssertTrue(kept.undo())
        XCTAssertNil(row.value)
    }

    /// A wipe in between removed the row: the undo writes nothing back.
    func testAnUndoAfterTheRowWasWipedWritesNothing() throws {
        let row = Row()
        row.value = Data("older words".utf8)
        let kept = try FreshPhraseKeep.keep(phrase: "fresh words", ports: row.ports)
        row.value = nil
        XCTAssertTrue(kept.undo())
        XCTAssertNil(row.value, "a wiped row is not resurrected")
    }

    /// Another write replaced the row: the undo leaves it to that write.
    func testAnUndoAfterTheRowWasRewrittenLeavesItAlone() throws {
        let row = Row()
        let kept = try FreshPhraseKeep.keep(phrase: "fresh words", ports: row.ports)
        row.value = Data("someone else's words".utf8)
        XCTAssertFalse(kept.isHeld)
        XCTAssertTrue(kept.undo())
        XCTAssertEqual(row.value, Data("someone else's words".utf8))
    }
}

// MARK: - The keys of a co-publisher request

final class CoPublishRequestKeysTests: XCTestCase {

    private let groupId = Data(repeating: 0xA1, count: 32)

    func testAFramedRowRestoresOnlyForItsOwnChannel() {
        let snapshot = Data([1, 2, 3, 4])
        let row = CoPublishRequestKeys.frame(groupId: groupId, snapshot: snapshot)
        XCTAssertEqual(CoPublishRequestKeys.snapshot(in: row, groupId: groupId), snapshot)
        XCTAssertNil(
            CoPublishRequestKeys.snapshot(in: row, groupId: Data(repeating: 0xB2, count: 32)),
            "a request for another channel starts from nothing")
    }

    func testNoRowOrAMalformedRowRestoresNothing() {
        XCTAssertNil(CoPublishRequestKeys.snapshot(in: nil, groupId: groupId))
        XCTAssertNil(CoPublishRequestKeys.snapshot(in: Data(), groupId: groupId))
        XCTAssertNil(CoPublishRequestKeys.snapshot(in: Data([0x00]), groupId: groupId))
        XCTAssertNil(CoPublishRequestKeys.snapshot(in: Data([0x00, 0x00, 0x01]), groupId: groupId))
        // A length running past the end.
        XCTAssertNil(CoPublishRequestKeys.snapshot(in: Data([0x00, 0x40]) + groupId, groupId: groupId))
        // A row that names the group but carries no snapshot.
        XCTAssertNil(CoPublishRequestKeys.snapshot(
            in: CoPublishRequestKeys.frame(groupId: groupId, snapshot: Data()), groupId: groupId))
    }
}

// MARK: - G-51: a publish counter that cannot be read is not a counter of 0

final class PublishCounterFloorTests: XCTestCase {

    func testAKeychainReadIsToldApartFromAnAbsentItem() {
        XCTAssertEqual(ChannelPublishCounterStore.classify(status: errSecItemNotFound, data: nil), .absent)
        XCTAssertEqual(
            ChannelPublishCounterStore.classify(status: errSecSuccess, data: Data([1])), .present(Data([1])))
        XCTAssertEqual(
            ChannelPublishCounterStore.classify(status: errSecInteractionNotAllowed, data: nil),
            .unreadable(status: errSecInteractionNotAllowed))
        XCTAssertEqual(
            ChannelPublishCounterStore.classify(status: errSecSuccess, data: nil),
            .unreadable(status: errSecSuccess))
    }

    func testOnlyAnAbsentItemReadsAsNoFloor() throws {
        XCTAssertEqual(ChannelsUiState.publishCounters(from: .absent), [:])
        XCTAssertNil(ChannelsUiState.publishCounters(from: .unreadable(status: errSecInteractionNotAllowed)))
        XCTAssertNil(ChannelsUiState.publishCounters(from: .present(Data([1]))), "undecodable")
        let stored = try JSONSerialization.data(withJSONObject: [["channelId": "c1", "counter": 7]])
        XCTAssertEqual(ChannelsUiState.publishCounters(from: .present(stored)), ["c1": 7])
    }

    /// A counter handed out from memory before the stored ones could be read
    /// again never pulls a stored floor down.
    func testTheHigherFloorWins() {
        XCTAssertEqual(
            ChannelsUiState.higherFloors(["c1": 1, "c2": 9], ["c1": 7, "c3": 2]),
            ["c1": 7, "c2": 9, "c3": 2])
    }
}
