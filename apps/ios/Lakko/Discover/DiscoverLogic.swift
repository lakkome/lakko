// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0059 sections 3-5, `docs/ux/feed.md` section 6 — the pure parts of
// Discover. Mirrors of web `discover/discover-store.ts` (the prefs codec),
// `discover/decoys.ts` and `discover/cards.ts`: same rounds, same weights,
// same copy, so the three clients order the same cards the same way.

// MARK: - Prefs

/// One cached bucket (only picked buckets are kept; decoys are discarded).
struct CachedBucket: Equatable, Codable {
    let fetchedAtMs: Int64
    let items: [DirectoryItem]
}

/// The decoy order this device uses for one month and one language.
struct DecoyDraw: Equatable, Codable {
    /// `YYYY-MM`, UTC.
    let month: String
    let language: String
    /// Every topic id, in a random order drawn once for the month.
    let order: [Int]
}

/// Discover's per-account preferences and caches on this device.
struct DiscoverPrefs: Equatable {
    static let version = 1
    /// Most not-interested channels kept (chosen).
    static let maxNotInterested = 500

    var topics: [Int] = []
    /// `nil` = not chosen yet; the app language is proposed.
    var language: String?
    var mixRandom: Bool = true
    var notInterested: [String] = []
    var hiddenTopics: [Int] = []
    var decoys: DecoyDraw?
    var lastRefreshMs: Int64?
    /// ``DiscoverDecoys/pickedKey(_:language:)`` of the last refresh.
    var lastFetchedKey: String?
    /// `<topic>:<language>` -> bucket.
    var buckets: [String: CachedBucket] = [:]
    /// The pick-topics card was answered with "Not now".
    var introDismissed: Bool = false

    static func bucketKey(topic: Int, language: String) -> String { "\(topic):\(language)" }

    private struct Wire: Codable {
        var version: Int?
        var topics: [Int]?
        var language: String?
        var mixRandom: Bool?
        var notInterested: [String]?
        var hiddenTopics: [Int]?
        var decoys: DecoyDraw?
        var lastRefreshMs: Int64?
        var lastFetchedKey: String?
        var buckets: [String: CachedBucket]?
        var introDismissed: Bool?
    }

    static func isTopic(_ t: Int) -> Bool { (0...255).contains(t) }

    static func isLanguage(_ s: String) -> Bool {
        (2...3).contains(s.utf8.count) && s.utf8.allSatisfy { (0x61...0x7a).contains($0) }
    }

    func encode() -> Data {
        let wire = Wire(
            version: Self.version, topics: topics, language: language, mixRandom: mixRandom,
            notInterested: notInterested, hiddenTopics: hiddenTopics, decoys: decoys,
            lastRefreshMs: lastRefreshMs, lastFetchedKey: lastFetchedKey, buckets: buckets,
            introDismissed: introDismissed)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return (try? encoder.encode(wire)) ?? Data()
    }

    /// Lenient per field: a malformed field falls back to its default alone.
    static func decode(_ data: Data) -> DiscoverPrefs {
        guard let w = try? JSONDecoder().decode(Wire.self, from: data), w.version == version else {
            return DiscoverPrefs()
        }
        var p = DiscoverPrefs()
        p.topics = dedup((w.topics ?? []).filter(isTopic))
        p.language = w.language.flatMap { isLanguage($0) ? $0 : nil }
        p.mixRandom = w.mixRandom ?? true
        p.notInterested = Array((w.notInterested ?? []).filter(SharedChannels.isHexId).suffix(maxNotInterested))
        p.hiddenTopics = dedup((w.hiddenTopics ?? []).filter(isTopic))
        if let d = w.decoys, d.order.allSatisfy(isTopic), d.month.count == 7 { p.decoys = d }
        p.lastRefreshMs = w.lastRefreshMs
        p.lastFetchedKey = w.lastFetchedKey
        for (k, v) in w.buckets ?? [:] {
            let parts = k.split(separator: ":")
            guard parts.count == 2, let t = Int(parts[0]), isTopic(t), isLanguage(String(parts[1])) else { continue }
            p.buckets[k] = CachedBucket(
                fetchedAtMs: v.fetchedAtMs,
                items: Array(v.items.filter { SharedChannels.isHexId($0.channelId) }.prefix(100)))
        }
        p.introDismissed = w.introDismissed ?? false
        return p
    }

    private static func dedup(_ xs: [Int]) -> [Int] {
        var seen = Set<Int>()
        return xs.filter { seen.insert($0).inserted }
    }
}

// MARK: - Decoys

/// Which directory buckets a Discover refresh fetches (ADR-0059 section 5,
/// open question 1 as decided on 2026-09-29): the picked topics in rounds of
/// ``roundSize``, each padded (unless "Mix in random topics" is off) with
/// decoy topics in the same language and shuffled. The decoys are a stable
/// random set per device per month, so they repeat every day like the picked
/// buckets and repetition does not separate them.
///
/// Pure: the caller supplies the clock and the random source.
enum DiscoverDecoys {
    /// Buckets per round (a chosen number).
    static let roundSize = 8
    /// A refresh at most this often (once a day).
    static let refreshIntervalMs: Int64 = 24 * 60 * 60 * 1000

    /// `YYYY-MM` of `nowMs`, UTC.
    static func monthKey(nowMs: Int64) -> String {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC") ?? .current
        let date = Date(timeIntervalSince1970: TimeInterval(nowMs) / 1000)
        let c = calendar.dateComponents([.year, .month], from: date)
        return String(format: "%04d-%02d", c.year ?? 1970, c.month ?? 1)
    }

    /// Fisher-Yates over a copy; `random` returns values in `[0, 1)`.
    static func shuffled<T>(_ items: [T], random: () -> Double) -> [T] {
        var out = items
        guard out.count > 1 else { return out }
        for i in stride(from: out.count - 1, to: 0, by: -1) {
            let j = min(i, Int(random() * Double(i + 1)))
            out.swapAt(i, j)
        }
        return out
    }

    /// The decoy draw to use now: `held` when it is for this month and
    /// language, else a fresh random order of `allTopics` (`fresh == true`,
    /// the caller persists it).
    static func current(
        held: DecoyDraw?, allTopics: [Int], language: String, nowMs: Int64, random: () -> Double
    ) -> (draw: DecoyDraw, fresh: Bool) {
        let month = monthKey(nowMs: nowMs)
        if let held, held.month == month, held.language == language { return (held, false) }
        return (DecoyDraw(month: month, language: language, order: shuffled(allTopics, random: random)), true)
    }

    /// The rounds of topic ids to fetch. No picked topics, no rounds: the
    /// shortlist is not downloaded at all.
    static func planRounds(
        picked: [Int], decoys: DecoyDraw, mixRandom: Bool, random: () -> Double
    ) -> [[Int]] {
        var seen = Set<Int>()
        let unique = picked.filter { seen.insert($0).inserted }
        guard !unique.isEmpty else { return [] }
        let pool = decoys.order.filter { !seen.contains($0) }
        var rounds: [[Int]] = []
        var next = 0
        var i = 0
        while i < unique.count {
            var round = Array(unique[i..<min(i + roundSize, unique.count)])
            if mixRandom, !pool.isEmpty {
                let want = min(roundSize - round.count, pool.count)
                var chosen: [Int] = []
                while chosen.count < want {
                    let t = pool[next % pool.count]
                    if !chosen.contains(t) { chosen.append(t) }
                    next += 1
                }
                round.append(contentsOf: chosen)
            }
            rounds.append(shuffled(round, random: random))
            i += roundSize
        }
        return rounds
    }

    /// Whether the shortlist is due: never fetched, a day old, or a different
    /// set of picked buckets than the one last fetched.
    static func refreshDue(lastRefreshMs: Int64?, lastFetchedKey: String?, pickedKey: String, nowMs: Int64) -> Bool {
        guard let last = lastRefreshMs, lastFetchedKey == pickedKey else { return true }
        return nowMs - last >= refreshIntervalMs || nowMs < last
    }

    /// A stable key for a set of picked topics in a language.
    static func pickedKey(_ picked: [Int], language: String) -> String {
        "\(language):" + Set(picked).sorted().map(String.init).joined(separator: ",")
    }
}

// MARK: - Cards

/// One directory item as the server sends it. The listing bytes are the
/// owner's; decode them with the shared codec, never trust the server's
/// channel id over the listing's.
struct DirectoryItem: Equatable, Codable {
    let channelId: String
    let listingB64: String
    /// A band: `<10`, `10+`, `100+`, `1k+`, `10k+`, `100k+`.
    let subscribers: String

    enum CodingKeys: String, CodingKey {
        case channelId = "channel_id"
        case listingB64 = "listing_b64"
        case subscribers
    }
}

/// A decoded listing (the core's `FfiChannelListing`, structurally).
struct DecodedListing: Equatable {
    let channelIdHex: String
    let handle: String
    let name: String
    let description: String
    let topic: Int
    let language: String
}

/// People in one group who follow a channel.
struct GroupFollowers: Equatable {
    let groupIdHex: String
    let groupTitle: String
    /// Distinct members of that group sharing it.
    let count: Int
    /// Display names of those among them who are the person's contacts.
    let contactNames: [String]
}

/// Where a card came from.
enum CardSource: Equatable {
    case bucket
    case search
    case group
}

/// One suggestion.
struct DirectoryCard: Equatable, Identifiable {
    /// 32-char lowercase hex.
    let channelIdHex: String
    /// The claimed handle. Never shown as a link until verified.
    let handle: String
    let name: String
    /// `nil` until loaded (a group-shared card fetches it when opened).
    let description: String?
    let topic: Int?
    let language: String?
    /// One of ``DiscoverCards/subscriberBands``, or `nil` when not known.
    let band: String?
    let source: CardSource
    /// Position in the server's list (bucket or search), 0 = first.
    let serverRank: Int
    let followers: GroupFollowers?
    /// ADR-0089 — the signed listing as shown, for a report's evidence.
    var listingWire: Data? = nil

    var id: String { channelIdHex }
}

/// What the scorer and the filter read about the person.
struct CardContext {
    let pickedTopics: Set<Int>
    let language: String
    let notInterested: Set<String>
    let hiddenTopics: Set<Int>
    /// Channels this device already follows or publishes.
    let following: Set<String>
    /// Cards found gone this session (subscribe refused).
    let gone: Set<String>
}

/// How often a suggestion appears in a feed (the rule's `suggestions`).
enum SuggestionRate: Equatable {
    case never
    case rarely
    case sometimes
}

/// A slot in a feed that mixes cards in.
enum MixedSlot: Equatable {
    case item(Int)
    case card(Int)
}

/// How this device stands to a card's channel (UX spec section 6.2).
enum CardRelation: Equatable {
    case own
    case following
    case none
}

enum DiscoverCards {
    /// The published subscriber bands, smallest first (ADR-0059 section 4).
    static let subscriberBands = ["<10", "10+", "100+", "1k+", "10k+", "100k+"]

    /// Directory items to cards. An item whose listing does not decode, or
    /// names another channel than the item's id, is dropped.
    static func fromItems(
        _ items: [DirectoryItem], source: CardSource, decode: (String) -> DecodedListing?
    ) -> [DirectoryCard] {
        var out: [DirectoryCard] = []
        var seen = Set<String>()
        for (i, item) in items.enumerated() {
            guard let listing = decode(item.listingB64) else { continue }
            let id = item.channelId.lowercased()
            guard listing.channelIdHex == id, seen.insert(id).inserted else { continue }
            out.append(DirectoryCard(
                channelIdHex: id,
                handle: listing.handle,
                name: listing.name,
                description: listing.description,
                topic: listing.topic,
                language: listing.language,
                band: subscriberBands.contains(item.subscribers) ? item.subscribers : nil,
                source: source,
                serverRank: i,
                followers: nil,
                listingWire: Data(base64UrlNoPadListing: item.listingB64)))
        }
        return out
    }

    /// Whether a card may be shown at all.
    static func visible(_ card: DirectoryCard, _ ctx: CardContext) -> Bool {
        if ctx.notInterested.contains(card.channelIdHex) { return false }
        if ctx.following.contains(card.channelIdHex) { return false }
        if ctx.gone.contains(card.channelIdHex) { return false }
        if let t = card.topic, ctx.hiddenTopics.contains(t) { return false }
        return true
    }

    /// The device-side score (chosen weights). Higher is better.
    static func score(_ card: DirectoryCard, _ ctx: CardContext) -> Int {
        var score = 0
        if let t = card.topic, ctx.pickedTopics.contains(t) { score += 100 }
        if let l = card.language, l == ctx.language { score += 50 }
        if let f = card.followers { score += 40 * min(f.count, 5) }
        if let band = card.band, let idx = subscriberBands.firstIndex(of: band) { score += 10 * idx }
        score += max(0, 100 - card.serverRank)
        return score
    }

    /// Merge cards from every source (a channel seen twice becomes one card
    /// with the richest fields and the best server rank), drop the hidden
    /// ones and order the rest: score, then name, then id.
    static func rank(_ cards: [DirectoryCard], _ ctx: CardContext) -> [DirectoryCard] {
        var order: [String] = []
        var merged: [String: DirectoryCard] = [:]
        for c in cards {
            guard let held = merged[c.channelIdHex] else {
                order.append(c.channelIdHex)
                merged[c.channelIdHex] = c
                continue
            }
            let base = held.description != nil ? held : c
            let followers = held.followers ?? c.followers
            merged[c.channelIdHex] = DirectoryCard(
                channelIdHex: base.channelIdHex, handle: base.handle, name: base.name,
                description: base.description, topic: base.topic, language: base.language,
                band: base.band,
                source: followers != nil ? .group : base.source,
                serverRank: min(held.serverRank, c.serverRank),
                followers: followers,
                listingWire: base.listingWire ?? held.listingWire ?? c.listingWire)
        }
        return order.compactMap { merged[$0] }
            .filter { visible($0, ctx) }
            .map { ($0, score($0, ctx)) }
            .sorted { a, b in
                if a.1 != b.1 { return a.1 > b.1 }
                if a.0.name != b.0.name { return a.0.name < b.0.name }
                return a.0.channelIdHex < b.0.channelIdHex
            }
            .map { $0.0 }
    }

    /// How this device stands to a card's channel (UX spec section 6.2). A
    /// channel this device can post to (owner or co-publisher) is its own;
    /// one it only holds in its list is followed. Ids compare lowercased.
    static func relation(channelIdHex: String, publishes: Set<String>, inList: Set<String>) -> CardRelation {
        let id = channelIdHex.lowercased()
        if publishes.contains(where: { $0.lowercased() == id }) { return .own }
        if inList.contains(where: { $0.lowercased() == id }) { return .following }
        return .none
    }

    /// The reason line for a card the device already holds, `nil` otherwise.
    static func relationLine(_ relation: CardRelation) -> String? {
        switch relation {
        case .own: return Localization.text("channels.row.own")
        case .following: return Localization.text("discover.card.following")
        case .none: return nil
        }
    }

    /// The reason line under a card (UX spec section 6). Names only contacts.
    static func reasonLine(_ card: DirectoryCard, topicName: (Int) -> String) -> String {
        if let f = card.followers {
            if let named = f.contactNames.first {
                let others = f.count - 1
                if others <= 0 {
                    return Localization.text(
                        "discover.reason.followed_by", texts: ["name": named, "group": f.groupTitle])
                }
                return Localization.text(
                    "discover.reason.followed_by_others",
                    texts: ["name": named, "group": f.groupTitle],
                    counts: ["count": UInt64(others)])
            }
            return Localization.text(
                "discover.reason.followed_count",
                texts: ["group": f.groupTitle],
                counts: ["count": UInt64(max(0, f.count))])
        }
        if card.source == .search { return Localization.text("discover.reason.search") }
        let topic = card.topic.map(topicName) ?? Localization.text("discover.topic.other")
        return Localization.text("discover.reason.popular", texts: ["topic": topic])
    }

    /// Before subscribing from a card (ADR-0059 section 3): the card's handle,
    /// resolved through the normal OPRF channel lookup, must name the card's
    /// channel. Anything else (no such handle, another channel) is gone.
    static func verifyDecision(cardChannelIdHex: String, resolvedHex: String?) -> Bool {
        guard let resolvedHex else { return false }
        return resolvedHex.lowercased() == cardChannelIdHex.lowercased()
    }

    /// One card after this many feed items (chosen).
    static func every(_ rate: SuggestionRate) -> Int {
        switch rate {
        case .never: return 0
        case .rarely: return 20
        case .sometimes: return 8
        }
    }

    /// Interleave up to `cardCount` cards into `itemCount` feed items: one
    /// card after every N items, never at the very top and never after the
    /// last item.
    static func mix(itemCount: Int, cardCount: Int, rate: SuggestionRate) -> [MixedSlot] {
        var out: [MixedSlot] = []
        let every = every(rate)
        var card = 0
        for i in 0..<max(0, itemCount) {
            out.append(.item(i))
            let placed = i + 1
            if every > 0, card < cardCount, placed % every == 0, placed < itemCount {
                out.append(.card(card))
                card += 1
            }
        }
        return out
    }

    /// Item index -> the card shown after it.
    static func cardAfter(itemCount: Int, cardCount: Int, rate: SuggestionRate) -> [Int: Int] {
        var out: [Int: Int] = [:]
        var last = -1
        for slot in mix(itemCount: itemCount, cardCount: cardCount, rate: rate) {
            switch slot {
            case .item(let i): last = i
            case .card(let c): out[last] = c
            }
        }
        return out
    }
}

// MARK: - Languages

/// The languages Discover offers (ISO 639, a chosen set of common languages).
enum DiscoverLanguages {
    static let codes = [
        "en", "es", "pt", "fr", "de", "it", "nl", "pl", "uk", "ru", "tr", "ar",
        "fa", "hi", "id", "zh", "ja", "ko",
    ]

    /// The app language as an ISO 639 code, `en` when nothing usable.
    static func appLanguage(_ preferred: String? = Locale.preferredLanguages.first) -> String {
        let code = (preferred ?? "").lowercased().split(whereSeparator: { $0 == "-" || $0 == "_" }).first
            .map(String.init) ?? ""
        return DiscoverPrefs.isLanguage(code) ? code : "en"
    }

    /// The offered list, the given languages first when missing from it.
    static func options(_ extra: String?...) -> [String] {
        var out = codes
        for e in extra {
            if let e, DiscoverPrefs.isLanguage(e), !out.contains(e) { out.insert(e, at: 0) }
        }
        return out
    }

    /// "English", "Deutsch", ... in the language itself; the code as a fallback.
    static func label(_ code: String) -> String {
        guard let name = Locale(identifier: code).localizedString(forLanguageCode: code), name != code else {
            return code
        }
        return name.prefix(1).uppercased() + name.dropFirst()
    }
}
