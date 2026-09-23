// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// The cross-device sync state has to survive a relaunch.
///
/// Drafts, pin / mute / archive, read cursors and the chat order were
/// `@Published` properties and nothing more, so a relaunch started them
/// empty - and the journal cannot refill them: the poll runs from cursor 0,
/// but the MLS snapshot is restored first, so every row it re-fetches is one
/// this device's state has already consumed.
final class DeviceGroupSyncStateCodecTests: XCTestCase {

    private var full: DeviceGroupSyncStateCodec.Snapshot {
        DeviceGroupSyncStateCodec.Snapshot(
            drafts: [
                "aa": DraftState(
                    text: "half a sentence",
                    replyToId: Data([1, 2, 3]),
                    updatedAtHlc: 7),
                "bb": DraftState(text: "", replyToId: Data(), updatedAtHlc: 1),
            ],
            flags: [
                "aa": FlagsState(
                    pinned: true,
                    muted: false,
                    archived: false,
                    unreadOverride: true,
                    updatedAtHlc: 9)
            ],
            readCursors: [
                "aa": ReadCursorState(
                    upToMessageId: Data([9, 9]),
                    readAtMs: 1_700_000_000_000,
                    updatedAtHlc: 11)
            ],
            order: OrderState(groupIds: [Data([1]), Data([2])], updatedAtHlc: 13))
    }

    func testRoundTripsEverythingARelaunchUsedToLose() throws {
        let blob = try XCTUnwrap(DeviceGroupSyncStateCodec.encode(full))
        XCTAssertEqual(DeviceGroupSyncStateCodec.decode(blob), full)
    }

    func testKeepsTheHlcExactlyBecauseItIsWhatDecidesWhoWins() throws {
        // The restored snapshot is folded against the journal by HLC. A stamp
        // that came back rounded - which is what a JSON number does to a
        // UInt64 near its top - would let a stale sibling write overwrite a
        // newer local one.
        let big = UInt64.max - 1
        let snapshot = DeviceGroupSyncStateCodec.Snapshot(
            flags: [
                "aa": FlagsState(
                    pinned: true,
                    muted: false,
                    archived: false,
                    unreadOverride: false,
                    updatedAtHlc: big)
            ])
        let blob = try XCTUnwrap(DeviceGroupSyncStateCodec.encode(snapshot))
        XCTAssertEqual(DeviceGroupSyncStateCodec.decode(blob)?.flags["aa"]?.updatedAtHlc, big)
    }

    func testRoundTripsAnEmptySnapshot() throws {
        let empty = DeviceGroupSyncStateCodec.Snapshot()
        let blob = try XCTUnwrap(DeviceGroupSyncStateCodec.encode(empty))
        XCTAssertEqual(DeviceGroupSyncStateCodec.decode(blob), empty)
        XCTAssertTrue(empty.isEmpty)
        XCTAssertFalse(full.isEmpty)
    }

    func testRefusesBytesThatAreNotThisSchema() {
        XCTAssertNil(DeviceGroupSyncStateCodec.decode(Data("not json".utf8)))
        XCTAssertNil(DeviceGroupSyncStateCodec.decode(Data()))
        XCTAssertNil(
            DeviceGroupSyncStateCodec.decode(
                Data(#"{"v":99,"d":{},"f":{},"c":{}}"#.utf8)))
    }

    func testAMalformedEntryIsDroppedWithoutTakingTheRestWithIt() throws {
        // Losing one chat's pin is better than losing every chat's, and the
        // poll can refill a single entry from a sibling's next write.
        let blob = try XCTUnwrap(DeviceGroupSyncStateCodec.encode(full))
        var wire = try XCTUnwrap(
            JSONSerialization.jsonObject(with: blob) as? [String: Any])
        var drafts = try XCTUnwrap(wire["d"] as? [String: Any])
        var aa = try XCTUnwrap(drafts["aa"] as? [String: Any])
        aa["h"] = "later"
        drafts["aa"] = aa
        wire["d"] = drafts
        let back = DeviceGroupSyncStateCodec.decode(
            try JSONSerialization.data(withJSONObject: wire))
        XCTAssertNil(back?.drafts["aa"])
        // The survivors, asserted too: a decoder that gave up on the whole
        // snapshot would satisfy the line above on its own.
        XCTAssertEqual(back?.drafts["bb"]?.text, "")
        XCTAssertEqual(back?.flags["aa"]?.pinned, true)
    }

    func testDropsTheWholeOrderWhenOneIdIsUnreadable() throws {
        // The order is one value, not a map: keeping the readable ids would
        // silently reorder the chat list rather than leave it alone.
        let blob = try XCTUnwrap(DeviceGroupSyncStateCodec.encode(full))
        var wire = try XCTUnwrap(
            JSONSerialization.jsonObject(with: blob) as? [String: Any])
        var order = try XCTUnwrap(wire["o"] as? [String: Any])
        order["g"] = ["AQ==", "!!!not base64!!!"]
        wire["o"] = order
        let back = DeviceGroupSyncStateCodec.decode(
            try JSONSerialization.data(withJSONObject: wire))
        XCTAssertNil(back?.order)
        XCTAssertEqual(back?.flags["aa"]?.pinned, true, "the rest still decodes")
    }
}
