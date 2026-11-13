// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0087 sections 12 and 13 — a channel post is the chat envelope, and the
/// timeline folds the ADR-0023 edit and delete ops by the post's cursor.
final class ChannelConversationTests: XCTestCase {

    private func post(_ cursor: UInt64, _ body: ChannelPostBody, epoch: UInt32 = 1) -> ChannelDecodedPost {
        ChannelDecodedPost(cursor: cursor, serverTsMs: 1_000 * cursor, epoch: epoch, state: .body(body))
    }

    func testAPostIsTheChatEnvelope() throws {
        let envelope = try composeEnvelope(text: "hello", attachments: [])
        let read = ChannelPostBody.read(String(decoding: envelope, as: UTF8.self))
        XCTAssertEqual(read, .chat(text: "hello", attachments: [], forward: nil))
        XCTAssertEqual(read.feedText, "hello")
    }

    func testAPostWrittenBeforeTheEnvelopeReadsAsText() {
        XCTAssertEqual(ChannelPostBody.read("plain words"), .chat(text: "plain words", attachments: [], forward: nil))
    }

    func testEditAndDeleteNameTheirTargetCursor() throws {
        let edit = try composeEditEnvelope(target: 7, newText: "fixed")
        XCTAssertEqual(ChannelPostBody.read(String(decoding: edit, as: UTF8.self)), .edit(target: 7, text: "fixed"))
        let delete = try composeDeleteEnvelope(target: 7)
        XCTAssertEqual(ChannelPostBody.read(String(decoding: delete, as: UTF8.self)), .delete(target: 7))
        XCTAssertNil(ChannelPostBody.edit(target: 7, text: "x").feedText)
    }

    func testFoldAppliesTheEditAndMarksIt() {
        let folded = ChannelTimeline.fold([
            post(1, .chat(text: "first", attachments: [], forward: nil)),
            post(2, .edit(target: 1, text: "first, fixed")),
        ])
        XCTAssertEqual(folded.items.count, 1)
        XCTAssertEqual(folded.items[0].text, "first, fixed")
        XCTAssertTrue(folded.items[0].edited)
        XCTAssertFalse(folded.earlierUnavailable)
    }

    func testTheLatestEditWinsWhateverOrderThePostsArrive() {
        let posts = [
            post(3, .edit(target: 1, text: "second edit")),
            post(1, .chat(text: "orig", attachments: [], forward: nil)),
            post(2, .edit(target: 1, text: "first edit")),
        ]
        XCTAssertEqual(ChannelTimeline.fold(posts).items[0].text, "second edit")
    }

    func testADeletedPostLeavesTheTimeline() {
        let folded = ChannelTimeline.fold([
            post(1, .chat(text: "a", attachments: [], forward: nil)),
            post(2, .chat(text: "b", attachments: [], forward: nil)),
            post(3, .delete(target: 1)),
            post(4, .edit(target: 1, text: "too late")),
        ])
        XCTAssertEqual(folded.items.map(\.cursor), [2])
    }

    func testAnOpNamingALaterOrUnknownPostDoesNothing() {
        let folded = ChannelTimeline.fold([
            post(1, .edit(target: 2, text: "from the future")),
            post(2, .chat(text: "real", attachments: [], forward: nil)),
            post(3, .delete(target: 99)),
        ])
        XCTAssertEqual(folded.items.map(\.text), ["real"])
    }

    func testAPostWithoutItsSeedSaysEarlierPostsAreUnavailable() {
        let folded = ChannelTimeline.fold([
            ChannelDecodedPost(cursor: 1, serverTsMs: 1, epoch: 0, state: .noKey),
            ChannelDecodedPost(cursor: 2, serverTsMs: 2, epoch: 1, state: .hidden),
            post(3, .chat(text: "now", attachments: [], forward: nil)),
        ])
        XCTAssertTrue(folded.earlierUnavailable)
        XCTAssertEqual(folded.items.map(\.cursor), [3])
    }

    func testAPostIsAnIncomingBubbleWithNoAuthor() {
        let item = ChannelTimelineItem(
            cursor: 5, serverTsMs: 5_000, epoch: 1, text: "x", attachments: [], forward: nil,
            edited: true, lastOpCursor: 6)
        let message = ChannelTimeline.message(item)
        XCTAssertEqual(message.direction, .incoming)
        XCTAssertNil(message.authorKeyB64)
        XCTAssertEqual(message.messageId, 5)
        XCTAssertTrue(message.edited)
    }

    func testTheHeaderLineFollowsTheFlag() {
        XCTAssertNil(ChannelHeaderLine.text(isPublic: nil, subscriberCount: 3))
        XCTAssertEqual(
            ChannelHeaderLine.text(isPublic: false, subscriberCount: 5),
            Localization.text("channels.header.private"))
        XCTAssertEqual(ChannelHeaderLine.text(isPublic: true, subscriberCount: nil), Localization.text("channels.header.public"))
        XCTAssertEqual(ChannelHeaderLine.band(12_408), "10k+")
        XCTAssertEqual(ChannelHeaderLine.band(3), "<10")
    }

    func testTheExtrasSayPublicFromTheRecordElseFromAReadKey() {
        var extras = ChannelExtras()
        XCTAssertNil(extras.isPublic)
        extras.readKey = Data(repeating: 1, count: 32)
        XCTAssertEqual(extras.isPublic, true)
        extras.handle = StoredConversationHandle(FfiConversationHandle(
            updatedAtMs: 1, public: false, handle: "news", certificate: Data(), signature: Data()))
        XCTAssertEqual(extras.isPublic, false)
        XCTAssertEqual(extras.allowed, .all)
        extras.reactions = StoredAllowedReactions(.only(emoji: ["👍"]))
        XCTAssertEqual(extras.allowed, .only(emoji: ["👍"]))
    }

    func testTheExtrasRoundTripThroughTheirStore() throws {
        var extras = ChannelExtras()
        extras.banUpdatedAtMs = 9
        extras.bannedAuthors = [Data(repeating: 2, count: 32)]
        extras.reactions = .off
        let data = try JSONEncoder().encode(["c1": extras])
        XCTAssertEqual(try JSONDecoder().decode([String: ChannelExtras].self, from: data)["c1"], extras)
    }

    func testTheLinkFormGivesTheWireTerms() {
        var form = ChannelLinkForm()
        XCTAssertEqual(form.expiryMs(nowMs: 1_000), 0)
        XCTAssertEqual(form.maxUses, UInt32.max)
        form.expiry = .oneDay
        form.uses = .one
        XCTAssertEqual(form.expiryMs(nowMs: 1_000), 1_000 + 86_400_000)
        XCTAssertEqual(form.maxUses, 0)
        form.uses = .upToTen
        XCTAssertEqual(form.maxUses, 10)
    }

    func testPendingReactionAddsAreKeptPerAccountAndDropped() {
        let account = "test-pending-reactions-\(UUID().uuidString)"
        defer { PendingChannelReactionStore.delete(account) }
        let entry = PendingChannelReactionStore.Entry(channelIdHex: "c1", cursor: 4, emoji: "👍", epoch: 2)
        PendingChannelReactionStore.add(entry, accountIdB64: account)
        PendingChannelReactionStore.add(entry, accountIdB64: account)
        XCTAssertEqual(PendingChannelReactionStore.load(account), [entry])
        XCTAssertEqual(PendingChannelReactionStore.load("someone-else-\(UUID().uuidString)"), [])
        PendingChannelReactionStore.remove(channelId: "c1", cursor: 4, emoji: "👍", accountIdB64: account)
        XCTAssertEqual(PendingChannelReactionStore.load(account), [])
    }

    func testAnInvitationCardIsFoundByConversationAndMessageId() {
        let account = "test-invite-cards-\(UUID().uuidString)"
        defer { ChannelInviteCardStore.shared.delete(accountIdB64: account) }
        let store = ChannelInviteCardStore.shared
        store.put(card: Data([1, 2, 3]), conversationIdHex: "AB01", messageId: -1, accountIdB64: account)
        XCTAssertEqual(store.card(conversationIdHex: "ab01", messageId: -1, accountIdB64: account), Data([1, 2, 3]))
        // Keyed on (conversation, message id): the same id elsewhere is not it.
        XCTAssertNil(store.card(conversationIdHex: "cd02", messageId: -1, accountIdB64: account))
        // A sent row's card moves from its local id to its DS id.
        store.move(conversationIdHex: "ab01", from: -1, to: 42, accountIdB64: account)
        XCTAssertEqual(store.card(conversationIdHex: "ab01", messageId: 42, accountIdB64: account), Data([1, 2, 3]))
        XCTAssertNil(store.card(conversationIdHex: "ab01", messageId: 43, accountIdB64: account))
        XCTAssertNil(ChannelInviteCardFacts.facts(card: Data([1, 2, 3])))
    }

    // MARK: - Admission seeds to a rotated co-publisher (ADR-0074 section 3.7)

    private func hex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }

    @MainActor
    func testASealGoesToTheCurrentKeyOnlyWhenTheChainProvesIt() throws {
        let old = try Identity.fromSeed(seed: Data(repeating: 0x11, count: 64))
        let current = try Identity.fromSeed(seed: Data(repeating: 0x22, count: 64))
        let stranger = try Identity.fromSeed(seed: Data(repeating: 0x33, count: 64))
        let x = Data(repeating: 0x44, count: 32).base64EncodedString()
        let currentKey = current.signaturePublicKey().base64EncodedString()

        // Not rotated: the same key.
        XCTAssertTrue(ChannelAdmissionSeedShare.provesSamePerson(
            requestKeyHex: hex(current.signaturePublicKey()),
            bundle: IdentityFetchResponse(signaturePubkey: currentKey, x25519Pubkey: x)))

        // Rotated, nothing to prove it: skipped.
        XCTAssertFalse(ChannelAdmissionSeedShare.provesSamePerson(
            requestKeyHex: hex(old.signaturePublicKey()),
            bundle: IdentityFetchResponse(signaturePubkey: currentKey, x25519Pubkey: x)))

        // Rotated, the bundle's two-way link names the request's key: sealed
        // to the current one.
        let link = try signConversationLink(
            conversationEd25519Seed: old.ed25519Seed(), currentEd25519Seed: current.ed25519Seed(),
            issuedAtUnixSecs: 1_800_000_000)
        let linked = IdentityFetchResponse(
            signaturePubkey: currentKey, x25519Pubkey: x, conversationLink: link.base64EncodedString())
        XCTAssertTrue(ChannelAdmissionSeedShare.provesSamePerson(
            requestKeyHex: hex(old.signaturePublicKey()), bundle: linked))

        // The same link does not vouch for anyone else.
        XCTAssertFalse(ChannelAdmissionSeedShare.provesSamePerson(
            requestKeyHex: hex(stranger.signaturePublicKey()), bundle: linked))
    }

    func testThePublisherRecordAnswersWhoElsePublishes() {
        let account = "test-publishers-\(UUID().uuidString)"
        defer { CoPublisherKeyBook.clear(accountIdB64: account) }
        XCTAssertFalse(CoPublisherKeyBook.hasOtherPublishers(channelIdHex: "AA", isOwner: true, accountIdB64: account))
        XCTAssertTrue(CoPublisherKeyBook.hasOtherPublishers(channelIdHex: "AA", isOwner: false, accountIdB64: account),
                      "a co-publisher always has the owner")
        CoPublisherKeyBook.setPublishers(["0102"], channelIdHex: "AA", accountIdB64: account)
        XCTAssertTrue(CoPublisherKeyBook.hasOtherPublishers(channelIdHex: "aa", isOwner: true, accountIdB64: account))
        XCTAssertFalse(CoPublisherKeyBook.hasOtherPublishers(channelIdHex: "bb", isOwner: true, accountIdB64: account))
    }

    // MARK: - ADR-0087 section 15.2: an appointment any owner device can revoke

    /// A publisher's own binding, sent in the PublisherGroup, is filed as the
    /// appointed key of its sender, and only of its sender: a binding naming
    /// another account, another channel, or sent by the owner is ignored.
    func testABindingSentInTheGroupIsFiledUnderItsSenderOnly() throws {
        let account = "binding-test-\(UUID().uuidString)"
        defer { CoPublisherKeyBook.clear(accountIdB64: account) }
        let channel = Data(repeating: 0x2a, count: 16)
        let channelHex = channel.map { String(format: "%02x", $0) }.joined()
        let publisher = try Identity.fromSeed(seed: Data(repeating: 0x71, count: 32))
        let stranger = try Identity.fromSeed(seed: Data(repeating: 0x72, count: 32))
        let owner = Data(repeating: 0x73, count: 32)
        let binding = try composeChannelCopublisherBinding(accountIdentity: publisher, channelId: channel)
        let expected = try decodeChannelCopublisherBinding(bytes: binding).copublisherPubkey
        let other = try composeChannelCopublisherBinding(
            accountIdentity: publisher, channelId: Data(repeating: 0x2b, count: 16))

        CoPublisherKeyBook.noteBindings([
            .init(data: binding, senderAccount: stranger.signaturePublicKey()),
            .init(data: binding, senderAccount: owner),
            .init(data: other, senderAccount: publisher.signaturePublicKey()),
            .init(data: Data("not a binding".utf8), senderAccount: publisher.signaturePublicKey()),
        ], ownerAccountKey: owner, channelIdHex: channelHex, accountIdB64: account)
        XCTAssertNil(CoPublisherKeyBook.appointedKey(
            channelIdHex: channelHex, accountKey: publisher.signaturePublicKey(), accountIdB64: account))
        XCTAssertNil(CoPublisherKeyBook.appointedKey(
            channelIdHex: channelHex, accountKey: stranger.signaturePublicKey(), accountIdB64: account))

        CoPublisherKeyBook.noteBindings(
            [.init(data: binding, senderAccount: publisher.signaturePublicKey())],
            ownerAccountKey: owner, channelIdHex: channelHex, accountIdB64: account)
        XCTAssertEqual(CoPublisherKeyBook.appointedKey(
            channelIdHex: channelHex, accountKey: publisher.signaturePublicKey(), accountIdB64: account), expected)
    }

    /// The publisher re-sends its binding once per epoch: a later epoch is
    /// owed, the same one is not.
    func testThePublisherBindingIsOwedOncePerEpoch() {
        let account = "binding-sent-\(UUID().uuidString)"
        defer { CoPublisherKeyBook.clear(accountIdB64: account) }
        XCTAssertNil(CoPublisherKeyBook.bindingSentEpoch(channelIdHex: "AB", accountIdB64: account))
        CoPublisherKeyBook.noteBindingSent(channelIdHex: "AB", epoch: 4, accountIdB64: account)
        XCTAssertEqual(CoPublisherKeyBook.bindingSentEpoch(channelIdHex: "ab", accountIdB64: account), 4)
        CoPublisherKeyBook.clear(accountIdB64: account)
        XCTAssertNil(CoPublisherKeyBook.bindingSentEpoch(channelIdHex: "ab", accountIdB64: account))
    }

    // MARK: - UX section 14.2: the group's link list

    /// A link made while the synced register was not loaded is still listed
    /// on this device, newest first, under its own group only.
    @MainActor
    func testAGroupLinkMadeWithoutTheRegisterIsListedForItsGroup() {
        let repo = ChatRepository()
        let group = String(repeating: "ab", count: 32)
        let older = GroupInviteLinkRow(
            ledgerId: Data([1, 2, 3, 4, 5, 6]), url: "https://lakko.me/g/a#x", label: "Friends",
            expiryMs: 0, maxUses: UInt32.max, mintedAtMs: 10)
        let newer = GroupInviteLinkRow(
            ledgerId: Data([6, 5, 4, 3, 2, 1]), url: "https://lakko.me/g/b#y", label: "",
            expiryMs: 99, maxUses: 1, mintedAtMs: 20)
        repo.unregisteredGroupLinks[group] = [older, newer]
        XCTAssertEqual(repo.groupInviteLinks(groupIdHex: group.uppercased()), [newer, older])
        XCTAssertEqual(repo.groupInviteLinks(groupIdHex: String(repeating: "cd", count: 32)), [])
        XCTAssertEqual(GroupInviteLinksScreen.row(older).label, "Friends")
    }

    // MARK: - ADR-0087 section 15: a named publisher is approved without asking

    private func publisherRecord(
        ledger: UInt8, capability: Data, channel: Data, revoked: Bool = false, uses: UInt32 = 0,
        maxUses: UInt32 = 0
    ) -> LinkRecord {
        LinkRecord(
            hlc: 0, linkId: Data(repeating: ledger, count: 16), linkSecret: Data(repeating: 9, count: 12),
            kind: .publisher, ledgerId: Data(repeating: ledger, count: 6), revocationSeed: Data(repeating: 1, count: 32),
            expiryMs: 0, maxUses: maxUses, uses: uses, revoked: revoked, mintedAtMs: 1, label: "", answered: [],
            targetId: channel, inviteSeed: Data(), admissionVersion: 0, capability: capability)
    }

    /// A request carrying a link this device minted matches it; one made on
    /// another device matches through the register by opening its capability.
    @MainActor
    func testANamedPublisherRequestMatchesItsLink() {
        let channel = Data(repeating: 0x11, count: 16)
        let wire = Data("capability-A".utf8)
        let own = PublisherLinkApproval.LocalLink(ledgerId: Data(repeating: 3, count: 6), capabilityWire: wire, used: false)
        let viaLocal = PublisherLinkApproval.match(
            requestCapability: wire, channelId: channel, records: [], local: [own], nowMs: 5, open: { _ in nil })
        XCTAssertEqual(viaLocal?.ledgerId, own.ledgerId)
        XCTAssertEqual(viaLocal?.local, true)

        let sealed = Data("sealed-B".utf8)
        let record = publisherRecord(ledger: 4, capability: sealed, channel: channel)
        let viaRegister = PublisherLinkApproval.match(
            requestCapability: Data("capability-B".utf8), channelId: channel, records: [record], local: [],
            nowMs: 5, open: { $0.capability == sealed ? Data("capability-B".utf8) : nil })
        XCTAssertEqual(viaRegister?.ledgerId, record.ledgerId)
        XCTAssertEqual(viaRegister?.local, false)
    }

    /// A link the register shows as revoked (or already used) approves
    /// nothing, whether this device minted it or another one did.
    @MainActor
    func testARevokedPublisherLinkApprovesNothing() {
        let channel = Data(repeating: 0x12, count: 16)
        let wire = Data("capability-C".utf8)
        let ledger = Data(repeating: 5, count: 6)
        let revoked = publisherRecord(ledger: 5, capability: Data("sealed-C".utf8), channel: channel, revoked: true)
        XCTAssertNil(PublisherLinkApproval.match(
            requestCapability: wire, channelId: channel, records: [revoked],
            local: [.init(ledgerId: ledger, capabilityWire: wire, used: false)], nowMs: 5, open: { _ in wire }))
        XCTAssertNil(PublisherLinkApproval.match(
            requestCapability: wire, channelId: channel, records: [revoked], local: [], nowMs: 5, open: { _ in wire }))
        let used = publisherRecord(ledger: 6, capability: Data("sealed-D".utf8), channel: channel, uses: 1)
        XCTAssertNil(PublisherLinkApproval.match(
            requestCapability: wire, channelId: channel, records: [used], local: [], nowMs: 5, open: { _ in wire }))
        XCTAssertNil(PublisherLinkApproval.match(
            requestCapability: wire, channelId: channel, records: [],
            local: [.init(ledgerId: ledger, capabilityWire: wire, used: true)], nowMs: 5, open: { _ in nil }))
    }

    /// A request by a link the account never minted, or one for another
    /// channel, stays pending.
    @MainActor
    func testAnUnknownPublisherLinkApprovesNothing() {
        let channel = Data(repeating: 0x13, count: 16)
        let record = publisherRecord(ledger: 7, capability: Data("sealed-E".utf8), channel: Data(repeating: 0x14, count: 16))
        XCTAssertNil(PublisherLinkApproval.match(
            requestCapability: Data("capability-E".utf8), channelId: channel, records: [record],
            local: [.init(ledgerId: Data(repeating: 8, count: 6), capabilityWire: Data("other".utf8), used: false)],
            nowMs: 5, open: { _ in Data("capability-E".utf8) }))
    }

    /// This device's record of its publisher links survives and marks use.
    func testPublisherLinkLocalRecordMarksUse() {
        let account = "publisher-links-\(UUID().uuidString)"
        defer { PublisherLinkApproval.clear(accountIdB64: account) }
        let ledger = Data(repeating: 2, count: 6)
        PublisherLinkApproval.noteMinted(
            channelIdHex: "AB", ledgerId: ledger, capabilityWire: Data([1]), accountIdB64: account)
        XCTAssertEqual(PublisherLinkApproval.localLinks(channelIdHex: "ab", accountIdB64: account).first?.used, false)
        PublisherLinkApproval.noteUsed(channelIdHex: "ab", ledgerId: ledger, accountIdB64: account)
        XCTAssertEqual(PublisherLinkApproval.localLinks(channelIdHex: "ab", accountIdB64: account).first?.used, true)
    }

    // MARK: - ADR-0087 section 13a: own comment reactions in the synced register

    /// An own comment reaction is read back from the register, a second
    /// write of the same state changes nothing, the register holds three per
    /// comment, and the post's own reactions stay apart from its comments'.
    func testOwnCommentReactionsLiveInTheSyncedRegister() throws {
        let state = SyncedState()
        let channel = Data(repeating: 0x21, count: 16)
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        XCTAssertNotNil(try state.setCommentReaction(
            hlc: 1, channelId: channel, postCursor: 7, commentCursor: 3, emoji: "👍", reacted: true, postAtMs: now))
        XCTAssertNil(try state.setCommentReaction(
            hlc: 2, channelId: channel, postCursor: 7, commentCursor: 3, emoji: "👍", reacted: true, postAtMs: now))
        XCTAssertEqual(try state.commentReactionsOn(channelId: channel, postCursor: 7, commentCursor: 3), ["👍"])
        XCTAssertTrue(try state.hasCommentReaction(channelId: channel, postCursor: 7, commentCursor: 3, emoji: "👍"))
        XCTAssertEqual(try state.channelReactionsOn(channelId: channel, postCursor: 7), [])

        _ = try state.setCommentReaction(
            hlc: 3, channelId: channel, postCursor: 7, commentCursor: 3, emoji: "❤️", reacted: true, postAtMs: now)
        _ = try state.setCommentReaction(
            hlc: 4, channelId: channel, postCursor: 7, commentCursor: 3, emoji: "😂", reacted: true, postAtMs: now)
        let fourth = try? state.setCommentReaction(
            hlc: 5, channelId: channel, postCursor: 7, commentCursor: 3, emoji: "😮", reacted: true, postAtMs: now)
        XCTAssertNil(fourth ?? nil)
        XCTAssertEqual(try state.commentReactionsOn(channelId: channel, postCursor: 7, commentCursor: 3).count, 3)

        XCTAssertNotNil(try state.setCommentReaction(
            hlc: 6, channelId: channel, postCursor: 7, commentCursor: 3, emoji: "👍", reacted: false, postAtMs: now))
        XCTAssertFalse(try state.hasCommentReaction(channelId: channel, postCursor: 7, commentCursor: 3, emoji: "👍"))
    }

    /// Only a one-use link names a person: a limit of 0 or 1 auto-admits,
    /// no limit or 5 never does (the request waits for a manual Approve),
    /// whether the link is this device's or another device's.
    @MainActor
    func testOnlyAOneUsePublisherLinkAutoAdmits() {
        let channel = Data(repeating: 0x15, count: 16)
        let wire = Data("capability-F".utf8)
        for (limit, admits) in [(UInt32(0), true), (1, true), (UInt32.max, false), (5, false)] {
            let record = publisherRecord(ledger: 9, capability: Data("sealed-F".utf8), channel: channel, maxUses: limit)
            let viaRegister = PublisherLinkApproval.match(
                requestCapability: wire, channelId: channel, records: [record], local: [], nowMs: 5,
                open: { _ in wire })
            XCTAssertEqual(viaRegister != nil, admits, "register limit \(limit)")
            let own = PublisherLinkApproval.LocalLink(
                ledgerId: Data(repeating: 10, count: 6), capabilityWire: wire, used: false, maxUses: limit)
            let viaLocal = PublisherLinkApproval.match(
                requestCapability: wire, channelId: channel, records: [], local: [own], nowMs: 5, open: { _ in nil })
            XCTAssertEqual(viaLocal != nil, admits, "local limit \(limit)")
        }
    }

    // MARK: - Live-walk defects (2026-11-06)

    /// UX navigation section 2 — a channel row's second line is its newest
    /// readable post, and its time that post's; the @name only while no post
    /// can be read here.
    @MainActor
    func testAChannelRowShowsItsLastPost() {
        let channels = ChannelsUiState()
        let withPost = String(repeating: "3", count: 32)
        let without = String(repeating: "4", count: 32)
        channels.addChannel(ChannelHandle(
            id: withPost, localName: "News", localDescription: "", addedAtMs: 1, handle: "news"))
        channels.addChannel(ChannelHandle(
            id: without, localName: "Quiet", localDescription: "", addedAtMs: 2, handle: "quiet"))
        let rows = ChatListScreen.channelRows(
            channels: channels, channelUnread: [:],
            lastPosts: [withPost: PersistedChannelPost(cursor: 5, serverTsMs: 9_000, text: "Line one\nline two")])
        let byId = Dictionary(uniqueKeysWithValues: rows.map { ($0.id, $0) })
        XCTAssertEqual(byId[withPost]?.preview, "Line one line two")
        XCTAssertEqual(byId[withPost]?.lastActivityMs, 9_000)
        XCTAssertEqual(byId[without]?.preview.contains("@quiet"), true)
    }

    /// The row's time is the newest real post's, never a newer raw server
    /// row (a ban list, a metadata record). Seen live 2026-11-12: a ban at
    /// 22:48 stood as the time of a channel whose newest post was at 22:39.
    @MainActor
    func testAChannelRowTimeIgnoresABanListOrRecordRow() {
        let channels = ChannelsUiState()
        let id = String(repeating: "5", count: 32)
        channels.addChannel(ChannelHandle(
            id: id, localName: "Walk 22", localDescription: "", addedAtMs: 1, handle: "walk22"))
        // The raw rows the server holds: the post, then a newer ban list.
        channels.setPosts(channelId: id, posts: [
            PostView(cursor: 5, serverTsMs: 9_000, payloadB64: ""),
            PostView(cursor: 6, serverTsMs: 18_000, payloadB64: ""),
        ])
        let rows = ChatListScreen.channelRows(
            channels: channels, channelUnread: [:],
            lastPosts: [id: PersistedChannelPost(cursor: 5, serverTsMs: 9_000, text: "Newest post")])
        XCTAssertEqual(rows.first?.lastActivityMs, 9_000)
    }

    /// The marker the app writes into the stored invitation lines is the one
    /// the notification extension replaces (`nseChannelNameMark`).
    func testTheInvitationLineMarkerMatchesTheExtension() {
        XCTAssertEqual(ChannelInviteNotificationMirror.channelMark, "\u{E002}")
    }

    /// A comment's menu is the message menu: Reply and Copy, then Delete for
    /// its writer or a publisher and Ban for a publisher on someone else's.
    func testTheCommentMenuOffersTheActionsThatApply() {
        XCTAssertEqual(ChannelCommentsView.commentActions(canReply: true, own: true, isPublisher: false, hasText: true),
                       [.reply, .copy, .delete])
        XCTAssertEqual(ChannelCommentsView.commentActions(canReply: true, own: false, isPublisher: false, hasText: true),
                       [.reply, .copy])
        XCTAssertEqual(ChannelCommentsView.commentActions(canReply: true, own: false, isPublisher: true, hasText: true),
                       [.reply, .copy, .delete, .ban])
        XCTAssertEqual(ChannelCommentsView.commentActions(canReply: false, own: false, isPublisher: false, hasText: false),
                       [])
    }

    /// One person, one colour: a one-to-one row tints by the person's key,
    /// the input the contact list and the pickers use; a group by its id.
    func testAOneToOneRowTintsByThePerson() {
        let key = Data(repeating: 0x5a, count: 32)
        let chat = ConversationListRow(
            id: "abcd", kind: .chat, title: "VG3", preview: "", lastActivityMs: nil, personKey: key)
        XCTAssertEqual(chat.tintIdentity, key.base64EncodedString())
        let group = ConversationListRow(id: "ef01", kind: .group, title: "G", preview: "", lastActivityMs: nil)
        XCTAssertEqual(group.tintIdentity, "ef01")
    }

    // MARK: - The seed a commit leaves (2026-11-08)

    /// Section 13 — the seed a commit is about to leave is kept under the
    /// server epoch its posts carry; nothing is kept twice, and a missing or
    /// malformed seed keeps nothing.
    func testTheOutgoingSeedIsKeptUnderItsServerEpoch() {
        let seed = Data(repeating: 0x42, count: 32)
        let kept = ChannelPublisherCommits.outgoingSeedToKeep(seed: seed, serverEpoch: 3, held: [:])
        XCTAssertEqual(kept?.epoch, 3)
        XCTAssertEqual(kept?.seed, seed)
        XCTAssertNil(ChannelPublisherCommits.outgoingSeedToKeep(
            seed: seed, serverEpoch: 3, held: [3: Data(repeating: 0x01, count: 32)]))
        XCTAssertNil(ChannelPublisherCommits.outgoingSeedToKeep(seed: nil, serverEpoch: 3, held: [:]))
        XCTAssertNil(ChannelPublisherCommits.outgoingSeedToKeep(
            seed: Data(repeating: 0x42, count: 16), serverEpoch: 3, held: [:]))
    }

    /// The owner's own channel: after a rotation (an approve or a removal
    /// moves the group the same way) the seed of the epoch it left still
    /// opens that epoch's posts on this device, and the new seed is a
    /// different one, so the channel's record at creation never reads as
    /// "Earlier posts aren't available.".
    @MainActor
    func testTheOwnerStillHoldsTheSeedOfTheEpochARotationLeft() throws {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x5B, count: 32))
        let channelId = Data((0..<16).map { UInt8($0) &+ 0x30 })
        let channelIdHex = channelId.map { String(format: "%02x", $0) }.joined()
        let group = try publisherGroupOpenOrCreate(identity: identity, channelId: channelId)
        let before = try group.channelSeed(identity: identity)
        let channels = ChannelsUiState()

        ChannelPublisherCommits.keepOutgoingSeed(
            state: channels, channelIdHex: channelIdHex, serverEpoch: 0, group: group, identity: identity)
        _ = try group.rotate(identity: identity)
        let after = try group.channelSeed(identity: identity)

        XCTAssertNotEqual(before, after)
        XCTAssertEqual(channels.subscribedSeeds(channelId: channelIdHex), [0: before])

        // A record sealed at epoch 0 under the old seed opens with what was kept.
        let keys = ChannelPublisherKeys(seed: before, signingKeySeed: ChannelSigningKey.generate().toSeed())
        let wire = try ChannelsComposer.compose(
            keys: keys, channelIdHex: channelIdHex, epochId: 0, messageCounter: 1, plaintext: Data("record".utf8))
        let kept = try XCTUnwrap(channels.subscribedSeeds(channelId: channelIdHex)[0])
        let opened = try ChannelsComposer.verifyAndDecrypt(
            seed: kept, channelIdHex: channelIdHex, postWireB64Url: Self.b64url(wire))
        XCTAssertEqual(opened, Data("record".utf8))

        // A second keep at the same epoch changes nothing.
        ChannelPublisherCommits.keepOutgoingSeed(
            state: channels, channelIdHex: channelIdHex, serverEpoch: 0, group: group, identity: identity)
        XCTAssertEqual(channels.subscribedSeeds(channelId: channelIdHex), [0: before])
    }

    private static func b64url(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
