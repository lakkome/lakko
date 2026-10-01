// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
import SwiftUI

/// The one conversation list (`docs/ux/navigation.md` section 2): one-to-one
/// chats, groups and channels, in one list, ordered by when something last
/// happened in them.
///
/// Mirror of:
///
///   * `apps/web/src/chat/ChatList.tsx`
///   * `apps/android/.../ui/chat/ChatListScreen.kt`
///
/// This screen used to be rendered TWICE with a `kind` parameter — once for
/// the Chats tab and once for Groups — and a list sorted by WHEN cannot be
/// filtered by kind: the newest thing that happened to you landed in one of
/// three places and you had to remember which. The parameter is gone, the
/// filter with it, and channels are mapped in from their own store.
///
/// **The row says nothing about which kind it is.** No badge, no label, no
/// differing avatar; the kind only routes a tap. A channel additionally draws
/// no pin, mute, archive or unread mark, because those are keyed on a
/// conversation id a channel does not carry — a badge that can never be
/// non-zero would be a lie about what the app knows.
///
/// Rendered as the root of the Chats tab's `NavigationStack`; tapping a row
/// asks the host to push the screen that row's kind opens.
public struct ChatListScreen: View {

    /// What the create menu was asked for (spec section 3).
    public enum CreateChoice: Equatable {
        case chat
        case group
        case channel
    }

    @ObservedObject public var viewModel: ChatListViewModel
    /// The channel store, so the list can hold channels beside chats and
    /// groups. Observed rather than passed as a snapshot: a post arriving
    /// re-times a channel's row, and re-timing is re-ordering.
    @ObservedObject public var channels: ChannelsUiState
    /// ADR-0061 — the title follows the interface language.
    @ObservedObject private var l10n = Localization.shared
    /// Open the conversation this row names, whichever kind it is.
    public var onOpen: (ConversationListRow) -> Void
    /// One of the three create-menu items.
    public var onCreate: (CreateChoice) -> Void
    /// A channel the network lookup found, which this device may not hold yet.
    public var onFoundChannel: (_ channelIdHex: String, _ handle: String) -> Void
    /// A person the network lookup found. The contact request asks for an
    /// introduction this field has no room for, so the name travels to
    /// Contacts rather than being half-answered here.
    public var onFoundPerson: (_ username: String) -> Void
    /// `docs/ux/chat-folders.md` 2.2 — Reorder folders / Edit folders open
    /// Settings > Chat folders (`true` = in its reorder state).
    public var onOpenFolderSettings: (_ reorder: Bool) -> Void
    /// ADR-0060 — "Search people" (`docs/ux/who-can-find-me.md` section 6.1):
    /// the network search for a username or a name, and the request to one of
    /// its results. `nil` hides the row.
    public var peopleSearch: PeopleSearchActions?

    /// Phase 2a3e2c-iOS — cross-device per-thread flags (pinned / muted /
    /// archived). The Combine subscription inside ``DeviceGroupSyncRepository``
    /// keeps the row indicators + the sort order live as sibling devices
    /// publish updates.
    @EnvironmentObject private var deviceGroupSync: DeviceGroupSyncRepository
    /// `docs/ux/calls.md` section 2.1.1 — the app-level 1x1 call host, read for
    /// ONE thing: a call arriving in a MUTED conversation, which is offered on
    /// that conversation's row here as well as in the chat itself. Every other
    /// call surface is the root view's (``CallHostOverlay``); this one has to be
    /// where the person already looks, and answerable without navigating.
    @EnvironmentObject private var callHost: CallHost

    public init(
        viewModel: ChatListViewModel,
        channels: ChannelsUiState,
        onOpen: @escaping (ConversationListRow) -> Void,
        onCreate: @escaping (CreateChoice) -> Void,
        onFoundChannel: @escaping (String, String) -> Void,
        onFoundPerson: @escaping (String) -> Void,
        onOpenFolderSettings: @escaping (Bool) -> Void = { _ in },
        peopleSearch: PeopleSearchActions? = nil
    ) {
        self.viewModel = viewModel
        self.channels = channels
        self.onOpen = onOpen
        self.onCreate = onCreate
        self.onFoundChannel = onFoundChannel
        self.onFoundPerson = onFoundPerson
        self.onOpenFolderSettings = onOpenFolderSettings
        self.peopleSearch = peopleSearch
    }

    /// Spec section 4 — the field covers users, groups and channels. The local
    /// half filters what this device holds; the network half is the single
    /// lookup row below the list.
    @State private var searchText: String = ""
    @State private var lookupBusy = false
    @State private var lookupNote: String?

    // MARK: Chat folders (`docs/ux/chat-folders.md`)

    /// The tab last selected on THIS device (section 2.5): a folder id in hex,
    /// or empty for All chats. Never synced. A folder id of the signed-in
    /// account under a device-wide key, so Forget device and Delete account
    /// remove it (``AccountLocalDataWipe/selectedFolderTabKey``).
    @AppStorage("lakko.chatFolders.selectedTab") private var storedTab: String = ""
    /// The editor sheet, when open.
    @State private var editor: FolderEditorRequest?
    /// The folder whose deletion is being confirmed.
    @State private var deleting: FolderView?

    public var body: some View {
#if DEBUG
        let _ = BodyEvaluationCounter.tick("ChatListScreen")
#endif
        content
            // Section 2.1 — the strip under the navigation title, above the
            // list. Hidden until a folder exists, and while searching, since a
            // search is over everything (section 2.4).
            .safeAreaInset(edge: .top, spacing: 0) {
                if showsFolderBar {
                    FolderBar(
                        folders: deviceGroupSync.folders,
                        selection: selectedTabBinding,
                        counts: folderCounts,
                        anyUnread: ChatFolders.anyUnread(allRows),
                        onEdit: { editor = FolderEditorRequest(folder: $0, rows: allRows) },
                        onMarkAllRead: { folder in Task { await markAllRead(in: folder) } },
                        onReorder: { onOpenFolderSettings(true) },
                        onEditFolders: { onOpenFolderSettings(false) },
                        onDelete: { deleting = $0 })
                }
            }
            .sheet(item: $editor) { request in
                FolderEditorScreen(
                    request: request,
                    listedFolderIds: deviceGroupSync.folders.map(\.folderId),
                    onSave: { changes in try await deviceGroupSync.applyFolderChanges(changes) },
                    onDelete: { folder in deleteFolder(folder) })
            }
            .alert(
                deleting.map { l10n.t("folders.delete_confirm.title", texts: ["name": $0.meta.name]) } ?? "",
                isPresented: Binding(
                    get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
                presenting: deleting
            ) { folder in
                Button(l10n.t("common.cancel"), role: .cancel) { deleting = nil }
                Button(l10n.t("common.delete"), role: .destructive) { deleteFolder(folder) }
            } message: { _ in
                Text(l10n.t("folders.delete_confirm.body"))
            }
            .navigationTitle(l10n.t("nav.chats"))
            .navigationBarTitleDisplayMode(.large)
            .searchable(text: $searchText, prompt: Text(l10n.t("chats.search.placeholder")))
            .onChange(of: searchText) { _, _ in lookupNote = nil }
            // Receiving is automatic; the pull gesture runs the same poll
            // once more, on demand (UX spec section 2, principle 3).
            .refreshable { await viewModel.pollNow() }
            // A channel's last post time is a server read, and it is the only
            // thing the shared order has to sort a channel by. Without this the
            // channels sit at the bottom of the list until something else
            // fetches them.
            .task { await refreshChannels() }
            .toolbar {
                // ADR-0050 / docs/ux/second-device.md section 2: one label
                // while this device is still being admitted to the chats a
                // pairing named. Chats appear as they arrive; never a
                // counter, never a per-chat row.
                if viewModel.synchronizing {
                    ToolbarItem(placement: .navigationBarLeading) {
                        Text(l10n.t("chats.synchronizing"))
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .accessibilityAddTraits(.updatesFrequently)
                    }
                }
                ToolbarItem(placement: .navigationBarTrailing) { createMenu }
            }
    }

    // MARK: - Create menu

    /// Spec section 3 — one button that opens a menu of exactly three items,
    /// not a compose action. A bare `+` reads as "add contact", so the icon is
    /// the compose one.
    private var createMenu: some View {
        Menu {
            Button { onCreate(.chat) } label: {
                Label(l10n.t("chats.create.chat"), systemImage: "bubble.left")
            }
            Button { onCreate(.group) } label: {
                Label(l10n.t("chats.create.group"), systemImage: "person.3")
            }
            Button { onCreate(.channel) } label: {
                Label(l10n.t("chats.create.channel"), systemImage: "megaphone")
            }
        } label: {
            Image(systemName: "square.and.pencil")
        }
        .accessibilityLabel(l10n.t("chats.create.menu"))
    }

    // MARK: - Rows

    /// Every conversation this device holds, ordered by the shared rule.
    ///
    /// The mapping is the work; the order is not here. Chats and groups come
    /// from one store that used to be split by a filter, channels from their
    /// own — and a channel's last post time has to become the same field as a
    /// chat's last message time, or the one ordering has nothing to order by.
    private var rows: [ConversationListRow] {
        ConversationList.filtered(allRows, query: searchText)
    }

    /// Every conversation, ordered, unfiltered: what All chats shows and what
    /// every folder is matched against.
    private var allRows: [ConversationListRow] {
        Self.allRows(
            viewModel: viewModel,
            channels: channels,
            flags: deviceGroupSync.flags,
            readCursors: deviceGroupSync.readCursors)
    }

    /// The merged list, ordered by the shared rule. One definition for the
    /// Chats tab, the folder editor and Settings > Chat folders.
    static func allRows(
        viewModel: ChatListViewModel,
        channels: ChannelsUiState,
        flags: [String: FlagsState],
        readCursors: [String: ReadCursorState]
    ) -> [ConversationListRow] {
        let chatRows = Self.chatRows(viewModel: viewModel, flags: flags, readCursors: readCursors)
        return ConversationList.sorted(chatRows + Self.channelRows(channels: channels))
    }

    /// The chat and group rows, in the shape the ordering takes. One
    /// definition: the Chats tab draws these, and ADR-0052's "Forward to"
    /// picker (``ForwardPickerScreen``) lists the same rows, so the two cannot
    /// disagree about what a conversation is called or which ones exist.
    static func chatRows(
        viewModel: ChatListViewModel,
        flags flagsMap: [String: FlagsState],
        readCursors: [String: ReadCursorState]
    ) -> [ConversationListRow] {
        viewModel.threads.map { thread -> ConversationListRow in
            let kind = viewModel.kind(of: thread.groupIdHex)
            let flags = flagsMap[thread.groupIdHex]
            return ConversationListRow(
                id: thread.groupIdHex,
                kind: kind == .group ? .group : .chat,
                title: viewModel.title(groupIdHex: thread.groupIdHex),
                preview: viewModel.preview(thread: thread, isGroup: kind == .group),
                lastActivityMs: viewModel.lastMessageMs(thread: thread),
                unread: UnreadCounting.displayed(
                    local: thread.unread,
                    cursorDerived: Self.unreadCount(
                        thread: thread,
                        cursor: readCursors[thread.groupIdHex])),
                pinned: flags?.pinned == true,
                muted: flags?.muted == true,
                archived: flags?.archived == true,
                // ADR-0045 UX §6.2.4 — the chat STAYS in the list and gains one
                // muted line in the slot the preview occupies. Removing it
                // would delete a transcript the user still has as a side effect
                // of something somebody else did.
                endedLine: chatListLine(
                    membershipState(
                        isActive: !thread.membershipEnded, leaveRequestedAtMs: nil)),
                markedUnread: flags?.unreadOverride == true)
        }
    }

    /// A subscribed channel, in the shape the ordering takes.
    ///
    /// Its title and second line come from ``ChannelsFormatting/rowModel`` —
    /// the vocabulary the channel screens already use, so there is one of them
    /// rather than two. Nothing posted yet sorts by when the channel was
    /// added, the same way a chat with no messages sorts by when it was
    /// created; the alternative is a freshly subscribed channel invisible at
    /// the bottom of a long list until a fetch lands.
    static func channelRows(channels: ChannelsUiState) -> [ConversationListRow] {
        let nowMs = ChatRepository.nowMs()
        return channels.channels.map { channel in
            let posts = channels.postsByChannel[channel.id] ?? []
            let lastPostMs = ChannelsFormatting.newestPostMs(posts.map(\.serverTsMs))
            let model = ChannelsFormatting.rowModel(
                sharedName: channel.sharedName,
                name: channel.localName,
                handle: channel.handle,
                isOwner: channels.keysByChannel[channel.id] != nil,
                subscriberCount: channels.subscriberCountByChannel[channel.id],
                newestPostMs: lastPostMs,
                nowMs: nowMs,
                calendar: .current,
                locale: Localization.currentLocale)
            return ConversationListRow(
                id: channel.id,
                kind: .channel,
                title: model.title,
                preview: model.secondLine,
                lastActivityMs: lastPostMs ?? Int64(clamping: channel.addedAtMs))
        }
    }

    @ViewBuilder
    private var content: some View {
        if showsFolderBar {
            // Section 2.1, iOS — the list pages sideways between folders and
            // the strip follows. All chats is always the first page.
            let all = allRows
            TabView(selection: selectedTabBinding) {
                listPage(rows: all, folder: nil)
                    .tag("")
                ForEach(deviceGroupSync.folders, id: \.folderId) { folder in
                    listPage(rows: ChatFolders.rows(in: folder, from: all), folder: folder)
                        .tag(ChatFolders.hex(folder.folderId))
                }
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
        } else {
            listPage(rows: rows, folder: nil)
        }
    }

    /// One page: All chats (`folder == nil`) or one folder's rows.
    @ViewBuilder
    private func listPage(rows visible: [ConversationListRow], folder: FolderView?) -> some View {
        if visible.isEmpty {
            // Inside a scroll view so the pull gesture works on the empty list
            // too.
            ScrollView {
                if let folder {
                    folderEmptyState(folder)
                } else if query.isEmpty {
                    emptyState
                } else {
                    VStack(spacing: 12) {
                        Text(l10n.t("chats.search.no_results"))
                            .foregroundStyle(Color.lakkoTextMuted)
                        lookupSection
                        if let peopleSearch {
                            PeopleSearchSection(query: query, actions: peopleSearch)
                        }
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.horizontal, 16)
                    .padding(.top, 80)
                }
            }
        } else {
            let quietCall = callHost.quietIncomingCall
            List {
                ForEach(visible) { row in
                    VStack(spacing: 8) {
                        Button { onOpen(row) } label: {
                            ConversationRowView(row: row, timeLabel: timeLabel(row))
                        }
                        .buttonStyle(.plain)
                        // `docs/ux/calls.md` section 2.1.1 — a muted
                        // conversation's ringing call is answerable from its own
                        // row, with no navigation. Outside the row's own tap
                        // target on purpose: a button nested in another button's
                        // label does not reliably receive the tap, and opening
                        // the chat has to keep working while the offer stands.
                        if let quietCall, QuietIncomingCallPresentation.showsOnListRow(
                            quietCall, rowId: row.id) {
                            QuietIncomingCallRowActions(
                                call: quietCall,
                                onAccept: {
                                    callHost.acceptIncomingCall(withVideo: quietCall.video)
                                },
                                onDecline: { callHost.declineIncomingCall() }
                            )
                        }
                    }
                    .listRowSeparator(.hidden)
                    .listRowInsets(EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
                    .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                        rowActions(row: row, folder: folder)
                    }
                    // Spec section 2.1, Per platform — the row's own menu is a
                    // long press on both natives: the swipe's actions plus the
                    // folder items (`docs/ux/chat-folders.md` section 5).
                    .contextMenu {
                        rowActions(row: row, folder: folder, inMenu: true)
                        folderActions(row: row, folder: folder)
                    }
                }
                // ADR-0060 — the network search stays offered under the local
                // results (UX section 6.1).
                if !query.isEmpty, let peopleSearch {
                    PeopleSearchSection(query: query, actions: peopleSearch)
                        .listRowSeparator(.hidden)
                }
            }
            .listStyle(.plain)
            // Rebuilt on a layout-direction flip: a UIKit-backed List keeps the
            // mirroring it was built with (see SettingsScreen).
            .id(l10n.layoutDirection)
        }
    }

    /// `docs/ux/chat-folders.md` 2.4 — one line and one secondary button, the
    /// same whatever the folder's rule is.
    private func folderEmptyState(_ folder: FolderView) -> some View {
        VStack(spacing: 12) {
            Text(l10n.t("folders.empty"))
                .foregroundStyle(Color.lakkoTextMuted)
            Button(l10n.t("folders.edit")) {
                editor = FolderEditorRequest(folder: folder, rows: allRows)
            }
            .buttonStyle(.bordered)
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 16)
        .padding(.top, 80)
    }

    // MARK: - Chat folders

    /// The strip shows once a folder exists, and never during a search.
    private var showsFolderBar: Bool {
        !deviceGroupSync.folders.isEmpty && query.isEmpty
    }

    /// The selected tab: the stored one while that folder exists, else All
    /// chats (section 2.5). Writing it stores it for the next launch.
    private var selectedTabBinding: Binding<String> {
        Binding(
            get: {
                ChatFolders.resolveSelectedTab(
                    stored: storedTab, folders: deviceGroupSync.folders) ?? ""
            },
            set: { storedTab = $0 })
    }

    private var folderCounts: [String: Int] {
        let all = allRows
        var out: [String: Int] = [:]
        for folder in deviceGroupSync.folders {
            out[ChatFolders.hex(folder.folderId)] = ChatFolders.unreadCount(folder, in: all)
        }
        return out
    }

    /// Section 5 — Add to folder, and inside a folder, Remove from folder.
    @ViewBuilder
    private func folderActions(row: ConversationListRow, folder: FolderView?) -> some View {
        let folders = deviceGroupSync.folders
        if folders.isEmpty {
            // With no folders yet, Add to folder goes straight to the editor.
            Button {
                editor = FolderEditorRequest(
                    folder: nil, prefill: ChatFolders.ref(for: row), rows: allRows)
            } label: {
                Label(l10n.t("folders.add_to_folder"), systemImage: "folder.badge.plus")
            }
        } else {
            Menu {
                ForEach(folders, id: \.folderId) { f in
                    let isIn = ChatFolders.matches(f, row)
                    Button {
                        Task { await toggleMembership(folder: f, row: row) }
                    } label: {
                        if isIn {
                            Label(f.meta.name, systemImage: "checkmark")
                        } else {
                            Text(f.meta.name)
                        }
                    }
                    .disabled(
                        !isIn && ChatFolders.handState(f, rowId: row.id) != .excluded
                            && ChatFolders.handAddLimitReached(f))
                }
                Divider()
                Button(l10n.t("folders.new_ellipsis")) {
                    editor = FolderEditorRequest(
                        folder: nil, prefill: ChatFolders.ref(for: row), rows: allRows)
                }
                .disabled(folders.count >= ChatFolders.maxFolders)
                if folders.count >= ChatFolders.maxFolders {
                    Text(ChatFolders.folderLimitLine)
                }
            } label: {
                Label(l10n.t("folders.add_to_folder"), systemImage: "folder.badge.plus")
            }
        }
        if let folder {
            Button {
                Task { await toggleMembership(folder: folder, row: row) }
            } label: {
                Label(l10n.t("folders.remove_from_folder"), systemImage: "folder.badge.minus")
            }
        }
    }

    private func toggleMembership(folder: FolderView, row: ConversationListRow) async {
        guard let changes = ChatFolders.toggleMembership(
            folder: folder, row: row, hlc: deviceGroupSync.mintHlc()) else { return }
        do {
            try await deviceGroupSync.applyFolderChanges(changes)
        } catch {
            NSLog("folder membership edit failed: %@", error.localizedDescription)
        }
    }

    private func toggleFolderPin(folder: FolderView, row: ConversationListRow) async {
        guard let changes = ChatFolders.togglePin(
            folder: folder, row: row, hlc: deviceGroupSync.mintHlc()) else { return }
        do {
            try await deviceGroupSync.applyFolderChanges(changes)
        } catch {
            NSLog("folder pin edit failed: %@", error.localizedDescription)
        }
    }

    private func deleteFolder(_ folder: FolderView) {
        deleting = nil
        let changes = ChatFolders.remove(folderId: folder.folderId, hlc: deviceGroupSync.mintHlc())
        Task {
            do {
                try await deviceGroupSync.applyFolderChanges(changes)
            } catch {
                NSLog("folder delete failed: %@", error.localizedDescription)
            }
        }
    }

    /// Section 2.2 — for every conversation in the tab that has something
    /// unread, the same mark-read the chat itself does: the local badge, the
    /// read cursor every device of the account folds, and a manual "mark as
    /// unread" cleared. Channels carry no unread state on this client.
    private func markAllRead(in folder: FolderView?) async {
        let all = allRows
        let targets = all.filter { row in
            guard ChatFolders.hasUnread(row), row.kind != .channel else { return false }
            guard let folder else { return true }
            return ChatFolders.matches(folder, row)
        }
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        for row in targets {
            guard let groupId = Self.hexToBytes(row.id) else { continue }
            if let upTo = viewModel.markReadLocally(groupIdHex: row.id) {
                try? await deviceGroupSync.setReadCursor(
                    groupId: groupId, upToMessageId: UnreadCounting.cursorBytes(upTo),
                    readAtMs: now)
            }
            if let flags = deviceGroupSync.flags[row.id], flags.unreadOverride {
                try? await deviceGroupSync.setFlags(
                    groupId: groupId, pinned: flags.pinned, muted: flags.muted,
                    archived: flags.archived, unreadOverride: false)
            }
        }
    }

    private var query: String {
        searchText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func timeLabel(_ row: ConversationListRow) -> String? {
        row.lastActivityMs.map {
            ChatListFormatting.lastMessageTime(
                ms: $0, nowMs: ChatRepository.nowMs(), calendar: .current, locale: l10n.locale)
        }
    }

    // MARK: - The one network lookup

    /// Spec section 4.3 — ONE row, for the whole name, offered only when
    /// nothing local matched and the text could be a name.
    ///
    /// Never a list of partial matches: the server evaluates the blinded name
    /// it was given and can answer "does this exact name exist" and nothing
    /// about prefixes, because a prefix search is the server learning the
    /// prefix.
    @ViewBuilder
    private var lookupSection: some View {
        if NameLookup.looksLikeAName(query) {
            Button { Task { await lookUp() } } label: {
                HStack(spacing: 12) {
                    Image(systemName: "magnifyingglass")
                        .foregroundStyle(Color.lakkoAccent)
                    Text(lookupBusy ? l10n.t("common.looking_up") : NameLookup.rowLabel(for: query))
                        .foregroundStyle(Color.lakkoText)
                }
            }
            .disabled(lookupBusy)
        }
        if let lookupNote {
            Text(lookupNote)
                .font(.footnote)
                .foregroundStyle(Color.lakkoTextMuted)
                .multilineTextAlignment(.center)
                .accessibilityAddTraits(.updatesFrequently)
        }
    }

    private func lookUp() async {
        if lookupBusy { return }
        lookupBusy = true
        lookupNote = nil
        defer { lookupBusy = false }
        let typed = query
        // A client per namespace rather than one captured across both: the two
        // lookups are sequential, a `DiscoveryClient` is a thin wrapper over a
        // session, and the alternative is a shared reference crossing a
        // concurrency boundary for no gain.
        let found = await NameLookup.find(
            typed: typed,
            lookups: NameLookups(
                channel: { try await DiscoveryClient().lookupChannel(handle: $0) },
                person: { handle in
                    let result = try await DiscoveryClient().lookup(username: handle)
                    return result.account == nil ? nil : result.normalisedUsername
                }))
        switch found {
        case .channel(let channelIdHex, let handle):
            searchText = ""
            onFoundChannel(channelIdHex, handle)
        case .person(let username):
            searchText = ""
            onFoundPerson(username)
        case .nothing, .unreachable, .allowanceUsed:
            lookupNote = NameLookup.message(for: found, typed: typed)
        }
    }

    // MARK: - Channel refresh

    /// Re-read each channel's posts, for the last-post time the order sorts on.
    /// Best effort: a failure leaves the cached value in place.
    private func refreshChannels() async {
        let client = ChannelsClient()
        for channel in channels.channels {
            if let page = try? await client.fetchPosts(channelIdHex: channel.id, after: 0) {
                channels.setPosts(channelId: channel.id, posts: page.posts)
            }
        }
    }

    // MARK: - Flags

    /// Compute unread count for a thread (Phase 2a3e2c-followup).
    ///
    /// Counts every incoming / system row whose `messageId` is strictly greater
    /// than the last broadcast read cursor. Outgoing rows never count — the
    /// sender already "read" them by composing.
    ///
    /// Returns `nil` when no read cursor has been broadcast for the thread, so
    /// the list falls back to the local count of messages the foreground
    /// receive applied while the chat was closed (``UnreadCounting``). Counting
    /// every prior row against a missing cursor would flip the whole history to
    /// unread on a cold start.
    private static func unreadCount(
        thread: ChatThread,
        cursor: ReadCursorState?
    ) -> Int? {
        guard let cursor else { return nil }
        let cursorId = Self.decodeBeI64(cursor.upToMessageId)
        return thread.messages.reduce(0) { acc, msg in
            if msg.direction == .outgoing { return acc }
            if msg.messageId > cursorId { return acc + 1 }
            return acc
        }
    }

    private static func decodeBeI64(_ data: Data) -> Int64 {
        guard data.count >= 8 else { return Int64.max }
        var v: UInt64 = 0
        for i in 0..<8 {
            v = (v << 8) | UInt64(data[data.startIndex + i])
        }
        return Int64(bitPattern: v)
    }

    /// The row's own actions, rendered both as a trailing swipe and as the
    /// long-press menu (spec section 2.1).
    ///
    /// Pin / mute / archive are keyed on a conversation id a channel does not
    /// carry, so in All chats a channel row offers none of them rather than
    /// offering one that could not be honoured. Inside a folder, Pin acts on
    /// that folder's own pin list (`docs/ux/chat-folders.md` 2.3), which does
    /// name channels.
    @ViewBuilder
    private func rowActions(
        row: ConversationListRow, folder: FolderView?, inMenu: Bool = false
    ) -> some View {
        if let folder {
            let pinned = ChatFolders.isPinned(row, in: folder)
            let atLimit = !pinned && folder.pinned.count >= ChatFolders.maxPins
            Button {
                Task { await toggleFolderPin(folder: folder, row: row) }
            } label: {
                Label(l10n.t(pinned ? "chats.action.unpin" : "chats.action.pin"), systemImage: pinned ? "pin.slash" : "pin")
            }
            .tint(Color.lakkoAccent)
            .disabled(atLimit)
            if atLimit && inMenu {
                Text(ChatFolders.pinLimitLine)
            }
        } else if row.kind != .channel {
            Button {
                Task { await toggleFlag(groupIdHex: row.id, field: .pin) }
            } label: {
                Label(l10n.t(row.pinned ? "chats.action.unpin" : "chats.action.pin"), systemImage: row.pinned ? "pin.slash" : "pin")
            }
            .tint(Color.lakkoAccent)
        }
        if row.kind != .channel {
            Button {
                Task { await toggleFlag(groupIdHex: row.id, field: .mute) }
            } label: {
                Label(l10n.t(row.muted ? "chats.action.unmute" : "chats.action.mute"), systemImage: row.muted ? "bell" : "bell.slash")
            }
            .tint(.gray)
            Button {
                Task { await toggleFlag(groupIdHex: row.id, field: .archive) }
            } label: {
                Label(
                    l10n.t(row.archived ? "chats.action.unarchive" : "chats.action.archive"),
                    systemImage: row.archived ? "tray.and.arrow.up" : "archivebox")
            }
            .tint(Color.lakkoTextMuted)
        }
    }

    private enum FlagField {
        case pin, mute, archive
    }

    @MainActor
    private func toggleFlag(groupIdHex: String, field: FlagField) async {
        guard let groupId = Self.hexToBytes(groupIdHex) else { return }
        let cur = deviceGroupSync.flags[groupIdHex]
        var pinned = cur?.pinned ?? false
        var muted = cur?.muted ?? false
        var archived = cur?.archived ?? false
        let unreadOverride = cur?.unreadOverride ?? false
        switch field {
        case .pin: pinned.toggle()
        case .mute: muted.toggle()
        case .archive: archived.toggle()
        }
        do {
            try await deviceGroupSync.setFlags(
                groupId: groupId,
                pinned: pinned,
                muted: muted,
                archived: archived,
                unreadOverride: unreadOverride
            )
        } catch {
            NSLog("setFlags failed: %@", error.localizedDescription)
        }
    }

    private static func hexToBytes(_ hex: String) -> Data? {
        guard hex.count % 2 == 0 else { return nil }
        var out = Data()
        out.reserveCapacity(hex.count / 2)
        var i = hex.startIndex
        while i < hex.endIndex {
            let next = hex.index(i, offsetBy: 2)
            guard let b = UInt8(hex[i..<next], radix: 16) else { return nil }
            out.append(b)
            i = next
        }
        return out
    }

    /// Spec section 2.3 — one line, and a button that opens the SAME create
    /// menu as the toolbar. Not a different set of choices in a different
    /// place.
    @ViewBuilder
    private var emptyState: some View {
        VStack(spacing: 12) {
            Image(systemName: "bubble.left.and.bubble.right")
                .font(.system(size: 40))
                .foregroundStyle(Color.lakkoTextMuted)
            Text(l10n.t("chats.empty.title"))
                .font(.headline)
            Menu {
                Button { onCreate(.chat) } label: {
                    Label(l10n.t("chats.create.chat"), systemImage: "bubble.left")
                }
                Button { onCreate(.group) } label: {
                    Label(l10n.t("chats.create.group"), systemImage: "person.3")
                }
                Button { onCreate(.channel) } label: {
                    Label(l10n.t("chats.create.channel"), systemImage: "megaphone")
                }
            } label: {
                Text(l10n.t("chats.empty.action"))
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                    .background(Color.lakkoAccent)
                    .foregroundStyle(Color.lakkoAccentForeground)
                    .clipShape(Capsule())
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity)
        .padding(.top, 80)
    }
}

/// One row of the merged list, drawn identically for all three kinds (spec
/// section 2.1): same avatar treatment, same title, same second line, same
/// trailing column.
/// Internal rather than private: ADR-0052's "Forward to" picker draws the same
/// row (section 5.1, "the same merged list the Chats tab shows").
struct ConversationRowView: View {
    let row: ConversationListRow
    /// The last activity's time, `nil` when nothing has happened yet.
    let timeLabel: String?

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            InitialsAvatar(
                // The id is hashed for the tint only, never shown.
                name: row.title, identity: row.id, size: 40)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(row.title)
                        .font(.body)
                        .fontWeight(row.showsUnread ? .semibold : .regular)
                        .foregroundStyle(Color.lakkoText)
                        .lineLimit(1)
                    Spacer(minLength: 8)
                    if let timeLabel {
                        Text(timeLabel)
                            .font(.caption)
                            .foregroundStyle(
                                row.showsUnread ? Color.lakkoAccent : Color.lakkoTextMuted)
                            .lineLimit(1)
                            .fixedSize()
                    }
                }
                HStack(alignment: .center, spacing: 6) {
                    Text(row.secondLine)
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .lineLimit(1)
                    Spacer(minLength: 8)
                    trailingMarks
                }
            }
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
        .opacity(row.archived ? 0.6 : 1)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityText)
    }

    /// Muted and pinned glyphs, then the unread capsule. A channel carries
    /// none of them, because none of them is keyed on anything a channel has.
    @ViewBuilder
    private var trailingMarks: some View {
        HStack(spacing: 6) {
            if row.muted {
                Image(systemName: "bell.slash.fill")
                    .font(.caption2)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if row.pinned {
                Image(systemName: "pin.fill")
                    .font(.caption2)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if row.archived {
                Image(systemName: "archivebox.fill")
                    .font(.caption2)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if row.showsUnread {
                // Accent capsule; a muted conversation renders it grey so a
                // muted-but-unread row still surfaces a hint without grabbing
                // attention.
                Text(row.unread > 99 ? Localization.text("chats.row.unread_overflow") : row.unread.formatted(.number.locale(Localization.shared.locale)))
                    .font(.caption2)
                    .fontWeight(.semibold)
                    .padding(.horizontal, 7)
                    .padding(.vertical, 2)
                    .background(row.muted ? Color.lakkoTextMuted : Color.lakkoAccent)
                    .foregroundStyle(Color.lakkoAccentForeground)
                    .clipShape(Capsule())
            }
        }
    }

    private var accessibilityText: String {
        var parts = [row.title]
        if let timeLabel { parts.append(timeLabel) }
        parts.append(row.secondLine)
        if row.showsUnread {
            parts.append(Localization.text("chats.row.unread", counts: ["count": UInt64(max(0, row.unread))]))
        }
        if row.muted { parts.append(Localization.text("chats.row.muted")) }
        if row.pinned { parts.append(Localization.text("chats.row.pinned")) }
        return parts.joined(separator: ", ")
    }
}

/// Lightweight VM that mirrors the threads list from
/// ``ChatRepository``. SwiftUI's `@ObservedObject` chain only
/// traverses one ObservableObject per body — the VM exposing the
/// thread list directly keeps the list view independent of any
/// per-thread state the screen-level ``ChatViewModel`` owns.
@MainActor
public final class ChatListViewModel: ObservableObject {

    @Published public private(set) var threads: [ChatThread] = []

    private let repository: ChatRepository
    private var cancellables: Set<AnyCancellable> = []

    public init(repository: ChatRepository) {
        self.repository = repository
        self.threads = repository.state.threads
        repository.$state
            .map(\.threads)
            .receive(on: RunLoop.main)
            .sink { [weak self] threads in
                self?.threads = threads
            }
            .store(in: &cancellables)
        // ADR-0048 — a profile or a verdict arriving renames rows in place.
        ProfileDirectory.shared.$revision
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        // ADR-0051 — a shared group name arriving (or set here) re-titles the
        // group's row in place.
        repository.$groupNameRevision
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
    }

    /// ADR-0050 section 2.9 — whether groups the pairing bundle named are not
    /// yet all joined here. Drives the single "Synchronizing..." label.
    var synchronizing: Bool {
        StateBundle.pendingKnownGroups(joinedGroupIdHexes: threads.map { $0.groupIdHex }) > 0
    }

    /// The conversation's kind. Only routes a tap and picks the preview's
    /// sender prefix; the list draws both the same.
    ///
    /// The decision — and the write it can make — lives on the repository,
    /// because the Calls index asks the same question and two copies of a
    /// decision that also writes would race each other.
    func kind(of groupIdHex: String) -> ConversationKind {
        repository.conversationKind(groupIdHex: groupIdHex)
    }

    /// The row title (ADR-0051 §5): the shared group name whenever the
    /// conversation has one — whatever its kind or member count, since a
    /// two-person conversation renamed from another client is still keyed as
    /// a chat here — else the other person's resolved name (ADR-0048) for a
    /// chat, else the members' names. Never a hex id (UX spec section 6).
    func title(groupIdHex: String) -> String {
        if let shared = repository.groupName(groupIdHex: groupIdHex) {
            return shared
        }
        if let peer = repository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex) {
            return ProfileDirectory.shared.resolve(signatureKey: peer).name
        }
        return ConversationTitle.groupTitle(
            otherMemberNames: Self.otherMemberNames(repository: repository, groupIdHex: groupIdHex))
    }

    /// One entry per other person in the group; `nil` for a person whose
    /// only label would be a key fingerprint.
    static func otherMemberNames(repository: ChatRepository, groupIdHex: String) -> [String?] {
        let members = repository.members(groupIdHex: groupIdHex) ?? []
        let own = repository.ownLeafIndex(groupIdHex: groupIdHex)
        return members
            .filter { $0.leafIndex != own }
            .map { member in
                let resolved = ProfileDirectory.shared.resolve(signatureKey: member.signaturePublicKey)
                return resolved.isPlaceholder ? nil : resolved.name
            }
    }

    /// UX spec section 5.1 — the second line of a row: the last message with
    /// a sender prefix in a group ("Anna: ..."), "You: " for own messages,
    /// and a one-word label for media. "No messages yet" for an empty thread.
    /// Only messages the timeline draws count (a bookkeeping system row is
    /// not a last message).
    func preview(thread: ChatThread, isGroup: Bool) -> String {
        guard let last = thread.messages.last(where: { !ChatTimeline.isHiddenSystemRow($0) }) else {
            return Localization.text("chats.row.no_messages")
        }
        let senderName = last.authorKeyB64.flatMap {
            ProfileDirectory.shared.resolve(signatureKeyB64: $0)?.name
        }
        return ChatListFormatting.preview(
            text: last.text,
            attachmentLabels: last.attachments.map {
                ChatListFormatting.mediaLabel(kind: $0.kind, mime: $0.mime)
            },
            direction: last.direction,
            deleted: last.deleted,
            isGroup: isGroup,
            senderName: senderName)
    }

    /// The server time of the last drawn message, for the row's time and the
    /// list order; `nil` when there is none or it does not parse.
    func lastMessageMs(thread: ChatThread) -> Int64? {
        guard let last = thread.messages.last(where: { !ChatTimeline.isHiddenSystemRow($0) }) else {
            return nil
        }
        return ChatRepository.parseDsTimestampMs(last.createdAt)
    }

    /// Chat folders' Mark all as read: clear the local badge and return the
    /// newest server message id, for the read cursor the caller broadcasts.
    /// `nil` when the chat holds no acknowledged message.
    func markReadLocally(groupIdHex: String) -> Int64? {
        repository.clearUnread(groupIdHex: groupIdHex)
        guard let thread = threads.first(where: { $0.groupIdHex == groupIdHex }) else {
            return nil
        }
        return thread.messages.map(\.messageId).filter { $0 > 0 }.max()
    }

    /// The pull-to-refresh action: the same poll the app already runs, once,
    /// for every conversation in the list. Best-effort; a failing group is
    /// skipped.
    func pollNow() async {
        for thread in threads {
            try? await repository.pollGroup(groupIdHex: thread.groupIdHex)
        }
    }
}
