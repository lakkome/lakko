// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0063 S4 on iOS — the feed's pure parts: assembling the ranking's
/// input, the seen rule, the channel-post log, saved feeds, and the rule
/// helpers. The ranking itself is the core's; these tests call it through
/// the UniFFI boundary so the input is checked against the real rule.
final class FeedTests: XCTestCase {

    private let now: Int64 = 1_800_000_000_000
    private let groupHex = String(repeating: "ab", count: 32)
    private let channelHex = String(repeating: "cd", count: 16)

    private func msg(
        _ id: Int64,
        _ direction: FeedMessage.Direction = .incoming,
        text: String = "hi",
        ageMs: Int64 = 1000,
        author: String? = "author",
        reactions: [(reactor: String, emoji: String)] = [],
        reply: Int64? = nil,
        deleted: Bool = false
    ) -> FeedMessage {
        FeedMessage(
            messageId: id, direction: direction, text: text, timeMs: now - ageMs,
            authorKeyB64: author, reactions: reactions, replyTarget: reply, deleted: deleted)
    }

    private func context(
        groupMessages: [FeedMessage],
        posts: [PersistedChannelPost] = [],
        ownChannels: Set<String> = [],
        contacts: Set<String> = [],
        username: String? = "alice"
    ) -> FeedContext {
        FeedContext(
            sources: [
                FeedSource(kind: .group, idHex: groupHex, title: "Book club", muted: false, audience: 5),
                FeedSource(kind: .channel, idHex: channelHex, title: "News", muted: false, audience: 0),
            ],
            groupMessages: [groupHex: groupMessages],
            channelPosts: [channelHex: ChannelPostSlot(cursor: posts.last?.cursor ?? 0, posts: posts)],
            ownChannels: ownChannels,
            ownUsername: username,
            contactKeysB64: contacts,
            mentions: { mentionsUsername(text: $0, username: $1) },
            nowMs: now)
    }

    // MARK: - Input

    func testInputLeavesOutSystemDeletedUnsentAndOldRows() {
        let input = FeedModel.buildInput(context(groupMessages: [
            msg(1),
            msg(2, .system),
            msg(3, deleted: true),
            msg(0, .outgoing),
            msg(4, ageMs: FeedModel.maxWindowMs + 1),
        ]))
        XCTAssertEqual(input.candidates.map(\.messageId), [1])
        XCTAssertEqual(input.refs.count, input.candidates.count)
    }

    func testInputFlagsContactMentionReplyAndReactions() {
        let input = FeedModel.buildInput(context(
            groupMessages: [
                msg(1, .outgoing, author: "self"),
                msg(2, text: "ping @alice", author: "friend",
                    reactions: [("a", "x"), ("a", "y"), ("b", "x")], reply: 1),
            ],
            contacts: ["friend"]))
        let c = input.candidates[1]
        XCTAssertTrue(input.candidates[0].fromSelf)
        XCTAssertTrue(c.fromContact)
        XCTAssertTrue(c.mentionsMe)
        XCTAssertTrue(c.repliesToMe)
        XCTAssertEqual(c.replyTo, 1)
        XCTAssertEqual(c.reactionsDistinct, 2)
        XCTAssertEqual(FeedCopy.whyLine(input.refs[1], preset: .latest), "Mentions you")
    }

    func testChannelPostsAreCandidatesAndOwnWhenPublished() {
        let posts = [PersistedChannelPost(cursor: 7, serverTsMs: now - 10, text: "post")]
        let theirs = FeedModel.buildInput(context(groupMessages: [], posts: posts))
        XCTAssertEqual(theirs.candidates.count, 1)
        XCTAssertEqual(theirs.candidates[0].conversation, 1)
        XCTAssertEqual(theirs.candidates[0].messageId, 7)
        XCTAssertFalse(theirs.candidates[0].fromSelf)
        let mine = FeedModel.buildInput(context(groupMessages: [], posts: posts, ownChannels: [channelHex]))
        XCTAssertTrue(mine.candidates[0].fromSelf)
    }

    func testLatestRanksNewestFirstAndFoldsReplies() {
        let input = FeedModel.buildInput(context(
            groupMessages: [msg(1, ageMs: 3000), msg(2, ageMs: 2000, reply: 1), msg(3, ageMs: 1000)],
            posts: [PersistedChannelPost(cursor: 9, serverTsMs: now - 500, text: "post")]))
        let ranked = FeedRanking.rank(
            rule: feedPresetRule(preset: .latest), input: input, nowMs: now)
        XCTAssertEqual(ranked.map(\.ref.message.messageId), [9, 3, 1])
        XCTAssertEqual(ranked[2].replies.map(\.message.messageId), [2])
    }

    func testReactionSummaryOrdersByCountThenFirstSeen() {
        let m = msg(1, reactions: [("a", "x"), ("b", "y"), ("c", "y"), ("d", "z")])
        let summary = FeedModel.reactionSummary(m)
        XCTAssertEqual(summary.map(\.emoji), ["y", "x", "z"])
        XCTAssertEqual(summary.map(\.count), [2, 1, 1])
    }

    // MARK: - Seen

    func testSeenAdvancesOnlyWhenContiguous() {
        let rows = [1, 2, 3, 4].map { FeedSeen.Row(messageId: $0, incoming: true) }
        let contiguous: ([Int64], [Int64]) -> Int64? = { contiguousSeen(ids: $0, seen: $1) }
        let first = FeedSeen.markSeen(
            rows: rows, through: 1, feedSeen: [], ids: [3], contiguous: contiguous)
        XCTAssertNil(first.advancedTo)
        XCTAssertEqual(first.lastViewedMessageId, 1)
        XCTAssertEqual(first.feedSeen, [3])
        let second = FeedSeen.markSeen(
            rows: rows, through: first.lastViewedMessageId, feedSeen: first.feedSeen, ids: [2],
            contiguous: contiguous)
        XCTAssertEqual(second.advancedTo, 3)
        XCTAssertEqual(second.feedSeen, [])
    }

    func testOwnRowsCountAsSeenAndUnknownIdsAreIgnored() {
        let rows = [
            FeedSeen.Row(messageId: 1, incoming: true),
            FeedSeen.Row(messageId: 2, incoming: false),
            FeedSeen.Row(messageId: 3, incoming: true),
        ]
        let r = FeedSeen.markSeen(
            rows: rows, through: nil, feedSeen: [], ids: [1, 99],
            contiguous: { contiguousSeen(ids: $0, seen: $1) })
        XCTAssertEqual(r.advancedTo, 2)
        XCTAssertEqual(r.feedSeen, [])
        XCTAssertFalse(FeedSeen.isSeen(messageId: 3, incoming: true, through: 2, feedSeen: []))
        XCTAssertTrue(FeedSeen.isSeen(messageId: 3, incoming: true, through: 2, feedSeen: [3]))
    }

    func testReadCursorBytesRoundTrip() {
        let bytes = FeedSeen.readCursorBytes(0x0102_0304_0506_0708)
        XCTAssertEqual(Array(bytes), [1, 2, 3, 4, 5, 6, 7, 8])
        XCTAssertEqual(FeedSeen.readCursorId(bytes), 0x0102_0304_0506_0708)
        XCTAssertNil(FeedSeen.readCursorId(Data([1, 2])))
    }

    func testPersistedGroupKeepsSeenStateAndReadsOldRows() throws {
        let old = #"{"cursor":5,"messages":[]}"#.data(using: .utf8)!
        let decoded = try JSONDecoder().decode(PersistedGroup.self, from: old)
        XCTAssertNil(decoded.lastViewedMessageId)
        XCTAssertNil(decoded.feedSeen)
        let slot = PersistedGroup(cursor: 5, messages: [], lastViewedMessageId: 3, feedSeen: [5, 7])
        let round = try JSONDecoder().decode(
            PersistedGroup.self, from: JSONEncoder().encode(slot))
        XCTAssertEqual(round, slot)
    }

    // MARK: - Channel-post log

    func testMergeDedupesTrimsAndNeverMovesCursorBack() {
        let slot = ChannelPostSlot(cursor: 10, posts: [
            PersistedChannelPost(cursor: 1, serverTsMs: now - ChannelPostLog.retentionMs - 1, text: "old"),
            PersistedChannelPost(cursor: 2, serverTsMs: now, text: "a"),
        ])
        let merged = ChannelPostLog.merge(
            slot, fresh: [PersistedChannelPost(cursor: 2, serverTsMs: now, text: "b")],
            cursor: 4, nowMs: now)
        XCTAssertEqual(merged.cursor, 10)
        XCTAssertEqual(merged.posts.map(\.text), ["b"])
        let many = (1...(ChannelPostLog.cap + 5)).map {
            PersistedChannelPost(cursor: UInt64($0), serverTsMs: now, text: "\($0)")
        }
        let capped = ChannelPostLog.merge(nil, fresh: many, cursor: UInt64(many.count), nowMs: now)
        XCTAssertEqual(capped.posts.count, ChannelPostLog.cap)
        XCTAssertEqual(capped.posts.first?.cursor, 6)
    }

    func testSyncStopsAtAPostThatWaitsForItsSeed() async throws {
        let page = ChannelPostLog.Page(posts: [
            (cursor: 1, serverTsMs: now, payloadB64: "text"),
            (cursor: 2, serverTsMs: now, payloadB64: "meta"),
            (cursor: 3, serverTsMs: now, payloadB64: "wait"),
            (cursor: 4, serverTsMs: now, payloadB64: "text"),
        ])
        let slot = try await ChannelPostLog.syncSlot(
            nil,
            fetchPage: { after, _ in after == 0 ? page : ChannelPostLog.Page(posts: []) },
            decode: { payload in
                switch payload {
                case "text": return .text("t")
                case "wait": return .wait
                default: return .skip
                }
            },
            nowMs: now)
        XCTAssertEqual(slot.cursor, 2)
        XCTAssertEqual(slot.posts.map(\.cursor), [1])
    }

    func testPostLogRefusesUnknownVersionAndDropsBadChannelIds() throws {
        let payload = ChannelPostLogPayload(channels: [
            channelHex: ChannelPostSlot(cursor: 1, posts: []),
        ])
        let round = try JSONDecoder().decode(
            ChannelPostLogPayload.self, from: JSONEncoder().encode(payload))
        XCTAssertEqual(round, payload)
        let bad = #"{"version":1,"channels":{"xyz":{"cursor":1,"posts":[]}}}"#.data(using: .utf8)!
        XCTAssertEqual(try JSONDecoder().decode(ChannelPostLogPayload.self, from: bad).channels.count, 0)
        let future = #"{"version":2,"channels":{}}"#.data(using: .utf8)!
        XCTAssertThrowsError(try JSONDecoder().decode(ChannelPostLogPayload.self, from: future))
    }

    // MARK: - Saved feeds and rules

    func testFeedStateCodecKeepsGoodEntriesOnly() throws {
        let rule = try encodeFeedRule(rule: FeedDraft.blank())
        let state = FeedState().upserting(SavedFeed(id: "abcdefgh12", rule: rule))
        let decoded = FeedState.decode(state.encode())
        XCTAssertEqual(decoded, state)
        let mixed = """
        {"version":1,"feeds":[{"id":"short","rule":"AA"},{"id":"abcdefgh12","rule":"\(FeedDraft.base64UrlNoPad(rule))"}]}
        """.data(using: .utf8)!
        XCTAssertEqual(FeedState.decode(mixed).feeds.map(\.id), ["abcdefgh12"])
        XCTAssertEqual(FeedState.decode(Data("nope".utf8)).feeds, [])
    }

    func testFeedStateCapsAtTwenty() {
        var state = FeedState()
        for i in 0..<25 {
            state = state.upserting(SavedFeed(id: "feed-id-\(i)", rule: Data([1])))
        }
        XCTAssertEqual(state.feeds.count, FeedState.maxFeeds)
    }

    func testProblemTextComesFromTheCoreRefusal() {
        var rule = FeedDraft.blank()
        XCTAssertNil(FeedDraft.problem(rule))
        rule.groups = false
        rule.channels = false
        XCTAssertEqual(FeedDraft.problem(rule), "Choose groups, channels or both.")
    }

    func testShareLinkRoundTripsAndStripsConversations() throws {
        var rule = FeedDraft.preset(.popular, name: "Busy")
        rule.include = [Data(repeating: 1, count: 32)]
        let bytes = try FeedDraft.exportBytes(rule)
        let link = FeedDraft.shareLink(origin: "https://lakko.me", bytes: bytes)
        XCTAssertTrue(link.hasPrefix("https://lakko.me/feed/import#"))
        let url = try XCTUnwrap(URL(string: link))
        let back = try XCTUnwrap(FeedDraft.bytes(fromFragment: url.fragment))
        let decoded = try decodeFeedRule(bytes: back)
        XCTAssertTrue(decoded.include.isEmpty)
        XCTAssertTrue(decoded.sourcesStripped)
        XCTAssertEqual(FeedDraft.summary(decoded).first, "Sources: all groups and channels")
        XCTAssertNil(FeedDraft.bytes(fromFragment: "not base64!"))
    }

    func testRefusedFileIsRefusedWhole() {
        XCTAssertThrowsError(try decodeFeedRule(bytes: Data("nope".utf8))) { error in
            XCTAssertEqual(FeedDraft.refusalCode(error), "wrongDomain")
        }
    }

    func testShareFileName() {
        XCTAssertEqual(FeedDraft.shareFileName(FeedDraft.blank(name: "My  feed!")), "My-feed.lakkofeed")
        XCTAssertEqual(FeedDraft.shareFileName(FeedDraft.blank(name: "!!!")), "feed.lakkofeed")
    }

    // MARK: - The item (UX spec section 3)

    private func attachment(_ byte: UInt8, mime: String?, kind: String? = nil) -> AttachmentRef {
        AttachmentRef(
            blobId: Data(repeating: byte, count: 32), key: Data(count: 32), nonce: Data(count: 12),
            mime: mime, size: 1, name: nil, kind: kind)
    }

    private func carrying(_ attachments: [AttachmentRef], text: String = "", labels: [String]? = nil) -> FeedMessage {
        FeedMessage(
            messageId: 1, direction: .incoming, text: text, timeMs: now,
            attachmentLabels: labels ?? attachments.map { _ in "Label" },
            attachments: attachments)
    }

    func testMediaRefsAreWhatTheTimelineDrawsAsABubble() {
        let photo = attachment(1, mime: "image/jpeg")
        let video = attachment(2, mime: "video/mp4")
        let gif = attachment(3, mime: "image/gif", kind: "gif")
        let voice = attachment(4, mime: "audio/mp4", kind: "voice")
        let note = attachment(5, mime: "video/mp4", kind: "video_note")
        let pdf = attachment(6, mime: "application/pdf")
        // The sender chose the kind; the MIME type decides.
        let disguised = attachment(7, mime: "text/html", kind: "voice")
        let unknownKind = attachment(8, mime: "image/png", kind: "hologram")
        let noMime = attachment(9, mime: nil)

        let refs = FeedCopy.mediaRefs(carrying([photo, pdf, video, gif, disguised, voice, note, unknownKind, noMime]))

        XCTAssertEqual(refs, [photo, video, gif, voice, note])
    }

    func testAChannelPostHasNoMedia() {
        let input = FeedModel.buildInput(context(
            groupMessages: [],
            posts: [PersistedChannelPost(cursor: 7, serverTsMs: now - 10, text: "post")]))
        XCTAssertEqual(FeedCopy.mediaRefs(input.refs[0].message), [])
    }

    func testAMediaOnlyItemHasNoTextLine() {
        let photo = attachment(1, mime: "image/jpeg")
        XCTAssertEqual(FeedCopy.itemBody(carrying([photo])), "")
        XCTAssertEqual(FeedCopy.itemBody(carrying([photo], text: "  \n ")), "")
    }

    func testAnItemKeepsItsText() {
        let photo = attachment(1, mime: "image/jpeg")
        XCTAssertEqual(FeedCopy.itemBody(carrying([photo], text: "look")), "look")
    }

    func testAFileStaysALineOfText() {
        let pdf = attachment(1, mime: "application/pdf")
        XCTAssertEqual(FeedCopy.itemBody(carrying([pdf], labels: ["File"])), "File")
    }

    func testAFileBesideAPhotoKeepsItsLine() {
        let refs = [attachment(1, mime: "image/jpeg"), attachment(2, mime: "application/pdf")]
        XCTAssertEqual(FeedCopy.itemBody(carrying(refs, labels: ["Photo", "File"])), "File")
    }

    func testTwoFilesBesideAPhotoAreCounted() {
        let refs = [
            attachment(1, mime: "image/jpeg"), attachment(2, mime: "application/pdf"),
            attachment(3, mime: "application/pdf"),
        ]
        let line = FeedCopy.itemBody(carrying(refs, labels: ["Photo", "File", "File"]))
        XCTAssertEqual(line, Localization.text("feed.attachments.count", counts: ["count": 2]))
    }

    /// A folded reply and the editor's preview draw no bubble, so a media-only
    /// message must still say what it is there.
    func testWhereNoBubbleIsDrawnTheLabelStays() {
        let photo = attachment(1, mime: "image/jpeg")
        XCTAssertEqual(FeedCopy.body(carrying([photo], labels: ["Photo"])), "Photo")
    }

    func testAMessageWhoseAttachmentsChangedIsADifferentMessage() {
        let photo = carrying([attachment(1, mime: "image/jpeg")])
        let other = carrying([attachment(2, mime: "image/jpeg")])
        XCTAssertNotEqual(photo, other)
        XCTAssertEqual(photo, carrying([attachment(1, mime: "image/jpeg")]))
    }

    func testTheAvatarIdentityIsTheConversationNotTheGroup() {
        let moved = FeedSource(
            kind: .group, idHex: groupHex, title: "T", muted: false, audience: 2,
            conversationHex: "root")
        XCTAssertEqual(moved.key, "root")
        let channel = FeedSource(kind: .channel, idHex: channelHex, title: "C", muted: false, audience: 0)
        XCTAssertEqual(channel.key, channelHex)
    }

    func testTheColumnIsWhatTheCardLeavesBesideTheAvatar() {
        // 390 - 2 x 16 list - 2 x 12 card - 40 avatar - 12 gap.
        XCTAssertEqual(FeedLayout.columnWidth(viewport: 390), 282)
        // The content width stops at 720.
        XCTAssertEqual(FeedLayout.columnWidth(viewport: 1366), 644)
        XCTAssertEqual(FeedLayout.columnWidth(viewport: 0), 0)
    }

    func testSelectionStorageValues() {
        XCTAssertEqual(FeedSelection(storageValue: "for-me"), .preset(.forMe))
        XCTAssertEqual(FeedSelection(storageValue: "custom:abc"), .saved("abc"))
        XCTAssertEqual(FeedSelection(storageValue: nil), .preset(.latest))
        XCTAssertEqual(FeedSelection.saved("abc").storageValue, "custom:abc")
    }

    @MainActor
    func testFeedImportLinkIsParked() throws {
        let intake = InviteIntake()
        let bytes = try FeedDraft.exportBytes(FeedDraft.blank())
        intake.handle(try XCTUnwrap(URL(string: FeedDraft.shareLink(origin: "https://lakko.me", bytes: bytes))))
        let request = try XCTUnwrap(intake.takeFeedImport())
        XCTAssertEqual(request.bytes, bytes)
        XCTAssertNil(intake.takeFeedImport())
        intake.handle(try XCTUnwrap(URL(string: "https://lakko.me/feed/import#@@")))
        XCTAssertEqual(intake.takeFeedImport()?.refused, true)
    }

    // MARK: - ADR-0090 — channel posts keep their files and content

    private func photo() throws -> AttachmentRef {
        AttachmentRef(
            blobId: Data(repeating: 1, count: 32), key: Data(count: 32), nonce: Data(count: 12),
            mime: "image/jpeg", size: 10, name: nil, kind: nil, durationMs: nil, waveform: nil,
            width: nil, height: nil, thumb: nil)
    }

    func testAnOldPostRowReadsWithNoFilesAndNoContent() throws {
        let old = Data(#"{"cursor":3,"serverTsMs":5,"text":"hi"}"#.utf8)
        let post = try JSONDecoder().decode(PersistedChannelPost.self, from: old)
        XCTAssertNil(post.attachments)
        XCTAssertNil(post.content)
        XCTAssertEqual(post.text, "hi")
    }

    func testSyncKeepsAPostsFilesAndContent() async throws {
        let ref = try photo()
        let card = SharedContactCard(name: "Anna", username: "anna", accountKey: Data(repeating: 9, count: 32))
        let envelope = try composeContentEnvelope(
            content: .contactCard(card: card), attachments: [], parts: ContentModel.emptyParts).base64EncodedString()
        let page = ChannelPostLog.Page(posts: [
            (cursor: 1, serverTsMs: now, payloadB64: "photo"),
            (cursor: 2, serverTsMs: now, payloadB64: "card"),
        ])
        let slot = try await ChannelPostLog.syncSlot(
            nil,
            fetchPage: { after, _ in after == 0 ? page : ChannelPostLog.Page(posts: []) },
            decode: { payload in
                payload == "photo"
                    ? .post(text: "", attachments: [ref], contentEnvelopeB64: nil)
                    : .post(text: "Contact", attachments: [], contentEnvelopeB64: envelope)
            },
            nowMs: now)
        let round = try JSONDecoder().decode(ChannelPostSlot.self, from: JSONEncoder().encode(slot))
        XCTAssertEqual(round.posts[0].attachmentRefs, [ref])
        XCTAssertNil(round.posts[0].content)
        XCTAssertEqual(round.posts[1].content?.envelopeB64, envelope)

        let input = FeedModel.buildInput(context(groupMessages: [], posts: round.posts))
        let shown = input.refs.map(\.message)
        XCTAssertEqual(shown.first?.attachments, [ref], "the feed draws the post's file")
        XCTAssertTrue(input.candidates.first?.hasMedia == true)
        XCTAssertEqual(shown.last?.content?.envelopeB64, envelope)
    }

    func testForwardFromTheFeedFollowsTheContentRules() throws {
        let card = SharedContactCard(name: "Anna", username: "anna", accountKey: Data(repeating: 9, count: 32))
        let cardEnvelope = try composeContentEnvelope(
            content: .contactCard(card: card), attachments: [], parts: ContentModel.emptyParts)
        let carded = FeedMessage(
            messageId: 1, direction: .incoming, text: "Contact", timeMs: now,
            content: PersistedContent(envelopeB64: cardEnvelope.base64EncodedString()))
        XCTAssertTrue(FeedForward.canForward(carded) { _ in true })
        XCTAssertFalse(FeedForward.canForward(carded) { _ in false })
        let body = ChecklistBody(title: "T", othersAdd: false, othersMark: false, items: [ChecklistItem(id: 1, text: "a")])
        let listEnvelope = try composeContentEnvelope(
            content: .checklist(checklist: SignedChecklist(body: body, authorSig: nil)), attachments: [],
            parts: ContentModel.emptyParts)
        let listed = FeedMessage(
            messageId: 2, direction: .incoming, text: "", timeMs: now,
            content: PersistedContent(envelopeB64: listEnvelope.base64EncodedString()))
        XCTAssertFalse(FeedForward.canForward(listed) { _ in true })
        XCTAssertTrue(FeedForward.canForward(msg(3)) { _ in false })
        XCTAssertFalse(FeedCopy.body(listed).isEmpty, "a content message with no text says what it is")
    }
}

