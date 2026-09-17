// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// Unit tests for the persisted channel log: the pure `Codable` payload
/// (``ChannelLogPayload``) and the pure publisher re-open decision
/// (``ChannelPublisherReopen``). Neither touches the Keychain or the file
/// system.
final class ChannelLogStoreTests: XCTestCase {

    private func handle(_ id: String, name: String = "n", handle: String = "") -> ChannelHandle {
        ChannelHandle(
            id: id,
            localName: name,
            localDescription: "d-\(id)",
            addedAtMs: 1_700_000_000_000,
            handle: handle
        )
    }

    private func seed(_ byte: UInt8) -> Data {
        Data(repeating: byte, count: 32)
    }

    // MARK: - Payload codec

    func testPayloadRoundTripsChannelsInOrderAndSeedsByEpoch() throws {
        let channels = [handle("b", name: "Second", handle: "second"), handle("a", name: "First")]
        let seeds: [String: [UInt32: Data]] = [
            "a": [3: seed(0x03), 1: seed(0x01)],
            "b": [7: seed(0x07)],
        ]
        let payload = ChannelLogPayload(channels: channels, subscribedSeedsByChannel: seeds)

        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let data = try encoder.encode(payload)
        let decoded = try JSONDecoder().decode(ChannelLogPayload.self, from: data)

        XCTAssertEqual(decoded, payload)
        XCTAssertEqual(decoded.version, ChannelLogPayload.currentVersion)
        // Order of the list is the display order and must come back as stored.
        XCTAssertEqual(decoded.channelHandles, channels)
        XCTAssertEqual(decoded.subscribedSeedsByChannel, seeds)
        // The handle survives: it is the only way back to a channel id.
        XCTAssertEqual(decoded.channelHandles.first?.handle, "second")
    }

    func testPayloadEncodingIsStableForTheSameState() throws {
        // Two payloads built from the same map in any iteration order encode
        // to identical bytes (rows are sorted on init, keys on encode).
        let seeds: [String: [UInt32: Data]] = [
            "a": [2: seed(0x02), 1: seed(0x01)],
            "c": [1: seed(0x0C)],
        ]
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let fromMap = try encoder.encode(
            ChannelLogPayload(channels: [handle("a")], subscribedSeedsByChannel: seeds))
        let fromRowsInAnotherOrder = try encoder.encode(
            ChannelLogPayload(
                channels: [PersistedChannel(handle: handle("a"))],
                subscribedSeeds: [
                    PersistedSubscribedSeed(channelId: "c", epoch: 1, seedB64: seed(0x0C).base64EncodedString()),
                    PersistedSubscribedSeed(channelId: "a", epoch: 2, seedB64: seed(0x02).base64EncodedString()),
                    PersistedSubscribedSeed(channelId: "a", epoch: 1, seedB64: seed(0x01).base64EncodedString()),
                ]
            ))
        XCTAssertEqual(fromMap, fromRowsInAnotherOrder)
    }

    func testEmptyPayloadRoundTrips() throws {
        let data = try JSONEncoder().encode(ChannelLogPayload.empty)
        let decoded = try JSONDecoder().decode(ChannelLogPayload.self, from: data)
        XCTAssertEqual(decoded, .empty)
        XCTAssertTrue(decoded.channelHandles.isEmpty)
        XCTAssertTrue(decoded.subscribedSeedsByChannel.isEmpty)
    }

    func testUnsupportedVersionDoesNotDecode() {
        let json = #"{"version":2,"channels":[],"subscribedSeeds":[]}"#
        XCTAssertThrowsError(
            try JSONDecoder().decode(ChannelLogPayload.self, from: Data(json.utf8))
        )
    }

    func testMalformedSeedRowsAreSkippedNotGuessed() {
        let payload = ChannelLogPayload(
            channels: [PersistedChannel(handle: handle("a"))],
            subscribedSeeds: [
                PersistedSubscribedSeed(channelId: "a", epoch: 1, seedB64: "not base64!"),
                PersistedSubscribedSeed(channelId: "a", epoch: 2, seedB64: Data(repeating: 1, count: 16).base64EncodedString()),
                PersistedSubscribedSeed(channelId: "a", epoch: 3, seedB64: seed(0x03).base64EncodedString()),
            ]
        )
        XCTAssertEqual(payload.subscribedSeedsByChannel, ["a": [3: seed(0x03)]])
    }

    // MARK: - Publisher re-open decision

    func testReopenSelectsListedChannelsWithBothSeeds() {
        let channels = [handle("owned"), handle("signing-only"), handle("subscribed")]
        let keys = ChannelPublisherReopen.keys(
            channels: channels,
            contentSeeds: ["owned": seed(0xAA), "unlisted": seed(0xBB)],
            signingSeeds: ["owned": seed(0x11), "signing-only": seed(0x22), "unlisted": seed(0x33)]
        )
        XCTAssertEqual(Set(keys.keys), ["owned"])
        XCTAssertEqual(keys["owned"], ChannelPublisherKeys(seed: seed(0xAA), signingKeySeed: seed(0x11)))
    }

    func testReopenRefusesWrongLengthSeeds() {
        let channels = [handle("short-content"), handle("short-signing")]
        let keys = ChannelPublisherReopen.keys(
            channels: channels,
            contentSeeds: ["short-content": Data(repeating: 1, count: 31), "short-signing": seed(0x01)],
            signingSeeds: ["short-content": seed(0x02), "short-signing": Data(repeating: 2, count: 33)]
        )
        XCTAssertTrue(keys.isEmpty)
    }

    func testReopenWithNoSeedsIsEmpty() {
        XCTAssertTrue(
            ChannelPublisherReopen.keys(channels: [handle("a")], contentSeeds: [:], signingSeeds: [:]).isEmpty
        )
    }
}
