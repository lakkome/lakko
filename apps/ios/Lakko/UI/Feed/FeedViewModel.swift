// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore
import UIKit

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

/// The Feed destination's model (`docs/ux/feed.md`, ADR-0063). Mirror of the
/// state web `routes/Feed.tsx` keeps.
///
/// Messages from the person's groups and posts from their channels, ranked on
/// this device by the one rule engine in the core. Channel posts come from
/// the channel-post log ``ChannelPostSync`` fills in the foreground.
///
/// Seen (ADR-0063 section 5): a group message fully on screen for
/// ``FeedSeen/dwellSeconds`` joins its group's seen set; when that makes the
/// set contiguous further than before, the group gets a read receipt and the
/// other devices a read cursor at the new watermark, never past a message the
/// person did not see. Channel posts are not marked.
@MainActor
final class FeedViewModel: ObservableObject {

    static let selectionKey = "lakko.feed.preset"
    /// How many items the editor's preview shows.
    static let previewItems = 8

    // MARK: Published

    @Published private(set) var selection: FeedSelection
    @Published private(set) var saved: [SavedFeedView] = []
    /// What is drawn; `nil` until the first build (the skeleton shows).
    @Published private(set) var shown: [RankedFeedItem]? {
        didSet { noteLook() }
    }
    /// A newer ranking waiting behind "Show N new".
    @Published private(set) var pending: [RankedFeedItem]?
    @Published private(set) var sourceCount: Int = 0
    @Published private(set) var candidateCount: Int = 0
    @Published var editing: FeedEditing?
    @Published var editError: String?
    @Published var sharing: SavedFeedView?
    /// A shared feed the import sheet is showing (a link or a file).
    @Published var importing: FeedImportRequest?
    @Published private(set) var nowMs: Int64 = FeedViewModel.clockMs()
    /// UX spec section 7 — the latest checks could not reach the server.
    /// Read from ``FeedRefreshCenter``, which every check reports to: the
    /// background group sweep, the channel post sync, and pull to refresh.
    @Published private(set) var refreshFailed = false
    private let refreshCenter: FeedRefreshCenter
    /// The conversation just hidden from feeds, while its toast shows.
    @Published private(set) var hiddenToast: String?
    /// The item whose "Forward to" picker is open.
    @Published var forwarding: FeedCandidateRef?
    /// A one-line notice at the bottom (a forward that failed).
    @Published private(set) var notice: String?
    /// Every group and channel this device holds, hidden ones included: the
    /// editor's list, the empty state and the hidden list read these.
    @Published private(set) var allSources: [FeedSource] = []

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
    /// Hidden conversations, last looks, channel read markers (this device).
    let reading: FeedReadingModel
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
        reading: FeedReadingModel,
        refreshCenter: FeedRefreshCenter,
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
        self.reading = reading
        self.refreshCenter = refreshCenter
        self.feedStore = feedStore
        self.defaults = defaults
        self.selection = FeedSelection(storageValue: defaults.string(forKey: Self.selectionKey))
        reloadSaved()
        refreshFailed = refreshCenter.failed
        refreshCenter.$failed.removeDuplicates()
            .sink { [weak self] in self?.refreshFailed = $0 }
            .store(in: &cancellables)

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
            reading.$reading.map(\.hidden).removeDuplicates().map { _ in () }.eraseToAnyPublisher(),
        ]
        Publishers.MergeMany(triggers)
            .debounce(for: .milliseconds(250), scheduler: RunLoop.main)
            .sink { [weak self] in self?.rebuild() }
            .store(in: &cancellables)
        // A conversation hidden or shown again changes the suggestions too.
        reading.$reading.map(\.hidden).removeDuplicates().dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak discover] _ in discover?.recompute() }
            .store(in: &cancellables)
    }

    /// Start the one-minute clock: recency moves with it, and a minute is
    /// fine-grained enough for a ranking whose shortest half-life is six
    /// hours.
    func start() {
        // UX spec section 5 — what each feed showed when the person last
        // left the Feed screen, read once on arrival so the "New" divider
        // stays put for the whole visit.
        lookedAtEntry = reading.reading.looked
        visitLooks = [:]
        noteLook()
        if backgroundObserver == nil {
            backgroundObserver = NotificationCenter.default.addObserver(
                forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main
            ) { [weak self] _ in
                Task { @MainActor in self?.persistLooks() }
            }
        }
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
        persistLooks()
        if let o = backgroundObserver {
            NotificationCenter.default.removeObserver(o)
            backgroundObserver = nil
        }
    }

    // MARK: - "New" divider (UX spec section 5)

    /// Per feed, the looks this device remembered at arrival; `nil` before
    /// the first arrival.
    private var lookedAtEntry: [String: [String]]?
    /// Every feed viewed during this visit, with what it showed when last on
    /// screen. Written when the person leaves the Feed screen or the app goes
    /// to the background; switching feeds inside the screen writes nothing.
    private var visitLooks: [String: [String]] = [:]
    private var backgroundObserver: NSObjectProtocol?

    private func noteLook() {
        guard let shown, let sel = shownSelection, sel == selection, sel != .discover else { return }
        visitLooks[sel.storageValue] = shown.map(\.key)
    }

    private func persistLooks() {
        guard !visitLooks.isEmpty else { return }
        let looks = visitLooks
        reading.update { r in
            looks.reduce(r) { $0.recordingLooked(feedKey: $1.key, itemKeys: $1.value) }
        }
    }

    /// Index (in drawn order) of the item the "New" divider sits above, or
    /// `nil` for none.
    var dividerIndex: Int? {
        guard selection != .discover, let shown, let entry = lookedAtEntry else { return nil }
        return FeedReading.dividerIndex(itemKeys: shown.map(\.key), previous: entry[selection.storageValue])
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
            editError = Localization.text("feed.error.save")
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
            editError = Localization.text("feed.error.delete")
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
                    // ADR-0073 R.5 — mute, "hide from feeds" and a rule's
                    // lists belong to the CONVERSATION.
                    muted: flags[thread.conversationIdHex]?.muted == true,
                    audience: chatRepository.memberCount(groupIdHex: thread.groupIdHex) ?? 0,
                    conversationHex: thread.conversationIdHex)
            }
        let channelSources = channels.channels.map { c in
            FeedSource(
                kind: .channel,
                idHex: c.id,
                title: c.title,
                // The channel row's Mute (``ChannelFlags``).
                muted: flags[ChannelFlags.key(channelIdHex: c.id)]?.muted == true,
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
            attachments: m.attachments,
            edited: m.edited,
            deleted: m.deleted,
            forwardAuthor: m.forward?.author)
    }

    private func rebuild() {
        let all = buildSources()
        allSources = all
        let sources = FeedReading.withoutHidden(all, hidden: reading.reading.hidden)
        var groupMessages: [String: [FeedMessage]] = [:]
        for thread in chatRepository.state.threads {
            // Rows of an older segment (history ids) are not feed
            // candidates: the feed reads the conversation's current group.
            groupMessages[thread.groupIdHex] = thread.messages
                .filter { $0.ack == .sent && $0.messageId > 0 }
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
        sourceCount = all.count
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
        FeedReading.pendingNewCount(shown: shown?.map(\.key), pending: pending?.map(\.key))
    }

    func showPending() {
        if let next = pending { shown = next }
        pending = nil
    }

    /// Pull to refresh (and the failure banner's Retry): poll every group and
    /// sync the channels now, then show what is new. The screen lands at the
    /// top.
    func refresh() async {
        let groups = await chatRepository.pollAllGroupsReporting()
        // The channel sync reports its own tally (``ChannelPostSync/onSyncDone``).
        await postSync.syncAndWait()
        // A refresh called off (the screen was left mid-pull) reached nobody
        // because it was stopped, not because nobody answered: it neither
        // raises nor clears "Couldn't check for new messages."
        if !Task.isCancelled { refreshCenter.record(.groups, groups) }
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

    /// Every source, hidden ones included, for the editor's per-conversation
    /// Only / Never.
    var sources: [FeedSource] { allSources }

    // MARK: - Hide from feeds (UX spec section 5)

    static let toastSeconds: UInt64 = 5
    private var toastTask: Task<Void, Never>?

    /// Take a group or channel out of every feed on this device.
    func hide(_ idHex: String) {
        guard reading.update({ $0.hiding(idHex) }) else { return }
        toastTask?.cancel()
        hiddenToast = idHex
        toastTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: Self.toastSeconds * 1_000_000_000)
            guard !Task.isCancelled else { return }
            self?.hiddenToast = nil
        }
    }

    func undoHide() {
        guard let id = hiddenToast else { return }
        toastTask?.cancel()
        hiddenToast = nil
        showAgain(id)
    }

    func showAgain(_ idHex: String) {
        reading.update { $0.showing(idHex) }
    }

    /// The hidden conversations this device still holds.
    var hiddenSources: [FeedSource] {
        FeedReading.hiddenSources(allSources, hidden: reading.reading.hidden)
    }

    // MARK: - Item menu (UX spec section 5)

    func copyText(_ ref: FeedCandidateRef) {
        UIPasteboard.general.string = ref.message.text
    }

    /// Section 5.1 of message-actions — the picker's rows: groups and chats
    /// this device is a live member of, in the conversation list's order.
    func forwardTargets() -> [ConversationListRow] {
        let rows = ChatListScreen.chatRows(
            viewModel: chatList, flags: deviceGroupSync.flags, readCursors: deviceGroupSync.readCursors)
        return ConversationList.sorted(rows.filter { $0.endedLine == nil })
    }

    static var forwardFailed: String { Localization.text("chat.forward.error.generic") }
    private var noticeTask: Task<Void, Never>?

    private func say(_ text: String) {
        noticeTask?.cancel()
        notice = text
        noticeTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: Self.toastSeconds * 1_000_000_000)
            guard !Task.isCancelled else { return }
            self?.notice = nil
        }
    }

    /// Forward a feed item to `targetGroupIdHex`, as the chat forwards: a
    /// group message carries its text, its attachments re-encrypted under
    /// fresh keys, the original author's name and its origin; a channel post
    /// carries its text, its attachments re-encrypted the same way, and the
    /// channel's name and no origin. Returns `true`
    /// when it was sent; a failure is said once, here.
    func forward(_ ref: FeedCandidateRef, to targetGroupIdHex: String) async -> Bool {
        forwarding = nil
        do {
            switch ref.source.kind {
            case .channel:
                let payload = FeedForward.channelPayload(text: ref.message.text, channelTitle: ref.source.title)
                // Its attachments under fresh keys, as a group message's are.
                let refs = try await ForwardAttachments.reencryptAll(ref.message.attachments)
                try await chatRepository.sendMessage(
                    groupIdHex: targetGroupIdHex, plaintext: payload.text,
                    attachments: refs, forward: payload.forward)
            case .group:
                guard let message = chatRepository.state.threads
                    .first(where: { $0.groupIdHex == ref.source.idHex })?
                    .messages.first(where: { $0.messageId == ref.message.messageId && !$0.deleted })
                else { throw CocoaError(.fileNoSuchFile) }
                let payload = ForwardPayload.forwardPayload(
                    of: message,
                    authorLabel: forwardAuthorLabel(message),
                    originGroupIdHex: ref.source.idHex,
                    // The feed never holds a one-to-one (UX spec section 3).
                    sourceIsOneToOne: false)
                let refs = try await ForwardAttachments.reencryptAll(message.attachments)
                try await chatRepository.sendMessage(
                    groupIdHex: targetGroupIdHex, plaintext: payload.text,
                    attachments: refs, forward: payload.forward)
            }
            return true
        } catch {
            NSLog("[feed] forward failed")
            say(Self.forwardFailed)
            return false
        }
    }

    /// The original author as a recipient should read it: the person's own
    /// name for their own message, never "You"; nothing for an unknown
    /// member.
    private func forwardAuthorLabel(_ message: ChatMessage) -> String? {
        switch message.direction {
        case .outgoing:
            let directory = ProfileDirectory.shared
            return directory.ownProfile?.displayName ?? directory.ownUsername.map { "@\($0)" }
        case .incoming, .system:
            guard let key = message.authorKeyB64,
                  let resolved = ProfileDirectory.shared.resolve(signatureKeyB64: key),
                  !resolved.isPlaceholder
            else { return nil }
            return resolved.name
        }
    }

    // MARK: - Words on an item

    /// ADR-0084 — whose photo sits beside the author's name.
    func authorPhoto(_ ref: FeedCandidateRef) -> AvatarPhoto? {
        if ref.source.kind == .channel { return nil }
        if ref.message.direction == .outgoing { return .own }
        return ref.message.authorKeyB64.flatMap(base64Decode).map(AvatarPhoto.person)
    }

    /// What the author's initials come from: "You" is a label, not a name.
    func authorAvatarName(_ ref: FeedCandidateRef) -> String {
        guard ref.source.kind == .group, ref.message.direction == .outgoing else { return author(ref) }
        let directory = ProfileDirectory.shared
        return directory.ownProfile?.displayName ?? directory.ownUsername ?? author(ref)
    }

    func author(_ ref: FeedCandidateRef) -> String {
        if ref.source.kind == .channel { return ref.source.title }
        if ref.message.direction == .outgoing { return Localization.text("chat.author.you") }
        guard let key = ref.message.authorKeyB64,
              let resolved = ProfileDirectory.shared.resolve(signatureKeyB64: key)
        else { return ResolvedDisplayName.placeholderMember }
        return resolved.name
    }

    // MARK: - Seen (ADR-0063 section 5)

    /// Seen ids waiting to be filed, per group.
    private var pendingSeen: [String: Set<Int64>] = [:]
    private var flushWork: DispatchWorkItem?

    /// Whether the item still needs the dwell to count as seen.
    func wantsSeen(_ ref: FeedCandidateRef) -> Bool {
        guard ref.message.direction == .incoming else { return false }
        if ref.source.kind == .channel {
            // A channel post is read once, here or in the channel (UX spec
            // section 5): only one above the newest read can move it.
            return FeedForward.channelPostWantsSeen(
                cursor: ref.message.messageId, newestRead: reading.reading.channelRead[ref.source.idHex])
        }
        guard let thread = chatRepository.state.threads.first(where: { $0.groupIdHex == ref.source.idHex })
        else { return false }
        if thread.feedSeen.contains(ref.message.messageId) { return false }
        let through = [thread.lastViewedMessageId, cursorThrough(ref.source.idHex)].compactMap { $0 }.max()
        if let through, ref.message.messageId <= through { return false }
        return true
    }

    /// How far the cross-device read position reaches into `groupIdHex`'s
    /// own message ids. ADR-0073 R.5 — the position is keyed on the
    /// conversation and carries a segment past the first one.
    private func cursorThrough(_ groupIdHex: String) -> Int64? {
        let conversation = chatRepository.conversationIdHex(ofGroup: groupIdHex)
        guard let cursor = deviceGroupSync.readCursors[conversation] else { return nil }
        switch LineageRules.readUpToInSegment(
            LineageRules.decodeReadPosition(cursor.upToMessageId),
            segment: chatRepository.segmentIndex(ofGroup: groupIdHex)) {
        case .nothing: return nil
        case .upTo(let id): return id
        // Read in a later segment: everything this group holds is read.
        case .all: return chatRepository.journalCursor(groupIdHex: groupIdHex)
        }
    }

    /// The item was fully on screen for the dwell.
    func markSeen(_ ref: FeedCandidateRef) {
        guard wantsSeen(ref) else { return }
        if ref.source.kind == .channel {
            reading.advanceChannelRead(ref.source.idHex, cursor: UInt64(ref.message.messageId))
            return
        }
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
                guard let groupId = Data(feedHex: repo.conversationIdHex(ofGroup: hex)) else { return }
                do {
                    try await sync.setReadCursor(
                        groupId: groupId,
                        upToMessageId: LineageRules.encodeReadPosition(
                            LineageRules.TimelinePosition(
                                segment: repo.segmentIndex(ofGroup: hex), messageId: upTo)),
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
