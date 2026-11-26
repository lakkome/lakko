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
/// differing avatar; the kind only routes a tap. A channel's pin, mute and
/// archive are the same synced flags as a group's, keyed on the channel id
/// (``ChannelFlags``). Its unread badge is its count of new posts
/// (`docs/ux/feed.md` section 5, "Channel posts"), kept on this device.
///
/// Rendered as the root of the Chats tab's `NavigationStack`; tapping a row
/// asks the host to push the screen that row's kind opens.
public struct ChatListScreen: View {

    /// What the create menu was asked for (spec section 3).
    public enum CreateChoice: Equatable {
        case chat
        case group
        case channel
        /// ADR-0065 / UX `saved-messages-and-file-lifetime.md` section 3 —
        /// not a creation: the Saved screen, reached from the same menu.
        case saved
    }

    @ObservedObject public var viewModel: ChatListViewModel
    /// The channel store, so the list can hold channels beside chats and
    /// groups. Observed rather than passed as a snapshot: a post arriving
    /// re-times a channel's row, and re-timing is re-ordering.
    @ObservedObject public var channels: ChannelsUiState
    /// `docs/ux/feed.md` section 5 — each channel's count of new posts (posts
    /// newer than the newest read, in the feed or the channel), drawn as the
    /// row's unread badge.
    @ObservedObject var feedReading: FeedReadingModel
    /// ADR-0061 — the title follows the interface language.
    @ObservedObject private var l10n = Localization.shared
    /// docs/ux/connection-state.md — "Waiting for network..." /
    /// "Connecting..." while this device cannot reach the servers.
    @ObservedObject private var connection = ConnectionState.shared
    /// ADR-0099 step 3 — the read-only app of a suspended account.
    @Environment(\.accountSuspended) private var accountSuspended
    /// The ledger openings of bare `/g/` links a row's second line reads
    /// (``BareGroupLink``): a finished opening redraws the row.
    @ObservedObject private var bareGroupLinks = BareGroupLinks.shared
    /// Open the conversation this row names, whichever kind it is.
    public var onOpen: (ConversationListRow) -> Void
    /// One of the three create-menu items.
    public var onCreate: (CreateChoice) -> Void
    /// A channel the search found by its handle, which this device may not
    /// hold yet: its subscribe page.
    public var onFoundChannel: (_ channelIdHex: String, _ handle: String) -> Void
    /// `Subscribe` on the card of a public channel found by search
    /// (`docs/ux/group-and-channel-privacy.md` section 5.1): subscribe in one
    /// step and open the channel under the name its record carries.
    public var onSubscribeFoundChannel: (_ channelIdHex: String, _ handle: String, _ name: String) -> Void
    /// **Open** on a found person who is already a contact: the conversation
    /// with them (`docs/ux/who-can-find-me.md` section 6.1).
    public var onOpenPerson: (FoundPerson) -> Void
    /// `docs/ux/chat-folders.md` 2.2 — Reorder folders / Edit folders open
    /// Settings > Chat folders (`true` = in its reorder state).
    public var onOpenFolderSettings: (_ reorder: Bool) -> Void
    /// UX 18.4, "Done" — the one-session line after the account was taken
    /// back on this device (``RecalledNoticeBanner``). A view of its own, so
    /// this screen is not re-evaluated by it; `nil` draws nothing.
    var recalledNotice: RecalledNoticeBanner?
    /// UX 19.6 — what the account's devices are told (ADR-0075).
    var accountNotice: AccountNoticeBanner?
    /// UX 18.3 — `Updating keys...` in the header while a kept device adopts
    /// the account's new keys (``KeysUpdatingLabel``); `nil` draws nothing.
    var keysUpdatingLabel: KeysUpdatingLabel?
    /// ADR-0060 — the search field's people (`docs/ux/who-can-find-me.md`
    /// section 6.1): the people this device knows, and from three characters
    /// the network search under People, asked without a button. `nil` shows
    /// conversations only.
    public var peopleSearch: PeopleSearchModel?

    /// Phase 2a3e2c-iOS — cross-device per-thread flags (pinned / muted /
    /// archived). The Combine subscription inside ``DeviceGroupSyncRepository``
    /// keeps the row indicators + the sort order live as sibling devices
    /// publish updates.
    @EnvironmentObject private var deviceGroupSync: DeviceGroupSyncRepository
    /// ADR-0079 — the people added through their invite link whose app has
    /// not answered yet are listed in All chats at once.
    @EnvironmentObject private var contactsRepository: ContactsRepository
    /// `docs/ux/calls.md` section 2.1.1 — the app-level 1x1 call host, read for
    /// ONE thing: a call arriving in a MUTED conversation, which is offered on
    /// that conversation's row here as well as in the chat itself. Every other
    /// call surface is the root view's (``CallHostOverlay``); this one has to be
    /// where the person already looks, and answerable without navigating.
    /// The account signed in here, for the restored-device notice.
    var accountIdB64: String?
    /// `Contacts` on the restored-device card.
    var onOpenContacts: () -> Void = {}
    /// The marker of a restore, read when the list opens and when a restore
    /// finishes (section 7.2 of `docs/ux/identity-recovery.md`).
    @State private var restored: RestoredDeviceStore.Record?
    @EnvironmentObject private var callHost: CallHost

    init(
        viewModel: ChatListViewModel,
        channels: ChannelsUiState,
        feedReading: FeedReadingModel,
        onOpen: @escaping (ConversationListRow) -> Void,
        onCreate: @escaping (CreateChoice) -> Void,
        onFoundChannel: @escaping (String, String) -> Void,
        onSubscribeFoundChannel: @escaping (String, String, String) -> Void = { _, _, _ in },
        onOpenPerson: @escaping (FoundPerson) -> Void = { _ in },
        onOpenFolderSettings: @escaping (Bool) -> Void = { _ in },
        accountNotice: AccountNoticeBanner? = nil,
        recalledNotice: RecalledNoticeBanner? = nil,
        keysUpdatingLabel: KeysUpdatingLabel? = nil,
        peopleSearch: PeopleSearchModel? = nil,
        accountIdB64: String? = nil,
        onOpenContacts: @escaping () -> Void = {}
    ) {
        self.accountIdB64 = accountIdB64
        self.onOpenContacts = onOpenContacts
        self.viewModel = viewModel
        self.channels = channels
        self.feedReading = feedReading
        self.onOpen = onOpen
        self.onCreate = onCreate
        self.onFoundChannel = onFoundChannel
        self.onSubscribeFoundChannel = onSubscribeFoundChannel
        self.onOpenPerson = onOpenPerson
        self.onOpenFolderSettings = onOpenFolderSettings
        self.recalledNotice = recalledNotice
        self.accountNotice = accountNotice
        self.keysUpdatingLabel = keysUpdatingLabel
        self.peopleSearch = peopleSearch
    }

    /// Spec section 4 — the field covers users, groups and channels. The local
    /// half filters what this device holds; the network half asks by itself
    /// (`docs/ux/who-can-find-me.md` section 6.1, ``PeopleSearchModel``).
    @State private var searchText: String = ""
    /// UX 6.2 — the `Requests sent` list.
    @State private var requestsSentOpen = false
    /// UX 5.1 — the card of a public group or channel opened from search.
    @State private var foundCard: ConversationHandles.Found?

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
    @State private var createMenuOpen = false
    /// Navigation 2.2 step 3 — the archive opened from the "Archived" bar:
    /// the tab it was opened in (empty for All chats, else the folder id in
    /// hex), `nil` while the chats list shows. Kept while the list exists,
    /// never stored.
    @State private var archiveTag: String?
    @Environment(\.layoutDirection) private var layoutDirection
    /// The toolbar's create menu carries Saved; the empty state's does not.
    @State private var createMenuWithSaved = true
    /// Navigation 2.2.1 — the pin order a move just wrote, per tab (empty for
    /// All chats, else the folder id in hex), drawn until its write has gone
    /// through. The synced state folds the move before it publishes, but the
    /// write runs a turn later, and without this the row would jump back to
    /// where it was for that turn.
    @State private var pendingPins: [String: [String]] = [:]

    public var body: some View {
#if DEBUG
        let _ = BodyEvaluationCounter.tick("ChatListScreen")
#endif
        let archive = openArchive
        panel(archive: archive)
            // Section 2.1 — the strip under the navigation title, above the
            // list. Hidden until a folder exists, and while searching, since a
            // search is over everything (section 2.4).
            .safeAreaInset(edge: .top, spacing: 0) {
              VStack(spacing: 0) {
                // ADR-0099 step 3 — reading a suspended account's chats: the
                // suspended screen is one tap away.
                if accountSuspended {
                    SuspendedBanner { SuspensionCenter.shared.showScreen() }
                }
                // ADR-0101 section 7 — a channel coming back after the key
                // reset whose old card this device could not confirm: the
                // ordinary invitation card, for the person to decide on.
                if archive == nil, let accountIdB64 {
                    ReturnedInvitationsStrip(channels: channels, accountIdB64: accountIdB64)
                }
                if showsFolderBar && archive == nil {
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
            }
            .sheet(item: $editor) { request in
                FolderEditorScreen(
                    request: request,
                    listedFolderIds: deviceGroupSync.folders.map(\.folderId),
                    onSave: { changes in try await deviceGroupSync.applyFolderChanges(changes) },
                    onDelete: { folder in deleteFolder(folder) })
            }
            .lakkoDialog(item: $deleting) { folder in
                LakkoDialogContent(
                    title: l10n.t("folders.delete_confirm.title", texts: ["name": folder.meta.name]),
                    message: l10n.t("folders.delete_confirm.body"),
                    actions: [
                        .cancel { deleting = nil },
                        .danger(l10n.t("common.delete")) { deleteFolder(folder) },
                    ])
            }
            .lakkoActionSheet(isPresented: $createMenuOpen) { createMenuItems(includeSaved: createMenuWithSaved) }
            .navigationTitle(
                connection.phase.headerKey.map { l10n.t($0) }
                    ?? (archive == nil ? l10n.t("nav.chats") : l10n.t("chats.archived.row")))
            // Large in both views: the archive list, drawn over the chats list
            // that stays alive underneath, still drives the title's collapse
            // (seen on the simulator 2026-11-25). Switching to inline for the
            // archive left Chats inline after going back.
            .navigationBarTitleDisplayMode(.large)
            // Always shown: with the folder bar inset above the list the system
            // collapsed the field and never brought it back, so Chats had no
            // search once a folder existed (base scenario 31, 2026-11-11).
            .searchable(
                text: $searchText,
                placement: .navigationBarDrawer(displayMode: .always),
                prompt: Text(l10n.t("chats.search.placeholder")))
            .onChange(of: searchText) { _, text in peopleSearch?.update(query: text) }
            // Receiving is automatic; the pull gesture runs the same poll
            // once more, on demand (UX spec section 2, principle 3).
            .refreshable { await viewModel.pollNow() }
            // A channel's last post time is a server read, and it is the only
            // thing the shared order has to sort a channel by. Without this the
            // channels sit at the bottom of the list until something else
            // fetches them.
            .task { await refreshChannels() }
            .task(id: accountIdB64) { reloadRestored() }
            .onReceive(NotificationCenter.default.publisher(for: .lakkoBackupRestored)) { _ in
                reloadRestored()
            }
            .onChange(of: allRows.isEmpty && invitedRows.isEmpty) { _, _ in noteConversationIfAny() }
            // Navigation 2.2 step 3 — the archive closes by itself once it
            // holds nothing and nothing is typed (unarchiving the last one),
            // or when the folder it was opened in is gone.
            .onChange(of: archiveTag != nil && archive == nil) { _, closed in
                if closed { archiveTag = nil }
            }
            .toolbar {
                if archive != nil {
                    ToolbarItem(placement: .navigationBarLeading) { archiveBackButton }
                }
                // ADR-0050 / docs/ux/second-device.md section 2: one label
                // while this device is still being admitted to the chats a
                // pairing named. Chats appear as they arrive; never a
                // counter, never a per-chat row.
                if viewModel.synchronizing && archive == nil {
                    ToolbarItem(placement: .navigationBarLeading) {
                        Text(l10n.t("chats.synchronizing"))
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .accessibilityAddTraits(.updatesFrequently)
                    }
                }
                if let keysUpdatingLabel, !viewModel.synchronizing, archive == nil {
                    ToolbarItem(placement: .navigationBarLeading) { keysUpdatingLabel }
                }
                // ADR-0099 step 3 — a suspended account starts nothing.
                if !accountSuspended {
                    ToolbarItem(placement: .navigationBarTrailing) { createMenu }
                }
            }
    }

    // MARK: - Create menu

    /// Spec section 3 — one button that opens a menu of exactly three items,
    /// not a compose action. A bare `+` reads as "add contact", so the icon is
    /// the compose one.
    private var createMenu: some View {
        Button {
            createMenuWithSaved = true
            createMenuOpen = true
        } label: {
            LkIconView(.compose, size: 24)
        }
        .accessibilityLabel(l10n.t("chats.create.menu"))
    }

    /// The create menu's rows (Lakko's own action sheet, never a system menu).
    private func createMenuItems(includeSaved: Bool) -> [LakkoMenuItem] {
        var items: [LakkoMenuItem] = [
            .action(l10n.t("chats.create.chat"), icon: .chat) { onCreate(.chat) },
            .action(l10n.t("chats.create.group"), icon: .person3) { onCreate(.group) },
            .action(l10n.t("chats.create.channel"), icon: .megaphone) { onCreate(.channel) },
        ]
        if includeSaved {
            items.append(.action(l10n.t("saved.title"), icon: .bookmark) { onCreate(.saved) })
        }
        return items
    }

    // MARK: - Rows

    /// Every conversation this device holds, ordered by the shared rule.
    ///
    /// The mapping is the work; the order is not here. Chats and groups come
    /// from one store that used to be split by a filter, channels from their
    /// own — and a channel's last post time has to become the same field as a
    /// chat's last message time, or the one ordering has nothing to order by.
    private var rows: [ConversationListRow] {
        let all = allRows
        let byText = ConversationList.filtered(all, query: searchText)
        // ADR-0087 — a group or channel the person is in is found by its
        // `@name` too, with or without the `@`.
        let needle = String(searchText.trimmingCharacters(in: .whitespacesAndNewlines)
            .drop(while: { $0 == "@" })).lowercased()
        guard !needle.isEmpty else { return byText }
        let found = Set(byText.map(\.id))
        return all.filter { row in
            found.contains(row.id) || (heldHandle(row).map { $0.contains(needle) } ?? false)
        }
    }

    /// The `@name` (without `@`, lower case) of a group or channel row.
    private func heldHandle(_ row: ConversationListRow) -> String? {
        switch row.kind {
        case .channel:
            let handle = channels.channels.first { $0.id == row.id }?.handle ?? ""
            return handle.isEmpty ? nil : handle.lowercased()
        case .group:
            return viewModel.repository.groupHandle(groupIdHex: row.id)?.handle.lowercased()
        default:
            return nil
        }
    }

    /// Every conversation, ordered, unfiltered: what All chats shows and what
    /// every folder is matched against.
    private var allRows: [ConversationListRow] {
        Self.allRows(
            viewModel: viewModel,
            channels: channels,
            flags: deviceGroupSync.flags,
            readCursors: deviceGroupSync.readCursors,
            channelUnread: feedReading.channelUnread,
            channelLastPosts: feedReading.channelLastPost,
            pinOrder: pendingPins[""] ?? PinOrder.storedIds(deviceGroupSync.order))
    }

    /// The merged list, ordered by the shared rule. One definition for the
    /// Chats tab, the folder editor and Settings > Chat folders.
    static func allRows(
        viewModel: ChatListViewModel,
        channels: ChannelsUiState,
        flags: [String: FlagsState],
        readCursors: [String: ReadCursorState],
        channelUnread: [String: Int],
        channelLastPosts: [String: PersistedChannelPost] = [:],
        pinOrder: [String]
    ) -> [ConversationListRow] {
        let chatRows = Self.chatRows(viewModel: viewModel, flags: flags, readCursors: readCursors)
        return ConversationList.sorted(
            chatRows + Self.channelRows(
                channels: channels, channelUnread: channelUnread, lastPosts: channelLastPosts,
                flags: flags),
            pinOrder: pinOrder)
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
            // ADR-0073 R.5 — the row IS the conversation: its identity and
            // every person-facing key (pin / mute / archive, folders, the
            // read position, the route) is the conversation id, which stays
            // what it was when the conversation continues in a successor
            // group. `thread.groupIdHex` is the group it currently is.
            let flags = flagsMap[thread.conversationIdHex]
            return ConversationListRow(
                id: thread.conversationIdHex,
                kind: kind == .group ? .group : .chat,
                title: viewModel.title(groupIdHex: thread.groupIdHex),
                // UX 9.4 — the row stays while a leave waits, and says so.
                preview: thread.leaveRequested
                    ? waitingToLeaveLine
                    : viewModel.preview(thread: thread, isGroup: kind == .group),
                lastActivityMs: viewModel.activityMs(thread: thread),
                // UX section 8.1 (group-and-channel-privacy) — each request
                // to join not yet shown on screen adds one.
                unread: JoinRequestSignals.rowUnread(
                    messages: UnreadCounting.displayed(
                        local: thread.unread,
                        cursorDerived: Self.unreadCount(
                            thread: thread,
                            cursor: readCursors[thread.conversationIdHex])),
                    unshown: thread.joinRequestsUnshown),
                pinned: flags?.pinned == true,
                muted: flags?.muted == true,
                archived: flags?.archived == true,
                // ADR-0045 UX §6.2.4 — a device whose own leaf was retired
                // keeps the chat, with one muted line in the preview's slot. A
                // person who is out has no row at all (ADR-0087 section 9).
                endedLine: chatListLine(
                    membershipState(
                        isActive: !thread.membershipEnded, leaveRequestedAtMs: nil)),
                markedUnread: flags?.unreadOverride == true,
                personKey: kind == .group ? nil : viewModel.peerSignatureKey(groupIdHex: thread.groupIdHex),
                photoGroupIdHex: kind == .group ? thread.groupIdHex : nil,
                hasUnseenReactions: viewModel.hasUnseenReactions(groupIdHex: thread.groupIdHex),
                previewMedia: thread.leaveRequested || thread.membershipEnded ? nil : viewModel.previewMedia(thread: thread))
        }
    }

    /// The line a channel's row shows for its newest post: its words, else
    /// `docs/ux/media-messages.md` section 11's words for its photos, videos
    /// or files, else the post's own line.
    static func channelPostLine(_ post: PersistedChannelPost) -> String {
        if post.content == nil, let caption = post.caption {
            let trimmed = caption.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty {
                return ChatListFormatting.textWithFileWords(
                    trimmed.replacingOccurrences(of: "\n", with: " "), refs: post.attachmentRefs)
            }
            if let words = ChatListFormatting.attachmentWords(
                ChatListFormatting.attachmentCounts(post.attachmentRefs)) {
                return words
            }
        }
        return post.text.trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "\n", with: " ")
    }

    /// A subscribed channel, in the shape the ordering takes.
    ///
    /// Its title and second line come from ``ChannelsFormatting/rowModel`` —
    /// the vocabulary the channel screens already use, so there is one of them
    /// rather than two. Nothing posted yet sorts by when the channel was
    /// added, the same way a chat with no messages sorts by when it was
    /// created; the alternative is a freshly subscribed channel invisible at
    /// the bottom of a long list until a fetch lands.
    static func channelRows(
        channels: ChannelsUiState, channelUnread: [String: Int],
        lastPosts: [String: PersistedChannelPost] = [:],
        flags flagsMap: [String: FlagsState] = [:],
        coPublished: (String) -> Bool = { CoPublishedJournals.shared.journalIdHex(channelIdHex: $0) != nil }
    ) -> [ConversationListRow] {
        let nowMs = ChatRepository.nowMs()
        return channels.channels.map { channel in
            let lastPost = lastPosts[channel.id]
            let flags = flagsMap[ChannelFlags.key(channelIdHex: channel.id)]
            // The newest REAL post's time, the post the second line shows
            // (the channel-post log holds text posts only). The raw rows the
            // server holds (`postsByChannel`) include ban lists and metadata
            // records: seen live 2026-11-12, a ban at 22:48 stood as the time
            // of a channel whose newest post was at 22:39.
            let lastPostMs = ChannelsFormatting.newestPostMs(
                [lastPost.map { UInt64(clamping: max(0, $0.serverTsMs)) }].compactMap { $0 })
            let model = ChannelsFormatting.rowModel(
                sharedName: channel.sharedName,
                name: channel.localName,
                handle: channel.handle,
                role: ChannelRowRole.of(
                    publishes: channels.keysByChannel[channel.id] != nil, coPublished: coPublished(channel.id)),
                subscriberCount: channels.subscriberCountByChannel[channel.id],
                newestPostMs: lastPostMs,
                nowMs: nowMs,
                calendar: .current,
                locale: Localization.currentLocale)
            return ConversationListRow(
                id: channel.id,
                kind: .channel,
                title: model.title,
                // ADR-0073 phase 2 (UX 16.6) — on the device that reset the
                // keys, a channel it owned says it can no longer post.
                // UX navigation section 2 — the second line is the last post;
                // the @name and role only while no post can be read here.
                preview: channels.orphanedByReset(channelId: channel.id)
                    ? Localization.text("channels.keys_reset.row_sub")
                    : (lastPost.map(Self.channelPostLine) ?? model.secondLine),
                lastActivityMs: lastPostMs ?? Int64(clamping: channel.addedAtMs),
                unread: channelUnread[channel.id] ?? 0,
                // UX group-and-channel-privacy 12 — pinned, muted and archived
                // like a group's row, through the same synced flags, keyed on
                // the channel id (``ChannelFlags``).
                pinned: flags?.pinned == true,
                muted: flags?.muted == true,
                archived: flags?.archived == true,
                // `docs/ux/media-messages.md` section 11 — the post's first
                // photo or video, as in a chat's row.
                previewMedia: channels.orphanedByReset(channelId: channel.id)
                    ? nil : lastPost.flatMap { ChatListFormatting.previewMedia($0.attachmentRefs) })
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
                    listPage(rows: folderRows(folder, from: all), folder: folder)
                        .tag(ChatFolders.hex(folder.folderId))
                }
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
        } else {
            listPage(rows: rows, folder: nil)
        }
    }

    /// The notices at the top of the list. Rows of the list, not an inset over
    /// it: an inset stays put while the large title and the search field move,
    /// so it hid the title and lay across the field on a pull. A `Group`, not a
    /// stack, so that with no notice the list holds no empty row.
    private var listNotices: some View {
        Group {
            if let accountNotice { accountNotice }
            if let recalledNotice { recalledNotice }
            // UX 6.2 — the requests to join this device sent, with a count.
            let requests = viewModel.requestsSent
            if !requests.isEmpty {
                Button { requestsSentOpen = true } label: {
                    HStack {
                        Label(l10n.t("requests_sent.title"), icon: .send)
                            .foregroundStyle(Color.lakkoText)
                        Spacer()
                        Text(requests.count.formatted(.number.locale(l10n.locale)))
                            .foregroundStyle(Color.lakkoTextMuted)
                        LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .sheet(isPresented: $requestsSentOpen) {
                    RequestsSentList(viewModel: viewModel)
                }
            }
        }
        .listRowInsets(EdgeInsets())
        .listRowSeparator(.hidden)
    }

    /// Section 7.2 — what the list says about a restored device. No spinner:
    /// the list stays what it is, a sentence says why.
    private var restoredNotice: RestoredListNotice {
        RestoredListNotice.decide(
            record: restored, listEmpty: allRows.isEmpty && invitedRows.isEmpty,
            rejoinAnswer: deviceGroupSync.rejoinAnswer)
    }

    private func reloadRestored() {
        restored = accountIdB64.flatMap { RestoredDeviceStore.load(accountIdB64: $0) }
        noteConversationIfAny()
    }

    /// The first conversation arrived: the notice is gone for good.
    private func noteConversationIfAny() {
        guard let accountIdB64, restored?.awaitingFirstConversation == true,
              !allRows.isEmpty || !invitedRows.isEmpty else { return }
        RestoredDeviceStore.noteConversationArrived(accountIdB64: accountIdB64)
        restored = RestoredDeviceStore.load(accountIdB64: accountIdB64)
    }

    private var restoredCard: some View {
        VStack(spacing: 16) {
            Text(l10n.t("chats.restored.card"))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
                .multilineTextAlignment(.center)
            Button(action: onOpenContacts) {
                Text(l10n.t("nav.contacts"))
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

    /// One page: All chats (`folder == nil`) or one folder's rows.
    @ViewBuilder
    private func listPage(rows visible: [ConversationListRow], folder: FolderView?) -> some View {
        let invited = folder == nil ? invitedRows : []
        if visible.isEmpty && invited.isEmpty {
            // Inside a scroll view so the pull gesture works on the empty list
            // too.
            ScrollView {
                listNotices
                if let folder {
                    folderEmptyState(folder)
                } else if query.isEmpty {
                    switch restoredNotice {
                    case .card:
                        restoredCard
                    case .none:
                        // Navigation 2.4 — no empty state before the
                        // conversations were read: an account with chats
                        // read "No chats" for a moment on a cold start.
                        if viewModel.loaded {
                            emptyState
                        }
                    }
                } else {
                    VStack(spacing: 12) {
                        if let peopleSearch {
                            PeopleSearchSection(
                                model: peopleSearch, includesChannels: true, showsLocalContacts: true,
                                onOpenPerson: onOpenPerson, onOpenChannel: openFoundChannel,
                                channelName: foundChannelName,
                                onOpenConversation: { foundCard = $0 },
                                isHeld: { idHex in allRows.contains { $0.id == idHex } },
                                onOpenHeld: { _ = openHeldConversation($0) })
                            .sheet(item: $foundCard) { found in foundCardScreen(found) }
                        } else {
                            Text(l10n.t("search.nothing_found"))
                                .foregroundStyle(Color.lakkoTextMuted)
                                .multilineTextAlignment(.center)
                        }
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.horizontal, 16)
                    .padding(.top, 80)
                }
            }
        } else {
            let quietCall = callHost.quietIncomingCall
            // Navigation 2.2 step 3 — archived conversations are never in the
            // chats list: one bar at the bottom opens them; while searching,
            // the archived matches follow under a divider.
            let section = ArchivedListSplit.split(
                visible,
                isArchived: { $0.archived },
                isPinned: { row in folder.map { ChatFolders.isPinned(row, in: $0) } ?? row.pinned },
                searching: !query.isEmpty,
                archiveOpen: false)
            List {
                listNotices
                ForEach(invited) { contact in
                    InvitedChatRow(contact: contact)
                        .listRowSeparator(.hidden)
                        .listRowInsets(EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
                }
                // Navigation 2.2.1 — held and moved, a pinned row lifts and
                // the others move aside (the list's own reorder, no edit
                // mode); an unpinned row never starts a drag. One run of rows,
                // so a pin dropped anywhere below the last pin lands last
                // (`PinOrder.dropTarget` clamps); the place is written through
                // the shared helper.
                let pinIds = PinnedRun.split(section.top, isPinned: { $0.pinned }).pinned.map(\.id)
                let moveHandler = pinMoveHandler(pins: pinIds, folder: folder)
                ForEach(section.top) { row in
                    conversationListRow(
                        row, folder: folder, quietCall: quietCall, pinIds: pinIds,
                        draggable: moveHandler != nil && row.pinned)
                        .moveDisabled(!row.pinned)
                }
                .onMove(perform: moveHandler)
                if section.divider {
                    // A bare Divider() as a List row lays out vertically and
                    // takes the 44 pt minimum row height; a 1 pt rule does not.
                    Rectangle()
                        .fill(Color.lakkoBorder)
                        .frame(maxWidth: .infinity)
                        .frame(height: 1)
                        .padding(.vertical, 6)
                        .accessibilityHidden(true)
                        .accessibilityIdentifier("archived-divider")
                        .listRowBackground(Color.clear)
                        .listRowSeparator(.hidden)
                        .listRowInsets(EdgeInsets(top: 0, leading: 16, bottom: 0, trailing: 16))
                    ForEach(section.archived) { row in
                        conversationListRow(row, folder: folder, quietCall: quietCall)
                    }
                }
                // ADR-0060 — the people this device knows, then what the
                // network found, in the same list with no heading (UX section
                // 6.1, "One list").
                if !query.isEmpty, let peopleSearch {
                    PeopleSearchSection(
                        model: peopleSearch, includesChannels: true, showsLocalContacts: true,
                        listShowsRows: true,
                        listed: Set(invited.map(\.signaturePubkey)),
                        chatPeers: Set(visible.filter { $0.kind == .chat }.compactMap {
                            viewModel.peerSignatureKey(groupIdHex: $0.id)
                        }),
                        listedConversations: Set(visible.map(\.id)),
                        onOpenPerson: onOpenPerson, onOpenChannel: openFoundChannel,
                        channelName: foundChannelName, onOpenConversation: { foundCard = $0 },
                        isHeld: { idHex in allRows.contains { $0.id == idHex } },
                        onOpenHeld: { _ = openHeldConversation($0) })
                        .sheet(item: $foundCard) { found in foundCardScreen(found) }
                        .listRowSeparator(.hidden)
                }
            }
            .listStyle(.plain)
            // The archive divider row is 13 pt tall; every other row is taller
            // than the default minimum on its own.
            .environment(\.defaultMinListRowHeight, 1)
            // Rebuilt on a layout-direction flip: a UIKit-backed List keeps the
            // mirroring it was built with (see SettingsScreen).
            .id(l10n.layoutDirection)
            // Fixed under the scrolling list, above the tab bar: never a row
            // of the list, so it stays put while the list scrolls.
            .safeAreaInset(edge: .bottom, spacing: 0) {
                if let count = section.barCount {
                    archivedBar(count: count, tag: folder.map { ChatFolders.hex($0.folderId) } ?? "")
                }
            }
        }
    }

    // MARK: - Archive (navigation 2.2 step 3)

    /// The archive while it is open: the folder it was opened in (`nil` for
    /// All chats) and its archived rows, filtered by the search typed in it.
    private struct OpenArchive {
        let folder: FolderView?
        let rows: [ConversationListRow]
    }

    /// The open archive, or `nil` when the chats list shows: never opened,
    /// gone back, its folder deleted, or emptied with nothing typed (the
    /// shared rule closes it by itself).
    private var openArchive: OpenArchive? {
        guard let tag = archiveTag else { return nil }
        let folder: FolderView?
        if tag.isEmpty {
            folder = nil
        } else {
            guard let found = deviceGroupSync.folders.first(where: { ChatFolders.hex($0.folderId) == tag })
            else { return nil }
            folder = found
        }
        let matches = rows
        let base = folder.map { folderRows($0, from: matches) } ?? matches
        let section = ArchivedListSplit.split(
            base,
            isArchived: { $0.archived },
            isPinned: { row in folder.map { ChatFolders.isPinned(row, in: $0) } ?? row.pinned },
            searching: !query.isEmpty,
            archiveOpen: true)
        guard section.pane == .archive else { return nil }
        return OpenArchive(folder: folder, rows: section.archived)
    }

    /// The chats list, with the archive drawn over it while open. The chats
    /// list stays alive underneath, so going back finds it where it was.
    @ViewBuilder
    private func panel(archive: OpenArchive?) -> some View {
        ZStack {
            content
                .opacity(archive == nil ? 1 : 0)
                .allowsHitTesting(archive == nil)
                .accessibilityHidden(archive != nil)
            // A container that always exists, so the gate below flips the
            // moment the archive closes: a leaving view keeps the modifiers
            // it had, and without this a tap during the exit transition
            // still opened an archived row (seen on the simulator).
            ZStack {
                if let archive {
                    archivePage(archive)
                        .background(Color.lakkoSurface)
                        .transition(.move(edge: .trailing))
                }
            }
            .allowsHitTesting(archive != nil)
        }
        .animation(.easeInOut(duration: 0.25), value: archive == nil)
    }

    /// The archived conversations in their order, the same rows and swipe
    /// actions as the chats list. A search typed here searches the archive
    /// alone.
    @ViewBuilder
    private func archivePage(_ archive: OpenArchive) -> some View {
        let quietCall = callHost.quietIncomingCall
        GeometryReader { geo in
            List {
                if archive.rows.isEmpty {
                    Text(l10n.t("search.nothing_found"))
                        .foregroundStyle(Color.lakkoTextMuted)
                        .frame(maxWidth: .infinity)
                        .multilineTextAlignment(.center)
                        .padding(.top, 80)
                        .listRowSeparator(.hidden)
                        .listRowBackground(Color.clear)
                }
                ForEach(archive.rows) { row in
                    conversationListRow(row, folder: archive.folder, quietCall: quietCall, inArchive: true)
                }
            }
            .listStyle(.plain)
            .id(l10n.layoutDirection)
            .accessibilityIdentifier("archive-list")
            // The platform's back gesture: a swipe from the leading edge.
            .simultaneousGesture(
                DragGesture(minimumDistance: 24, coordinateSpace: .local)
                    .onEnded { value in
                        let fromLeading = layoutDirection == .rightToLeft
                            ? value.startLocation.x > geo.size.width - 28
                            : value.startLocation.x < 28
                        let forward = layoutDirection == .rightToLeft
                            ? -value.translation.width : value.translation.width
                        if fromLeading && forward > 80 && abs(value.translation.height) < forward {
                            archiveTag = nil
                        }
                    })
        }
    }

    /// The archive's back control, in the navigation bar beside its title.
    private var archiveBackButton: some View {
        Button {
            archiveTag = nil
        } label: {
            LkIconView(.chevronBack, size: 22, relativeTo: .body)
                .foregroundStyle(Color.lakkoAccent)
                .frame(minWidth: 44, minHeight: 44)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(l10n.t("common.back"))
        .accessibilityIdentifier("archive-header")
    }

    /// One conversation row of the list, with its swipe and long-press menus.
    @ViewBuilder
    private func conversationListRow(
        _ row: ConversationListRow, folder: FolderView?, quietCall: QuietIncomingCall?,
        inArchive: Bool = false, pinIds: [String] = [], draggable: Bool = false
    ) -> some View {
                VStack(spacing: 8) {
                    Button { onOpen(row) } label: {
                        ConversationRowView(
                            row: row, timeLabel: timeLabel(row), playsVideoOnHover: true,
                            marksArchived: !inArchive)
                    }
                    .buttonStyle(.plain)
                    // Navigation 2.2.1 — the drag's equivalent for VoiceOver:
                    // Move up / Move down on a pinned row, each only where
                    // the shared rule says it applies. Not while searching.
                    .accessibilityActions {
                        if query.isEmpty && !inArchive {
                            if let up = PinOrder.stepTarget(shown: pinIds, id: row.id, up: true) {
                                Button(l10n.t("chats.action.move_up")) {
                                    movePin(id: row.id, toIndex: up, shown: pinIds, folder: folder)
                                }
                            }
                            if let down = PinOrder.stepTarget(shown: pinIds, id: row.id, up: false) {
                                Button(l10n.t("chats.action.move_down")) {
                                    movePin(id: row.id, toIndex: down, shown: pinIds, folder: folder)
                                }
                            }
                        }
                    }
                    // `docs/ux/calls.md` section 2.1.1 — a muted
                    // conversation's ringing call is answerable from its own
                    // row, with no navigation. Outside the row's own tap
                    // target on purpose: a button nested in another button's
                    // label does not reliably receive the tap, and opening
                    // the chat has to keep working while the offer stands.
                    if let quietCall, QuietIncomingCallPresentation.showsOnListRow(
                        // The offer names a GROUP; the row is a
                        // conversation (ADR-0073 R.5).
                        quietCall, rowId: viewModel.headGroupIdHex(ofRow: row.id)) {
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
                // folder items (`docs/ux/chat-folders.md` section 5). On a
                // row the list can drag (navigation 2.2.1) it opens when the
                // finger lifts without having moved, since the drag's lift
                // takes the ordinary long press.
                .modifier(ConversationRowMenu(draggable: draggable) { rowMenuItems(row: row, folder: folder) })

    }

    /// Navigation 2.2 step 3 — the "Archived" bar fixed to the bottom of the
    /// list: a tap opens the archive in place of the chats list.
    private func archivedBar(count: Int, tag: String) -> some View {
        Button {
            archiveTag = tag
        } label: {
            HStack(spacing: 12) {
                LkIconView(.archive, size: 20, relativeTo: .body)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(width: 28)
                Text(l10n.t("chats.archived.row"))
                    .font(.body)
                    .foregroundStyle(Color.lakkoText)
                Spacer()
                Text(count.formatted(.number.locale(l10n.locale)))
                    .foregroundStyle(Color.lakkoTextMuted)
                LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            .padding(.horizontal, 16)
            .frame(minHeight: 48)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(Color.lakkoSurface)
        .overlay(alignment: .top) {
            Rectangle()
                .fill(Color.lakkoBorder)
                .frame(height: 0.5)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(l10n.t("chats.archived.row_a11y", counts: ["count": UInt64(count)]))
        .accessibilityAddTraits(.isButton)
        .accessibilityIdentifier("archived-bar")
    }

    /// ADR-0079 section 4 — the pending contacts with no one-to-one yet,
    /// filtered by the search like any row.
    private var invitedRows: [PendingContact] {
        let peers = Set(contactsRepository.state.accepted.map(\.request.fromPubkey))
        let shown = PendingContacts.shown(contactsRepository.invited, oneToOnePeerKeys: peers)
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return shown }
        return shown.filter { contact in
            let title = PendingContacts.shownName(contact) ?? l10n.t("invite.contact.page.unnamed")
            return ChatListFormatting.matches(query: trimmed, title: title, preview: "")
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

    /// The row's long-press menu (Lakko's own, never the system context
    /// menu): the swipe's actions, then the folder items; "Add to folder" is
    /// a second level of the same sheet listing the folders.
    private func rowMenuItems(row: ConversationListRow, folder: FolderView?) -> [LakkoMenuItem] {
        var items: [LakkoMenuItem] = []
        if let folder {
            let pinned = ChatFolders.isPinned(row, in: folder)
            let atLimit = !pinned && folder.pinned.count >= ChatFolders.maxPins
            items.append(.action(
                l10n.t(pinned ? "chats.action.unpin" : "chats.action.pin"),
                icon: pinned ? .pinSlash : .pin, enabled: !atLimit
            ) { Task { await toggleFolderPin(folder: folder, row: row) } })
            if atLimit { items.append(.text(ChatFolders.pinLimitLine, secondary: true)) }
        } else {
            items.append(.action(
                l10n.t(row.pinned ? "chats.action.unpin" : "chats.action.pin"),
                icon: row.pinned ? .pinSlash : .pin
            ) { Task { await toggleFlag(groupIdHex: row.id, field: .pin) } })
        }
        // A channel's row offers what a group's does (UX
        // group-and-channel-privacy 12); its flags are keyed on the channel id.
        items.append(.action(
            l10n.t(row.muted ? "chats.action.unmute" : "chats.action.mute"),
            icon: row.muted ? .bell : .bellSlash
        ) { Task { await toggleFlag(groupIdHex: row.id, field: .mute) } })
        items.append(.action(
            l10n.t(row.archived ? "chats.action.unarchive" : "chats.action.archive"),
            icon: row.archived ? .unarchive : .archive
        ) { Task { await toggleFlag(groupIdHex: row.id, field: .archive) } })
        let folders = deviceGroupSync.folders
        let openEditor = {
            editor = FolderEditorRequest(folder: nil, prefill: ChatFolders.ref(for: row), rows: allRows)
        }
        if folders.isEmpty {
            items.append(.action(l10n.t("folders.add_to_folder"), icon: .folderPlus, perform: openEditor))
        } else {
            var sub: [LakkoMenuItem] = folders.map { f in
                let isIn = ChatFolders.matches(f, row)
                return .action(
                    f.meta.name, icon: isIn ? .checkmark : nil,
                    enabled: isIn || ChatFolders.handState(f, rowId: row.id) == .excluded
                        || !ChatFolders.handAddLimitReached(f)
                ) { Task { await toggleMembership(folder: f, row: row) } }
            }
            sub.append(.action(
                l10n.t("folders.new_ellipsis"), icon: .folderPlus,
                enabled: folders.count < ChatFolders.maxFolders, perform: openEditor))
            if folders.count >= ChatFolders.maxFolders { sub.append(.text(ChatFolders.folderLimitLine, secondary: true)) }
            items.append(.submenu(l10n.t("folders.add_to_folder"), icon: .folderPlus, items: sub))
        }
        if let folder {
            items.append(.action(l10n.t("folders.remove_from_folder"), icon: .folderMinus) {
                Task { await toggleMembership(folder: folder, row: row) }
            })
        }
        return items
    }

    /// The channel rows Mark all read reads: those with new posts.
    static func channelIdsToMarkRead(_ rows: [ConversationListRow]) -> [String] {
        rows.filter { $0.kind == .channel && $0.unread > 0 }.map(\.id)
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
            folder: folder, row: row, shown: shownPins(in: folder),
            hlc: deviceGroupSync.mintHlc()) else { return }
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
    /// unread" cleared. A channel with new posts is read through the newest
    /// post this device holds, as opening it does (the feed's read state).
    private func markAllRead(in folder: FolderView?) async {
        let all = allRows
        let inTab = all.filter { row in
            guard ChatFolders.hasUnread(row) else { return false }
            guard let folder else { return true }
            return ChatFolders.matches(folder, row)
        }
        feedReading.markChannelsRead(ChatListScreen.channelIdsToMarkRead(inTab))
        let targets = inTab.filter { $0.kind != .channel }
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        for row in targets {
            guard let groupId = Self.hexToBytes(row.id) else { continue }
            if let upTo = viewModel.markReadLocally(groupIdHex: row.id) {
                try? await deviceGroupSync.setReadCursor(
                    groupId: groupId, upToMessageId: upTo,
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

    // MARK: - A channel the search found

    /// A channel row under People: its subscribe page. The field is cleared,
    /// as it was for the lookup row this replaced.
    private func foundChannelName(_ channelIdHex: String) -> String? {
        channels.channels.first { $0.id == channelIdHex }.map(\.title)
    }

    private func foundCardScreen(_ found: ConversationHandles.Found) -> some View {
        ConversationCardScreen(
            found: found, repository: viewModel.repository,
            isHeld: { idHex in allRows.contains { $0.id == idHex } },
            openHeld: openHeldConversation,
            subscribe: { idHex, handle, name in
                searchText = ""
                guard found.kind == .channel else { onSubscribeFoundChannel(idHex, handle, name); return }
                // ADR-0087 section 6 — the record's read key opens the
                // public seed: the channel reads at once. The same Subscribe
                // as a Discover card's.
                FoundChannelReadKeys.subscribe(found.asPublicChannel, open: onSubscribeFoundChannel)
            },
            onClose: { foundCard = nil })
    }

    /// UX 5.1 — `Open` on the card of a group or channel this device holds.
    private func openHeldConversation(_ idHex: String) -> Bool {
        guard let row = allRows.first(where: { $0.id == idHex }) else { return false }
        searchText = ""
        onOpen(row)
        return true
    }

    private func openFoundChannel(_ channelIdHex: String, _ handle: String) {
        searchText = ""
        onFoundChannel(channelIdHex, handle)
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
    ///
    /// A row the pairing history brought is never unread here, with a cursor
    /// or without one (``PairingHistory``; the local count leaves those rows
    /// out as they arrive). Rows of older segments carry history ids below
    /// zero and are not counted at all.
    static func unreadCount(
        thread: ChatThread,
        cursor: ReadCursorState?
    ) -> Int? {
        guard let cursor else { return nil }
        // ADR-0073 R.5 — the position is `(segment, message id)`; what it
        // means for the rows of the conversation's CURRENT group is decided
        // by `readUpToInSegment`. Rows of older segments are behind the
        // position or were read where they were.
        let cursorId: Int64
        switch LineageRules.readUpToInSegment(
            LineageRules.decodeReadPosition(cursor.upToMessageId), segment: thread.segmentIndex) {
        case .all: return 0
        case .nothing: cursorId = 0
        case .upTo(let id): cursorId = id
        }
        return thread.messages.reduce(0) { acc, msg in
            // Only a message someone else wrote is unread: a system row (a
            // commit, a membership line, a call row) is not a message, and a
            // plain commit row is not even drawn. Counting them put "2" on a
            // group whose timeline showed nothing new (2026-11-22).
            if msg.direction != .incoming { return acc }
            if msg.messageId <= 0 { return acc }
            // `docs/ux/message-actions.md` section 9 — a deleted message is
            // never unread.
            if msg.deleted { return acc }
            if PairingHistory.isHistory(messageId: msg.messageId, floor: thread.syncedThroughMessageId) {
                return acc
            }
            // ADR-0063 section 5 — a message the feed showed is seen, even
            // when it sits above messages that are not.
            if thread.feedSeen.contains(msg.messageId) { return acc }
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
    /// A channel's row offers the same pin / mute / archive as a group's (UX
    /// group-and-channel-privacy 12), keyed on the channel id
    /// (``ChannelFlags``). Inside a folder, Pin acts on that folder's own pin
    /// list (`docs/ux/chat-folders.md` 2.3).
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
                Label(l10n.t(pinned ? "chats.action.unpin" : "chats.action.pin"), icon: pinned ? .pinSlash : .pin)
            }
            .tint(Color.lakkoAccent)
            .disabled(atLimit)
            if atLimit && inMenu {
                Text(ChatFolders.pinLimitLine)
            }
        } else {
            Button {
                Task { await toggleFlag(groupIdHex: row.id, field: .pin) }
            } label: {
                Label(l10n.t(row.pinned ? "chats.action.unpin" : "chats.action.pin"), icon: row.pinned ? .pinSlash : .pin)
            }
            .tint(Color.lakkoAccent)
        }
        Button {
            Task { await toggleFlag(groupIdHex: row.id, field: .mute) }
        } label: {
            Label(l10n.t(row.muted ? "chats.action.unmute" : "chats.action.mute"), icon: row.muted ? .bell : .bellSlash)
        }
        .tint(.gray)
        Button {
            Task { await toggleFlag(groupIdHex: row.id, field: .archive) }
        } label: {
            Label(
                l10n.t(row.archived ? "chats.action.unarchive" : "chats.action.archive"),
                icon: row.archived ? .unarchive : .archive)
        }
        .tint(Color.lakkoTextMuted)
    }

    private enum FlagField {
        case pin, mute, archive
    }

    @MainActor
    private func toggleFlag(groupIdHex: String, field: FlagField) async {
        // A chat's or group's conversation id (32 bytes), or a channel's id
        // (16 bytes, ``ChannelFlags``): the synced flags take either.
        guard let groupId = Self.hexToBytes(groupIdHex) else { return }
        let cur = deviceGroupSync.flags[groupIdHex.lowercased()]
        var pinned = cur?.pinned ?? false
        var muted = cur?.muted ?? false
        var archived = cur?.archived ?? false
        let unreadOverride = cur?.unreadOverride ?? false
        switch field {
        case .pin: pinned.toggle()
        case .mute: muted.toggle()
        case .archive: archived.toggle()
        }
        // Navigation 2.2.1 — a pin goes to the top of the pins, an unpin
        // leaves the others in their order; read before the flag changes the
        // list.
        let pinOrderAfter: [String]? = field == .pin
            ? PinOrder.next(
                pinned ? .pin : .unpin,
                stored: PinOrder.storedIds(deviceGroupSync.order),
                shown: PinOrder.shownPins(allRows),
                id: groupIdHex)
            : nil
        do {
            try await deviceGroupSync.setFlags(
                groupId: groupId,
                pinned: pinned,
                muted: muted,
                archived: archived,
                unreadOverride: unreadOverride
            )
            if let pinOrderAfter {
                try await deviceGroupSync.setOrder(groupIds: PinOrder.bytes(pinOrderAfter))
            }
        } catch {
            NSLog("setFlags failed: %@", error.localizedDescription)
        }
    }

    /// Navigation 2.2.1 — the folder's rows in its order, with a move that is
    /// still being written drawn where it was dropped.
    private func folderRows(_ folder: FolderView, from all: [ConversationListRow]) -> [ConversationListRow] {
        ChatFolders.rows(in: folder, from: all, pins: pendingPins[ChatFolders.hex(folder.folderId)])
    }

    /// The pins of a tab as the list draws them, top first.
    private func shownPins(in folder: FolderView?) -> [String] {
        let all = allRows
        return PinOrder.shownPins(folder.map { folderRows($0, from: all) } ?? all)
    }

    /// Navigation 2.2.1 — what a drop among the pins does, or `nil` where no
    /// drag may start (fewer than two pins, a search typed).
    private func pinMoveHandler(pins: [String], folder: FolderView?) -> ((IndexSet, Int) -> Void)? {
        guard PinOrder.dragEnabled(pinCount: pins.count, searching: !query.isEmpty, inArchive: false)
        else { return nil }
        return { source, destination in
            guard let drop = PinOrder.dropTarget(pins: pins, source: source, destination: destination)
            else { return }
            movePin(id: drop.id, toIndex: drop.toIndex, shown: pins, folder: folder)
        }
    }

    /// Navigation 2.2.1 — move a pin to `toIndex` among the shown pins: a
    /// drop, or Move up / Move down. All chats writes the account's order,
    /// a folder its own pin list. Drawn at once, before the write runs.
    private func movePin(id: String, toIndex: Int, shown: [String], folder: FolderView?) {
        if let folder {
            let tag = ChatFolders.hex(folder.folderId)
            guard let changes = ChatFolders.movePin(
                folder: folder, shown: shown, id: id, toIndex: toIndex,
                hlc: deviceGroupSync.mintHlc()),
                let written = changes.pins.first else { return }
            let ids = written.pinned.map(ChatFolders.rowId)
            pendingPins[tag] = ids
            Task {
                do {
                    try await deviceGroupSync.applyFolderChanges(changes)
                } catch {
                    NSLog("folder pin move failed: %@", error.localizedDescription)
                }
                if pendingPins[tag] == ids { pendingPins[tag] = nil }
            }
        } else {
            let stored = PinOrder.storedIds(deviceGroupSync.order)
            let ids = PinOrder.next(.move(toIndex: toIndex), stored: stored, shown: shown, id: id)
            guard ids != stored else { return }
            pendingPins[""] = ids
            Task {
                do {
                    try await deviceGroupSync.setOrder(groupIds: PinOrder.bytes(ids))
                } catch {
                    NSLog("pin move failed: %@", error.localizedDescription)
                }
                if pendingPins[""] == ids { pendingPins[""] = nil }
            }
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
            LkIconView(.bubbles, size: 48, relativeTo: nil)
                .foregroundStyle(Color.lakkoTextMuted)
            Text(l10n.t("chats.empty.title"))
                .font(.headline)
            if !accountSuspended {
                Button {
                    createMenuWithSaved = false
                    createMenuOpen = true
                } label: {
                    Text(l10n.t("chats.empty.action"))
                        .padding(.horizontal, 16)
                        .padding(.vertical, 10)
                        .background(Color.lakkoAccent)
                        .foregroundStyle(Color.lakkoAccentForeground)
                        .clipShape(Capsule())
                }
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
    /// ADR-0084, UX profile-screen section 7 — the Chats list plays a profile
    /// (or group) video while a pointer hovers the row; other lists drawing
    /// this row (Forward to, the folder picker) keep the still.
    var playsVideoOnHover: Bool = false
    /// Navigation 2.2 step 3 — an archived row is dimmed and carries the
    /// archive glyph where it stands among other rows (a chats-view search);
    /// inside the archive view everything is archived, so neither is drawn.
    var marksArchived: Bool = true

    /// A pointer (iPad trackpad or mouse) is over the row. `.onHover` never
    /// fires for a finger, so on a touch-only screen the still stays.
    @State private var hovering = false

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            InitialsAvatar(
                // The id is hashed for the tint only, never shown.
                name: row.title, identity: row.tintIdentity, size: 40,
                isGroup: row.kind == .group,
                photo: row.personKey.map(AvatarPhoto.person) ?? row.photoGroupIdHex.map(AvatarPhoto.group),
                playsVideo: playsVideoOnHover && hovering)
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
                    if row.endedLine == nil, let media = row.previewMedia {
                        PreviewMediaThumbnail(media: media)
                    }
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
        .onHover { hovering = $0 }
        .opacity(row.archived && marksArchived ? 0.6 : 1)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityText)
    }

    /// Muted and pinned glyphs, then the unread capsule. A channel carries
    /// none of them, because none of them is keyed on anything a channel has.
    @ViewBuilder
    private var trailingMarks: some View {
        HStack(spacing: 6) {
            if row.muted {
                LkIconView(.bellSlashFill, size: 14, relativeTo: .caption2)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if row.pinned {
                LkIconView(.pinFill, size: 14, relativeTo: .caption2)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if row.archived && marksArchived {
                LkIconView(.archiveFill, size: 14, relativeTo: .caption2)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if row.hasUnseenReactions {
                LkIconView(.heartFill, size: 14, relativeTo: .caption2,
                           label: Localization.text("chats.row.unseen_reactions"))
                    .foregroundStyle(Color.lakkoAccent)
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
    /// `docs/ux/navigation.md` section 2.4 — whether the chat layer has read
    /// this device's conversations (groups, names and stored messages are
    /// read together in the bootstrap). Until then the list draws nothing,
    /// not even its empty state.
    @Published public private(set) var loaded = false

    let repository: ChatRepository
    private var cancellables: Set<AnyCancellable> = []

    public init(repository: ChatRepository) {
        self.repository = repository
        self.threads = repository.state.threads
        self.loaded = repository.state.bootstrapped
        repository.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] state in
                self?.threads = state.threads
                self?.loaded = state.bootstrapped
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
        // ADR-0087 / reactions spec section 12 — a group photo, or unseen
        // reactions, changed.
        repository.$groupSettingsRevision
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        repository.$unseenReactionsRevision
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
    }

    /// UX 6.2 — the open requests to join this device sent.
    var requestsSent: [RequestsSentStore.Entry] {
        guard let accountIdB64 = AcceptedContactsStore.boundAccountIdB64 else { return [] }
        return RequestsSentStore.open(
            accountIdB64: accountIdB64, heldGroups: Set(threads.map(\.groupIdHex)))
    }

    /// UX 6.2 — `Ask again` on a request a day old or more.
    func askAgain(_ entry: RequestsSentStore.Entry) async {
        guard let accountIdB64 = AcceptedContactsStore.boundAccountIdB64 else { return }
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        // A request from search: sealed again to the key the record named.
        if let keyB64 = entry.knockKeyB64, let key = Data(base64Encoded: keyB64),
           let groupId = ChatRepository.hexToBytes(entry.groupIdHex) {
            let outcome = await repository.requestToJoinPublicGroup(
                groupId: groupId, knockKey: key, knockVersion: entry.knockVersion ?? 0)
            if outcome == .sent {
                RequestsSentStore.recordPublic(
                    groupId: groupId, name: entry.name ?? "", handle: entry.handle ?? "", knockKey: key,
                    knockVersion: entry.knockVersion ?? 0, accountIdB64: accountIdB64, nowMs: nowMs)
                objectWillChange.send()
            }
            return
        }
        guard let cap = RequestsSentStore.capability(entry) else { return }
        do {
            let result = try await repository.requestToJoinFromLink(
                capabilityWire: cap.wire, nowMs: UInt64(nowMs), blobId: cap.blobId)
            RequestsSentStore.record(
                groupId: result.groupId, name: entry.name, capabilityWire: cap.wire,
                blobId: cap.blobId, accountIdB64: accountIdB64, nowMs: nowMs)
            objectWillChange.send()
        } catch {
            NSLog("requests sent: ask again failed: %@", String(describing: error))
        }
    }

    /// Reactions-and-emoji spec section 12 — the small heart beside the
    /// unread badge.
    func hasUnseenReactions(groupIdHex: String) -> Bool {
        !repository.unseenReactionIds(groupIdHex: groupIdHex).isEmpty
    }

    /// ADR-0050 section 2.9 — whether groups the pairing bundle named are not
    /// yet all joined here. Drives the single "Synchronizing..." label.
    var synchronizing: Bool {
        StateBundle.pendingKnownGroups(joinedGroupIdHexes: repository.knownGroupIdHexes) > 0
    }

    /// The conversation's kind (ADR-0062, read from the group's own
    /// GroupContext). Routes a tap, splits Chats from Groups and picks the
    /// preview's sender prefix. A two-person group is a group here, on every
    /// member's device.
    /// The kind the creator fixed for a conversation this device holds
    /// (ADR-0062), as the Calls index names it; `nil` when it holds none. A
    /// call a history bundle filed carries no kind of its own.
    func callHistoryKind(groupIdHex: String) -> CallHistoryKind? {
        repository.groupKind(groupIdHex: groupIdHex).map { $0 == .oneToOne ? .chat : .group }
    }

    func kind(of groupIdHex: String) -> ConversationKind {
        repository.conversationKind(groupIdHex: groupIdHex)
    }

    /// The row title (ADR-0051 §5): the shared group name whenever the
    /// conversation has one (only a group can: a name received in a
    /// one-to-one is dropped, ADR-0062 §7) — else the other person's resolved
    /// name (ADR-0048) for a one-to-one, else the members' names. Never a hex id (UX spec section 6).
    /// ADR-0084 — the other person of a one-to-one, for the row's photo.
    func peerSignatureKey(groupIdHex: String) -> Data? {
        repository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex)
    }

    /// The avatar photo of a conversation, as the Chats row draws it (base
    /// walk 2026-11-04: Feed and Calls drew initials over a group photo).
    func avatarPhoto(groupIdHex: String, isGroup: Bool) -> AvatarPhoto? {
        AvatarPhoto.conversation(
            isGroup: isGroup, groupIdHex: groupIdHex,
            peerKey: isGroup ? nil : peerSignatureKey(groupIdHex: groupIdHex))
    }

    func title(groupIdHex: String) -> String {
        Self.title(repository: repository, groupIdHex: groupIdHex)
    }

    /// A conversation's title as its Chats row shows it; also the `{group}`
    /// of a request-to-join notification (`docs/ux/group-and-channel-privacy.md`
    /// section 8.1).
    static func title(repository: ChatRepository, groupIdHex: String) -> String {
        if let shared = repository.groupName(groupIdHex: groupIdHex) {
            return shared
        }
        if let peer = repository.oneToOnePeerSignatureKey(groupIdHex: groupIdHex) {
            return ProfileDirectory.shared.resolve(signatureKey: peer).name
        }
        // ADR-0087 / UX 4.3 — a solo group with no name is titled `@name`.
        if repository.isAloneInGroup(groupIdHex: groupIdHex),
           let handle = repository.groupHandle(groupIdHex: groupIdHex) {
            return "@" + handle.handle
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
        guard let last = DeletedRows.listPreviewRow(thread.messages, isPreviewRow: ChatTimeline.isListPreviewRow) else {
            return Localization.text("chats.row.no_messages")
        }
        let senderName = last.authorKeyB64.flatMap {
            ProfileDirectory.shared.resolve(signatureKeyB64: $0)?.name
        }
        // ADR-0087 sections 14 and 15.2 — a message that carries a channel
        // invitation card holds the `/g/` link as its text; the row reads like
        // the card instead. Cards are filed under the group the bubble reads
        // them from (the conversation's current group).
        var text = last.text
        if let channel = last.channelInvite(conversationIdHex: thread.groupIdHex) {
            text = ChatListFormatting.channelInvitePreview(channelName: channel.displayName, publisher: false)
        } else if let publisher = last.coPublisherInvite(conversationIdHex: thread.groupIdHex) {
            text = ChatListFormatting.channelInvitePreview(
                channelName: publisher.displayName, publisher: true)
        } else if let line = ContentPreview.line(last) {
            // ADR-0092 — `Poll: {question}`, `Location`, ...
            text = line
        } else if let link = BareGroupLink.link(of: text) {
            // A bare `/g/` link with no card (an older message): never its
            // secret (``BareGroupLink``; seen live 2026-11-13).
            text = BareGroupLink.rowLine(text: text, opening: BareGroupLinks.shared.opening(link)) ?? text
        }
        // `docs/ux/media-messages.md` section 11 — files only: the words, then
        // the text ("2 files · Weekend plans").
        if last.content == nil, CallTranscriptRow.parse(text) == nil {
            text = ChatListFormatting.textWithFileWords(text, refs: last.attachments)
        }
        return ChatListFormatting.preview(
            text: text,
            attachmentLabels: last.content != nil ? [] : ChatListFormatting.attachmentLabels(last.attachments),
            direction: last.direction,
            unsupported: last.unsupportedVersion != nil,
            isGroup: isGroup,
            senderName: senderName)
    }

    /// `docs/ux/media-messages.md` section 11 — the row's thumbnail of the
    /// last message's first photo or video.
    func previewMedia(thread: ChatThread) -> ConversationPreviewMedia? {
        guard let last = DeletedRows.listPreviewRow(thread.messages, isPreviewRow: ChatTimeline.isListPreviewRow),
              last.content == nil, last.unsupportedVersion == nil else { return nil }
        return ChatListFormatting.previewMedia(last.attachments)
    }

    /// The server time of the last drawn message, for the row's time and the
    /// list order; `nil` when there is none or it does not parse.
    func lastMessageMs(thread: ChatThread) -> Int64? {
        guard let last = DeletedRows.listPreviewRow(thread.messages, isPreviewRow: ChatTimeline.isListPreviewRow) else {
            return nil
        }
        return ChatRepository.parseDsTimestampMs(last.createdAt)
    }

    /// The time this row sorts by and shows (``GroupArrival``).
    func activityMs(thread: ChatThread) -> Int64? {
        GroupArrival.activityMs(
            lastMessageMs: lastMessageMs(thread: thread), arrivedAtMs: thread.arrivedAtMs)
    }

    /// Chat folders' Mark all as read: clear the local badge and return the
    /// newest server message id, for the read cursor the caller broadcasts.
    /// `nil` when the chat holds no acknowledged message.
    func markReadLocally(groupIdHex: String) -> Data? {
        // The id is the row's: the CONVERSATION (ADR-0073 R.5).
        guard let thread = threads.first(where: { $0.conversationIdHex == groupIdHex }) else {
            repository.clearUnread(groupIdHex: groupIdHex)
            return nil
        }
        for hex in thread.historyGroupIdHexes + [thread.groupIdHex] {
            repository.clearUnread(groupIdHex: hex)
        }
        guard let highest = thread.messages.map(\.messageId).filter({ $0 > 0 }).max() else {
            return nil
        }
        return LineageRules.encodeReadPosition(
            LineageRules.TimelinePosition(segment: thread.segmentIndex, messageId: highest))
    }

    /// The group a list row's conversation currently is (ADR-0073 R.5), for
    /// the few things on a row that name a group rather than a conversation.
    func headGroupIdHex(ofRow id: String) -> String {
        threads.first(where: { $0.conversationIdHex == id })?.groupIdHex ?? id
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

/// ADR-0079 section 4 — the chat with a person added through their invite
/// link before their app has answered: drawn like any row (no "pending"
/// badge), opening the pending chat.
struct InvitedChatRow: View {
    let contact: PendingContact
    /// ADR-0084, UX profile-screen section 7 — the photo's video plays while
    /// a pointer hovers the row; `.onHover` never fires for a finger.
    @State private var hovering = false

    var body: some View {
        let title = PendingContacts.shownName(contact) ?? Localization.text("invite.contact.page.unnamed")
        NavigationLink(value: TabRoute.pendingChat(accountIdHex: contact.accountIdHex)) {
            HStack(alignment: .center, spacing: 12) {
                InitialsAvatar(
                    name: title, identity: contact.accountIdHex, size: 40,
                    photo: .person(contact.signaturePubkey), playsVideo: hovering)
                VStack(alignment: .leading, spacing: 3) {
                    Text(title)
                        .font(.body)
                        .foregroundStyle(Color.lakkoText)
                        .lineLimit(1)
                    Text(Localization.text("chat.pending.waiting", texts: ["name": title]))
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .lineLimit(1)
                }
            }
            .padding(.vertical, 4)
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
        }
        .buttonStyle(.plain)
    }
}

/// UX group-and-channel-privacy 9.1 (2026-11-06) — "You are no longer in
/// {name}." as a short notice at the bottom of the screen, gone by itself
/// after 4 seconds; no close icon. It observes the center itself, so the
/// screen under it is not re-evaluated.
struct MembershipEndToast: View {
    @ObservedObject var center: MembershipEndNoticeCenter
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if let shown = center.shown {
            Text(shown.text)
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .background(Capsule().fill(Color.lakkoSurface).shadow(radius: 6, y: 2))
                .padding(.horizontal, 16)
                .padding(.bottom, 72)
                .id(shown.id)
                .transition(.opacity)
                .allowsHitTesting(false)
                .accessibilityAddTraits(.updatesFrequently)
                .onAppear { UIAccessibility.post(notification: .announcement, argument: shown.text) }
        }
    }
}

/// UX 6.2 — the requests to join this device sent: avatar, name, one line;
/// `Ask again` after a day with no answer. A request is never withdrawn, and
/// the person is never told one was refused.
struct RequestsSentList: View {
    @ObservedObject var viewModel: ChatListViewModel
    @ObservedObject private var l10n = Localization.shared
    @State private var asking: String?

    var body: some View {
        NavigationStack {
            List(viewModel.requestsSent) { entry in
                let now = Int64(Date().timeIntervalSince1970 * 1000)
                let late = RequestsSentStore.mayAskAgain(entry, nowMs: now)
                HStack(spacing: 12) {
                    InitialsAvatar(
                        name: entry.name ?? "", identity: entry.groupIdHex, size: 40, isGroup: true,
                        photo: .group(entry.groupIdHex))
                    VStack(alignment: .leading, spacing: 2) {
                        Text(entry.name ?? l10n.t("group_info.header.aria"))
                            .foregroundStyle(Color.lakkoText)
                        if let handle = entry.handle {
                            Text("@" + handle).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                        }
                        Text(l10n.t(late ? "requests_sent.no_answer" : "requests_sent.waiting"))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    Spacer()
                    if late {
                        Button(l10n.t("requests_sent.ask_again")) {
                            asking = entry.id
                            Task {
                                await viewModel.askAgain(entry)
                                asking = nil
                            }
                        }
                        .buttonStyle(.bordered)
                        .tint(Color.lakkoAccent)
                        .disabled(asking != nil)
                    }
                }
            }
            .navigationTitle(l10n.t("requests_sent.title"))
            .navigationBarTitleDisplayMode(.inline)
        }
        .presentationDetents([.medium, .large])
    }
}

/// The row's long-press menu: the ordinary one, or, on a row the list can
/// drag, the one that opens on a lift without movement
/// (``View/lakkoHoldReleaseMenu(_:)``).
struct ConversationRowMenu: ViewModifier {
    let draggable: Bool
    let items: () -> [LakkoMenuItem]

    func body(content: Content) -> some View {
        if draggable {
            content.lakkoHoldReleaseMenu(items)
        } else {
            content.lakkoLongPressMenu(items)
        }
    }
}
