// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0073 R.13b - the alignment pass of the silent-continuity model: the
/// pure decisions it added on iOS (items C to G and the iOS half of H).
final class SilentContinuityAlignmentTests: XCTestCase {

    // MARK: - C: nothing from a superseded sender acts (the app's receive paths)

    func testEveryApplicationRowFromASupersededSenderIsDropped() {
        // One batch: a chat row, an op, a service envelope and a call offer
        // from a retired key; the same four from a live key; and two MLS
        // handshake rows, which have no application sender.
        let rows = [
            "chat-old", "op-old", "envelope-old", "call-old",
            "chat-live", "op-live", "envelope-live", "call-live",
            "commit", "proposal",
        ]
        let senders: [String?] = [
            "old", "old", "old", "old",
            "live", "live", "live", "live",
            nil, nil,
        ]
        XCTAssertEqual(
            ResetRepair.keptAfterSupersededSenders(rows, senders: senders, supersededKeys: ["old"]),
            ["chat-live", "op-live", "envelope-live", "call-live", "commit", "proposal"])
    }

    func testWithNoRetiredKeyNothingIsDropped() {
        let rows = ["a", "b"]
        XCTAssertEqual(
            ResetRepair.keptAfterSupersededSenders(rows, senders: ["old", nil], supersededKeys: []), rows)
    }

    func testARowPastTheEndOfTheSenderListIsKept() {
        // A producer that resolved no senders drops nothing by sender.
        XCTAssertEqual(
            ResetRepair.keptAfterSupersededSenders(["a", "b"], senders: [], supersededKeys: ["old"]),
            ["a", "b"])
        XCTAssertEqual(
            ResetRepair.keptAfterSupersededSenders(["a", "b"], senders: ["old"], supersededKeys: ["old"]),
            ["b"])
    }

    func testAProvenPredecessorIsNotASupersededKey() {
        // The set is the keys accepted RESETS retired; a proven predecessor
        // of a current key is the same person and is never in it.
        XCTAssertFalse(ResetRepair.dropsSupersededSender("predecessor", supersededKeys: ["retired"]))
        XCTAssertTrue(ResetRepair.dropsSupersededSender("retired", supersededKeys: ["retired"]))
        XCTAssertFalse(ResetRepair.dropsSupersededSender(nil, supersededKeys: ["retired"]))
    }

    // MARK: - C / E: the background reader

    private let groupHex = String(repeating: "a", count: 64)
    private let rootHex = String(repeating: "c", count: 64)
    private let pushGidHex = String(repeating: "b", count: 64)
    private let account = "acct=="

    private final class Group: NseThrowawayGroup {
        let decode: (Data) -> NseProcessedRow?
        init(_ decode: @escaping (Data) -> NseProcessedRow?) { self.decode = decode }
        func process(_ payload: Data) -> NseProcessedRow? { decode(payload) }
        func dsProofHeader(method: String, path: String, query: String, body: Data) -> String? { "v1.fake" }
        func close() {}
    }

    private final class Deps: NsePushDecryptDeps {
        var group: Group?
        var rows: [NseFetchedRow] = []
        var superseded: Set<String> = []
        /// `(origin group b64, origin message id, sender)` the app holds.
        var held: Set<String> = []
        var conversation: [String: String] = [:]
        var route: [String: String] = [:]
        var known: Set<String> = []
        var bound: String? = "acct=="
        var asked: [String] = []

        func boundAccount(wakeSubscriptionIdHex: String?) -> String? { bound }
        func lookupRoute(accountIdB64: String, pushGidHex: String, subscriptionIdHex: String?) -> String? {
            route[pushGidHex] ?? (known.contains(pushGidHex) ? pushGidHex : nil)
        }
        func accountKnowsGroup(accountIdB64: String, groupHex: String) -> Bool { known.contains(groupHex) }
        func loadIdentitySeed(accountIdB64: String) -> Data? { Data(repeating: 1, count: 32) }
        func loadSnapshot(accountIdB64: String) -> Data? { Data(repeating: 2, count: 8) }
        func openThrowawayGroup(seed: Data, snapshot: Data, groupIdBytes: Data) throws -> NseThrowawayGroup? {
            group
        }
        func dsFetch(groupIdBytes: Data, afterId: Int64, group: NseThrowawayGroup) async throws -> [NseFetchedRow] {
            rows
        }
        func startCursor(accountIdB64: String, groupHex: String) -> Int64? { 0 }
        func isAntiCensorshipArmed() -> Bool { false }
        func conversationHex(accountIdB64: String, groupIdHex: String) -> String {
            conversation[groupIdHex] ?? groupIdHex
        }
        func isSupersededSender(accountIdB64: String, senderKeyB64: String?) -> Bool {
            senderKeyB64.map { superseded.contains($0) } ?? false
        }
        func holdsResendOriginal(
            accountIdB64: String, receivedGroupIdHex: String,
            origin: NseResendOrigin, senderKeyB64: String?
        ) -> Bool {
            asked.append("\(receivedGroupIdHex):\(origin.groupIdB64):\(origin.messageId)")
            return held.contains("\(origin.groupIdB64):\(origin.messageId):\(senderKeyB64 ?? "-")")
        }
    }

    private func deps(_ rows: [(UInt8, NseProcessedRow?)]) -> Deps {
        let table = Dictionary(uniqueKeysWithValues: rows.map { ($0.0, $0.1) })
        let deps = Deps()
        deps.known = [groupHex]
        deps.group = Group { payload in table[payload[payload.startIndex]] ?? nil }
        deps.rows = rows.enumerated().map {
            NseFetchedRow(id: Int64($0.offset + 1), payload: Data([$0.element.0]))
        }
        return deps
    }

    private func chat(_ text: String, sender: String?, resend: NseResendOrigin? = nil) -> NseProcessedRow {
        NseProcessedRow(kind: .chat, text: text, attachmentCount: 0, senderKeyB64: sender, resendOrigin: resend)
    }

    private var wake: NsePushWake { NsePushWake(groupHex: groupHex, pointer: 1, subscriptionIdHex: "dd") }

    func testAChatRowFromASupersededSenderIsNotPreviewed() async {
        let deps = deps([(1, chat("from the old key", sender: "old"))])
        deps.superseded = ["old"]
        let outcome = await decryptPushOutcome(wake, deps)
        XCTAssertEqual(outcome, .quiet(tag: groupHex))
    }

    func testACallOfferFromASupersededSenderDoesNotRing() async {
        let offer = NseProcessedRow(
            kind: .callOffer(video: false), text: "", attachmentCount: 0,
            callIdHex: String(repeating: "1", count: 32), senderKeyB64: "old")
        let deps = deps([(1, offer)])
        deps.superseded = ["old"]
        let outcome = await decryptPushOutcome(wake, deps)
        XCTAssertEqual(outcome, .quiet(tag: groupHex), "a call from a retired key never rings")
    }

    func testALiveSendersRowBesideADroppedOneIsStillPreviewed() async {
        let deps = deps([
            (1, chat("hello", sender: "live")),
            (2, chat("from the old key", sender: "old")),
        ])
        deps.superseded = ["old"]
        guard case .preview(let preview) = await decryptPushOutcome(wake, deps) else {
            return XCTFail("the live sender's message is previewed")
        }
        XCTAssertEqual(preview.body, "hello")
    }

    func testAResendWhoseOriginalIsHeldPostsNoPreview() async {
        let origin = NseResendOrigin(groupIdB64: "b2xk", messageId: 7)
        let deps = deps([(1, chat("sent before", sender: "own", resend: origin))])
        deps.held = ["b2xk:7:own"]
        let outcome = await decryptPushOutcome(wake, deps)
        XCTAssertEqual(outcome, .quiet(tag: groupHex))
        XCTAssertEqual(deps.asked, ["\(groupHex):b2xk:7"])
    }

    func testAResendWhoseOriginalIsNotHeldIsAnOrdinaryMessage() async {
        let origin = NseResendOrigin(groupIdB64: "b2xk", messageId: 7)
        let deps = deps([(1, chat("sent before", sender: "own", resend: origin))])
        guard case .preview(let preview) = await decryptPushOutcome(wake, deps) else {
            return XCTFail("a marker that matches nothing changes nothing")
        }
        XCTAssertEqual(preview.body, "sent before")
    }

    func testOnlyAChatRowCarriesAResendMarker() {
        let origin = NseResendOrigin(groupIdB64: "b2xk", messageId: 7)
        let edit = NseProcessedRow(kind: .edit, text: "x", attachmentCount: 0, resendOrigin: origin)
        XCTAssertNil(edit.resendOrigin)
        let deps = Deps()
        deps.held = ["b2xk:7:-"]
        XCTAssertFalse(nseDropsSilently(edit, accountIdB64: account, receivedGroupIdHex: groupHex, deps: deps))
    }

    func testABatchWithNothingReadableStaysTheGenericLine() async {
        // Not `.quiet`: an unread message may be behind a row that did not
        // decrypt.
        let deps = deps([(1, nil)])
        deps.superseded = ["old"]
        let outcome = await decryptPushOutcome(wake, deps)
        XCTAssertEqual(outcome, NsePushOutcome.none)
    }

    func testTheDefaultDependencyObjectDropsNothing() {
        final class Plain: NsePushDecryptDeps {
            func boundAccount(wakeSubscriptionIdHex: String?) -> String? { nil }
            func lookupRoute(accountIdB64: String, pushGidHex: String, subscriptionIdHex: String?) -> String? { nil }
            func accountKnowsGroup(accountIdB64: String, groupHex: String) -> Bool { false }
            func loadIdentitySeed(accountIdB64: String) -> Data? { nil }
            func loadSnapshot(accountIdB64: String) -> Data? { nil }
            func openThrowawayGroup(seed: Data, snapshot: Data, groupIdBytes: Data) throws -> NseThrowawayGroup? { nil }
            func dsFetch(groupIdBytes: Data, afterId: Int64, group: NseThrowawayGroup) async throws -> [NseFetchedRow] { [] }
            func startCursor(accountIdB64: String, groupHex: String) -> Int64? { nil }
            func isAntiCensorshipArmed() -> Bool { false }
        }
        let row = chat("x", sender: "old", resend: NseResendOrigin(groupIdB64: "b2xk", messageId: 1))
        XCTAssertFalse(nseDropsSilently(row, accountIdB64: account, receivedGroupIdHex: groupHex, deps: Plain()))
    }

    // MARK: - H (iOS): one notification stack per conversation

    func testTheStackIsTheConversationBeforeAndAfterTheMove() {
        let deps = Deps()
        // Before the move: the wake's routing id names the OLD group, which
        // is the conversation's root.
        deps.route = [pushGidHex: rootHex]
        let before = nseConversationThread(
            NsePushWake(groupHex: pushGidHex, pointer: 1, subscriptionIdHex: "dd"), deps)
        // After the move: another routing id, naming the successor, which
        // the lineage mirror places in the same conversation.
        let successorGid = String(repeating: "e", count: 64)
        deps.route = [successorGid: groupHex]
        deps.conversation = [groupHex: rootHex]
        let after = nseConversationThread(
            NsePushWake(groupHex: successorGid, pointer: 1, subscriptionIdHex: "dd"), deps)
        XCTAssertEqual(before, rootHex)
        XCTAssertEqual(after, rootHex, "a successor's notifications stack with those delivered before the move")
    }

    func testARotatedRoutingIdKeepsTheStack() {
        let deps = Deps()
        let first = String(repeating: "1", count: 64)
        let second = String(repeating: "2", count: 64)
        deps.route = [first: groupHex, second: groupHex]
        XCTAssertEqual(
            nseConversationThread(NsePushWake(groupHex: first, pointer: 1, subscriptionIdHex: "dd"), deps),
            nseConversationThread(NsePushWake(groupHex: second, pointer: 1, subscriptionIdHex: "dd"), deps))
    }

    func testAWakeThatCannotBeRoutedKeepsItsOwnGroupingKey() {
        let deps = Deps()
        XCTAssertNil(nseConversationThread(
            NsePushWake(groupHex: pushGidHex, pointer: 1, subscriptionIdHex: "dd"), deps))
        deps.bound = nil
        deps.known = [pushGidHex]
        XCTAssertNil(nseConversationThread(
            NsePushWake(groupHex: pushGidHex, pointer: 1, subscriptionIdHex: "dd"), deps),
            "no binding: nothing is read")
        XCTAssertNil(nseConversationThread(nil, deps))
        XCTAssertNil(nseConversationThread(
            NsePushWake(groupHex: "not-hex", pointer: 1, subscriptionIdHex: "dd"), deps))
    }

    func testALegacyWakeNamingTheGroupItselfIsGroupedByItsConversation() {
        let deps = Deps()
        deps.known = [groupHex]
        deps.conversation = [groupHex: rootHex]
        XCTAssertEqual(
            nseConversationThread(NsePushWake(groupHex: groupHex, pointer: 1, subscriptionIdHex: "dd"), deps),
            rootHex)
    }

    func testAPreviewCarriesTheConversationTag() async {
        let deps = deps([(1, chat("hello", sender: "live"))])
        deps.conversation = [groupHex: rootHex]
        guard case .preview(let preview) = await decryptPushOutcome(wake, deps) else {
            return XCTFail("a preview")
        }
        XCTAssertEqual(preview.tag, rootHex)
    }

    // MARK: - The mirror the extension reads

    private func suite() -> UserDefaults {
        UserDefaults(suiteName: "continuity-mirror-\(UUID().uuidString)")!
    }

    func testTheContinuityMirrorRoundTripsPerAccount() {
        let defaults = suite()
        let row = ContinuityMirror.Row(
            supersededKeysB64: ["zz", "aa"], ownPersonKeyB64: "own",
            segmentIndexOfGroupHex: ["ab": 1, "cd": 2])
        ContinuityMirror.replace(accountIdB64: "a", row: row, defaults: defaults)
        XCTAssertEqual(ContinuityMirror.read(accountIdB64: "a", defaults: defaults), row)
        XCTAssertEqual(ContinuityMirror.read(accountIdB64: "a", defaults: defaults).supersededKeysB64, ["aa", "zz"])
        XCTAssertEqual(ContinuityMirror.read(accountIdB64: "b", defaults: defaults), .empty)
        ContinuityMirror.clear(accountIdB64: "a", defaults: defaults)
        XCTAssertEqual(ContinuityMirror.read(accountIdB64: "a", defaults: defaults), .empty)
    }

    func testAMissingOrUnreadableMirrorDropsNothing() {
        let defaults = suite()
        XCTAssertEqual(ContinuityMirror.read(accountIdB64: nil, defaults: defaults), .empty)
        XCTAssertEqual(ContinuityMirror.read(accountIdB64: "a", defaults: nil), .empty)
        defaults.set(Data("junk".utf8), forKey: "me.lakko.continuity-mirror.v1.a")
        let row = ContinuityMirror.read(accountIdB64: "a", defaults: defaults)
        XCTAssertEqual(row, .empty)
        XCTAssertFalse(ContinuityMirror.isSuperseded("old", in: row))
        XCTAssertFalse(ContinuityMirror.isSuperseded(nil, in: ContinuityMirror.Row(supersededKeysB64: ["old"])))
        XCTAssertTrue(ContinuityMirror.isSuperseded("old", in: ContinuityMirror.Row(supersededKeysB64: ["old"])))
    }

    func testTheReaderDecidesAResendWithTheAppsOwnRule() {
        // What `NseRealDecryptDeps.holdsResendOriginal` assembles: the
        // message log's rows, the lineage mirror's conversation and the
        // continuity mirror's order and own key, through the SAME predicate
        // the app's receive paths call.
        let oldHex = String(repeating: "0", count: 62) + "01"
        let newHex = String(repeating: "0", count: 62) + "02"
        let oldB64 = Data(repeating: 0, count: 31).appending(1).base64EncodedString()
        func held(sender: String?, own: String?, index: [String: Int], rows: [ResendMarker.HeldRow]) -> Bool {
            let received = ResendMarker.Received(
                resendOrigin: ResendMarker.Origin(groupIdB64: oldB64, messageId: 7), senderKeyB64: sender)
            return ResendMarker.heldResendPredicate(
                batch: [received],
                port: ResendMarker.ReceivePort(
                    receivedGroupIdHex: newHex,
                    conversationOf: { $0 == newHex ? oldHex : $0 },
                    segmentIndexOf: { index[$0] ?? 0 },
                    ownPersonKeyB64: own,
                    canonical: { $0 },
                    loadLog: { ResendMarker.HeldLog(rowsByGroupHex: [oldHex: rows]) }))(received)
        }
        let sent = [ResendMarker.HeldRow(messageId: 7, direction: .outgoing)]
        XCTAssertTrue(held(sender: "own", own: "own", index: [newHex: 1], rows: sent))
        XCTAssertFalse(held(sender: "own", own: "own", index: [:], rows: sent), "no order mirrored: not an older segment")
        XCTAssertFalse(held(sender: "other", own: "own", index: [newHex: 1], rows: sent))
        XCTAssertFalse(held(sender: "own", own: nil, index: [newHex: 1], rows: sent), "no own key mirrored")
        XCTAssertFalse(held(sender: "own", own: "own", index: [newHex: 1], rows: []))
    }

    func testQuietNotificationsAreFoundByTheirMark() {
        let delivered: [(identifier: String, userInfo: [AnyHashable: Any])] = [
            ("a", [LakkoQuietNotification.userInfoKey: true]),
            ("b", ["g": "x"]),
            ("c", [LakkoQuietNotification.userInfoKey: false]),
            ("d", [LakkoQuietNotification.userInfoKey: "true"]),
        ]
        XCTAssertEqual(QuietNotifications.quietIdentifiers(delivered), ["a"])
    }

    // MARK: - F: the founder's re-send is resumable and faithful

    private func own(_ id: Int64, at ms: Int64 = 2_000) -> ResetRepair.OldSegmentMessage {
        ResetRepair.OldSegmentMessage(messageId: id, own: true, sentAtMs: ms, resendable: true)
    }

    func testAnInterruptedResendContinuesAfterTheLastMessageThatLanded() {
        let messages = [own(3), own(5), own(9), own(12)]
        XCTAssertEqual(ResetRepair.messagesToResend(messages, resetSignedAtMs: 1_000), [3, 5, 9, 12])
        XCTAssertEqual(
            ResetRepair.messagesToResend(messages, resetSignedAtMs: 1_000, afterMessageId: 5), [9, 12],
            "never from the first message again")
        XCTAssertEqual(ResetRepair.messagesToResend(messages, resetSignedAtMs: 1_000, afterMessageId: 12), [])
        XCTAssertEqual(
            ResetRepair.messagesToResend(messages, resetSignedAtMs: 1_000, afterMessageId: nil), [3, 5, 9, 12])
    }

    func testTheResendCursorIsDurableAndNeverMovesBackward() {
        let defaults = UserDefaults(suiteName: "resend-cursor-\(UUID().uuidString)")!
        let store = ContinuityStore(defaults: defaults, ownerAccountIdB64: "acct")
        XCTAssertNil(store.resendCursor(oldGroupIdB64: "G"))
        store.advanceResendCursor(oldGroupIdB64: "G", to: 5)
        store.advanceResendCursor(oldGroupIdB64: "G", to: 3)
        store.advanceResendCursor(oldGroupIdB64: "G", to: 0)
        // A relaunch: another instance over the same storage.
        let again = ContinuityStore(defaults: defaults, ownerAccountIdB64: "acct")
        XCTAssertEqual(again.resendCursor(oldGroupIdB64: "G"), 5)
        XCTAssertNil(again.resendCursor(oldGroupIdB64: "H"), "one cursor per conversation")
        XCTAssertNil(ContinuityStore(defaults: defaults, ownerAccountIdB64: "other").resendCursor(oldGroupIdB64: "G"))
        again.advanceResendCursor(oldGroupIdB64: "G", to: 9)
        XCTAssertEqual(store.resendCursor(oldGroupIdB64: "G"), 9)
        // The other marks of the row are untouched by it.
        XCTAssertFalse(store.wasResent(oldGroupIdB64: "G"))
        store.markResent(oldGroupIdB64: "G")
        XCTAssertEqual(store.resendCursor(oldGroupIdB64: "G"), 9)
    }

    func testAResentReplyKeepsItsQuoteWithNoJumpTarget() {
        let parts = ResetRepair.resendParts(
            replyAuthor: "Ada", replyExcerpt: "the quoted words", isReply: true,
            forwardAuthor: nil, forwardOriginGroupB64: nil, forwardOriginMessageId: nil, isForward: false,
            originGroupB64: "T0xE", originMessageId: 7)
        XCTAssertEqual(parts.replyTo, 0)
        XCTAssertEqual(parts.replyTo, HeldSend.replyNoJumpTarget)
        XCTAssertEqual(parts.replyAuthor, "Ada")
        XCTAssertEqual(parts.replyExcerpt, "the quoted words")
        XCTAssertNil(parts.forwardedFrom)
        XCTAssertEqual(parts.resendOriginGroupB64, "T0xE")
        XCTAssertEqual(parts.resendOriginMessageId, 7)
    }

    func testAResentForwardKeepsItsAttributionAndOrigin() {
        let parts = ResetRepair.resendParts(
            replyAuthor: nil, replyExcerpt: nil, isReply: false,
            forwardAuthor: "Grace", forwardOriginGroupB64: "T1JJ", forwardOriginMessageId: 42, isForward: true,
            originGroupB64: "T0xE", originMessageId: 8)
        XCTAssertEqual(parts.forwardedFrom, "Grace")
        XCTAssertEqual(parts.forwardOriginGroupB64, "T1JJ")
        XCTAssertEqual(parts.forwardOriginMessageId, 42)
        XCTAssertNil(parts.replyTo)
    }

    func testAForwardsOriginIsAWholePairOrAbsent() {
        let half = ResetRepair.resendParts(
            replyAuthor: nil, replyExcerpt: nil, isReply: false,
            forwardAuthor: "Grace", forwardOriginGroupB64: "T1JJ", forwardOriginMessageId: nil, isForward: true,
            originGroupB64: "T0xE", originMessageId: 8)
        XCTAssertEqual(half.forwardedFrom, "Grace")
        XCTAssertNil(half.forwardOriginGroupB64)
        XCTAssertNil(half.forwardOriginMessageId)
    }

    func testAPlainMessageAndARowHoldingBoth() {
        let plain = ResetRepair.resendParts(
            replyAuthor: "x", replyExcerpt: "y", isReply: false,
            forwardAuthor: "z", forwardOriginGroupB64: "g", forwardOriginMessageId: 1, isForward: false,
            originGroupB64: "T0xE", originMessageId: 9)
        XCTAssertNil(plain.replyTo)
        XCTAssertNil(plain.replyAuthor)
        XCTAssertNil(plain.forwardedFrom)
        XCTAssertNil(plain.forwardOriginGroupB64)
        // A forward never carries a quote: a row holding both is the forward.
        let both = ResetRepair.resendParts(
            replyAuthor: "x", replyExcerpt: "y", isReply: true,
            forwardAuthor: "z", forwardOriginGroupB64: "g", forwardOriginMessageId: 1, isForward: true,
            originGroupB64: "T0xE", originMessageId: 9)
        XCTAssertNil(both.replyTo)
        XCTAssertEqual(both.forwardedFrom, "z")
    }

    func testAResentEnvelopeCarriesTheQuoteTheAttributionAndTheMarker() throws {
        // Through the ONE composer and the ONE parser of the wire.
        let reply = ResetRepair.resendParts(
            replyAuthor: "Ada", replyExcerpt: "the quoted words", isReply: true,
            forwardAuthor: nil, forwardOriginGroupB64: nil, forwardOriginMessageId: nil, isForward: false,
            originGroupB64: Data(repeating: 1, count: 32).base64EncodedString(), originMessageId: 7)
        let parsedReply = AttachmentBridge.parseBodyEnvelopeBytes(try LakkoCore.composeChatEnvelope(
            text: "an answer", attachments: [],
            parts: ChatEnvelopeParts(
                replyTo: reply.replyTo, replyAuthor: reply.replyAuthor, replyExcerpt: reply.replyExcerpt,
                resendOriginGroupB64: reply.resendOriginGroupB64,
                resendOriginMessageId: reply.resendOriginMessageId)))
        XCTAssertEqual(parsedReply.kind, .chat)
        XCTAssertEqual(parsedReply.text, "an answer")
        XCTAssertEqual(parsedReply.replyTo, 0)
        XCTAssertEqual(parsedReply.replyAuthor, "Ada")
        XCTAssertEqual(parsedReply.replyExcerpt, "the quoted words")
        XCTAssertEqual(parsedReply.resendOriginMessageId, 7)

        let originGroup = Data(repeating: 2, count: 32).base64EncodedString()
        let forward = ResetRepair.resendParts(
            replyAuthor: nil, replyExcerpt: nil, isReply: false,
            forwardAuthor: "Grace", forwardOriginGroupB64: originGroup, forwardOriginMessageId: 42,
            isForward: true,
            originGroupB64: Data(repeating: 1, count: 32).base64EncodedString(), originMessageId: 8)
        let parsedForward = AttachmentBridge.parseBodyEnvelopeBytes(try LakkoCore.composeChatEnvelope(
            text: "passed on", attachments: [],
            parts: ChatEnvelopeParts(
                forwardedFrom: forward.forwardedFrom,
                forwardOriginGroupB64: forward.forwardOriginGroupB64,
                forwardOriginMessageId: forward.forwardOriginMessageId,
                resendOriginGroupB64: forward.resendOriginGroupB64,
                resendOriginMessageId: forward.resendOriginMessageId)))
        XCTAssertEqual(parsedForward.forwardedFrom, "Grace")
        XCTAssertEqual(parsedForward.forwardOriginGroupB64, originGroup)
        XCTAssertEqual(parsedForward.forwardOriginMessageId, 42)
        XCTAssertEqual(parsedForward.resendOriginMessageId, 8)
    }

    // MARK: - D: an /i/ link from an established contact opens that contact

    @MainActor
    func testAKnownContactOpensWithNoResultScreen() {
        typealias Model = InviteLandingViewModel
        XCTAssertNil(Model.contactToOpen(.running))
        XCTAssertFalse(Model.showsResult(.running))

        let known = Model.State.done(.knownContact(contactMessageId: 41))
        XCTAssertEqual(Model.contactToOpen(known), 41)
        XCTAssertFalse(Model.showsResult(known), "no intermediate result screen")

        for result: AcceptInviteResult in [
            .alreadyAccepted, .expired, .exhausted, .notFound, .damaged, .rateLimited,
            .accepted(recipientAccountId: Data(), inviterFingerprint: "f", groupIdHex: nil),
        ] {
            XCTAssertNil(Model.contactToOpen(.done(result)))
            XCTAssertTrue(Model.showsResult(.done(result)))
        }
    }
}

private extension Data {
    func appending(_ byte: UInt8) -> Data {
        var out = self
        out.append(byte)
        return out
    }
}
