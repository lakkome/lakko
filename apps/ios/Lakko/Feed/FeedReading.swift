// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import CryptoKit
import Foundation

/// The feed's per-device reading state (`docs/ux/feed.md` sections 4.1, 5
/// and 9 item 8). Mirror of web `feed/feed-marks.ts` and Android
/// `feed/FeedReading.kt`:
///
/// - `hidden`: conversations hidden from feeds ("Hide from feeds"), taken out
///   of every feed and of the suggestions before ranking. Not a chat mute.
/// - `looked`: per feed, the item keys it showed the last time the person
///   left the Feed screen, for the "New" divider.
/// - `channelRead`: per channel, the cursor of the newest post read, in the
///   feed or in the channel. The channel's count of new posts is what lies
///   above it.
///
/// Pure: the codec and every rule are here and tested; ``FeedReadingStore``
/// seals it per account; ``FeedReadingModel`` is what the screens observe.
/// Nothing here leaves the device.
struct FeedReading: Equatable {
    static let version = 1
    /// Most item keys remembered per feed (chosen, as on web).
    static let maxLookedKeys = 2000
    /// Most conversations hidden (chosen): bounds a stored list.
    static let maxHidden = 1000

    var hidden: [String] = []
    var looked: [String: [String]] = [:]
    var channelRead: [String: UInt64] = [:]

    // MARK: - Codec

    private struct Wire: Codable {
        let version: Int?
        let hidden: [String]?
        let looked: [String: [String]]?
        let channelRead: [String: UInt64]?
    }

    static func isConversationId(_ s: String) -> Bool {
        (2...256).contains(s.utf8.count) && s.utf8.count % 2 == 0
            && s.utf8.allSatisfy { (0x30...0x39).contains($0) || (0x61...0x66).contains($0) }
    }

    static func isFeedKey(_ s: String) -> Bool {
        (1...64).contains(s.utf8.count)
            && s.utf8.allSatisfy {
                (0x30...0x39).contains($0) || (0x41...0x5a).contains($0) || (0x61...0x7a).contains($0)
                    || $0 == 0x3a || $0 == 0x5f || $0 == 0x2d
            }
    }

    /// Lenient per entry: a malformed one is dropped alone; an unknown
    /// version reads as empty.
    static func decode(_ data: Data) -> FeedReading {
        guard let wire = try? JSONDecoder().decode(Wire.self, from: data), wire.version == version else {
            return FeedReading()
        }
        var hidden: [String] = []
        for h in wire.hidden ?? [] where isConversationId(h) && !hidden.contains(h) {
            hidden.append(h)
            if hidden.count >= maxHidden { break }
        }
        var looked: [String: [String]] = [:]
        for (k, v) in wire.looked ?? [:] where isFeedKey(k) {
            looked[k] = Array(v.prefix(maxLookedKeys))
        }
        var channelRead: [String: UInt64] = [:]
        for (k, v) in wire.channelRead ?? [:] where ChannelPostLogPayload.isChannelId(k) {
            channelRead[k] = v
        }
        return FeedReading(hidden: hidden, looked: looked, channelRead: channelRead)
    }

    func encode() -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let wire = Wire(version: Self.version, hidden: hidden, looked: looked, channelRead: channelRead)
        return (try? encoder.encode(wire)) ?? Data()
    }

    // MARK: - Hide from feeds

    func hiding(_ id: String) -> FeedReading {
        guard Self.isConversationId(id), !hidden.contains(id) else { return self }
        var next = self
        next.hidden.append(id)
        if next.hidden.count > Self.maxHidden { next.hidden.removeFirst(next.hidden.count - Self.maxHidden) }
        return next
    }

    func showing(_ id: String) -> FeedReading {
        guard hidden.contains(id) else { return self }
        var next = self
        next.hidden.removeAll { $0 == id }
        return next
    }

    /// The sources a feed may draw from: every one not hidden. Applied before
    /// ranking, so a hidden conversation never reaches the core.
    static func withoutHidden(_ sources: [FeedSource], hidden: [String]) -> [FeedSource] {
        guard !hidden.isEmpty else { return sources }
        let set = Set(hidden)
        return sources.filter { !set.contains($0.key) }
    }

    /// The hidden conversations this device still holds, the most recently
    /// hidden first (the "Hidden from feeds" list; `hidden` is kept in hide
    /// order).
    static func hiddenSources(_ sources: [FeedSource], hidden: [String]) -> [FeedSource] {
        hidden.reversed().compactMap { id in sources.first { $0.key == id } }
    }

    // MARK: - "New" divider and "Show N new"

    /// Remember what a feed showed when the person left it.
    func recordingLooked(feedKey: String, itemKeys: [String]) -> FeedReading {
        guard Self.isFeedKey(feedKey) else { return self }
        var next = self
        next.looked[feedKey] = Array(itemKeys.prefix(Self.maxLookedKeys))
        return next
    }

    /// Where the "New" divider goes: the index (in drawn order) of the first
    /// item that was not in this feed the last time, or `nil` for no divider —
    /// the first visit (`previous` nil) or nothing new.
    static func dividerIndex(itemKeys: [String], previous: [String]?) -> Int? {
        guard let previous else { return nil }
        let before = Set(previous)
        return itemKeys.firstIndex { !before.contains($0) }
    }

    /// "Show N new": items in the waiting result that the drawn list lacks.
    static func pendingNewCount(shown: [String]?, pending: [String]?) -> Int {
        guard let shown, let pending else { return 0 }
        let drawn = Set(shown)
        return pending.filter { !drawn.contains($0) }.count
    }

    // MARK: - Channel read state

    /// The newest post read in a channel moves forward only.
    func advancingChannelRead(_ channelIdHex: String, cursor: UInt64) -> FeedReading {
        guard ChannelPostLogPayload.isChannelId(channelIdHex), cursor > 0 else { return self }
        if let current = channelRead[channelIdHex], current >= cursor { return self }
        var next = self
        next.channelRead[channelIdHex] = cursor
        return next
    }

    /// Mark all read in Chats (`docs/ux/chat-folders.md` 2.2) on channels:
    /// each of `channelIds` is read through the newest post this device holds
    /// of it (its slot's cursor, never below its newest post), as opening it
    /// to its newest end would. A channel with no slot is left as it is.
    func markingChannelsRead(_ channelIds: [String], slots: [String: ChannelPostSlot]) -> FeedReading {
        channelIds.reduce(self) { reading, id in
            guard let slot = slots[id] else { return reading }
            return reading.advancingChannelRead(id, cursor: max(slot.cursor, slot.posts.map(\.cursor).max() ?? 0))
        }
    }

    /// Give every channel that has no marker one at the newest post this
    /// device holds of it now (its slot's cursor, never below its newest
    /// post), so a fresh subscription starts with no count. A channel already
    /// marked is left alone.
    func seedingChannelReads(_ slots: [String: ChannelPostSlot]) -> FeedReading {
        var next = self
        for (hex, slot) in slots where ChannelPostLogPayload.isChannelId(hex) && next.channelRead[hex] == nil {
            next.channelRead[hex] = max(slot.cursor, slot.posts.map(\.cursor).max() ?? 0)
        }
        return next
    }

    /// A channel's count of new posts: posts newer than the newest one read.
    /// A channel this device posts to counts none (its posts are the person's
    /// own); a channel not yet marked counts none (it is about to be seeded).
    static func channelUnreadCount(postCursors: [UInt64], newestRead: UInt64?, own: Bool) -> Int {
        guard !own, let newestRead else { return 0 }
        return postCursors.filter { $0 > newestRead }.count
    }
}

// MARK: - Icons (UX spec section 4.1)

/// The twelve feed icons, in the index order the rule file carries, each
/// mapped to an icon of the Lakko set. An index this client does not know draws the
/// default. Presets keep fixed icons of their own.
enum FeedIcons {
    struct Entry: Equatable {
        let icon: LkIcon
        /// The catalog key of the icon's spoken name (ADR-0061).
        let labelKey: String
        /// The icon's name in the interface language, read when shown.
        var label: String { Localization.text(labelKey) }
    }

    static let all: [Entry] = [
        Entry(icon: .feed, labelKey: "feed.icon.feed"),
        Entry(icon: .star, labelKey: "feed.icon.star"),
        Entry(icon: .heart, labelKey: "feed.icon.heart"),
        Entry(icon: .bolt, labelKey: "feed.icon.lightning"),
        Entry(icon: .book, labelKey: "feed.icon.book"),
        Entry(icon: .code, labelKey: "feed.icon.code"),
        Entry(icon: .musicNote, labelKey: "feed.icon.music"),
        Entry(icon: .camera, labelKey: "feed.icon.camera"),
        Entry(icon: .globe, labelKey: "feed.icon.globe"),
        Entry(icon: .chat, labelKey: "feed.icon.speech_bubble"),
        Entry(icon: .bell, labelKey: "feed.icon.bell"),
        Entry(icon: .flag, labelKey: "feed.icon.flag"),
    ]

    static let defaultIndex: UInt8 = 0

    /// The icon for a rule's index; unknown draws the default.
    static func icon(_ index: UInt8) -> LkIcon {
        Int(index) < all.count ? all[Int(index)].icon : all[Int(defaultIndex)].icon
    }

    static func label(_ index: UInt8) -> String {
        Int(index) < all.count ? all[Int(index)].label : all[Int(defaultIndex)].label
    }

    /// The presets' fixed icons: Latest the feed, Popular lightning, For me a
    /// person, Discover the globe.
    static func presetIcon(_ key: FeedPresetKey) -> LkIcon {
        switch key {
        case .latest: return .feed
        case .popular: return .bolt
        case .forMe: return .person
        }
    }

    static let discoverIcon = LkIcon.globe
}

// MARK: - Store

/// Sealed per-account file behind ``FeedReading``: the ``FeedStore`` posture
/// (AES-256-GCM under a Keychain-resident wrap key, device-bound, excluded
/// from backup), under `Application Support/lakko-feed-reading/`.
final class FeedReadingStore {

    static let wrapKeyService: String = "me.lakko.feed-reading-wrap.v1"
    private static let wrapKeyAccount: String = "wrap-key"
    private static let subdir: String = "lakko-feed-reading"

    init() {}

    func load(accountIdB64: String) -> FeedReading {
        guard let url = fileURL(accountIdB64: accountIdB64),
              let combined = try? Data(contentsOf: url),
              let keyBytes = KeychainWrapKey.read(
                service: Self.wrapKeyService, account: Self.wrapKeyAccount),
              let sealed = try? AES.GCM.SealedBox(combined: combined),
              let plaintext = try? AES.GCM.open(sealed, using: SymmetricKey(data: keyBytes))
        else { return FeedReading() }
        return FeedReading.decode(plaintext)
    }

    /// Throws ``AccountStoreGate/Sealed`` once the account's session has
    /// ended: a feed screen still on the way out must not write it back.
    func save(accountIdB64: String, reading: FeedReading) throws {
        let gate = try AccountStoreGate.enterOrEnd(accountIdB64)
        defer { gate.leave() }
        guard let url = fileURL(accountIdB64: accountIdB64) else {
            throw CocoaError(.fileWriteUnknown)
        }
        let wrapKey = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        guard let combined = try AES.GCM.seal(reading.encode(), using: wrapKey).combined else {
            throw CocoaError(.fileWriteUnknown)
        }
        let dir = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        SharedStorage.excludeFromBackup(dir)
        try combined.write(to: url, options: .atomic)
        SharedStorage.excludeFromBackup(url)
    }

    /// Erase an account's reading state. Called from `forgetDevice`.
    func delete(accountIdB64: String) {
        if let url = fileURL(accountIdB64: accountIdB64) {
            try? FileManager.default.removeItem(at: url)
        }
    }

    private func fileURL(accountIdB64: String) -> URL? {
        guard let appSupport = try? FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true)
        else { return nil }
        return appSupport
            .appendingPathComponent(Self.subdir)
            .appendingPathComponent("\(accountIdB64.replacingOccurrences(of: "/", with: "_")).bin")
    }
}

// MARK: - Model

/// The one in-memory copy of the reading state for the session, shared by the
/// feed, the channel view, the channel-post sync and the conversation list.
/// Every write is a read-modify-write on the main actor, written through to
/// ``FeedReadingStore``.
@MainActor
final class FeedReadingModel: ObservableObject {

    @Published private(set) var reading: FeedReading
    /// Per channel id, its count of new posts (the row badge).
    @Published private(set) var channelUnread: [String: Int] = [:]
    /// Per channel id, its newest post with readable text: the Chats row's
    /// second line (UX navigation section 2).
    @Published private(set) var channelLastPost: [String: PersistedChannelPost] = [:]

    private let accountIdB64: String
    private let store: FeedReadingStore
    private var cancellables: Set<AnyCancellable> = []

    init(accountIdB64: String, store: FeedReadingStore = FeedReadingStore()) {
        self.accountIdB64 = accountIdB64
        self.store = store
        self.reading = store.load(accountIdB64: accountIdB64)
    }

    /// Change the state and write it; a mutation that changes nothing writes
    /// nothing. Returns `false` when the write failed (the in-memory state is
    /// left as it was).
    @discardableResult
    func update(_ mutate: (FeedReading) -> FeedReading) -> Bool {
        let next = mutate(reading)
        guard next != reading else { return true }
        do {
            try store.save(accountIdB64: accountIdB64, reading: next)
        } catch {
            NSLog("[feed] saving the reading state failed")
            return false
        }
        reading = next
        return true
    }

    func advanceChannelRead(_ channelIdHex: String, cursor: UInt64) {
        update { $0.advancingChannelRead(channelIdHex, cursor: cursor) }
    }

    /// The post log as last seen (``bind(postSync:channels:)``): what Mark
    /// all read reads channels through.
    private var latestSlots: [String: ChannelPostSlot] = [:]

    /// Mark all read on channels (``FeedReading/markingChannelsRead(_:slots:)``).
    func markChannelsRead(_ channelIds: [String]) {
        guard !channelIds.isEmpty else { return }
        let slots = latestSlots
        update { $0.markingChannelsRead(channelIds, slots: slots) }
    }

    /// Keep ``channelUnread`` current from the post log and the channels this
    /// device publishes to.
    func bind(postSync: ChannelPostSync, channels: ChannelsUiState) {
        Publishers.CombineLatest3(
            postSync.$log,
            $reading,
            channels.objectWillChange.map { _ in () }.prepend(()).eraseToAnyPublisher()
        )
        .debounce(for: .milliseconds(100), scheduler: RunLoop.main)
        .sink { [weak self, weak channels] log, reading, _ in
            guard let self, let channels else { return }
            self.latestSlots = log.channels
            let own = Set(channels.keysByChannel.keys)
            var counts: [String: Int] = [:]
            for c in channels.channels {
                let n = FeedReading.channelUnreadCount(
                    postCursors: log.channels[c.id]?.posts.map(\.cursor) ?? [],
                    newestRead: reading.channelRead[c.id],
                    own: own.contains(c.id))
                if n > 0 { counts[c.id] = n }
            }
            if counts != self.channelUnread { self.channelUnread = counts }
            var last: [String: PersistedChannelPost] = [:]
            for c in channels.channels {
                if let post = log.channels[c.id]?.posts.last(where: {
                    !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                }) {
                    last[c.id] = post
                }
            }
            if last != self.channelLastPost { self.channelLastPost = last }
        }
        .store(in: &cancellables)
    }
}

// MARK: - Item menu helpers (UX spec section 5)

/// The pure parts of the feed's item actions. Mirror of web
/// `feed/feed-forward.ts`.
enum FeedForward {

    /// Forwarding a channel post: its text, attributed to the channel by name,
    /// and no origin (the origin reference names a group; a channel is not
    /// one). A blank title carries no name ("Forwarded").
    static func channelPayload(text: String, channelTitle: String) -> (text: String, forward: ForwardRef) {
        let name = channelTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        return (text, ForwardRef(author: name.isEmpty ? nil : name, originGroupIdHex: nil, originMessageId: nil))
    }

    /// A channel post still counts toward the channel's new posts, so the
    /// feed's dwell can read it.
    static func channelPostWantsSeen(cursor: Int64, newestRead: UInt64?) -> Bool {
        cursor > 0 && UInt64(cursor) > (newestRead ?? 0)
    }

    /// Copy is offered only when there is text to copy.
    static func canCopy(_ message: FeedMessage) -> Bool {
        !message.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// Reply is offered for group messages only: a channel is one-way.
    static func canReply(_ ref: FeedCandidateRef) -> Bool {
        ref.source.kind == .group && ref.message.messageId > 0
    }
}
