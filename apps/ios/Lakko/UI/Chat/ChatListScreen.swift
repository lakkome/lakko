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

    private var title: String { kind == .chat ? "Chats" : "Groups" }

    public var body: some View {
        content
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.large)
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

    /// This list's threads with the active flags applied:
    ///   * pinned → top of the list
    ///   * archived → bottom
    ///   * otherwise stable on the underlying repository order
    private var sortedThreads: [ChatThread] {
        let flagsMap = deviceGroupSync.flags
        return viewModel.threads(of: kind).sorted { lhs, rhs in
            let lf = flagsMap[lhs.groupIdHex]
            let rf = flagsMap[rhs.groupIdHex]
            let lpinned = lf?.pinned == true
            let rpinned = rf?.pinned == true
            if lpinned != rpinned { return lpinned }
            let larchived = lf?.archived == true
            let rarchived = rf?.archived == true
            if larchived != rarchived { return !larchived }
            return false
        }
    }

    @ViewBuilder
    private var content: some View {
        let threads = sortedThreads
        if threads.isEmpty {
            // Inside a scroll view so the pull gesture works on the empty list too.
            ScrollView { emptyState }
        } else {
            List(threads, id: \.groupIdHex) { thread in
                Button(action: { onOpenChat(thread.groupIdHex) }) {
                    ThreadRow(
                        thread: thread,
                        title: viewModel.title(groupIdHex: thread.groupIdHex),
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
        .tint(.orange)
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
        .tint(.indigo)
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
    let isGroup: Bool
    /// Phase 2a3e2c-iOS — cross-device flags snapshot for this
    /// thread. `nil` until a `Flags` payload lands.
    let flags: FlagsState?
    /// Phase 2a3e2c-followup — unread count derived from the
    /// per-thread read cursor + the incoming/system row IDs.
    let unread: Int

    var body: some View {
        HStack(spacing: 12) {
            InitialsAvatar(
                name: title,
                // The group id is hashed for the tint only, never shown.
                identity: thread.groupIdHex,
                size: 40,
                isGroup: isGroup
            )
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 4) {
                    if flags?.pinned == true {
                        Image(systemName: "pin.fill")
                            .font(.caption)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    Text(title)
                        .font(.body)
                        .lineLimit(1)
                    if flags?.muted == true {
                        Image(systemName: "bell.slash.fill")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    if flags?.archived == true {
                        Image(systemName: "archivebox.fill")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                // ADR-0045 UX §6.2.4 — the chat STAYS in the list and gains one
                // muted line, in the slot the last-message preview already
                // occupies (§5.2 uses the same slot for pending annotations).
                //
                // It stays because removing it would delete a transcript the
                // user still has, with no warning, as a side effect of
                // something somebody else did — the single worst thing in this
                // design's reach. The row keeps its position, its label and its
                // flags: `pinned` / `muted` / `archived` are deliberately
                // untouched, because auto-archiving would be the client
                // silently reorganising the user's list on their behalf, and
                // archive is a decision, not a consequence.
                Text(endedLine ?? lastMessagePreview)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer()
            // ADR-0045 UX §6.2.4 / §6.2.11 — the row goes QUIET. An unread
            // badge asserts a conversation that can still move, and this one
            // cannot: nothing new will ever decrypt, so whatever count is
            // standing is frozen at the moment of the eviction and would sit
            // there for good.
            if unread > 0, endedLine == nil {
                // Pill badge. Muted threads render a hollow
                // ring instead of the filled accent fill so a
                // muted-but-unread row still surfaces a hint
                // without grabbing attention.
                Text(unread > 99 ? "99+" : "\(unread)")
                    .font(.caption2)
                    .fontWeight(.semibold)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(
                        flags?.muted == true
                            ? Color.secondary.opacity(0.2)
                            : Color.lakkoAccent
                    )
                    .foregroundStyle(
                        flags?.muted == true ? Color.lakkoTextMuted : Color.lakkoAccentForeground
                    )
                    .clipShape(Capsule())
            }
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
        .opacity(flags?.archived == true ? 0.6 : 1)
    }

    /// ADR-0045 UX §6.2.4 — the ended line, or `nil` while the reader is still
    /// a member.
    ///
    /// The `left` / `removed` distinction deliberately does NOT reach this line
    /// (§6.2.4 gives it one string): the list is a glance surface and the
    /// distinction belongs in the transcript's terminal row, where the durable
    /// record is read. Passing `nil` for the record here is therefore not a
    /// shortcut — ``chatListLine(_:)`` returns the same string either way.
    private var endedLine: String? {
        chatListLine(
            membershipState(isActive: !thread.membershipEnded, leaveRequestedAtMs: nil))
    }

    private var lastMessagePreview: String {
        guard let last = thread.messages.last else {
            return "No messages yet"
        }
        switch last.direction {
        case .outgoing: return "You: \(last.text)"
        case .incoming: return last.text
        case .system: return last.text
        }
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

    func kind(of groupIdHex: String) -> ConversationKind {
        ConversationKind.of(peopleCount: repository.memberCount(groupIdHex: groupIdHex))
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
                return resolved.source == .fingerprint ? nil : resolved.name
            }
    }

    /// The pull-to-refresh action: the same poll the app already runs, once,
    /// for every thread of this list. Best-effort; a failing group is skipped.
    func pollNow(kind: ConversationKind) async {
        for thread in threads(of: kind) {
            try? await repository.pollGroup(groupIdHex: thread.groupIdHex)
        }
    }
}
