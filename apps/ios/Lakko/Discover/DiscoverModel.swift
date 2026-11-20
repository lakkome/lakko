// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// Discover's copy (`docs/ux/feed.md` sections 6 and 7), from the catalog
/// (ADR-0061); read when shown, so a language switch reaches it.
enum DiscoverCopy {
    static var pickTitle: String { Localization.text("discover.pick.title") }
    static var pickBody: String { Localization.text("discover.pick.body") }
    static var mixLabel: String { Localization.text("discover.mix.label") }
    static var unavailable: String { Localization.text("discover.unavailable") }
    static var noResults: String { Localization.text("discover.search.no_results") }
    static var gone: String { Localization.text("discover.gone") }
    static var rateLimited: String { AnonTokenCopy.searchLimited }
    static var searchTooShort: String { Localization.text("discover.search.too_short") }
    static var nothingYet: String { Localization.text("discover.nothing_yet") }
}

/// A Discover topic's name in the interface language, by its id. The core's
/// `channelTopicName` is English; only its ids are used. Any id not in the
/// table reads "Other".
enum DiscoverTopics {
    private static let keys: [UInt8: String] = [
        0: "discover.topic.other", 1: "discover.topic.news", 2: "discover.topic.technology",
        3: "discover.topic.science", 4: "discover.topic.business", 5: "discover.topic.finance",
        6: "discover.topic.politics", 7: "discover.topic.sports", 8: "discover.topic.gaming",
        9: "discover.topic.music", 10: "discover.topic.film_tv", 11: "discover.topic.books",
        12: "discover.topic.art_design", 13: "discover.topic.photography", 14: "discover.topic.food",
        15: "discover.topic.travel", 16: "discover.topic.health", 17: "discover.topic.education",
        18: "discover.topic.humour", 19: "discover.topic.lifestyle", 20: "discover.topic.nature",
        21: "discover.topic.religion", 22: "discover.topic.local",
        23: "discover.topic.software_development",
    ]

    static func name(_ id: Int) -> String {
        let key = (0...255).contains(id) ? keys[UInt8(id)] : nil
        return Localization.text(key ?? "discover.topic.other")
    }
}

/// The state of an explicit search.
enum DiscoverSearchState: Equatable {
    case idle
    case busy
    case done(query: String, cards: [DirectoryCard])
    case error(String)
}

/// Discover (`docs/ux/feed.md` section 6, ADR-0059): the Discover chip of the
/// Feed and the source of suggestions other feeds mix in. Mirror of web
/// `discover/Discover.tsx` `createDiscoverModel`.
///
/// Holds the state (per account, sealed on this device, ``DiscoverStore``)
/// and the network: the once-a-day shortlist refresh in rounds of eight
/// buckets padded with the month's decoys (``DiscoverDecoys``), the explicit
/// search (one `directory-search` token; the UI says it is sent), the listing
/// fetch of a group-shared card when it is opened, and the handle check before
/// subscribing (``DiscoverCards/verifyDecision(cardChannelIdHex:resolvedHex:)``).
@MainActor
final class DiscoverModel: ObservableObject {

    struct Deps {
        /// The groups this device is in, for the group-shared source.
        var liveGroups: @MainActor () -> Set<String>
        var groupTitle: @MainActor (_ groupIdHex: String) -> String
        /// A contact's display name for a member key, `nil` for a non-contact.
        var contactName: @MainActor (_ memberKeyB64: String) -> String?
        var ownKeyB64: @MainActor () -> String?
        /// Channels this device already follows or publishes.
        var following: @MainActor () -> Set<String>
        /// Channels this device can post to (holds publisher keys).
        var publishing: @MainActor () -> Set<String>
        /// Resolve a handle through the OPRF channel lookup.
        var resolveHandle: (_ handle: String) async throws -> String?
        /// Conversations hidden from feeds (`docs/ux/feed.md` section 5): a
        /// hidden channel is no suggestion, and a hidden group contributes
        /// no group-shared suggestions.
        var hiddenFromFeeds: @MainActor () -> Set<String> = { [] }
    }

    @Published private(set) var prefs = DiscoverPrefs()
    @Published private(set) var loaded = false
    @Published private(set) var refreshing = false
    @Published private(set) var unavailable = false
    @Published private(set) var search: DiscoverSearchState = .idle
    @Published private(set) var notice: String?
    @Published private(set) var cards: [DirectoryCard] = []
    /// Opens the subscribe screen for a verified card. Set by the Feed
    /// screen, which owns the navigation.
    var onOpen: ((DirectoryCard) -> Void)?
    /// Opens a channel this device already holds. Set by the Feed screen.
    var onOpenChannel: ((String) -> Void)?

    let topics: [FfiChannelTopic]

    private let accountIdB64: String
    private let client: ChannelsClient
    private let store: DiscoverStore
    private let hub: SharedChannelsHub
    private let deps: Deps
    private var gone: Set<String> = []
    private var details: [String: DirectoryItem] = [:]
    private var cancellables: Set<AnyCancellable> = []

    init(
        accountIdB64: String,
        client: ChannelsClient,
        hub: SharedChannelsHub,
        deps: Deps,
        store: DiscoverStore = DiscoverStore()
    ) {
        self.accountIdB64 = accountIdB64
        self.client = client
        self.hub = hub
        self.deps = deps
        self.store = store
        self.topics = channelTopics()
        hub.$row
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.recompute() }
            .store(in: &cancellables)
    }

    /// Load the prefs once.
    func load() {
        guard !loaded else { return }
        hub.bind()
        prefs = store.loadPrefs(accountIdB64: accountIdB64)
        loaded = true
        recompute()
    }

    var language: String { prefs.language ?? DiscoverLanguages.appLanguage() }

    /// Whether a card's channel is this device's own, followed, or neither.
    func relation(_ channelIdHex: String) -> CardRelation {
        DiscoverCards.relation(
            channelIdHex: channelIdHex, publishes: deps.publishing(), inList: deps.following())
    }

    func topicName(_ id: Int) -> String {
        DiscoverTopics.name(id)
    }

    private func persist(_ next: DiscoverPrefs) {
        prefs = next
        do {
            try store.savePrefs(accountIdB64: accountIdB64, prefs: next)
        } catch {
            NSLog("[discover] saving failed")
        }
        recompute()
    }

    private func decode(_ listingB64: String) -> DecodedListing? {
        guard let bytes = Data(base64UrlNoPadListing: listingB64),
              let r = try? decodeChannelListing(bytes: bytes)
        else { return nil }
        return DecodedListing(
            channelIdHex: r.channelId.map { String(format: "%02x", $0) }.joined(),
            handle: r.handle, name: r.name, description: r.description,
            topic: Int(r.topic), language: r.language)
    }

    private func context(following: Set<String>? = nil, picked: Set<Int>? = nil) -> CardContext {
        CardContext(
            pickedTopics: picked ?? Set(prefs.topics),
            language: language,
            notInterested: Set(prefs.notInterested),
            hiddenTopics: Set(prefs.hiddenTopics),
            following: following ?? deps.following(),
            gone: gone)
    }

    private func groupCards() -> [DirectoryCard] {
        let shared = SharedChannels.aggregate(
            row: hub.row, ownKeyB64: deps.ownKeyB64(),
            liveGroups: deps.liveGroups().subtracting(deps.hiddenFromFeeds()))
        return shared.map { s in
            let best = s.byGroup.max { a, b in
                a.value.count != b.value.count ? a.value.count < b.value.count : a.key > b.key
            }
            let names = (best?.value ?? []).compactMap { deps.contactName($0) }
            let listing = details[s.channelIdHex]
            let decoded = listing.flatMap { decode($0.listingB64) }
            return DirectoryCard(
                channelIdHex: s.channelIdHex,
                handle: s.handle,
                name: s.name,
                description: decoded?.description,
                topic: decoded?.topic,
                language: decoded?.language,
                band: listing?.subscribers,
                source: .group,
                serverRank: 1000,
                followers: best.map {
                    GroupFollowers(
                        groupIdHex: $0.key, groupTitle: deps.groupTitle($0.key),
                        count: $0.value.count, contactNames: names)
                },
                listingWire: listing.flatMap { Data(base64UrlNoPadListing: $0.listingB64) })
        }
    }

    /// Rebuild the ranked suggestions from the cache and the shared lists.
    func recompute() {
        guard loaded else { return }
        var fromBuckets: [DirectoryCard] = []
        for t in prefs.topics {
            if let b = prefs.buckets[DiscoverPrefs.bucketKey(topic: t, language: language)] {
                fromBuckets += DiscoverCards.fromItems(b.items, source: .bucket, decode: decode)
            }
        }
        let hidden = deps.hiddenFromFeeds()
        let candidates = (fromBuckets + groupCards()).filter { !hidden.contains($0.channelIdHex) }
        cards = DiscoverCards.rank(candidates, context())
    }

    /// The once-a-day shortlist refresh (or at once when the picked set
    /// changed, or `force`).
    func refresh(force: Bool = false) async {
        load()
        let p = prefs
        guard !refreshing, !p.topics.isEmpty else { return }
        let lang = language
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let key = DiscoverDecoys.pickedKey(p.topics, language: lang)
        guard force || DiscoverDecoys.refreshDue(
            lastRefreshMs: p.lastRefreshMs, lastFetchedKey: p.lastFetchedKey, pickedKey: key, nowMs: now)
        else { return }
        refreshing = true
        defer { refreshing = false }
        let random = { Double.random(in: 0..<1) }
        let (draw, _) = DiscoverDecoys.current(
            held: p.decoys, allTopics: topics.map { Int($0.id) }, language: lang, nowMs: now, random: random)
        let rounds = DiscoverDecoys.planRounds(picked: p.topics, decoys: draw, mixRandom: p.mixRandom, random: random)
        let picked = Set(p.topics)
        var buckets: [String: CachedBucket] = [:]
        var asked = 0
        var failures = 0
        let client = self.client
        for round in rounds {
            let results = await withTaskGroup(of: (Int, [DirectoryItem]?).self) { group in
                for topic in round {
                    group.addTask { (topic, try? await client.fetchBucket(topic: topic, language: lang)) }
                }
                var out: [(Int, [DirectoryItem]?)] = []
                for await r in group { out.append(r) }
                return out
            }
            for (topic, items) in results {
                asked += 1
                guard let items else {
                    failures += 1
                    continue
                }
                // A decoy's answer is discarded: only picked buckets are kept.
                if picked.contains(topic) {
                    buckets[DiscoverPrefs.bucketKey(topic: topic, language: lang)] =
                        CachedBucket(fetchedAtMs: now, items: items)
                }
            }
        }
        // The screen was left mid-refresh: every bucket still in flight reads
        // as a failure, which is not the directory being unavailable. Nothing
        // is filed, so the refresh is still due on the next visit.
        if Task.isCancelled { return }
        var next = prefs
        next.decoys = draw
        if asked > 0, failures == asked {
            unavailable = true
            persist(next)
            return
        }
        unavailable = false
        // Keep a previously cached picked bucket this round failed to refresh.
        for t in picked {
            let k = DiscoverPrefs.bucketKey(topic: t, language: lang)
            if buckets[k] == nil, let old = p.buckets[k] { buckets[k] = old }
        }
        next.buckets = buckets
        next.lastRefreshMs = now
        next.lastFetchedKey = key
        persist(next)
    }

    func savePicks(_ topics: [Int], language: String) async {
        var next = prefs
        var seen = Set<Int>()
        next.topics = topics.filter { seen.insert($0).inserted }
        next.language = language
        next.introDismissed = true
        persist(next)
        await refresh(force: true)
    }

    func dismissIntro() {
        var next = prefs
        next.introDismissed = true
        persist(next)
    }

    func setMixRandom(_ on: Bool) {
        var next = prefs
        next.mixRandom = on
        persist(next)
    }

    func notInterested(_ channelIdHex: String) {
        var next = prefs
        next.notInterested = Array(
            (next.notInterested.filter { $0 != channelIdHex } + [channelIdHex]).suffix(DiscoverPrefs.maxNotInterested))
        persist(next)
    }

    func hideTopic(_ topic: Int) {
        var next = prefs
        if !next.hiddenTopics.contains(topic) { next.hiddenTopics.append(topic) }
        persist(next)
    }

    func clearSearch() { search = .idle }

    func runSearch(_ query: String) async {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard q.count >= 2 else {
            search = .error(DiscoverCopy.searchTooShort)
            return
        }
        search = .busy
        do {
            let items = try await client.searchDirectory(query: String(q.prefix(64)))
            let found = DiscoverCards.rank(
                DiscoverCards.fromItems(items, source: .search, decode: decode),
                context(following: [], picked: []))
            search = .done(query: q, cards: found)
        } catch {
            search = .error(Self.searchErrorText(error))
        }
    }

    /// The line for a failed search (UX spec section 7).
    static func searchErrorText(_ error: Error) -> String {
        if AnonTokenUnavailable.isQuotaExhausted(error) { return DiscoverCopy.rateLimited }
        if let api = error as? ChannelsApiError, api.status == 429 { return DiscoverCopy.rateLimited }
        return DiscoverCopy.unavailable
    }

    /// Fetch the public listing of a group-shared card (description, topic).
    func openCard(_ card: DirectoryCard) async {
        guard card.description == nil else { return }
        do {
            guard let item = try await client.fetchListing(channelIdHex: card.channelIdHex) else {
                gone.insert(card.channelIdHex)
                notice = DiscoverCopy.gone
                recompute()
                return
            }
            details[card.channelIdHex] = item
            recompute()
        } catch {
            notice = DiscoverCopy.unavailable
        }
    }

    /// Verify the handle, then hand the card to the subscribe screen.
    func subscribe(_ card: DirectoryCard) async {
        notice = nil
        let resolved: String?
        do {
            resolved = try await deps.resolveHandle(card.handle)
        } catch {
            notice = AnonTokenUnavailable.isQuotaExhausted(error)
                ? AnonTokenCopy.searchAllowanceUsed : DiscoverCopy.unavailable
            return
        }
        guard DiscoverCards.verifyDecision(cardChannelIdHex: card.channelIdHex, resolvedHex: resolved) else {
            gone.insert(card.channelIdHex)
            notice = DiscoverCopy.gone
            recompute()
            if case .done(let q, let found) = search {
                search = .done(query: q, cards: found.filter { $0.channelIdHex != card.channelIdHex })
            }
            return
        }
        onOpen?(card)
    }
}
