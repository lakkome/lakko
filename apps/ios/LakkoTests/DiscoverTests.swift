// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0059 on iOS: the pure parts of Discover and the shared-channels
/// envelope, mirroring web `discover/*.test.ts`.
final class DiscoverTests: XCTestCase {

    /// A deterministic source in [0, 1).
    private func seeded(_ seed: UInt64) -> () -> Double {
        var x = seed
        return {
            x = (x &* 1_103_515_245 &+ 12345) % 2_147_483_648
            return Double(x) / 2_147_483_648
        }
    }

    private let all = Array(0..<24)
    private let sept: Int64 = 1_790_640_000_000 // 2026-09-29 UTC
    private let oct: Int64 = 1_790_812_800_000 // 2026-10-01 UTC

    private func hex(_ b: UInt8) -> String { String(repeating: String(format: "%02x", b), count: 16) }

    // MARK: - Shared channels: the envelope through the real core

    func testChatClientRecognisesAComposedListAndKeepsItsOrder() throws {
        let list = FfiSharedChannels(updatedAtMs: 42, channels: [
            FfiSharedChannel(channelId: Data(repeating: 1, count: 16), handle: "books", name: "Books"),
            FfiSharedChannel(channelId: Data(repeating: 2, count: 16), handle: "news_24", name: "News"),
        ])
        let bytes = try composeSharedChannelsEnvelope(list: list)
        guard case .list(let decoded) = ChatClient.sniffSharedChannels(bytes) else {
            return XCTFail("a composed list is a list")
        }
        XCTAssertEqual(decoded, list)
    }

    func testChatClientDropsADamagedListAndIgnoresWhatIsNotOne() throws {
        let bytes = try composeSharedChannelsEnvelope(list: FfiSharedChannels(updatedAtMs: 1, channels: [
            FfiSharedChannel(channelId: Data(repeating: 1, count: 16), handle: "books", name: "Books"),
        ]))
        // Truncated: the domain matched, the bounds fail.
        XCTAssertEqual(ChatClient.sniffSharedChannels(bytes.dropLast(3)), .malformed)
        XCTAssertEqual(ChatClient.sniffSharedChannels(Data(#"{"v":1,"text":"hi"}"#.utf8)), .none)
        XCTAssertEqual(ChatClient.sniffSharedChannels(Data("lakko is a chat app".utf8)), .none)
        XCTAssertEqual(ChatClient.sniffSharedChannels(Data()), .none)
    }

    // MARK: - Shared channels: the row

    private func list(_ stamp: UInt64, _ ids: [UInt8]) -> SharedChannels.List {
        SharedChannels.List(
            updatedAtMs: stamp,
            channels: ids.map { SharedChannels.Channel(channelIdHex: hex($0), handle: "ch\($0)x", name: "C\($0)") })
    }

    func testFilesPerMemberStrictlyNewerWinsEqualIsNot() {
        let newer: (UInt64, UInt64) -> Bool = { $0 > $1 }
        var row = SharedChannels.Row()
        var r = SharedChannels.applyIncoming(row: row, groupIdHex: "g", memberKeyB64: "a", incoming: list(5, [1]), isNewer: newer)
        XCTAssertEqual(r.verdict, .accept)
        row = r.row
        r = SharedChannels.applyIncoming(row: row, groupIdHex: "g", memberKeyB64: "a", incoming: list(5, [2]), isNewer: newer)
        XCTAssertEqual(r.verdict, .rejectStale)
        r = SharedChannels.applyIncoming(row: row, groupIdHex: "g", memberKeyB64: "a", incoming: list(4, [2]), isNewer: newer)
        XCTAssertEqual(r.verdict, .rejectStale)
        r = SharedChannels.applyIncoming(row: row, groupIdHex: "g", memberKeyB64: "b", incoming: list(1, [3]), isNewer: newer)
        XCTAssertEqual(r.verdict, .accept)
        row = r.row
        r = SharedChannels.applyIncoming(row: row, groupIdHex: "g", memberKeyB64: "a", incoming: list(6, []), isNewer: newer)
        XCTAssertEqual(r.verdict, .accept)
        XCTAssertEqual(r.row.received["g"]?["a"]?.channels, [])
        XCTAssertEqual(r.row.received["g"]?["b"]?.updatedAtMs, 1)
    }

    func testRefusesAListItCannotAttribute() {
        let r = SharedChannels.applyIncoming(
            row: SharedChannels.Row(), groupIdHex: "g", memberKeyB64: nil, incoming: list(1, [1]), isNewer: { $0 > $1 })
        XCTAssertEqual(r.verdict, .rejectUnattributed)
        XCTAssertTrue(r.row.received.isEmpty)
    }

    func testStampsASentListAfterThePreviousEvenWhenTheClockWentBack() {
        XCTAssertEqual(SharedChannels.freshStamp(nowMs: 100, previous: nil), 100)
        XCTAssertEqual(SharedChannels.freshStamp(nowMs: 100, previous: list(200, [])), 201)
        XCTAssertEqual(SharedChannels.freshStamp(nowMs: 300, previous: list(200, [])), 300)
    }

    func testRoundTripsThroughTheSealedRowCodec() {
        var row = SharedChannels.Row()
        row.received["g1"] = ["k1": list(9, [1, 2])]
        row.sent["g1"] = list(7, [3])
        row.shareOn = ["g1", "g2"]
        XCTAssertEqual(SharedChannels.decode(SharedChannels.encode(row)), row)
        XCTAssertEqual(SharedChannels.decode(Data("not json".utf8)), SharedChannels.Row())
    }

    func testSharesOnlySubscribedListedChannelsWithAHandle() {
        let out = SharedChannels.toShare([
            .init(channelIdHex: hex(3), subscribed: true, listed: true, handle: "books", name: " Books "),
            .init(channelIdHex: hex(1), subscribed: true, listed: true, handle: "news", name: "News"),
            .init(channelIdHex: hex(2), subscribed: true, listed: false, handle: "secret", name: "Secret"),
            .init(channelIdHex: hex(4), subscribed: false, listed: true, handle: "mine", name: "Mine"),
            .init(channelIdHex: hex(5), subscribed: true, listed: true, handle: "", name: "No handle"),
            .init(channelIdHex: hex(6), subscribed: true, listed: true, handle: "Caps", name: "Caps"),
            .init(channelIdHex: hex(7), subscribed: true, listed: true, handle: "noname", name: "  "),
        ])
        XCTAssertEqual(out.map(\.channelIdHex), [hex(1), hex(3)])
        XCTAssertEqual(out.last?.name, "Books")
    }

    func testAggregatesAcrossGroupsSkippingThisPersonAndGroupsNoLongerHeld() {
        var row = SharedChannels.Row()
        row.received["g1"] = ["a": list(1, [1]), "me": list(1, [2])]
        row.received["g2"] = ["b": list(1, [1]), "c": list(1, [1])]
        row.received["gone"] = ["d": list(1, [3])]
        let out = SharedChannels.aggregate(row: row, ownKeyB64: "me", liveGroups: ["g1", "g2"])
        XCTAssertEqual(out.map(\.channelIdHex), [hex(1)])
        XCTAssertEqual(out.first?.byGroup["g1"], ["a"])
        XCTAssertEqual(Set(out.first?.byGroup["g2"] ?? []), ["b", "c"])
        XCTAssertEqual(out.first?.handle, "ch1x")
    }

    // MARK: - Decoys

    func testDecoyDrawIsReusedWithinAMonthAndLanguage() {
        let first = DiscoverDecoys.current(held: nil, allTopics: all, language: "en", nowMs: sept, random: seeded(1))
        XCTAssertTrue(first.fresh)
        XCTAssertEqual(first.draw.month, "2026-09")
        XCTAssertEqual(first.draw.order.sorted(), all)
        let again = DiscoverDecoys.current(
            held: first.draw, allTopics: all, language: "en", nowMs: sept + 86_400_000, random: seeded(99))
        XCTAssertFalse(again.fresh)
        XCTAssertEqual(again.draw, first.draw)
        XCTAssertTrue(DiscoverDecoys.current(held: first.draw, allTopics: all, language: "en", nowMs: oct, random: seeded(2)).fresh)
        XCTAssertTrue(DiscoverDecoys.current(held: first.draw, allTopics: all, language: "de", nowMs: sept, random: seeded(2)).fresh)
        XCTAssertEqual(DiscoverDecoys.monthKey(nowMs: oct), "2026-10")
    }

    func testPadsPickedTopicsWithTheSameDecoysEveryDay() {
        let draw = DecoyDraw(month: "2026-09", language: "en", order: DiscoverDecoys.shuffled(all, random: seeded(7)))
        let a = DiscoverDecoys.planRounds(picked: [3, 5], decoys: draw, mixRandom: true, random: seeded(1))
        let b = DiscoverDecoys.planRounds(picked: [3, 5], decoys: draw, mixRandom: true, random: seeded(2))
        XCTAssertEqual(a.count, 1)
        XCTAssertEqual(a[0].count, DiscoverDecoys.roundSize)
        XCTAssertEqual(Set(a[0]).count, DiscoverDecoys.roundSize)
        XCTAssertTrue(a[0].contains(3) && a[0].contains(5))
        XCTAssertEqual(Set(a[0]), Set(b[0]), "the decoys repeat, only the order changes")
    }

    func testNeverUsesAPickedTopicAsADecoyAndSplitsRounds() {
        let draw = DecoyDraw(month: "2026-09", language: "en", order: all)
        let picked = Array(0..<10)
        let rounds = DiscoverDecoys.planRounds(picked: picked, decoys: draw, mixRandom: true, random: seeded(3))
        XCTAssertEqual(rounds.count, 2)
        XCTAssertTrue(rounds.allSatisfy { $0.count == DiscoverDecoys.roundSize })
        let decoys = rounds.flatMap { $0 }.filter { !picked.contains($0) }
        XCTAssertEqual(decoys.count, 6)
        XCTAssertEqual(Set(rounds.flatMap { $0 }.filter { picked.contains($0) }), Set(picked))
    }

    func testDropsThePaddingWhenMixingIsOffAndFetchesNothingWithNoPicks() {
        let draw = DecoyDraw(month: "2026-09", language: "en", order: all)
        XCTAssertEqual(
            Set(DiscoverDecoys.planRounds(picked: [4], decoys: draw, mixRandom: false, random: seeded(1)).flatMap { $0 }),
            [4])
        XCTAssertTrue(DiscoverDecoys.planRounds(picked: [], decoys: draw, mixRandom: true, random: seeded(1)).isEmpty)
    }

    func testRefreshIsOnceADayOrAtOnceWhenThePicksChange() {
        let key = DiscoverDecoys.pickedKey([5, 3, 3], language: "en")
        XCTAssertEqual(key, "en:3,5")
        XCTAssertTrue(DiscoverDecoys.refreshDue(lastRefreshMs: nil, lastFetchedKey: nil, pickedKey: key, nowMs: sept))
        XCTAssertFalse(DiscoverDecoys.refreshDue(lastRefreshMs: sept, lastFetchedKey: key, pickedKey: key, nowMs: sept + 1000))
        XCTAssertTrue(DiscoverDecoys.refreshDue(
            lastRefreshMs: sept, lastFetchedKey: key, pickedKey: key, nowMs: sept + DiscoverDecoys.refreshIntervalMs))
        XCTAssertTrue(DiscoverDecoys.refreshDue(lastRefreshMs: sept, lastFetchedKey: "en:3", pickedKey: key, nowMs: sept + 1))
        XCTAssertTrue(DiscoverDecoys.refreshDue(lastRefreshMs: sept, lastFetchedKey: key, pickedKey: key, nowMs: sept - 1))
    }

    // MARK: - Cards

    private func card(
        _ b: UInt8, name: String = "N", topic: Int? = 1, language: String? = "en", band: String? = nil,
        rank: Int = 0, followers: GroupFollowers? = nil, description: String? = "d", source: CardSource = .bucket
    ) -> DirectoryCard {
        DirectoryCard(
            channelIdHex: hex(b), handle: "h\(b)xx", name: name, description: description, topic: topic,
            language: language, band: band, source: source, serverRank: rank, followers: followers)
    }

    private func ctx(
        picked: Set<Int> = [1], notInterested: Set<String> = [], hidden: Set<Int> = [],
        following: Set<String> = [], gone: Set<String> = []
    ) -> CardContext {
        CardContext(
            pickedTopics: picked, language: "en", notInterested: notInterested, hiddenTopics: hidden,
            following: following, gone: gone)
    }

    func testDropsAnUndecodableListingAndOneNamingAnotherChannel() {
        let items = [
            DirectoryItem(channelId: hex(1), listingB64: "ok1", subscribers: "1k+"),
            DirectoryItem(channelId: hex(2), listingB64: "bad", subscribers: "10+"),
            DirectoryItem(channelId: hex(3), listingB64: "other", subscribers: "weird"),
            DirectoryItem(channelId: hex(4), listingB64: "ok4", subscribers: "<10"),
        ]
        let cards = DiscoverCards.fromItems(items, source: .bucket) { b64 in
            switch b64 {
            case "ok1": return DecodedListing(channelIdHex: self.hex(1), handle: "a", name: "A", description: "", topic: 1, language: "en")
            case "other": return DecodedListing(channelIdHex: self.hex(9), handle: "c", name: "C", description: "", topic: 1, language: "en")
            case "ok4": return DecodedListing(channelIdHex: self.hex(4), handle: "d", name: "D", description: "", topic: 2, language: "de")
            default: return nil
            }
        }
        XCTAssertEqual(cards.map(\.channelIdHex), [hex(1), hex(4)])
        XCTAssertEqual(cards.map(\.band), ["1k+", "<10"])
        XCTAssertEqual(cards.map(\.serverRank), [0, 3])
    }

    func testPutsAPickedTopicInThePersonsLanguageFirst() {
        let ranked = DiscoverCards.rank([card(2, topic: 7, language: "de"), card(1)], ctx())
        XCTAssertEqual(ranked.first?.channelIdHex, hex(1))
    }

    func testHidesNotInterestedFollowedGoneAndHiddenTopicChannels() {
        let cards = [card(1), card(2), card(3), card(4, topic: 9), card(5)]
        let ranked = DiscoverCards.rank(
            cards, ctx(notInterested: [hex(1)], hidden: [9], following: [hex(2)], gone: [hex(3)]))
        XCTAssertEqual(ranked.map(\.channelIdHex), [hex(5)])
    }

    func testMergesAChannelSeenTwiceKeepingTheGroupFollowers() {
        let followers = GroupFollowers(groupIdHex: "g", groupTitle: "Club", count: 2, contactNames: [])
        let ranked = DiscoverCards.rank([
            card(1, rank: 5),
            card(1, topic: nil, language: nil, rank: 1000, followers: followers, description: nil, source: .group),
        ], ctx())
        XCTAssertEqual(ranked.count, 1)
        XCTAssertEqual(ranked[0].followers, followers)
        XCTAssertEqual(ranked[0].source, .group)
        XCTAssertEqual(ranked[0].serverRank, 5)
        XCTAssertEqual(ranked[0].description, "d")
    }

    func testIsTotalEqualScoresOrderByNameThenId() {
        let ranked = DiscoverCards.rank([card(3, name: "B"), card(2, name: "A"), card(1, name: "B")], ctx())
        XCTAssertEqual(ranked.map(\.channelIdHex), [hex(2), hex(1), hex(3)])
    }

    func testReasonLineNamesContactsOnlyAndCountsTheRest() {
        // The catalog wraps text arguments in bidi isolates (ADR-0061).
        func plain(_ text: String) -> String {
            text.filter { !"\u{2066}\u{2067}\u{2068}\u{2069}".contains($0) }
        }
        let name: (Int) -> String = { _ in "Books" }
        XCTAssertEqual(
            plain(DiscoverCards.reasonLine(card(1, followers: GroupFollowers(groupIdHex: "g", groupTitle: "Club", count: 3, contactNames: ["Anna"])), topicName: name)),
            "Followed by Anna and 2 others in Club")
        XCTAssertEqual(
            plain(DiscoverCards.reasonLine(card(1, followers: GroupFollowers(groupIdHex: "g", groupTitle: "Club", count: 1, contactNames: ["Anna"])), topicName: name)),
            "Followed by Anna in Club")
        XCTAssertEqual(
            plain(DiscoverCards.reasonLine(card(1, followers: GroupFollowers(groupIdHex: "g", groupTitle: "Club", count: 1, contactNames: [])), topicName: name)),
            "Followed by 1 person in Club")
        XCTAssertEqual(plain(DiscoverCards.reasonLine(card(1, source: .search), topicName: name)), "Matches your search")
        XCTAssertEqual(plain(DiscoverCards.reasonLine(card(1), topicName: name)), "Popular in Books")
    }

    func testSubscribesOnlyWhenTheHandleLeadsToTheCardsChannel() {
        XCTAssertTrue(DiscoverCards.verifyDecision(cardChannelIdHex: hex(1), resolvedHex: hex(1).uppercased()))
        XCTAssertFalse(DiscoverCards.verifyDecision(cardChannelIdHex: hex(1), resolvedHex: hex(2)))
        XCTAssertFalse(DiscoverCards.verifyDecision(cardChannelIdHex: hex(1), resolvedHex: nil))
    }

    func testMixesOneCardAfterEveryNItemsNeverFirstOrLast() {
        XCTAssertTrue(DiscoverCards.mix(itemCount: 30, cardCount: 5, rate: .never).allSatisfy {
            if case .item = $0 { return true } else { return false }
        })
        XCTAssertEqual(DiscoverCards.cardAfter(itemCount: 17, cardCount: 5, rate: .sometimes), [7: 0, 15: 1])
        XCTAssertEqual(DiscoverCards.cardAfter(itemCount: 16, cardCount: 5, rate: .sometimes), [7: 0])
        XCTAssertEqual(DiscoverCards.cardAfter(itemCount: 30, cardCount: 1, rate: .sometimes), [7: 0])
        XCTAssertEqual(DiscoverCards.cardAfter(itemCount: 41, cardCount: 5, rate: .rarely), [19: 0, 39: 1])
    }

    // MARK: - Prefs

    func testPrefsRoundTripIncludingTheDecoyDrawAndTheCache() {
        var p = DiscoverPrefs()
        p.topics = [3, 5]
        p.language = "de"
        p.mixRandom = false
        p.notInterested = [hex(1)]
        p.hiddenTopics = [7]
        p.decoys = DecoyDraw(month: "2026-09", language: "de", order: [1, 2, 3])
        p.lastRefreshMs = 12
        p.lastFetchedKey = "de:3,5"
        p.buckets = ["3:de": CachedBucket(fetchedAtMs: 12, items: [DirectoryItem(channelId: hex(2), listingB64: "x", subscribers: "10+")])]
        p.introDismissed = true
        XCTAssertEqual(DiscoverPrefs.decode(p.encode()), p)
    }

    func testPrefsDropAMalformedFieldAloneAndDefaultTheRest() throws {
        let json = #"{"version":1,"topics":[3,999,3],"language":"English","mixRandom":false,"notInterested":["nothex"],"introDismissed":true}"#
        let p = DiscoverPrefs.decode(Data(json.utf8))
        XCTAssertEqual(p.topics, [3])
        XCTAssertNil(p.language)
        XCTAssertFalse(p.mixRandom)
        XCTAssertEqual(p.notInterested, [])
        XCTAssertTrue(p.introDismissed)
        XCTAssertEqual(DiscoverPrefs.decode(Data(#"{"version":2,"topics":[1]}"#.utf8)), DiscoverPrefs())
    }

    // MARK: - Channel metadata v2 on this device

    func testChannelLogKeepsTheDiscoverFieldsOfTheMetadata() {
        let handle = ChannelHandle(
            id: hex(1), localName: "L", localDescription: "", addedAtMs: 1, handle: "books",
            sharedName: "Books", sharedDescription: "d", sharedUpdatedAtMs: 5,
            sharedTopic: 4, sharedLanguage: "fr", sharedListed: true)
        let back = PersistedChannel(handle: handle).channelHandle
        XCTAssertEqual(back, handle)
        XCTAssertEqual(back.sharedMeta?.listed, true)
        XCTAssertEqual(back.sharedMeta?.topic, 4)
        let kept = ChannelMetaRecord.fresh(name: "New", description: "", nowMs: 9, keeping: back.sharedMeta)
        XCTAssertEqual(kept?.listed, true)
        XCTAssertEqual(kept?.language, "fr")
    }

    // MARK: - Relation of a card to this device (UX spec section 6.2)

    func testACardThisDevicePublishesIsItsOwnEvenWhenAlsoInTheList() {
        XCTAssertEqual(
            DiscoverCards.relation(channelIdHex: hex(1), publishes: [hex(1)], inList: [hex(1)]), .own)
        XCTAssertEqual(DiscoverCards.relationLine(.own), "Your channel")
    }

    func testACardOnlyInTheListIsFollowed() {
        XCTAssertEqual(
            DiscoverCards.relation(channelIdHex: hex(2), publishes: [hex(1)], inList: [hex(1), hex(2)]),
            .following)
        XCTAssertEqual(DiscoverCards.relationLine(.following), "You follow this channel")
    }

    func testAnyOtherCardIsLeftToSubscribe() {
        XCTAssertEqual(
            DiscoverCards.relation(channelIdHex: hex(3), publishes: [hex(1)], inList: [hex(2)]), .none)
        XCTAssertNil(DiscoverCards.relationLine(.none))
    }

    func testTheCaseOfTheHexIdDoesNotMatter() {
        let upper = String(repeating: "AB", count: 16)
        let lower = upper.lowercased()
        XCTAssertEqual(DiscoverCards.relation(channelIdHex: upper, publishes: [lower], inList: []), .own)
        XCTAssertEqual(DiscoverCards.relation(channelIdHex: lower, publishes: [], inList: [upper]), .following)
    }
}
