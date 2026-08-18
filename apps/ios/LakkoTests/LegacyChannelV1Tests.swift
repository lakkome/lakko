// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// Retired-v1 PublisherGroup error mapping (Swift mirror of the web
/// `legacy-v1.test.ts`).
///
/// The point of these tests is **discrimination**, not just that the new copy
/// appears: a network / server / codec failure must keep its own text, or the
/// mapping has simply swallowed every channel error.
final class LegacyChannelV1Tests: XCTestCase {

    /// The refusal `refuse_if_legacy_v1_present` raises, as it reaches Swift
    /// through UniFFI's `FfiError.Group(msg)` carrying `Error::Group`'s payload.
    private static let rustRefusal =
        "legacy v1 PublisherGroup state present for this channel: the v1 group id was "
        + "41 bytes, which the Delivery Service rejects, and an MLS group id is bound into "
        + "the GroupContext so it cannot be re-keyed to the 32-byte v2 id. The v1 state is "
        + "left intact; this channel needs a migration or a fresh channel."

    /// A stand-in for an arbitrary non-Lakko failure (URLSession, decoding, …).
    private struct PlainError: Error, LocalizedError {
        let text: String
        var errorDescription: String? { text }
    }

    private func v1GroupId(fill: UInt8 = 0xAB) -> Data {
        var out = Data(LegacyChannelV1.v1Prefix)
        out.append(Data(repeating: fill, count: 16))
        return out
    }

    private func v2GroupId(fill: UInt8 = 0xAB) -> Data {
        var out = Data("lakko/pubgrp/v2/".utf8)
        out.append(Data(repeating: fill, count: 16))
        return out
    }

    // MARK: - Structural handle: the dead invite link

    func testRecognisesA41ByteV1GroupId() {
        XCTAssertTrue(LegacyChannelV1.isLegacyV1PublisherGroupId(v1GroupId()))
    }

    func testRejectsACurrentV2PublisherGroupId() {
        XCTAssertEqual(v2GroupId().count, 32)
        XCTAssertFalse(LegacyChannelV1.isLegacyV1PublisherGroupId(v2GroupId()))
    }

    func testRejectsA32ByteChatGroupId() {
        XCTAssertFalse(
            LegacyChannelV1.isLegacyV1PublisherGroupId(Data(repeating: 0x7F, count: 32)))
    }

    func testRejects41BytesWithoutTheV1Prefix() {
        XCTAssertFalse(
            LegacyChannelV1.isLegacyV1PublisherGroupId(Data(repeating: 0x00, count: 41)))
    }

    func testRejectsAnEmptyId() {
        XCTAssertFalse(LegacyChannelV1.isLegacyV1PublisherGroupId(Data()))
    }

    // MARK: - String handle: the unopenable channel

    func testRecognisesTheRustRefusal() {
        XCTAssertTrue(
            LegacyChannelV1.isLegacyPublisherGroupError(PlainError(text: Self.rustRefusal)))
    }

    /// The pure text predicate — the same one `isLegacyPublisherGroupError`
    /// runs against a UniFFI `FfiError.Group` payload in production. Driven
    /// directly here because this test target is deliberately FFI-free (it does
    /// not link `LakkoCore`, so it cannot name `FfiError`).
    func testRecognisesTheRefusalAsRawText() {
        XCTAssertTrue(LegacyChannelV1.isLegacyRefusalText(Self.rustRefusal))
    }

    func testRefusalTextPredicateRejectsUnrelatedText() {
        XCTAssertFalse(LegacyChannelV1.isLegacyRefusalText("group operation failed: no such group"))
        XCTAssertFalse(LegacyChannelV1.isLegacyRefusalText("The Internet connection appears to be offline."))
        XCTAssertFalse(LegacyChannelV1.isLegacyRefusalText(""))
    }

    /// Discrimination: the unrelated failures must NOT be captured.
    func testDoesNotFireOnUnrelatedFailures() {
        let unrelated: [(String, Error)] = [
            ("a network failure", PlainError(text: "The Internet connection appears to be offline.")),
            ("a server rejection", PlainError(text: "channels service returned 503")),
            ("a bad channel id", CoPublisherError.badChannelId("zzzz")),
            ("an unrelated MLS group error", PlainError(text: "group operation failed: no such group")),
            ("a chat-group /g/ link", CoPublisherError.notAChannelGroup),
        ]
        for (label, err) in unrelated {
            XCTAssertFalse(
                LegacyChannelV1.isLegacyPublisherGroupError(err),
                "should not fire on \(label)")
        }
    }

    // MARK: - Mapping

    func testMapOpenErrorConvertsTheRefusal() {
        let mapped = LegacyChannelV1.mapOpenError(PlainError(text: Self.rustRefusal))
        guard let legacy = mapped as? LegacyChannelError else {
            return XCTFail("expected a LegacyChannelError, got \(mapped)")
        }
        XCTAssertEqual(legacy.kind, .channel)
        XCTAssertEqual(legacy.message, LegacyChannelV1.channelMessage)
    }

    func testMapOpenErrorLeavesAnUnrelatedFailureUntouched() {
        let original = PlainError(text: "The Internet connection appears to be offline.")
        let mapped = LegacyChannelV1.mapOpenError(original)
        XCTAssertNil(mapped as? LegacyChannelError)
        XCTAssertEqual((mapped as? PlainError)?.text, original.text)
    }

    func testDescribeReturnsTheChannelCopyForTheRefusal() {
        let msg = LegacyChannelV1.describe(PlainError(text: Self.rustRefusal)) { "fallback: \($0)" }
        XCTAssertEqual(msg, LegacyChannelV1.channelMessage)
    }

    func testDescribeReturnsTheInviteLinkCopyForALinkError() {
        let msg = LegacyChannelV1.describe(LegacyChannelError(kind: .inviteLink)) {
            "fallback: \($0)"
        }
        XCTAssertEqual(msg, LegacyChannelV1.coPublisherLinkMessage)
    }

    /// Discrimination: a network failure still reads as a network failure.
    func testDescribeFallsBackForANetworkFailure() {
        let original = PlainError(text: "The Internet connection appears to be offline.")
        let msg = LegacyChannelV1.describe(original) {
            "Could not open the channel group: \($0.localizedDescription)"
        }
        XCTAssertTrue(msg.contains("Internet connection"))
        XCTAssertNotEqual(msg, LegacyChannelV1.channelMessage)
    }

    // MARK: - Copy

    func testChannelCopySaysAllThreeThingsInOrder() {
        let msg = LegacyChannelV1.channelMessage
        guard let older = msg.range(of: "older version"),
            let readable = msg.range(of: "stay readable"),
            let create = msg.range(of: "create a new channel")
        else {
            return XCTFail("channel copy is missing one of its three required parts")
        }
        XCTAssertTrue(older.lowerBound < readable.lowerBound)
        XCTAssertTrue(readable.lowerBound < create.lowerBound)
        XCTAssertTrue(msg.contains("cannot be repaired"))
    }

    func testCopyOffersNoRepairOrRetryAffordance() {
        for msg in [LegacyChannelV1.channelMessage, LegacyChannelV1.coPublisherLinkMessage] {
            let lowered = msg.lowercased()
            for forbidden in ["migrat", "retry", "try again", "repair this", "recover"] {
                XCTAssertFalse(lowered.contains(forbidden), "copy must not offer \(forbidden)")
            }
        }
    }

    func testInviteLinkCopyTellsTheJoinerToAskForANewLink() {
        XCTAssertTrue(LegacyChannelV1.coPublisherLinkMessage.contains("older version"))
        XCTAssertTrue(LegacyChannelV1.coPublisherLinkMessage.contains("new invite link"))
    }

    /// The copy must be byte-identical to the web + Android ports.
    func testCopyMatchesTheCrossPlatformWording() {
        XCTAssertEqual(
            LegacyChannelV1.channelMessage,
            "This channel was created by an older version of Lakko and cannot be opened in this "
                + "build. Posts you already published stay readable for your subscribers. "
                + "To publish again, create a new channel — this one cannot be repaired.")
        XCTAssertEqual(
            LegacyChannelV1.coPublisherLinkMessage,
            "This co-publisher invite link was created by an older version of Lakko and no longer "
                + "works. Ask the channel owner to send you a new invite link.")
    }
}
