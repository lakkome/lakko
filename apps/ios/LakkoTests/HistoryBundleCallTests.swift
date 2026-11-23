// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// History placeholder version 7: a call row ("Outgoing call, 1:02",
/// "Declined", ...) reaches a device added to the account and draws there
/// exactly as the live row does. Seen live 2026-11-23: no call row arrived.
final class HistoryBundleCallTests: XCTestCase {
    private let secret = Data(repeating: 0x42, count: 32)
    private let groupHex = String(repeating: "cd", count: 32)

    private func callRow(_ id: Int64, _ row: CallTranscriptRow, second: Int = 1) -> PersistedMessage {
        PersistedMessage(
            messageId: id, direction: "system", text: row.marker, attachments: [],
            createdAt: "2026-11-23T12:00:0\(second)Z")
    }

    private func wire(_ m: PersistedMessage) throws -> HistoryPlaceholder {
        try decodeHistoryPlaceholder(bytes: try HistoryBundleCodec.encodePlaceholder(m))
    }

    private func restored(_ m: PersistedMessage) throws -> CallTranscriptRow {
        let back = try XCTUnwrap(HistoryBundleCodec.decodePlaceholder(try HistoryBundleCodec.encodePlaceholder(m)))
        XCTAssertEqual(back.direction, "system")
        return try XCTUnwrap(CallTranscriptRow.parse(back.text))
    }

    // MARK: - The wire record

    func testAnOutgoingCallTravelsWithItsDurationInWholeSeconds() throws {
        let live = CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 62_750), video: true)
        let placeholder = try wire(callRow(-1, live))
        XCTAssertEqual(placeholder.call, HistoryCall(outcome: .outgoing, video: true, durationSecs: 62))
        XCTAssertEqual(placeholder.text, "", "the iOS marker is not put on the wire")
        XCTAssertEqual(placeholder.direction, .system)

        let back = try restored(callRow(-1, live))
        XCTAssertEqual(back, CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 62_000), video: true))
        XCTAssertEqual(back.text, live.text, "the row reads as the live one: 1:02 both ways")
        XCTAssertEqual(back.accessibilityLabel, live.accessibilityLabel)
    }

    func testADeclinedCallTravelsWithoutDirectionAndReadsTheSame() throws {
        for direction in [CallTranscriptRow.Direction.outgoing, .incoming] {
            let live = CallTranscriptRow(direction: direction, outcome: .declined)
            XCTAssertEqual(try wire(callRow(-2, live)).call, HistoryCall(outcome: .declined, video: false, durationSecs: nil))
            let back = try restored(callRow(-2, live))
            XCTAssertEqual(back.outcome, .declined)
            XCTAssertEqual(back.text, live.text)
            XCTAssertEqual(back.accessibilityLabel, live.accessibilityLabel)
        }
    }

    func testAnOutgoingCallNobodyAnsweredIsAnOutgoingRecordWithNoDuration() throws {
        let live = CallTranscriptRow(direction: .outgoing, outcome: .missed)
        XCTAssertEqual(try wire(callRow(-3, live)).call, HistoryCall(outcome: .outgoing, video: false, durationSecs: nil))
        XCTAssertEqual(try restored(callRow(-3, live)), live)
    }

    func testAnIncomingRecordWithoutDurationFromAnotherClientReadsAsAnAnsweredCall() throws {
        let placeholder = HistoryPlaceholder(
            messageId: 9, direction: .system, createdAt: "2026-11-23T12:00:00Z", text: "",
            attachments: [], call: HistoryCall(outcome: .incoming))
        let back = try XCTUnwrap(HistoryBundleCodec.decodePlaceholder(try encodeHistoryPlaceholder(placeholder: placeholder)))
        XCTAssertEqual(CallTranscriptRow.parse(back.text), CallTranscriptRow(direction: .incoming, outcome: .answered(durationMs: 0)))
    }

    // MARK: - Round trip through a real bundle

    func testEveryCallRowShapeRoundTripsThroughABundle() throws {
        let sender = "history-calls-sender-\(UUID().uuidString)"
        let joiner = "history-calls-joiner-\(UUID().uuidString)"
        let store = MessageLogStore()
        defer {
            store.deleteLog(accountIdB64: sender)
            store.deleteLog(accountIdB64: joiner)
        }
        let live: [CallTranscriptRow] = [
            CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 74_000)),
            CallTranscriptRow(direction: .incoming, outcome: .answered(durationMs: 0), video: true),
            CallTranscriptRow(direction: .incoming, outcome: .missed, video: true),
            CallTranscriptRow(direction: .outgoing, outcome: .missed),
            CallTranscriptRow(direction: .incoming, outcome: .declined),
            CallTranscriptRow(direction: .outgoing, outcome: .failed, video: true),
        ]
        var rows = live.enumerated().map { callRow(-Int64($0.offset + 1), $0.element, second: $0.offset) }
        rows.append(PersistedMessage(
            messageId: 5, direction: "incoming", text: "hello", attachments: [],
            createdAt: "2026-11-23T12:00:09Z"))
        store.putLog(accountIdB64: sender, payload: MessageLogPayload(groups: [
            groupHex: PersistedGroup(cursor: 5, messages: rows),
        ]))

        let sealed = try XCTUnwrap(HistoryBundleCodec.buildFromMessageLog(
            accountIdB64: sender, secret: secret, store: store))
        let applied = try HistoryBundleCodec.applyToMessageLog(
            accountIdB64: joiner, secret: secret, sealedBytes: sealed, store: store,
                callIndex: InMemoryCallHistoryBacking())
        XCTAssertEqual(applied, live.count + 1)

        let back = try XCTUnwrap(store.getLog(accountIdB64: joiner)?.groups[groupHex]?.messages)
        let byId = Dictionary(uniqueKeysWithValues: back.map { ($0.messageId, $0) })
        for (index, original) in live.enumerated() {
            let row = try XCTUnwrap(byId[-Int64(index + 1)], "call row \(index) arrived")
            XCTAssertEqual(row.direction, "system")
            XCTAssertEqual(row.createdAt, rows[index].createdAt)
            let parsed = try XCTUnwrap(CallTranscriptRow.parse(row.text))
            XCTAssertEqual(parsed.text, original.text, "row \(index) reads as the live row")
            XCTAssertEqual(parsed.accessibilityLabel, original.accessibilityLabel)
            XCTAssertEqual(parsed.video, original.video)
            XCTAssertEqual(parsed.outcome, original.outcome)
            if case .answered = original.outcome { XCTAssertEqual(parsed.direction, original.direction) }
            if case .missed = original.outcome { XCTAssertEqual(parsed.direction, original.direction) }
            // The chat list's preview of the row.
            XCTAssertNotNil(row.toChatMessage())
        }
        XCTAssertEqual(byId[5]?.text, "hello")
    }

    /// The web client's shape: system, empty text, no author, the call start
    /// as `createdAt`, the message id from the call id. Any row carrying a
    /// call record is a call, whatever its text or direction.
    func testARowCarryingACallRecordIsACallWhateverItsTextOrDirection() throws {
        let callId = Data((1...16).map { UInt8($0) })
        let id = try XCTUnwrap(CallTranscriptRow.logMessageId(callId: callId))
        XCTAssertEqual(UInt64(bitPattern: id), 0x8102_0304_0506_0708)
        for (direction, text) in [(HistoryMessageDirection.system, ""), (.incoming, "anything")] {
            let placeholder = HistoryPlaceholder(
                messageId: UInt64(bitPattern: id), direction: direction, createdAt: "2026-11-23T12:00:00Z",
                text: text, attachments: [], call: HistoryCall(outcome: .declined, video: true))
            let back = try XCTUnwrap(HistoryBundleCodec.decodePlaceholder(try encodeHistoryPlaceholder(placeholder: placeholder)))
            XCTAssertEqual(back.messageId, id)
            XCTAssertEqual(back.direction, "system")
            XCTAssertEqual(CallTranscriptRow.parse(back.text)?.outcome, .declined)
            XCTAssertEqual(CallTranscriptRow.parse(back.text)?.video, true)
        }
    }

    func testTheSameCallInTwoBundlesIsOneRowAndSitsAtItsTime() throws {
        let joiner = "history-calls-twice-\(UUID().uuidString)"
        let store = MessageLogStore()
        defer { store.deleteLog(accountIdB64: joiner) }
        let callId = Data(repeating: 0x33, count: 16)
        let id = try XCTUnwrap(CallTranscriptRow.logMessageId(callId: callId))
        let call = PersistedMessage(
            messageId: id, direction: "system",
            text: CallTranscriptRow(direction: .incoming, outcome: .missed).marker, attachments: [],
            createdAt: "2026-11-23T12:00:05Z")
        let before = PersistedMessage(
            messageId: 4, direction: "incoming", text: "before", attachments: [], createdAt: "2026-11-23T12:00:01Z")
        let after = PersistedMessage(
            messageId: 6, direction: "incoming", text: "after", attachments: [], createdAt: "2026-11-23T12:00:09Z")
        for _ in 0..<2 {
            let messages = try [before, call, after].map { m in
                StructuredHistoryMessage(
                    payloadBytes: try HistoryBundleCodec.encodePlaceholder(m), senderLeaf: 0,
                    messageId: withUnsafeBytes(of: m.messageId.bigEndian) { Data($0) }, receivedAtMs: 0)
            }
            let sealed = try sealHistoryBundleStructured(
                secret: secret,
                groups: [StructuredHistoryGroup(
                    groupId: try XCTUnwrap(StateBundle.hexDecode(groupHex)), messages: messages, countCapped: false)],
                maxHlc: 6)
            _ = try HistoryBundleCodec.applyToMessageLog(
                accountIdB64: joiner, secret: secret, sealedBytes: sealed, store: store,
                callIndex: InMemoryCallHistoryBacking())
        }
        let rows = try XCTUnwrap(store.getLog(accountIdB64: joiner)?.groups[groupHex]?.messages)
        XCTAssertEqual(rows.map(\.messageId), [4, id, 6], "one row, between the messages around its time")
    }

    func testAnUnreadableCallRowIsNotCarried() throws {
        let m = PersistedMessage(
            messageId: -1, direction: "system", text: "[call] sideways", attachments: [],
            createdAt: "2026-11-23T12:00:00Z")
        XCTAssertThrowsError(try HistoryBundleCodec.encodePlaceholder(m))
    }

    // MARK: - The Calls tab

    func testACallsIndexEntryComesFromTheRestoredRowWithItsVideo() throws {
        let live = CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 30_000), video: true)
        let m = try XCTUnwrap(HistoryBundleCodec.decodePlaceholder(
            try HistoryBundleCodec.encodePlaceholder(callRow(-1, live))))
        let entry = try XCTUnwrap(CallHistoryEntry.fromLogRow(
            conversationId: groupHex, kind: .chat, text: m.text, createdAt: m.createdAt))
        XCTAssertEqual(entry.outcome, .answered)
        XCTAssertEqual(entry.direction, .outgoing)
        XCTAssertEqual(entry.durationMs, 30_000)
        XCTAssertTrue(entry.video)
        XCTAssertEqual(entry.transcriptRow.text, live.text)
        XCTAssertNil(CallHistoryEntry.fromLogRow(
            conversationId: groupHex, kind: .chat, text: "hello", createdAt: m.createdAt))
    }

    /// The disk-log path (no chat repository running): the call rows a bundle
    /// writes into the log are filed in the Calls index in the same step,
    /// once, and a conversation the index already knows keeps its kind.
    func testTheDiskLogPathFilesBundleCallsInTheCallsIndexOnce() throws {
        let sender = "history-calls-index-sender-\(UUID().uuidString)"
        let joiner = "history-calls-index-joiner-\(UUID().uuidString)"
        let store = MessageLogStore()
        let index = InMemoryCallHistoryBacking()
        defer {
            store.deleteLog(accountIdB64: sender)
            store.deleteLog(accountIdB64: joiner)
        }
        let otherHex = String(repeating: "ab", count: 32)
        CallHistoryStore.save(backing: index, accountIdB64: joiner, entries: [CallHistoryEntry(
            id: "\(otherHex).1.outgoing", conversationId: otherHex, kind: .chat, direction: .outgoing,
            outcome: .answered, video: false, startedAtMs: 1, durationMs: 1_000)])
        let outgoing = CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 42_000), video: true)
        let declined = CallTranscriptRow(direction: .incoming, outcome: .declined)
        store.putLog(accountIdB64: sender, payload: MessageLogPayload(groups: [
            groupHex: PersistedGroup(cursor: 0, messages: [callRow(-1, outgoing, second: 1), callRow(-2, declined, second: 2)]),
            otherHex: PersistedGroup(cursor: 0, messages: [callRow(-3, declined, second: 3)]),
        ]))
        let sealed = try XCTUnwrap(HistoryBundleCodec.buildFromMessageLog(
            accountIdB64: sender, secret: secret, store: store))

        for _ in 0..<2 {
            _ = try HistoryBundleCodec.applyToMessageLog(
                accountIdB64: joiner, secret: secret, sealedBytes: sealed, store: store, callIndex: index)
        }

        let entries = CallHistoryStore.load(backing: index, accountIdB64: joiner)
        XCTAssertEqual(entries.count, 4, "the one held before and the three the bundle brought, each once")
        let here = entries.filter { $0.conversationId == groupHex }
        XCTAssertEqual(Set(here.map(\.outcome)), [.answered, .declined])
        let call = try XCTUnwrap(here.first { $0.outcome == .answered })
        XCTAssertEqual(call.transcriptRow.text, outgoing.text)
        XCTAssertTrue(call.video)
        XCTAssertEqual(call.kind, .group, "not known yet: the Calls screen reads the live kind")
        let other = entries.filter { $0.conversationId == otherHex && $0.outcome == .declined }
        XCTAssertEqual(other.map(\.kind), [.chat], "the kind the index already holds for the conversation")
    }

    // MARK: - Gap history (ADR-0091)

    func testAGapCallRowIsKeptOnlyFromThisAccountsOwnDevice() {
        let row = CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 5_000), video: true)
        XCTAssertEqual(HistoryBundleCodec.gapCallRow(row, route: "own-device"), row)
        XCTAssertNil(HistoryBundleCodec.gapCallRow(row, route: "mailbox"))
        XCTAssertNil(HistoryBundleCodec.gapCallRow(row, route: "knock"))
        XCTAssertNil(HistoryBundleCodec.gapCallRow(row, route: nil))
    }

    func testAGroupReadmittersCallRowIsNotFiledOnTheDeviceThatCameBack() throws {
        let receiver = "history-gap-call-\(UUID().uuidString)"
        let store = MessageLogStore()
        defer { store.deleteLog(accountIdB64: receiver) }
        let readmitter = try Identity.fromSeed(seed: Data(repeating: 0x44, count: 32))
        let gapKey = generateGapKey()
        let groupId = Data(repeating: 0xCD, count: 32)
        let rows = [
            callRow(-1, CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 9_000))),
            PersistedMessage(
                messageId: 3, direction: "outgoing", text: "hi", attachments: [],
                createdAt: "2026-11-23T12:00:03Z"),
        ]
        let sealed = try XCTUnwrap(HistoryBundleCodec.buildGapForGroup(
            hex: groupHex, rows: rows, accountIdB64: "sender", readmitter: readmitter,
            gapKey: gapKey.publicKey, groupId: groupId))
        let opened = try openGapHistoryStructured(gapSecret: gapKey.secret, groupId: groupId, sealed: sealed).bundle

        _ = HistoryBundleCodec.applyOpenedGapBundle(opened, accountIdB64: receiver, route: "knock", store: store,
            callIndex: InMemoryCallHistoryBacking())
        let back = try XCTUnwrap(store.getLog(accountIdB64: receiver)?.groups[groupHex]?.messages)
        XCTAssertEqual(back.map(\.messageId), [3])

        let other = "history-gap-call-own-\(UUID().uuidString)"
        defer { store.deleteLog(accountIdB64: other) }
        _ = HistoryBundleCodec.applyOpenedGapBundle(opened, accountIdB64: other, route: "own-device", store: store,
            callIndex: InMemoryCallHistoryBacking())
        let own = try XCTUnwrap(store.getLog(accountIdB64: other)?.groups[groupHex]?.messages)
        XCTAssertEqual(own.map(\.messageId), [-1, 3])
        XCTAssertEqual(
            CallTranscriptRow.parse(own[0].text),
            CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 9_000)))
    }
}
