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
/// no pin, mute or archive mark, because those are keyed on a conversation id
/// a channel does not carry. Its unread badge is its count of new posts
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
    /// Open the conversation this row names, whichever kind it is.
    public var onOpen: (ConversationListRow) -> Void
    /// One of the three create-menu items.
    public var onCreate: (CreateChoice) -> Void
    /// A channel the search found by its handle, which this device may not
    /// hold yet: its subscribe page.
    public var onFoundChannel: (_ channelIdHex: String, _ handle: String) -> Void
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
    /// The toolbar's create menu carries Saved; the empty state's does not.
    @State private var createMenuWithSaved = true

    public var body: some View {
#if DEBUG
        let _ = BodyEvaluationCounter.tick("ChatListScreen")
#endif
        content
            // Section 2.1 — the strip under the navigation title, above the
            // list. Hidden until a folder exists, and while searching, since a
            // search is over everything (section 2.4).
            .safeAreaInset(edge: .top, spacing: 0) {
              VStack(spacing: 0) {
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
            .navigationTitle(l10n.t("nav.chats"))
            .navigationBarTitleDisplayMode(.large)
            .searchable(text: $searchText, prompt: Text(l10n.t("chats.search.placeholder")))
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
                if let keysUpdatingLabel, !viewModel.synchronizing {
                    ToolbarItem(placement: .navigationBarLeading) { keysUpdatingLabel }
                }
                ToolbarItem(placement: .navigationBarTrailing) { createMenu }
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
            Image(systemName: "square.and.pencil")
        }
        .accessibilityLabel(l10n.t("chats.create.menu"))
    }

    /// The create menu's rows (Lakko's own action sheet, never a system menu).
    private func createMenuItems(includeSaved: Bool) -> [LakkoMenuItem] {
        var items: [LakkoMenuItem] = [
            .action(l10n.t("chats.create.chat"), systemImage: "bubble.left") { onCreate(.chat) },
            .action(l10n.t("chats.create.group"), systemImage: "person.3") { onCreate(.group) },
            .action(l10n.t("chats.create.channel"), systemImage: "megaphone") { onCreate(.channel) },
        ]
        if includeSaved {
            items.append(.action(l10n.t("saved.title"), systemImage: "bookmark") { onCreate(.saved) })
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
        ConversationList.filtered(allRows, query: searchText)
    }

    /// Every conversation, ordered, unfiltered: what All chats shows and what
    /// every folder is matched against.
    private var allRows: [ConversationListRow] {
        Self.allRows(
            viewModel: viewModel,
            channels: channels,
            flags: deviceGroupSync.flags,
            readCursors: deviceGroupSync.readCursors,
            channelUnread: feedReading.channelUnread)
    }

    /// The merged list, ordered by the shared rule. One definition for the
    /// Chats tab, the folder editor and Settings > Chat folders.
    static func allRows(
        viewModel: ChatListViewModel,
        channels: ChannelsUiState,
        flags: [String: FlagsState],
        readCursors: [String: ReadCursorState],
        channelUnread: [String: Int]
    ) -> [ConversationListRow] {
        let chatRows = Self.chatRows(viewModel: viewModel, flags: flags, readCursors: readCursors)
        return ConversationList.sorted(
            chatRows + Self.channelRows(channels: channels, channelUnread: channelUnread))
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
                lastActivityMs: viewModel.lastMessageMs(thread: thread),
                unread: UnreadCounting.displayed(
                    local: thread.unread,
                    cursorDerived: Self.unreadCount(
                        thread: thread,
                        cursor: readCursors[thread.conversationIdHex])),
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
                personKey: kind == .group ? nil : viewModel.peerSignatureKey(groupIdHex: thread.groupIdHex))
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
    static func channelRows(
        channels: ChannelsUiState, channelUnread: [String: Int]
    ) -> [ConversationListRow] {
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
                // ADR-0073 phase 2 (UX 16.6) — on the device that reset the
                // keys, a channel it owned says it can no longer post.
                preview: channels.orphanedByReset(channelId: channel.id)
                    ? Localization.text("channels.keys_reset.row_sub")
                    : model.secondLine,
                lastActivityMs: lastPostMs ?? Int64(clamping: channel.addedAtMs),
                unread: channelUnread[channel.id] ?? 0)
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

    /// The notices at the top of the list. Rows of the list, not an inset over
    /// it: an inset stays put while the large title and the search field move,
    /// so it hid the title and lay across the field on a pull. A `Group`, not a
    /// stack, so that with no notice the list holds no empty row.
    private var listNotices: some View {
        Group {
            // UX 9.1 — removed from a group, anywhere but in it.
            MembershipEndBanner(
                center: viewModel.endNotices, onClose: viewModel.closeEndNotice(groupIdHex:))
            if let accountNotice { accountNotice }
            if let recalledNotice { recalledNotice }
        }
        .listRowInsets(EdgeInsets())
        .listRowSeparator(.hidden)
    }

    /// Section 7.2 — what the list says about a restored device. No spinner:
    /// the list stays what it is, a sentence says why.
    private var restoredNotice: RestoredListNotice {
        RestoredListNotice.decide(
            record: restored, listEmpty: allRows.isEmpty && invitedRows.isEmpty,
            otherDeviceSignedIn: deviceGroupSync.otherDeviceSignedIn)
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

    private var restoredBanner: some View {
        Text(l10n.t("chats.restored.banner_other_devices"))
            .font(.footnote)
            .foregroundStyle(Color.lakkoTextMuted)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .padding(.horizontal, 16)
            .padding(.top, 12)
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
                    case .banner:
                        restoredBanner
                        emptyState
                    case .none:
                        emptyState
                    }
                } else {
                    VStack(spacing: 12) {
                        if let peopleSearch {
                            PeopleSearchSection(
                                model: peopleSearch, includesChannels: true, showsLocalContacts: true,
                                emptyText: l10n.t("chats.search.no_results"),
                                onOpenPerson: onOpenPerson, onOpenChannel: openFoundChannel,
                                channelName: foundChannelName)
                        } else {
                            Text(l10n.t("chats.search.no_results"))
                                .foregroundStyle(Color.lakkoTextMuted)
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
                listNotices
                ForEach(invited) { contact in
                    InvitedChatRow(contact: contact)
                        .listRowSeparator(.hidden)
                        .listRowInsets(EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
                }
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
                    // folder items (`docs/ux/chat-folders.md` section 5).
                    .lakkoLongPressMenu { rowMenuItems(row: row, folder: folder) }
                }
                // ADR-0060 — the people this device knows, then the network
                // results under People (UX section 6.1).
                if !query.isEmpty, let peopleSearch {
                    PeopleSearchSection(
                        model: peopleSearch, includesChannels: true, showsLocalContacts: true,
                        listed: Set(invited.map(\.signaturePubkey)),
                        chatPeers: Set(visible.filter { $0.kind == .chat }.compactMap {
                            viewModel.peerSignatureKey(groupIdHex: $0.id)
                        }),
                        onOpenPerson: onOpenPerson, onOpenChannel: openFoundChannel,
                                channelName: foundChannelName)
                        .listRowSeparator(.hidden)
                }
            }
            .listStyle(.plain)
            // Rebuilt on a layout-direction flip: a UIKit-backed List keeps the
            // mirroring it was built with (see SettingsScreen).
            .id(l10n.layoutDirection)
        }
    }

    /// ADR-0079 section 4 — the pending contacts with no one-to-one yet,
    /// filtered by the search like any row.
    private var invitedRows: [PendingContact] {
        let peers = Set(contactsRepository.state.accepted.map(\.request.fromPubkey))
        let shown = PendingContacts.shown(contactsRepository.invited, oneToOnePeerKeys: peers)
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return shown }
        return shown.filter { contact in
            let title = PendingContacts.name(contact) ?? l10n.t("invite.contact.page.unnamed")
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
                systemImage: pinned ? "pin.slash" : "pin", enabled: !atLimit
            ) { Task { await toggleFolderPin(folder: folder, row: row) } })
            if atLimit { items.append(.text(ChatFolders.pinLimitLine, secondary: true)) }
        } else if row.kind != .channel {
            items.append(.action(
                l10n.t(row.pinned ? "chats.action.unpin" : "chats.action.pin"),
                systemImage: row.pinned ? "pin.slash" : "pin"
            ) { Task { await toggleFlag(groupIdHex: row.id, field: .pin) } })
        }
        if row.kind != .channel {
            items.append(.action(
                l10n.t(row.muted ? "chats.action.unmute" : "chats.action.mute"),
                systemImage: row.muted ? "bell" : "bell.slash"
            ) { Task { await toggleFlag(groupIdHex: row.id, field: .mute) } })
            items.append(.action(
                l10n.t(row.archived ? "chats.action.unarchive" : "chats.action.archive"),
                systemImage: row.archived ? "tray.and.arrow.up" : "archivebox"
            ) { Task { await toggleFlag(groupIdHex: row.id, field: .archive) } })
        }
        let folders = deviceGroupSync.folders
        let openEditor = {
            editor = FolderEditorRequest(folder: nil, prefill: ChatFolders.ref(for: row), rows: allRows)
        }
        if folders.isEmpty {
            items.append(.action(l10n.t("folders.add_to_folder"), systemImage: "folder.badge.plus", perform: openEditor))
        } else {
            var sub: [LakkoMenuItem] = folders.map { f in
                let isIn = ChatFolders.matches(f, row)
                return .action(
                    f.meta.name, systemImage: isIn ? "checkmark" : nil,
                    enabled: isIn || ChatFolders.handState(f, rowId: row.id) == .excluded
                        || !ChatFolders.handAddLimitReached(f)
                ) { Task { await toggleMembership(folder: f, row: row) } }
            }
            sub.append(.action(
                l10n.t("folders.new_ellipsis"), systemImage: "folder.badge.plus",
                enabled: folders.count < ChatFolders.maxFolders, perform: openEditor))
            if folders.count >= ChatFolders.maxFolders { sub.append(.text(ChatFolders.folderLimitLine, secondary: true)) }
            items.append(.submenu(l10n.t("folders.add_to_folder"), systemImage: "folder.badge.plus", items: sub))
        }
        if let folder {
            items.append(.action(l10n.t("folders.remove_from_folder"), systemImage: "folder.badge.minus") {
                Task { await toggleMembership(folder: folder, row: row) }
            })
        }
        return items
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
            if msg.direction == .outgoing { return acc }
            if msg.messageId <= 0 { return acc }
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
                name: row.title, identity: row.id, size: 40,
                photo: row.personKey.map(AvatarPhoto.person))
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
        StateBundle.pendingKnownGroups(joinedGroupIdHexes: repository.knownGroupIdHexes) > 0
    }

    /// UX 9.1 — the notices the banner shows.
    var endNotices: MembershipEndNoticeCenter { repository.membershipEndNotices }

    /// UX 9.1 — the close icon. The name goes with it.
    func closeEndNotice(groupIdHex: String) {
        repository.closeEndNotice(groupIdHex: groupIdHex)
    }

    /// The conversation's kind (ADR-0062, read from the group's own
    /// GroupContext). Routes a tap, splits Chats from Groups and picks the
    /// preview's sender prefix. A two-person group is a group here, on every
    /// member's device.
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
            unsupported: last.unsupportedVersion != nil,
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

    var body: some View {
        let title = PendingContacts.name(contact) ?? Localization.text("invite.contact.page.unnamed")
        NavigationLink(value: TabRoute.pendingChat(accountIdHex: contact.accountIdHex)) {
            HStack(alignment: .center, spacing: 12) {
                InitialsAvatar(
                    name: title, identity: contact.accountIdHex, size: 40,
                    photo: .person(contact.signaturePubkey))
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
        }
        .buttonStyle(.plain)
    }
}

/// UX 9.1 — "You are no longer in {name}." with a close icon, at the top of
/// Chats. It observes the notices itself, so the list is not re-evaluated by
/// them. One at a time: closing one shows the next.
struct MembershipEndBanner: View {
    @ObservedObject var center: MembershipEndNoticeCenter
    let onClose: (String) -> Void
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if let notice = center.notices.first {
            HStack(alignment: .center, spacing: 12) {
                Text(l10n.t("chat.ended.notice", texts: ["name": notice.name]))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoText)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button {
                    onClose(notice.groupIdHex)
                } label: {
                    Image(systemName: "xmark")
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .frame(minWidth: 44, minHeight: 44)
                        .contentShape(Rectangle())
                }
                .accessibilityLabel(l10n.t("common.dismiss"))
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 4)
            .background(Color.lakkoSurface)
        }
    }
}
