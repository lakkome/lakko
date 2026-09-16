// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import SwiftUI

/// The conversation list, used twice (UX spec section 5.1): the Chats tab
/// shows the two-person conversations, the Groups tab everything else. Same
/// rows, same actions; only the title, the compose action and the row title
/// rule differ.
///
/// Mirror of:
///
///   * `apps/web/src/chat/ChatListRoute.tsx` (1.17f4b)
///   * `apps/android/.../ui/chat/ChatListScreen.kt` (1.15f4b)
///
/// Rendered as the root of its tab's `NavigationStack`; tapping a row asks
/// the host to push ``ChatScreen``.
public struct ChatListScreen: View {

    @ObservedObject public var viewModel: ChatListViewModel
    /// Which list this is.
    public var kind: ConversationKind
    public var onOpenChat: (String) -> Void
    /// New chat (Chats) or New group (Groups).
    public var onCompose: () -> Void

    /// Phase 2a3e2c-iOS — cross-device per-thread flags
    /// (pinned / muted / archived). The Combine subscription
    /// inside ``DeviceGroupSyncRepository`` keeps the row
    /// indicators + the sort order live as sibling devices
    /// publish updates.
    @EnvironmentObject private var deviceGroupSync: DeviceGroupSyncRepository

    public init(
        viewModel: ChatListViewModel,
        kind: ConversationKind,
        onOpenChat: @escaping (String) -> Void,
        onCompose: @escaping () -> Void
    ) {
        self.viewModel = viewModel
        self.kind = kind
        self.onOpenChat = onOpenChat
        self.onCompose = onCompose
    }

    /// UX spec section 5.1 — the search field filters by name and preview.
    @State private var searchText: String = ""

    private var title: String { kind == .chat ? "Chats" : "Groups" }

    public var body: some View {
#if DEBUG
        let _ = BodyEvaluationCounter.tick("ChatListScreen")
#endif
        content
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.large)
            .searchable(text: $searchText, prompt: "Search")
            // Receiving is automatic; the pull gesture runs the same poll
            // once more, on demand (UX spec section 2, principle 3).
            .refreshable { await viewModel.pollNow(kind: kind) }
            .toolbar {
                // ADR-0050 / docs/ux/second-device.md section 2: one label
                // while this device is still being admitted to the chats a
                // pairing named. Chats appear as they arrive; never a
                // counter, never a per-chat row.
                if viewModel.synchronizing {
                    ToolbarItem(placement: .navigationBarLeading) {
                        Text("Synchronizing...")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .accessibilityAddTraits(.updatesFrequently)
                    }
                }
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button(action: onCompose) {
                        Image(systemName: kind == .chat ? "square.and.pencil" : "person.badge.plus")
                    }
                    .accessibilityLabel(kind == .chat ? "New chat" : "New group")
                }
            }
    }

    /// One row's rendered data, computed once per body pass so the sort, the
    /// search filter and the row read the same strings.
    private struct RowData: Identifiable {
        let thread: ChatThread
        let title: String
        let preview: String
        let lastMessageMs: Int64?
        var id: String { thread.groupIdHex }
    }

    /// This list's rows with the active flags applied (UX spec section 5.1):
    ///   * pinned first
    ///   * archived last
    ///   * otherwise by the time of the last message, newest first
    /// and the search text applied to name and preview.
    private var sortedRows: [RowData] {
        let flagsMap = deviceGroupSync.flags
        let isGroup = kind == .group
        let rows = viewModel.threads(of: kind).map { thread in
            RowData(
                thread: thread,
                title: viewModel.title(groupIdHex: thread.groupIdHex),
                preview: viewModel.preview(thread: thread, isGroup: isGroup),
                lastMessageMs: viewModel.lastMessageMs(thread: thread))
        }
        .filter { ChatListFormatting.matches(query: searchText, title: $0.title, preview: $0.preview) }
        return rows.sorted { lhs, rhs in
            let lf = flagsMap[lhs.thread.groupIdHex]
            let rf = flagsMap[rhs.thread.groupIdHex]
            let lpinned = lf?.pinned == true
            let rpinned = rf?.pinned == true
            if lpinned != rpinned { return lpinned }
            let larchived = lf?.archived == true
            let rarchived = rf?.archived == true
            if larchived != rarchived { return !larchived }
            switch (lhs.lastMessageMs, rhs.lastMessageMs) {
            case let (l?, r?) where l != r: return l > r
            case (_?, nil): return true
            case (nil, _?): return false
            default: return lhs.thread.groupIdHex < rhs.thread.groupIdHex
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        let rows = sortedRows
        if rows.isEmpty {
            // Inside a scroll view so the pull gesture works on the empty list too.
            ScrollView {
                if searchText.trimmingCharacters(in: .whitespaces).isEmpty {
                    emptyState
                } else {
                    Text("No results")
                        .foregroundStyle(Color.lakkoTextMuted)
                        .frame(maxWidth: .infinity)
                        .padding(.top, 80)
                }
            }
        } else {
            List(rows) { row in
                let thread = row.thread
                Button(action: { onOpenChat(thread.groupIdHex) }) {
                    ThreadRow(
                        thread: thread,
                        title: row.title,
                        preview: row.preview,
                        timeLabel: row.lastMessageMs.map {
                            ChatListFormatting.lastMessageTime(
                                ms: $0, nowMs: ChatRepository.nowMs(),
                                calendar: .current, locale: .current)
                        },
                        isGroup: kind == .group,
                        flags: deviceGroupSync.flags[thread.groupIdHex],
                        unread: UnreadCounting.displayed(
                            local: thread.unread,
                            cursorDerived: Self.unreadCount(
                                thread: thread,
                                cursor: deviceGroupSync.readCursors[thread.groupIdHex]
                            )
                        )
                    )
                }
                .buttonStyle(.plain)
                .listRowSeparator(.hidden)
                .listRowInsets(EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
                .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                    swipeActions(thread: thread)
                }
            }
            .listStyle(.plain)
        }
    }

    /// Compute unread count for a thread (Phase 2a3e2c-followup).
    ///
    /// Counts every incoming / system row whose `messageId` is
    /// strictly greater than the last broadcast read cursor.
    /// Outgoing rows never count — the sender already "read"
    /// them by composing.
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

    @ViewBuilder
    private func swipeActions(thread: ChatThread) -> some View {
        let cur = deviceGroupSync.flags[thread.groupIdHex]
        let pinned = cur?.pinned == true
        let muted = cur?.muted == true
        let archived = cur?.archived == true
        Button {
            Task { await toggleFlag(thread: thread, field: .pin) }
        } label: {
            Label(
                pinned ? "Unpin" : "Pin",
                systemImage: pinned ? "pin.slash" : "pin"
            )
        }
        .tint(Color.lakkoAccent)
        Button {
            Task { await toggleFlag(thread: thread, field: .mute) }
        } label: {
            Label(
                muted ? "Unmute" : "Mute",
                systemImage: muted ? "bell" : "bell.slash"
            )
        }
        .tint(.gray)
        Button {
            Task { await toggleFlag(thread: thread, field: .archive) }
        } label: {
            Label(
                archived ? "Unarchive" : "Archive",
                systemImage: archived ? "tray.and.arrow.up" : "archivebox"
            )
        }
        .tint(Color.lakkoTextMuted)
    }

    private enum FlagField {
        case pin, mute, archive
    }

    @MainActor
    private func toggleFlag(thread: ChatThread, field: FlagField) async {
        guard let groupId = Self.hexToBytes(thread.groupIdHex) else { return }
        let cur = deviceGroupSync.flags[thread.groupIdHex]
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

    /// UX spec section 5.1: one sentence and one action.
    @ViewBuilder
    private var emptyState: some View {
        VStack(spacing: 12) {
            Image(systemName: kind == .chat ? "bubble.left.and.bubble.right" : "person.3")
                .font(.system(size: 40))
                .foregroundStyle(Color.lakkoTextMuted)
            Text(kind == .chat ? "No chats yet" : "No groups yet")
                .font(.headline)
            Button(kind == .chat ? "Start a chat" : "Create a group", action: onCompose)
                .buttonStyle(.borderedProminent)
        }
        .padding(24)
        .frame(maxWidth: .infinity)
        .padding(.top, 80)
    }
}

private struct ThreadRow: View {
    let thread: ChatThread
    /// The row title: the other person's name in a chat, the group's
    /// member-derived title in a group. Never a hex id.
    let title: String
    /// The second line (``ChatListFormatting/preview``), already prefixed.
    let preview: String
    /// The last message's time, `nil` when the thread has no timed message.
    let timeLabel: String?
    let isGroup: Bool
    /// Phase 2a3e2c-iOS — cross-device flags snapshot for this
    /// thread. `nil` until a `Flags` payload lands.
    let flags: FlagsState?
    /// Phase 2a3e2c-followup — unread count derived from the
    /// per-thread read cursor + the incoming/system row IDs.
    let unread: Int

    /// The row goes quiet once this device's membership has ended (ADR-0045
    /// UX §6.2.4 / §6.2.11): no badge, and the ended line in the preview slot.
    private var showsUnread: Bool { unread > 0 && endedLine == nil }

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            InitialsAvatar(
                name: title,
                // The group id is hashed for the tint only, never shown.
                identity: thread.groupIdHex,
                size: 40,
                isGroup: isGroup
            )
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(title)
                        .font(.body)
                        .fontWeight(showsUnread ? .semibold : .regular)
                        .foregroundStyle(Color.lakkoText)
                        .lineLimit(1)
                    Spacer(minLength: 8)
                    if let timeLabel {
                        Text(timeLabel)
                            .font(.caption)
                            .foregroundStyle(showsUnread ? Color.lakkoAccent : Color.lakkoTextMuted)
                            .lineLimit(1)
                            .fixedSize()
                    }
                }
                HStack(alignment: .center, spacing: 6) {
                    // ADR-0045 UX §6.2.4 — the chat STAYS in the list and gains
                    // one muted line, in the slot the last-message preview
                    // already occupies. Removing it would delete a transcript
                    // the user still has as a side effect of something somebody
                    // else did; the flags are deliberately untouched too, since
                    // auto-archiving would be the client reorganising the
                    // user's list on their behalf.
                    Text(endedLine ?? preview)
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
        .opacity(flags?.archived == true ? 0.6 : 1)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityText)
    }

    /// Muted and pinned glyphs, then the unread capsule (UX spec section 5.1).
    @ViewBuilder
    private var trailingMarks: some View {
        HStack(spacing: 6) {
            if flags?.muted == true {
                Image(systemName: "bell.slash.fill")
                    .font(.caption2)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if flags?.pinned == true {
                Image(systemName: "pin.fill")
                    .font(.caption2)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if flags?.archived == true {
                Image(systemName: "archivebox.fill")
                    .font(.caption2)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if showsUnread {
                // Accent capsule; a muted thread renders it grey so a
                // muted-but-unread row still surfaces a hint without
                // grabbing attention.
                Text(unread > 99 ? "99+" : "\(unread)")
                    .font(.caption2)
                    .fontWeight(.semibold)
                    .padding(.horizontal, 7)
                    .padding(.vertical, 2)
                    .background(flags?.muted == true ? Color.lakkoTextMuted : Color.lakkoAccent)
                    .foregroundStyle(Color.lakkoAccentForeground)
                    .clipShape(Capsule())
            }
        }
    }

    private var accessibilityText: String {
        var parts = [title]
        if let timeLabel { parts.append(timeLabel) }
        parts.append(endedLine ?? preview)
        if showsUnread { parts.append(unread == 1 ? "1 unread" : "\(unread) unread") }
        if flags?.muted == true { parts.append("muted") }
        if flags?.pinned == true { parts.append("pinned") }
        return parts.joined(separator: ", ")
    }

    /// ADR-0045 UX §6.2.4 — the ended line, or `nil` while the reader is still
    /// a member. The `left` / `removed` distinction deliberately does NOT
    /// reach this line (§6.2.4 gives it one string): the list is a glance
    /// surface and the distinction belongs in the transcript's terminal row.
    private var endedLine: String? {
        chatListLine(
            membershipState(isActive: !thread.membershipEnded, leaveRequestedAtMs: nil))
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
    }

    /// ADR-0050 section 2.9 — whether groups the pairing bundle named are not
    /// yet all joined here. Drives the single "Synchronizing..." label.
    var synchronizing: Bool {
        StateBundle.pendingKnownGroups(joinedGroupIdHexes: threads.map { $0.groupIdHex }) > 0
    }

    /// The threads of one list (UX spec section 3): two people make a chat,
    /// more make a group, counted by account rather than device leaf
    /// (``ChatRepository/memberCount(groupIdHex:)``).
    func threads(of kind: ConversationKind) -> [ChatThread] {
        threads.filter { self.kind(of: $0.groupIdHex) == kind }
    }

    /// The persisted kind wins; the people count is the first-sight fallback
    /// and the one observation that can promote a chat to a group
    /// (``ConversationKindDecision``). Whatever the decision says to persist
    /// is written here, so a conversation first seen through a Welcome or a
    /// restore is decided once and then stays put.
    func kind(of groupIdHex: String) -> ConversationKind {
        let account = repository.accountIdB64
        let stored = account.flatMap {
            repository.conversationKindStore.load(accountIdB64: $0, groupIdHex: groupIdHex)
        }
        let verdict = ConversationKindDecision.resolve(
            stored: stored, peopleCount: repository.memberCount(groupIdHex: groupIdHex))
        if let persist = verdict.persist, let account {
            repository.conversationKindStore.record(
                accountIdB64: account, groupIdHex: groupIdHex, kind: persist)
        }
        return verdict.kind
    }

    /// The row title: the other person's resolved name (ADR-0048) for a chat,
    /// the members' names for a group. Never a hex id (UX spec section 6).
    func title(groupIdHex: String) -> String {
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
            return "No messages yet"
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

    /// The pull-to-refresh action: the same poll the app already runs, once,
    /// for every thread of this list. Best-effort; a failing group is skipped.
    func pollNow(kind: ConversationKind) async {
        for thread in threads(of: kind) {
            try? await repository.pollGroup(groupIdHex: thread.groupIdHex)
        }
    }
}
