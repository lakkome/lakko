// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import UIKit
import UserNotifications

/// Channel post notifications (`docs/ux/channel-notifications.md`). Mirror of
/// web `feed/channel-notify.ts` + `feed/ChannelPostNotifier.tsx`.
///
/// There is no push for channels yet: a post is noticed when this device
/// reads the channel, through ``ChannelPostSync`` in the foreground and in
/// the background app refresh task (``ChannelPostBackgroundRefresh``). Every
/// read is handed here once the log holding it is saved; the shared rule
/// (`lakko_ui_rules::channel_notify`, through UniFFI) decides which post, if
/// any, notifies, and the per-channel "last notified post" cursor
/// (``ChannelNotifiedStore``) is stored after every read.

/// One channel's read, as the sync hands it over.
struct ChannelRead: Equatable {
    let channelIdHex: String
    /// The channel's slot before this read; `nil` when it had none.
    let slotBefore: ChannelPostSlot?
    /// The slot after it.
    let slotAfter: ChannelPostSlot
    /// The channel's read marker before this read; `nil` when none.
    let readMarker: UInt64?
}

/// The pure half: the inputs of the shared rule, built from a read.
enum ChannelNotify {

    /// What the device knows of the channel when the read is judged.
    struct Facts: Equatable {
        var muted: Bool
        /// The channel is open in front of the person (the app active, the
        /// channel screen showing).
        var openAndVisible: Bool
        /// This device publishes to the channel (owner or publisher): every
        /// post counts as the person's own, as on web (a post signing key is
        /// random per device and channel, so a post from the person's other
        /// device cannot be told from a co-publisher's).
        var own: Bool
        /// The cursor stored after this device's last read of the channel.
        var lastNotified: UInt64
        var nowMs: UInt64
    }

    /// The decision for one read.
    struct Decision: Equatable {
        /// The post to show, the newest that notifies; `nil` for none.
        let post: PersistedChannelPost?
        /// The cursor to store for the channel.
        let lastNotified: UInt64
    }

    /// The shared rule, injectable for tests.
    typealias Rule = (ChannelNotifyContext, [ChannelPostCandidate]) -> ChannelNotifyBatch

    static let liveRule: Rule = { channelPostsToNotify(context: $0, posts: $1) }

    /// What kind of read produced a channel's new records. A channel with no
    /// slot before this read is read from its start: the first read after
    /// subscribing when it has no read marker yet (the marker is seeded right
    /// after that read, ``FeedReading/seedingChannelReads(_:)``), else a
    /// refetch of history (the log was dropped by a format change, or the
    /// slot was lost while its marker survived).
    static func readKind(slotBefore: ChannelPostSlot?, readMarker: UInt64?) -> ChannelReadKind {
        if slotBefore != nil { return .ongoing }
        return readMarker == nil ? .firstAfterSubscribe : .history
    }

    /// The records the rule judges: the posts the read added and, when the
    /// read also passed over records that are not posts, one non-post record
    /// at the slot's new cursor, so the stored cursor moves past them too.
    static func candidates(read: ChannelRead, own: Bool) -> (fresh: [PersistedChannelPost], posts: [ChannelPostCandidate]) {
        let before = read.slotBefore?.cursor ?? 0
        let fresh = read.slotAfter.posts.filter { $0.cursor > before }
        var posts = fresh.map {
            ChannelPostCandidate(
                cursor: $0.cursor, isPost: true, own: own,
                serverTsMs: UInt64(clamping: max(0, $0.serverTsMs)))
        }
        let after = read.slotAfter.cursor
        if after > before, !fresh.contains(where: { $0.cursor == after }) {
            posts.append(ChannelPostCandidate(cursor: after, isPost: false, own: own, serverTsMs: 0))
        }
        return (fresh, posts)
    }

    /// Judge one channel's read by the shared rule.
    static func decide(read: ChannelRead, facts: Facts, rule: Rule = liveRule) -> Decision {
        let (fresh, posts) = candidates(read: read, own: facts.own)
        let context = ChannelNotifyContext(
            muted: facts.muted,
            openAndVisible: facts.openAndVisible,
            readMarker: read.readMarker ?? 0,
            lastNotified: facts.lastNotified,
            readKind: readKind(slotBefore: read.slotBefore, readMarker: read.readMarker),
            nowMs: facts.nowMs)
        let batch = rule(context, posts)
        // One notification per channel: the last index the rule returns.
        let post = batch.notify.last.flatMap { index -> PersistedChannelPost? in
            let i = Int(index)
            return i < fresh.count ? fresh[i] : nil
        }
        return Decision(post: post, lastNotified: batch.lastNotified)
    }

    /// Notifications of one channel replace each other: the request
    /// identifier and the thread identifier.
    static func tag(channelIdHex: String) -> String {
        "channel:\(channelIdHex.lowercased())"
    }

    /// The notification's body: the post as the channel's row in Chats reads
    /// it (``ChatListScreen/channelRows``), else the generic line.
    static func body(post: PersistedChannelPost, generic: String) -> String {
        let line = post.text.trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "\n", with: " ")
        return line.isEmpty ? generic : line
    }
}

/// The per-channel "last notified post" cursor (spec section 5): per device,
/// per account, never synced, in `UserDefaults`. It holds channel ids and
/// cursors, nothing secret. Not written once the account's session has
/// ended (``AccountStoreGate``); removed with the account by
/// ``AccountLocalDataWipe`` (step `channel-notified`).
struct ChannelNotifiedStore {
    var defaults: UserDefaults = .standard

    static let keyPrefix = "me.lakko.channel-notified.v1."

    static func key(accountIdB64: String) -> String { keyPrefix + accountIdB64 }

    /// Every channel's stored cursor; empty when there is none or it is
    /// unreadable.
    func load(accountIdB64: String) -> [String: UInt64] {
        guard let data = defaults.data(forKey: Self.key(accountIdB64: accountIdB64)),
              let raw = try? JSONDecoder().decode([String: UInt64].self, from: data)
        else { return [:] }
        return raw.filter { ChannelPostLogPayload.isChannelId($0.key) }
    }

    /// Store one channel's cursor; never moves it back. Channels not in
    /// `keep` (no longer held) are dropped from the row.
    func save(accountIdB64: String, channelIdHex: String, cursor: UInt64, keep: Set<String>) {
        let id = channelIdHex.lowercased()
        guard ChannelPostLogPayload.isChannelId(id) else { return }
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        let current = load(accountIdB64: accountIdB64)
        var next = current.filter { keep.contains($0.key) }
        next[id] = max(current[id] ?? 0, cursor)
        guard let data = try? JSONEncoder().encode(next) else { return }
        defaults.set(data, forKey: Self.key(accountIdB64: accountIdB64))
    }

    func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
    }
}

/// Shows a channel's notification. Injectable for tests.
protocol ChannelPostNotificationPoster {
    @MainActor func post(channelIdHex: String, title: String, body: String)
}

/// The system notification through `UNUserNotificationCenter`, under the
/// app's existing notification permission (never asked for here). The
/// request identifier is the channel's tag, so a newer post replaces the
/// older notification; the thread identifier groups it with the channel.
struct SystemChannelPostNotifier: ChannelPostNotificationPoster {
    func post(channelIdHex: String, title: String, body: String) {
        let tag = ChannelNotify.tag(channelIdHex: channelIdHex)
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        content.threadIdentifier = tag
        content.userInfo = [ChannelNotificationTaps.routeKey: channelIdHex.lowercased()]
        let request = UNNotificationRequest(identifier: tag, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request) { error in
            if error != nil { NSLog("[feed] channel notification not shown") }
        }
    }
}

/// A tap on a channel notification: the channel to open. The session root
/// takes it (``SessionRoot``), also when the tap launched the app.
@MainActor
final class ChannelNotificationTaps: ObservableObject {
    static let shared = ChannelNotificationTaps()

    /// The notification's `userInfo` key carrying the channel id (hex).
    nonisolated static let routeKey = "lakko.channel-notify.channel"

    @Published var openChannelRequested: String?
}

/// Which channel screen is on screen, if any. Set by the channel
/// conversation screen as it appears and disappears.
@MainActor
enum VisibleChannel {
    private(set) static var current: String?

    static func appeared(_ channelIdHex: String) { current = channelIdHex.lowercased() }

    static func disappeared(_ channelIdHex: String) {
        if current == channelIdHex.lowercased() { current = nil }
    }

    /// The channel is open in front of the person: the app active and the
    /// channel screen showing.
    static func isOpenAndVisible(_ channelIdHex: String) -> Bool {
        UIApplication.shared.applicationState == .active && current == channelIdHex.lowercased()
    }
}

/// Hears every channel read a sync makes and shows what the rule picks.
@MainActor
final class ChannelPostNotifier {

    /// What the notifier reads of the channels, injectable for tests.
    struct Deps {
        var muted: (String) -> Bool
        var openAndVisible: (String) -> Bool
        var own: (String) -> Bool
        var title: (String) -> String
        /// The channels this device holds (the stored cursors of others go).
        var held: () -> Set<String>
        var nowMs: () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }
    }

    private let deps: Deps
    private let store: ChannelNotifiedStore
    private let poster: ChannelPostNotificationPoster
    private let rule: ChannelNotify.Rule

    init(
        deps: Deps,
        store: ChannelNotifiedStore = ChannelNotifiedStore(),
        poster: ChannelPostNotificationPoster = SystemChannelPostNotifier(),
        rule: @escaping ChannelNotify.Rule = ChannelNotify.liveRule
    ) {
        self.deps = deps
        self.store = store
        self.poster = poster
        self.rule = rule
    }

    /// Judge every read, store each channel's cursor, show what notifies.
    func handle(accountIdB64: String, reads: [ChannelRead]) {
        guard !reads.isEmpty else { return }
        let stored = store.load(accountIdB64: accountIdB64)
        let keep = deps.held()
        let now = deps.nowMs()
        let generic = Localization.text("notification.message.new")
        for read in reads {
            let id = read.channelIdHex.lowercased()
            let decision = ChannelNotify.decide(
                read: read,
                facts: ChannelNotify.Facts(
                    muted: deps.muted(id),
                    openAndVisible: deps.openAndVisible(id),
                    own: deps.own(id),
                    lastNotified: stored[id] ?? 0,
                    nowMs: now),
                rule: rule)
            // Always stored: the cursor moves past every record read, so
            // unmuting or closing the channel later never announces old posts.
            store.save(accountIdB64: accountIdB64, channelIdHex: id, cursor: decision.lastNotified, keep: keep)
            guard let post = decision.post else { continue }
            poster.post(
                channelIdHex: id,
                title: deps.title(id),
                body: ChannelNotify.body(post: post, generic: generic))
        }
    }

    /// Wire a sync to a live notifier: the read markers it reads before each
    /// sync, and the reads it hands over after. The notifier lives as long
    /// as the sync holds the closure.
    static func attach(
        to sync: ChannelPostSync,
        channels: ChannelsUiState,
        reading: FeedReadingModel,
        flags: @escaping () -> [String: FlagsState]?
    ) {
        let notifier = live(accountIdB64: sync.account, channels: channels, flags: flags)
        sync.channelReadMarkers = { [weak reading] in reading?.reading.channelRead ?? [:] }
        sync.onChannelReads = { account, reads in
            notifier.handle(accountIdB64: account, reads: reads)
        }
    }

    /// The live notifier for one account's channel state. `flags` is the
    /// synced flags snapshot when the DeviceGroup sync is loaded, else `nil`
    /// (a background launch): mute is then read from the App Group mirror
    /// the app writes on every flags change (``ConversationMuteMirror``).
    static func live(
        accountIdB64: String,
        channels: ChannelsUiState,
        flags: @escaping () -> [String: FlagsState]?
    ) -> ChannelPostNotifier {
        ChannelPostNotifier(deps: Deps(
            muted: { id in
                if let flags = flags() {
                    return flags[ChannelFlags.key(channelIdHex: id)]?.muted == true
                }
                return ConversationMuteMirror.isMuted(accountIdB64: accountIdB64, groupIdHex: id)
            },
            openAndVisible: { VisibleChannel.isOpenAndVisible($0) },
            own: { [weak channels] id in
                channels?.keysByChannel[id] != nil
                    || CoPublishedJournals.shared.journalIdHex(channelIdHex: id) != nil
            },
            title: { [weak channels] id in
                guard let channel = channels?.channels.first(where: { $0.id.lowercased() == id }) else {
                    return Localization.text("channels.fallback_name")
                }
                return ChannelsFormatting.title(
                    sharedName: channel.sharedName, localName: channel.localName, handle: channel.handle)
            },
            held: { [weak channels] in Set(channels?.channels.map { $0.id.lowercased() } ?? []) }))
    }
}
