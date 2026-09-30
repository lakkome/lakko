// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// The selected feed: a preset, or a saved feed by its local id.
enum FeedSelection: Hashable {
    case preset(FeedPresetKey)
    /// ADR-0059 — Discover: channel suggestions instead of items.
    case discover
    case saved(String)

    /// The form kept in `UserDefaults` (the web's `localStorage` value).
    var storageValue: String {
        switch self {
        case .preset(let key): return key.rawValue
        case .discover: return "discover"
        case .saved(let id): return "custom:\(id)"
        }
    }

    init(storageValue: String?) {
        if let v = storageValue, v.hasPrefix("custom:") {
            self = .saved(String(v.dropFirst("custom:".count)))
        } else if storageValue == "discover" {
            self = .discover
        } else if let v = storageValue, let key = FeedPresetKey(rawValue: v) {
            self = .preset(key)
        } else {
            self = .preset(.latest)
        }
    }
}

/// A saved feed as the switcher holds it: its id and its decoded rule.
struct SavedFeedView: Equatable, Identifiable {
    let id: String
    let rule: FeedRule
}

/// What the editor is editing.
enum FeedEditing: Hashable, Identifiable {
    case new
    case edit(String)

    var id: String {
        switch self {
        case .new: return "new"
        case .edit(let id): return "edit:\(id)"
        }
    }
}

/// One line of the editor's preview.
struct FeedPreviewLine: Identifiable, Equatable {
    let id: String
    let source: String
    let text: String
}

/// The Feed destination's model (`docs/ux/feed.md`, ADR-0058). Mirror of the
/// state web `routes/Feed.tsx` keeps.
///
/// Messages from the person's groups and posts from their channels, ranked on
/// this device by the one rule engine in the core. Channel posts come from
/// the channel-post log ``ChannelPostSync`` fills in the foreground.
///
/// Seen (ADR-0058 section 5): a group message fully on screen for
/// ``FeedSeen/dwellSeconds`` joins its group's seen set; when that makes the
/// set contiguous further than before, the group gets a read receipt and the
/// other devices a read cursor at the new watermark, never past a message the
/// person did not see. Channel posts are not marked.
@MainActor
final class FeedViewModel: ObservableObject {

    static let selectionKey = "lakko.feed.preset"
    static let bannerKey = "lakko.feed.device-banner-dismissed"
    /// How many items the editor's preview shows.
    static let previewItems = 8

    // MARK: Published

    @Published private(set) var selection: FeedSelection
    @Published private(set) var saved: [SavedFeedView] = []
    /// What is drawn; `nil` until the first build (the skeleton shows).
    @Published private(set) var shown: [RankedFeedItem]?
    /// A newer ranking waiting behind "Show N new".
    @Published private(set) var pending: [RankedFeedItem]?
    @Published private(set) var sourceCount: Int = 0
    @Published private(set) var candidateCount: Int = 0
    @Published private(set) var bannerDismissed: Bool
    @Published var editing: FeedEditing?
    @Published var editError: String?
    @Published var sharing: SavedFeedView?
    /// A shared feed the import sheet is showing (a link or a file).
    @Published var importing: FeedImportRequest?
    @Published private(set) var nowMs: Int64 = FeedViewModel.clockMs()

    /// Set by the list: whether the reader is at the top, where new items
    /// show at once.
    var nearTop = true {
        didSet {
            if nearTop, let next = pending {
                shown = next
                pending = nil
            }
        }
    }

    // MARK: Inputs

    private let account: SessionAccount
    private let chatRepository: ChatRepository
    private let chatList: ChatListViewModel
    private let channels: ChannelsUiState
    private let postSync: ChannelPostSync
    private let deviceGroupSync: DeviceGroupSyncRepository
    private let contacts: ContactsRepository
    private let feedStore: FeedStore
    private let defaults: UserDefaults
    /// ADR-0059 — Discover's model: the Discover chip, and the cards a saved
    /// feed that asks for suggestions mixes in.
    let discover: DiscoverModel

    private var input: FeedInput?
    private var savedState = FeedState()
    private var shownSelection: FeedSelection?
    private var cancellables: Set<AnyCancellable> = []
    private var clock: Timer?

    init(
        account: SessionAccount,
        chatRepository: ChatRepository,
        chatList: ChatListViewModel,
        channels: ChannelsUiState,
        postSync: ChannelPostSync,
        deviceGroupSync: DeviceGroupSyncRepository,
        contacts: ContactsRepository,
        discover: DiscoverModel,
        feedStore: FeedStore = FeedStore(),
        defaults: UserDefaults = .standard
    ) {
        self.account = account
        self.chatRepository = chatRepository
        self.chatList = chatList
        self.channels = channels
        self.postSync = postSync
        self.deviceGroupSync = deviceGroupSync
        self.contacts = contacts
        self.discover = discover
        self.feedStore = feedStore
        self.defaults = defaults
        self.selection = FeedSelection(storageValue: defaults.string(forKey: Self.selectionKey))
        self.bannerDismissed = defaults.bool(forKey: Self.bannerKey)
        reloadSaved()

        // Every input change rebuilds, coalesced: a poll batch, a channel
        // sync, a contact accepted, a mute, a name arriving.
        let triggers: [AnyPublisher<Void, Never>] = [
            chatRepository.$state.map { _ in () }.eraseToAnyPublisher(),
            chatRepository.$groupNameRevision.map { _ in () }.eraseToAnyPublisher(),
            channels.objectWillChange.map { _ in () }.eraseToAnyPublisher(),
            postSync.$log.map { _ in () }.eraseToAnyPublisher(),
            deviceGroupSync.$flags.map { _ in () }.eraseToAnyPublisher(),
            contacts.$state.map { _ in () }.eraseToAnyPublisher(),
            ProfileDirectory.shared.$revision.map { _ in () }.eraseToAnyPublisher(),
        ]
        Publishers.MergeMany(triggers)
            .debounce(for: .milliseconds(250), scheduler: RunLoop.main)
            .sink { [weak self] in self?.rebuild() }
            .store(in: &cancellables)
    }

    /// Start the one-minute clock: recency moves with it, and a minute is
    /// fine-grained enough for a ranking whose shortest half-life is six
    /// hours.
    func start() {
        rebuild()
        if suggestionRate != .never { Task { await discover.refresh() } }
        guard clock == nil else { return }
        clock = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.nowMs = Self.clockMs()
                self?.rebuild()
            }
        }
    }

    func stop() {
        clock?.invalidate()
        clock = nil
        flushSeen()
    }

    static func clockMs() -> Int64 {
        Int64(Date().timeIntervalSince1970 * 1000)
    }

    // MARK: - Selection and saved feeds

    func choose(_ next: FeedSelection) {
        selection = next
        defaults.set(next.storageValue, forKey: Self.selectionKey)
        rerank()
        if suggestionRate != .never { Task { await discover.refresh() } }
    }

    /// How often the selected feed mixes suggestions in: its rule's field for
    /// a saved feed; the presets never do (their rules say never).
    var suggestionRate: SuggestionRate {
        switch selectedSaved?.rule.suggestions {
        case .rarely: return .rarely
        case .sometimes: return .sometimes
        case .never, .none: return .never
        }
    }


    var selectedSaved: SavedFeedView? {
        guard case .saved(let id) = selection else { return nil }
        return saved.first { $0.id == id }
    }

    /// What the "why" line reads the item under: a saved feed that weighs
    /// reactions reads like Popular, anything else like Latest.
    var whyPreset: FeedPresetKey {
        switch selection {
        case .preset(let key): return key
        case .discover: return .latest
        case .saved: return (selectedSaved?.rule.weights.reactions ?? 0) > 0 ? .popular : .latest
        }
    }

    private func reloadSaved() {
        savedState = feedStore.load(accountIdB64: account.accountIdB64)
        saved = savedState.feeds.compactMap { f in
            // A stored rule the core refuses is not shown.
            (try? decodeFeedRule(bytes: f.rule)).map { SavedFeedView(id: f.id, rule: $0) }
        }
        if case .saved(let id) = selection, !saved.contains(where: { $0.id == id }) {
            choose(.preset(.latest))
        }
    }

    private func persist(_ next: FeedState) throws {
        try feedStore.save(accountIdB64: account.accountIdB64, state: next)
        savedState = next
        reloadSaved()
    }

    func editorInitial(_ editing: FeedEditing) -> FeedRule? {
        switch editing {
        case .new: return FeedDraft.blank()
        case .edit(let id): return saved.first { $0.id == id }?.rule
        }
    }

    /// Save the editor's rule. Returns `true` when it was saved.
    @discardableResult
    func save(_ rule: FeedRule, editing: FeedEditing) -> Bool {
        editError = nil
        do {
            let bytes = try encodeFeedRule(rule: rule)
            let id: String
            switch editing {
            case .new: id = FeedState.newId()
            case .edit(let existing): id = existing
            }
            try persist(savedState.upserting(SavedFeed(id: id, rule: bytes)))
            self.editing = nil
            choose(.saved(id))
            return true
        } catch {
            editError = "Couldn't save the feed. Try again."
            return false
        }
    }

    func deleteSaved(_ id: String) {
        editError = nil
        do {
            try persist(savedState.removing(id: id))
            editing = nil
            choose(.preset(.latest))
        } catch {
            editError = "Couldn't delete the feed. Try again."
        }
    }

    /// Save an imported feed as a new one and select it.
    func addImported(_ rule: FeedRule) -> Bool {
        do {
            let bytes = try encodeFeedRule(rule: rule)
            let id = FeedState.newId()
            try persist(savedState.upserting(SavedFeed(id: id, rule: bytes)))
            choose(.saved(id))
            return true
        } catch {
            return false
        }
    }

    func dismissBanner() {
        bannerDismissed = true
        defaults.set(true, forKey: Self.bannerKey)
    }

    // MARK: - Sources and input

    /// The groups and channels the feed draws from. Groups only: a one-to-one
    /// is never in the feed (UX spec section 3), and a group this device is no
    /// longer in leaves it.
    private func buildSources() -> [FeedSource] {
        let flags = deviceGroupSync.flags
        let groups = chatRepository.state.threads
            .filter { !$0.membershipEnded && chatList.kind(of: $0.groupIdHex) == .group }
            .map { thread in
                FeedSource(
                    kind: .group,
                    idHex: thread.groupIdHex,
                    title: chatList.title(groupIdHex: thread.groupIdHex),
                    muted: flags[thread.groupIdHex]?.muted == true,
                    audience: chatRepository.memberCount(groupIdHex: thread.groupIdHex) ?? 0)
            }
        let channelSources = channels.channels.map { c in
            FeedSource(
                kind: .channel,
                idHex: c.id,
                title: c.title,
                muted: flags[c.id]?.muted == true,
                audience: 0)
        }
        return groups + channelSources
    }

    static func feedMessage(_ m: ChatMessage) -> FeedMessage {
        FeedMessage(
            messageId: m.messageId,
            direction: m.direction == .outgoing ? .outgoing : (m.direction == .incoming ? .incoming : .system),
            text: m.text,
            timeMs: ChatRepository.parseDsTimestampMs(m.createdAt),
            authorKeyB64: m.authorKeyB64,
            reactions: m.reactions.map { (reactor: $0.reactor, emoji: $0.emoji) },
            replyTarget: m.reply?.target,
            attachmentLabels: m.attachments.map {
                ChatListFormatting.mediaLabel(kind: $0.kind, mime: $0.mime)
            },
            edited: m.edited,
            deleted: m.deleted,
            forwardAuthor: m.forward?.author)
    }

    private func rebuild() {
        let sources = buildSources()
        var groupMessages: [String: [FeedMessage]] = [:]
        for thread in chatRepository.state.threads {
            groupMessages[thread.groupIdHex] = thread.messages
                .filter { $0.ack == .sent }
                .map(Self.feedMessage)
        }
        let username = account.username.isEmpty ? nil : account.username
        let contactKeys = Set(contacts.state.accepted.map { base64Encode($0.request.fromPubkey) })
        input = FeedModel.buildInput(FeedContext(
            sources: sources,
            groupMessages: groupMessages,
            channelPosts: postSync.log.channels,
            ownChannels: Set(channels.keysByChannel.keys),
            ownUsername: username,
            contactKeysB64: contactKeys,
            mentions: { mentionsUsername(text: $0, username: $1) },
            nowMs: nowMs))
        sourceCount = sources.count
        candidateCount = input?.candidates.count ?? 0
        rerank()
    }

    /// The rule of the selected feed, or `nil` while a saved one is missing.
    private func selectedRule() -> FeedRule? {
        switch selection {
        case .preset(let key): return feedPresetRule(preset: key.preset)
        case .discover: return nil
        case .saved: return selectedSaved?.rule
        }
    }

    private func rerank() {
        guard let input, let rule = selectedRule() else { return }
        let next = FeedRanking.rank(rule: rule, input: input, nowMs: nowMs)
        // New results replace what is drawn at once near the top or on a
        // feed change; further down they wait behind "Show N new".
        if shown == nil || shownSelection != selection || nearTop {
            shownSelection = selection
            shown = next
            pending = nil
        } else if next != shown {
            pending = next
        }
    }

    var newCount: Int {
        guard let next = pending, let current = shown else { return 0 }
        let seen = Set(current.map(\.key))
        return next.filter { !seen.contains($0.key) }.count
    }

    func showPending() {
        if let next = pending { shown = next }
        pending = nil
    }

    /// Pull to refresh: sync the channels now and show what is new.
    func refresh() async {
        await postSync.syncAndWait()
        nowMs = Self.clockMs()
        rebuild()
        showPending()
    }

    /// The top of the feed a rule would build, for the editor's preview.
    func preview(_ rule: FeedRule) -> [FeedPreviewLine]? {
        guard let input else { return nil }
        return FeedRanking.rank(rule: rule, input: input, nowMs: nowMs)
            .prefix(Self.previewItems)
            .map { FeedPreviewLine(id: $0.key, source: $0.ref.source.title, text: FeedCopy.body($0.ref.message)) }
    }

    /// Every source, for the editor's per-conversation Only / Never.
    var sources: [FeedSource] { input?.sources ?? [] }

    // MARK: - Words on an item

    func author(_ ref: FeedCandidateRef) -> String {
        if ref.source.kind == .channel { return ref.source.title }
        if ref.message.direction == .outgoing { return "You" }
        guard let key = ref.message.authorKeyB64,
              let resolved = ProfileDirectory.shared.resolve(signatureKeyB64: key)
        else { return ResolvedDisplayName.placeholderMember }
        return resolved.name
    }

    // MARK: - Seen (ADR-0058 section 5)

    /// Seen ids waiting to be filed, per group.
    private var pendingSeen: [String: Set<Int64>] = [:]
    private var flushWork: DispatchWorkItem?

    /// Whether the item still needs the dwell to count as seen.
    func wantsSeen(_ ref: FeedCandidateRef) -> Bool {
        guard ref.source.kind == .group, ref.message.direction == .incoming else { return false }
        guard let thread = chatRepository.state.threads.first(where: { $0.groupIdHex == ref.source.idHex })
        else { return false }
        if thread.feedSeen.contains(ref.message.messageId) { return false }
        let through = [thread.lastViewedMessageId, cursorThrough(ref.source.idHex)].compactMap { $0 }.max()
        if let through, ref.message.messageId <= through { return false }
        return true
    }

    private func cursorThrough(_ groupIdHex: String) -> Int64? {
        deviceGroupSync.readCursors[groupIdHex].flatMap { FeedSeen.readCursorId($0.upToMessageId) }
    }

    /// The item was fully on screen for the dwell.
    func markSeen(_ ref: FeedCandidateRef) {
        guard wantsSeen(ref) else { return }
        pendingSeen[ref.source.idHex, default: []].insert(ref.message.messageId)
        guard flushWork == nil else { return }
        let work = DispatchWorkItem { [weak self] in
            Task { @MainActor in self?.flushSeen() }
        }
        flushWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.8, execute: work)
    }

    private func flushSeen() {
        flushWork = nil
        let batch = pendingSeen
        pendingSeen.removeAll()
        for (hex, ids) in batch {
            guard let upTo = chatRepository.recordFeedSeen(
                groupIdHex: hex, ids: Array(ids), cursorThrough: cursorThrough(hex))
            else { continue }
            let ended = chatRepository.state.threads.first { $0.groupIdHex == hex }?.membershipEnded ?? true
            guard !ended else { continue }
            let repo = chatRepository
            let sync = deviceGroupSync
            Task { @MainActor in
                // A read receipt wakes nobody (ADR-0024): no wake header.
                do {
                    try await repo.sendReadReceipt(groupIdHex: hex, upTo: upTo)
                } catch {
                    NSLog("[feed] read receipt failed")
                }
                guard let groupId = Data(feedHex: hex) else { return }
                do {
                    try await sync.setReadCursor(
                        groupId: groupId,
                        upToMessageId: FeedSeen.readCursorBytes(upTo),
                        readAtMs: UInt64(Self.clockMs()))
                } catch {
                    NSLog("[feed] read cursor failed")
                }
            }
        }
    }
}

/// A shared feed to import: from a link's fragment or a file.
struct FeedImportRequest: Identifiable, Equatable {
    let id = UUID()
    /// The bytes a link carried, or `nil` to start from a file picker.
    let bytes: Data?
    /// A link arrived with a fragment that is not a feed.
    let refused: Bool
}
