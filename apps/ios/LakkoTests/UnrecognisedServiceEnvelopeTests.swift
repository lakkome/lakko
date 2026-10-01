// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// Live walk 2026-09-29 — a service envelope this build cannot read is never
/// rendered: not as a timeline bubble, not as a chat-list preview or an unread
/// message, not as a notification.
///
/// ## The defect this pins
///
/// A client on another branch sent a profile envelope v3 into shared groups.
/// The foreground sniff (`decodeProfileIfEnvelope`) and the NSE pre-filter both
/// refuse a version they do not know, correctly, and the bytes then fell
/// through to `parseBodyEnvelope`, whose legacy fallback turned the non-JSON
/// plaintext into text: every chat and the chat list showed a bubble reading
/// `lakko/profile/v1` plus binary. The core now classifies any non-JSON
/// plaintext opening with `lakko/` as `MessageBodyKind.serviceEnvelope`; these
/// cases drive that verdict through the real FFI parser and assert each client
/// path drops it.
///
/// The fixture is built from ASCII plus an explicit NUL byte, never a literal,
/// so this source file carries no raw NUL.
@MainActor
final class UnrecognisedServiceEnvelopeTests: XCTestCase {

    /// `"lakko/profile/v1\0" + [0x03, ...]`: a profile envelope at a version
    /// this build does not know, shaped like the one seen live.
    private func profileV3() -> Data {
        var bytes = Data("lakko/profile/v1".utf8)
        bytes.append(0x00)
        bytes.append(contentsOf: [0x03, 0x00, 0x00, 0x01, 0x92, 0x5a, 0x10, 0x20, 0x30, 0x05])
        bytes.append(Data("Alice".utf8))
        bytes.append(0x00)
        return bytes
    }

    // MARK: - The core verdict

    func testTheRealParserClassifiesAProfileV3AsAServiceEnvelopeWithNoText() {
        let parsed = parseBodyEnvelope(bytes: profileV3())
        XCTAssertEqual(parsed.kind, .serviceEnvelope)
        XCTAssertEqual(parsed.text, "")
        XCTAssertTrue(parsed.attachments.isEmpty)
    }

    func testTheForegroundProfileSniffDoesNotRecogniseTheV3() {
        // The precondition of the defect: the version-gated sniff refuses the
        // envelope, so without the parser's verdict it would fall to text.
        XCTAssertNil(ChatClient.decodeProfileIfEnvelope(profileV3()))
    }

    // MARK: - Foreground: timeline, chat list, unread

    private func classify(_ plaintext: Data) -> IncomingChatEvent {
        ChatClient.toEvent(
            messageId: 7,
            createdAt: "2026-09-29T10:00:00Z",
            processed: .application(data: plaintext, senderLeaf: 2, senderAccount: nil),
            resolveSender: { "member-\($0)" }
        )
    }

    func testAProfileV3IsNeverATimelineRow() {
        let event = classify(profileV3())
        guard case .serviceEnvelope(let messageId, _) = event else {
            return XCTFail("a profile v3 must classify as .serviceEnvelope, got \(event)")
        }
        XCTAssertEqual(messageId, 7)
        if case .application(_, let text, _, _, _, _, _) = event {
            XCTFail("rendered as a chat bubble reading '\(text)'")
        }
    }

    func testAProfileV3NeverCountsAsAnUnreadMessage() {
        XCTAssertEqual(UnreadCounting.incomingMessages(in: [classify(profileV3())]), 0)
        XCTAssertEqual(UnreadCounting.next(current: 3, applied: [classify(profileV3())], chatOpen: false), 3)
    }

    func testAnyUnknownLakkoEnvelopeIsDroppedAndAChatEnvelopeStillRenders() throws {
        var unknown = Data("lakko/something-new/v9".utf8)
        unknown.append(contentsOf: [0x00, 0x01, 0xff])
        guard case .serviceEnvelope = classify(unknown) else {
            return XCTFail("an unknown lakko/ envelope must be dropped")
        }
        let chat = try composeEnvelope(text: "lakko/ is a prefix", attachments: [])
        guard case .application(_, let text, _, _, _, _, _) = classify(chat) else {
            return XCTFail("a chat envelope must still render")
        }
        XCTAssertEqual(text, "lakko/ is a prefix")
        guard case .application(_, let legacy, _, _, _, _, _) = classify(Data("see lakko/ docs".utf8)) else {
            return XCTFail("legacy plaintext must still render")
        }
        XCTAssertEqual(legacy, "see lakko/ docs")
    }

    // MARK: - Notification Service Extension

    /// A throwaway group shaped like the production `NseRealThrowawayGroup
    /// .process`: the pre-filter first (here it recognises nothing, as for an
    /// envelope version the extension does not know), then the REAL parser,
    /// mapped exactly as `NseDecryptDeps` maps it. The MLS decrypt is the only
    /// stubbed part.
    private final class ParsingFakeGroup: NseThrowawayGroup {
        var closed = false

        func process(_ payload: Data) -> NseProcessedRow? {
            let body = parseBodyEnvelope(bytes: payload)
            let kind: NseProcessedKind
            switch body.kind {
            case .chat: kind = .chat
            case .edit: kind = .edit
            case .delete: kind = .delete
            case .reaction: kind = .reaction
            case .typing: kind = .typing
            case .readReceipt: kind = .readReceipt
            case .serviceEnvelope: kind = .serviceEnvelope
            case .unsupported: kind = .unsupported
            }
            return NseProcessedRow(kind: kind, text: body.text, attachmentCount: body.attachments.count)
        }

        func dsProofHeader(method: String, path: String, query: String, body: Data) -> String? { nil }
        func close() { closed = true }
    }

    /// A group that hands back a `.serviceEnvelope` row CARRYING text, so the
    /// pure core's own arm is what drops it rather than the parser's empty text.
    private final class TextCarryingServiceEnvelopeGroup: NseThrowawayGroup {
        func process(_ payload: Data) -> NseProcessedRow? {
            if payload.starts(with: Data("lakko/".utf8)) {
                return NseProcessedRow(
                    kind: .serviceEnvelope,
                    text: String(decoding: payload, as: UTF8.self),
                    attachmentCount: 0)
            }
            return NseProcessedRow(kind: .chat, text: String(decoding: payload, as: UTF8.self), attachmentCount: 0)
        }

        func dsProofHeader(method: String, path: String, query: String, body: Data) -> String? { nil }
        func close() {}
    }

    private struct StubDeps: NsePushDecryptDeps {
        let group: NseThrowawayGroup
        let rows: [NseFetchedRow]

        func boundAccount(wakeDevicePseudonymHex: String?) -> String? { "acct" }
        func lookupRoute(accountIdB64: String, pushGidHex: String) -> String? { nil }
        func accountKnowsGroup(accountIdB64: String, groupHex: String) -> Bool {
            accountIdB64 == "acct"
        }
        func loadIdentitySeed(accountIdB64: String) -> Data? { Data(repeating: 1, count: 64) }
        func loadSnapshot(accountIdB64: String) -> Data? { Data(repeating: 2, count: 8) }
        func openThrowawayGroup(seed: Data, snapshot: Data, groupIdBytes: Data) throws -> NseThrowawayGroup? {
            group
        }
        func dsFetch(groupIdBytes: Data, afterId: Int64, group: NseThrowawayGroup) async throws -> [NseFetchedRow] {
            rows
        }
        func startCursor(accountIdB64: String, groupHex: String) -> Int64? { nil }
        func isAntiCensorshipArmed() -> Bool { false }
    }

    private func preview(_ group: NseThrowawayGroup, _ payloads: [Data]) async -> NsePushPreview? {
        let rows = payloads.enumerated().map { NseFetchedRow(id: Int64(10 + $0.offset), payload: $0.element) }
        return await decryptPushPreview(
            NsePushWake(groupHex: String(repeating: "a", count: 64), pointer: 99),
            StubDeps(group: group, rows: rows))
    }

    func testANewestProfileV3FallsBackToTheRealMessageInTheNotification() async throws {
        let group = ParsingFakeGroup()
        let chat = try composeEnvelope(text: "real message", attachments: [])
        let result = await preview(group, [chat, profileV3()])
        XCTAssertEqual(result?.body, "real message")
        XCTAssertTrue(group.closed)
    }

    func testOnlyAProfileV3YieldsNoNotificationText() async {
        let result = await preview(ParsingFakeGroup(), [profileV3()])
        XCTAssertNil(result, "no real message: the generic banner, never the envelope")
    }

    func testTheExtensionCoreNeverPreviewsAServiceEnvelopeWhateverTextItCarries() async {
        let result = await preview(TextCarryingServiceEnvelopeGroup(), [Data("hello".utf8), profileV3()])
        XCTAssertEqual(result?.body, "hello")
        let only = await preview(TextCarryingServiceEnvelopeGroup(), [profileV3()])
        XCTAssertNil(only)
    }
}
