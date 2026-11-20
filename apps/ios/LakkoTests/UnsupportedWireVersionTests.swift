// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0061 section 6 — a chat message written in a newer wire version than
/// this build reads is a real message: a timeline row, a chat-list preview, an
/// unread message and a notification body. It is rendered from the catalog
/// sentence `chat.message.unsupported` and never from the core's legacy
/// English placeholder ("[unsupported vN message]").
@MainActor
final class UnsupportedWireVersionTests: XCTestCase {

    /// A chat envelope at a version this build does not know.
    private let future = Data(#"{"v":99,"text":"future","attachments":[]}"#.utf8)

    private var sentence: String { Localization.text("chat.message.unsupported") }

    // MARK: - The core verdict

    func testTheRealParserClassifiesANewerVersionAsUnsupportedWithNoText() {
        let parsed = parseBodyEnvelope(bytes: future)
        XCTAssertEqual(parsed.kind, .unsupported)
        XCTAssertEqual(parsed.text, "")
        XCTAssertTrue(parsed.attachments.isEmpty)
        XCTAssertEqual(parsed.target, 99)
    }

    func testTheCatalogHasTheSentence() {
        XCTAssertFalse(sentence.isEmpty)
        XCTAssertNotEqual(sentence, "chat.message.unsupported")
        XCTAssertFalse(sentence.contains("[unsupported"))
    }

    // MARK: - Foreground

    private func classify(_ plaintext: Data) -> IncomingChatEvent {
        ChatClient.toEvent(
            messageId: 7,
            createdAt: "2026-10-01T10:00:00Z",
            processed: .application(data: plaintext, senderLeaf: 2, senderAccount: nil),
            resolveSender: { "member-\($0)" }
        )
    }

    func testANewerVersionIsAnUnsupportedEventCarryingTheVersion() {
        let event = classify(future)
        guard case .unsupported(let messageId, let version, let senderKey, _) = event else {
            return XCTFail("a newer wire version must classify as .unsupported, got \(event)")
        }
        XCTAssertEqual(messageId, 7)
        XCTAssertEqual(version, 99)
        XCTAssertEqual(senderKey, "member-2")
    }

    func testANewerVersionCountsAsAnUnreadMessage() {
        XCTAssertEqual(UnreadCounting.incomingMessages(in: [classify(future)]), 1)
    }

    func testTheMarkerSurvivesTheMessageLogWithNoStoredText() throws {
        let message = ChatMessage(
            messageId: 7, direction: .incoming, text: "",
            createdAt: "2026-10-01T10:00:00Z", unsupportedVersion: 99)
        let blob = try JSONEncoder().encode(PersistedMessage(from: message))
        XCTAssertFalse(String(decoding: blob, as: UTF8.self).contains("[unsupported"))
        let restored = try XCTUnwrap(
            JSONDecoder().decode(PersistedMessage.self, from: blob).toChatMessage())
        XCTAssertEqual(restored.unsupportedVersion, 99)
        XCTAssertEqual(restored.text, "")
    }

    func testARowWrittenBeforeTheFieldReadsAsAnOrdinaryMessage() throws {
        let blob = Data(#"{"messageId":1,"direction":"incoming","text":"hi","attachments":[],"createdAt":"x"}"#.utf8)
        let restored = try XCTUnwrap(
            JSONDecoder().decode(PersistedMessage.self, from: blob).toChatMessage())
        XCTAssertNil(restored.unsupportedVersion)
    }

    func testTheConversationRowShowsTheSentence() {
        let line = ChatListFormatting.preview(
            text: "",
            attachmentLabels: [],
            direction: .incoming,
            deleted: false,
            unsupported: true,
            isGroup: false,
            senderName: nil)
        XCTAssertEqual(line, sentence)
    }

    // MARK: - Notification Service Extension

    func testTheExtensionReadsTheSentenceFromTheAppGroupWords() {
        XCTAssertTrue(SharedStorage.notificationWordKeys.contains("chat.message.unsupported"))
    }

    private final class ParsingFakeGroup: NseThrowawayGroup {
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
            case .poll, .checklist, .location, .contactCard, .article: kind = .content
            case .pollVote, .pollClose, .checklistTick, .checklistAdd, .editChecklist, .editArticle:
                kind = .contentOp
            }
            return NseProcessedRow(kind: kind, text: body.text, attachmentCount: body.attachments.count)
        }

        func dsProofHeader(method: String, path: String, query: String, body: Data) -> String? { nil }
        func close() {}
    }

    private struct StubDeps: NsePushDecryptDeps {
        func personName(accountIdB64: String, keyB64: String) -> String? { nil }
        func reactionNotificationsOn() -> Bool { true }
        let rows: [NseFetchedRow]

        func boundAccount(wakeSubscriptionIdHex: String?) -> String? { "acct" }
        func lookupRoute(accountIdB64: String, pushGidHex: String, subscriptionIdHex: String?) -> String? {
            accountIdB64 == "acct" ? pushGidHex : nil
        }
        func accountKnowsGroup(accountIdB64: String, groupHex: String) -> Bool { accountIdB64 == "acct" }
        func loadIdentitySeed(accountIdB64: String) -> Data? { Data(repeating: 1, count: 64) }
        func loadSnapshot(accountIdB64: String) -> Data? { Data(repeating: 2, count: 8) }
        func openThrowawayGroup(seed: Data, snapshot: Data, groupIdBytes: Data) throws -> NseThrowawayGroup? {
            ParsingFakeGroup()
        }
        func dsFetch(groupIdBytes: Data, afterId: Int64, group: NseThrowawayGroup) async throws -> [NseFetchedRow] {
            rows
        }
        func startCursor(accountIdB64: String, groupHex: String) -> Int64? { nil }
        func isAntiCensorshipArmed() -> Bool { false }
    }

    func testTheNotificationBodyIsTheSentenceNeverCoreProse() async throws {
        let older = try composeEnvelope(text: "older", attachments: [])
        let rows = [older, future].enumerated().map {
            NseFetchedRow(id: Int64(10 + $0.offset), payload: $0.element)
        }
        let result = await decryptPushPreview(
            NsePushWake(groupHex: String(repeating: "a", count: 64), pointer: 99),
            StubDeps(rows: rows))
        XCTAssertEqual(result?.body, nseUnsupportedMessageLine())
        XCTAssertFalse(result?.body.contains("[unsupported") ?? true)
    }

    // MARK: - Device link (ADR-0056 join by name)

    func testAJoinIntoAnotherAccountRendersByKind() {
        XCTAssertEqual(
            AuthErrorCopy.deviceRestore(PairingOrchestrator.JoinByNameError.wrongAccount),
            Localization.text("device_link.error.wrong_account"))
        XCTAssertEqual(
            AuthErrorCopy.deviceRestore(PairingOrchestrator.JoinByNameError.noAccess),
            Localization.text("device_link.error.not_let_in"))
        XCTAssertEqual(
            PairingOrchestrator.JoinByNameError.wrongAccount.errorDescription,
            Localization.text("device_link.error.wrong_account"))
    }
}
