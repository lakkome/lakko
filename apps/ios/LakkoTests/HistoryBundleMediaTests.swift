// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0047 §5: a media message moved to a new device through the history
/// bundle restores as itself, through the real core codec.
final class HistoryBundleMediaTests: XCTestCase {
    private func b64(_ count: Int, _ fill: UInt8) -> String {
        Data(repeating: fill, count: count).base64EncodedString()
    }

    private func roundTrip(_ attachment: PersistedAttachment) throws -> PersistedAttachment {
        let message = PersistedMessage(
            messageId: 42,
            direction: "incoming",
            text: "",
            attachments: [attachment],
            createdAt: "2026-09-14T12:00:00Z"
        )
        let bytes = try HistoryBundleCodec.encodePlaceholder(message)
        let back = try XCTUnwrap(HistoryBundleCodec.decodePlaceholder(bytes))
        return try XCTUnwrap(back.attachments.first)
    }

    func testVoiceMessageRestoresAsVoiceMessage() throws {
        let waveform = Data([3, 9, 27, 81]).base64EncodedString()
        let back = try roundTrip(PersistedAttachment(
            blobIdB64: b64(32, 1), keyB64: b64(32, 2), nonceB64: b64(12, 3),
            mime: "audio/mp4", size: 1234, name: "voice.m4a",
            kind: "voice", durationMs: 7250, waveformB64: waveform
        ))
        XCTAssertEqual(back.kind, "voice")
        XCTAssertEqual(back.durationMs, 7250)
        XCTAssertEqual(back.waveformB64, waveform)
    }

    func testVideoNoteKeepsItsPosterFrame() throws {
        let thumb = PersistedThumb(
            blobIdB64: b64(32, 7), keyB64: b64(32, 8), nonceB64: b64(12, 9),
            mime: "image/jpeg", size: 20_000, width: 240, height: 240
        )
        let back = try roundTrip(PersistedAttachment(
            blobIdB64: b64(32, 1), keyB64: b64(32, 2), nonceB64: b64(12, 3),
            mime: "video/mp4", size: 800_000, name: "note.mp4",
            kind: "video_note", durationMs: 42_000, width: 480, height: 480, thumb: thumb
        ))
        XCTAssertEqual(back.kind, "video_note")
        XCTAssertEqual(back.width, 480)
        XCTAssertEqual(back.thumb, thumb)
    }

    /// ADR-0047 section 6 — the first photo's micro thumbnail travels in the
    /// bundle, so a restored list row has its picture.
    func testThePhotoKeepsItsInlineThumb() throws {
        let inline = (Data([0xFF, 0xD8, 0xFF, 0xE0]) + Data(repeating: 4, count: 60)).base64EncodedString()
        let back = try roundTrip(PersistedAttachment(
            blobIdB64: b64(32, 1), keyB64: b64(32, 2), nonceB64: b64(12, 3),
            mime: "image/jpeg", size: 50_000, name: "photo.jpg",
            width: 400, height: 300, inlineThumbB64: inline
        ))
        XCTAssertEqual(back.inlineThumbB64, inline)
    }

    func testOutOfBoundValuesAreDroppedLikeTheEnvelope() throws {
        let back = try roundTrip(PersistedAttachment(
            blobIdB64: b64(32, 1), keyB64: b64(32, 2), nonceB64: b64(12, 3),
            mime: "image/gif", size: 10, name: "a.gif",
            kind: "gif", durationMs: 1000, width: 0, height: 5000
        ))
        XCTAssertEqual(back.kind, "gif")
        XCTAssertNil(back.durationMs)
        XCTAssertNil(back.width)
        XCTAssertNil(back.height)
    }

    // MARK: - MLS bookkeeping rows never travel

    private func row(_ id: Int64, _ direction: String, _ text: String) -> PersistedMessage {
        PersistedMessage(
            messageId: id, direction: direction, text: text, attachments: [],
            createdAt: "2026-10-28T12:00:0\(id)Z")
    }

    private let secret = Data(repeating: 0x42, count: 32)
    private let groupHex = String(repeating: "cd", count: 32)

    func testTheBuilderLeavesOutCommitAndExternalJoinRows() throws {
        let account = "history-bookkeeping-build-\(UUID().uuidString)"
        let store = MessageLogStore()
        defer { store.deleteLog(accountIdB64: account) }
        store.putLog(accountIdB64: account, payload: MessageLogPayload(groups: [
            groupHex: PersistedGroup(cursor: 4, messages: [
                row(1, "incoming", "hello"),
                row(2, "system", "[mls commit]"),
                row(3, "system", "[mls external join]"),
                row(4, "outgoing", "[mls commit]"),
            ]),
        ]))
        XCTAssertNotNil(store.getLog(accountIdB64: account), "the test log was written")

        let sealed = try XCTUnwrap(HistoryBundleCodec.buildFromMessageLog(
            accountIdB64: account, secret: secret, store: store))
        let opened = try openHistoryBundleStructured(secret: secret, sealedBytes: sealed)
        let rows = opened.groups.flatMap(\.messages).compactMap {
            HistoryBundleCodec.decodePlaceholder($0.payloadBytes)
        }
        XCTAssertEqual(rows.map(\.messageId), [1, 4], "a message reading like the marker is still carried")
    }

    func testTheReceiverDropsCommitRowsItIsSent() throws {
        let account = "history-bookkeeping-apply-\(UUID().uuidString)"
        let store = MessageLogStore()
        defer { store.deleteLog(accountIdB64: account) }
        let messages = try [
            row(1, "incoming", "hello"),
            row(2, "system", "[mls commit]"),
        ].map { m in
            StructuredHistoryMessage(
                payloadBytes: try HistoryBundleCodec.encodePlaceholder(m), senderLeaf: 0,
                messageId: withUnsafeBytes(of: m.messageId.bigEndian) { Data($0) }, receivedAtMs: 0)
        }
        let sealed = try sealHistoryBundleStructured(
            secret: secret,
            groups: [StructuredHistoryGroup(
                groupId: try XCTUnwrap(StateBundle.hexDecode(groupHex)), messages: messages, countCapped: false)],
            maxHlc: 2)

        let applied = try HistoryBundleCodec.applyToMessageLog(
            accountIdB64: account, secret: secret, sealedBytes: sealed, store: store)

        XCTAssertEqual(applied, 1)
        XCTAssertEqual(store.getLog(accountIdB64: account)?.groups[groupHex]?.messages.map(\.text), ["hello"])
    }

    // MARK: - The invitation card, the reply and the forward travel

    /// Seen live 2026-11-13: an invitation the old device drew as a card
    /// reached the new device as its bare `/g/` link.
    func testCardReplyAndForwardReachTheNewDevice() throws {
        let sender = "history-card-sender-\(UUID().uuidString)"
        let joiner = "history-card-joiner-\(UUID().uuidString)"
        let store = MessageLogStore()
        let cards = ChannelInviteCardStore()
        defer {
            store.deleteLog(accountIdB64: sender)
            store.deleteLog(accountIdB64: joiner)
            cards.delete(accountIdB64: sender)
            cards.delete(accountIdB64: joiner)
        }
        let card = Data(repeating: 0x5A, count: 400)
        let originHex = String(repeating: "42", count: 32)
        let reply = PersistedReply(target: 10, author: "Ann", excerpt: "come along")
        let forward = PersistedForward(author: "Bob", originGroupIdHex: originHex, originMessageId: 77)
        store.putLog(accountIdB64: sender, payload: MessageLogPayload(groups: [
            groupHex: PersistedGroup(cursor: 12, messages: [
                row(1, "outgoing", "https://lakko.me/g/abcdefgh#secret"),
                PersistedMessage(
                    messageId: 2, direction: "incoming", text: "yes", attachments: [],
                    createdAt: "2026-11-13T12:00:02Z", reply: reply),
                PersistedMessage(
                    messageId: 3, direction: "incoming", text: "look", attachments: [],
                    createdAt: "2026-11-13T12:00:03Z", forward: forward),
            ]),
        ]))
        cards.put(card: card, conversationIdHex: groupHex, messageId: 1, accountIdB64: sender)

        let sealed = try XCTUnwrap(HistoryBundleCodec.buildFromMessageLog(
            accountIdB64: sender, secret: secret, store: store, cards: cards))
        let applied = try HistoryBundleCodec.applyToMessageLog(
            accountIdB64: joiner, secret: secret, sealedBytes: sealed, store: store, cards: cards)

        XCTAssertEqual(applied, 3)
        XCTAssertEqual(cards.card(conversationIdHex: groupHex, messageId: 1, accountIdB64: joiner), card)
        XCTAssertNil(cards.card(conversationIdHex: groupHex, messageId: 2, accountIdB64: joiner))
        let rows = try XCTUnwrap(store.getLog(accountIdB64: joiner)?.groups[groupHex]?.messages)
        XCTAssertEqual(rows.first { $0.messageId == 2 }?.reply, reply)
        XCTAssertEqual(rows.first { $0.messageId == 3 }?.forward, forward)
        XCTAssertNil(rows.first { $0.messageId == 1 }?.reply)
    }

    // MARK: - ADR-0091 walk 3 (W3-R3): the author travels

    private func authored(_ id: Int64, _ direction: String, _ author: Data?) -> PersistedMessage {
        PersistedMessage(
            messageId: id, direction: direction, text: "m\(id)", attachments: [],
            createdAt: "2026-11-20T12:00:0\(id)Z", authorKeyB64: author.map { $0.base64EncodedString() })
    }

    /// Seen live 2026-11-20: every row of a gap bundle reached the device
    /// that came back with no author, so none showed a name or an avatar,
    /// and the re-admitter's own messages showed as the receiver's.
    func testAGapBundleRowShowsItsAuthorAndItsDirectionOnTheDeviceThatCameBack() throws {
        let receiver = "history-gap-author-\(UUID().uuidString)"
        let store = MessageLogStore()
        defer { store.deleteLog(accountIdB64: receiver) }
        let admitter = Data(repeating: 0x41, count: 32)
        let member = Data(repeating: 0x42, count: 32)
        let third = Data(repeating: 0x43, count: 32)
        // The re-admitter's log: its own message, the returning member's, a
        // third person's, and a system row.
        let rows = [
            authored(1, "outgoing", admitter),
            authored(2, "incoming", member),
            authored(3, "incoming", third),
            authored(4, "system", nil),
        ]
        let readmitter = try Identity.fromSeed(seed: Data(repeating: 0x44, count: 32))
        let gapKey = generateGapKey()
        let groupId = Data(repeating: 0xCD, count: 32)
        let sealed = try XCTUnwrap(HistoryBundleCodec.buildGapForGroup(
            hex: groupHex, rows: rows, accountIdB64: "sender", readmitter: readmitter,
            gapKey: gapKey.publicKey, groupId: groupId, ownKey: admitter))
        let opened = try openGapHistoryStructured(gapSecret: gapKey.secret, groupId: groupId, sealed: sealed).bundle
        XCTAssertEqual(
            opened.groups.flatMap(\.messages).map { HistoryBundleCodec.decodeOpenedPlaceholder($0.payloadBytes)?.authorKey },
            [admitter, member, third, nil])

        _ = HistoryBundleCodec.applyOpenedGapBundle(
            opened, accountIdB64: receiver, ownKey: member, route: "knock", store: store)

        let back = try XCTUnwrap(store.getLog(accountIdB64: receiver)?.groups[groupHex]?.messages)
        let byId = Dictionary(uniqueKeysWithValues: back.map { ($0.messageId, $0) })
        XCTAssertEqual(byId[1]?.direction, "incoming")
        XCTAssertEqual(byId[1]?.authorKeyB64, admitter.base64EncodedString())
        XCTAssertEqual(byId[2]?.direction, "outgoing", "this person's own message is theirs here")
        XCTAssertEqual(byId[2]?.authorKeyB64, member.base64EncodedString())
        XCTAssertEqual(byId[3]?.direction, "incoming")
        XCTAssertEqual(byId[3]?.authorKeyB64, third.base64EncodedString())
        XCTAssertEqual(byId[4]?.direction, "system")
        XCTAssertNil(byId[4]?.authorKeyB64)
        // The row a chat draws names the author the way a live row does.
        XCTAssertEqual(byId[3]?.toChatMessage()?.authorKeyB64, third.base64EncodedString())
    }

    /// The pairing bundle (the same account): incoming rows keep their
    /// author; directions are as sent.
    func testAPairingBundleRowKeepsItsAuthor() throws {
        let sender = "history-pair-author-sender-\(UUID().uuidString)"
        let joiner = "history-pair-author-joiner-\(UUID().uuidString)"
        let store = MessageLogStore()
        defer {
            store.deleteLog(accountIdB64: sender)
            store.deleteLog(accountIdB64: joiner)
        }
        let own = Data(repeating: 0x51, count: 32)
        let them = Data(repeating: 0x52, count: 32)
        store.putLog(accountIdB64: sender, payload: MessageLogPayload(groups: [
            groupHex: PersistedGroup(cursor: 2, messages: [authored(1, "outgoing", own), authored(2, "incoming", them)]),
        ]))
        let sealed = try XCTUnwrap(HistoryBundleCodec.buildFromMessageLog(
            accountIdB64: sender, secret: secret, store: store))
        _ = try HistoryBundleCodec.applyToMessageLog(
            accountIdB64: joiner, secret: secret, sealedBytes: sealed, store: store)
        let back = try XCTUnwrap(store.getLog(accountIdB64: joiner)?.groups[groupHex]?.messages)
        XCTAssertEqual(back.first { $0.messageId == 2 }?.authorKeyB64, them.base64EncodedString())
        XCTAssertEqual(back.first { $0.messageId == 2 }?.direction, "incoming")
        XCTAssertEqual(back.first { $0.messageId == 1 }?.direction, "outgoing")
    }
}
