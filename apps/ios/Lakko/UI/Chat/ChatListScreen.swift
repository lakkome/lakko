// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import SwiftUI

/// SwiftUI list of 1×1 chat threads.
///
/// Mirror of:
///
///   * `apps/web/src/chat/ChatListRoute.tsx` (1.17f4b)
///   * `apps/android/.../ui/chat/ChatListScreen.kt` (1.15f4b)
///
/// Shows every thread the ``ChatRepository`` knows about — one row
/// per group, sorted by hex group id (the chronological-by-last-
/// message sort is a 1.14g concern alongside read-receipts +
/// unread-badge work). Tapping a row routes to ``ChatScreen``.
public struct ChatListScreen: View {

    @ObservedObject public var viewModel: ChatListViewModel
    public var onBack: () -> Void
    public var onOpenChat: (String) -> Void

    public init(
        viewModel: ChatListViewModel,
        onBack: @escaping () -> Void,
        onOpenChat: @escaping (String) -> Void
    ) {
        self.viewModel = viewModel
        self.onBack = onBack
        self.onOpenChat = onOpenChat
    }

    public var body: some View {
        NavigationStack {
            content
                .navigationTitle("Chats")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .navigationBarLeading) {
                        Button(action: onBack) {
                            Label("Home", systemImage: "chevron.backward")
                        }
                    }
                }
        }
    }

    @ViewBuilder
    private var content: some View {
        if viewModel.threads.isEmpty {
            emptyState
        } else {
            List(viewModel.threads, id: \.groupIdHex) { thread in
                Button(action: { onOpenChat(thread.groupIdHex) }) {
                    ThreadRow(thread: thread)
                }
                .buttonStyle(.plain)
            }
            .listStyle(.plain)
        }
    }

    @ViewBuilder
    private var emptyState: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("No conversations yet")
                .font(.headline)
            Text(
                "Add a contact from the Contacts screen — once you accept " +
                "their request (or they accept yours), a 1×1 chat lands " +
                "here automatically."
            )
            .font(.body)
            .foregroundStyle(.secondary)
        }
        .padding(24)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ThreadRow: View {
    let thread: ChatThread

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            // First 16 chars of the hex id — placeholder for the
            // username surface that 1.14f4 lights up via
            // ContactsRepository.accepted's stored peer pubkey.
            Text(thread.groupIdHex.prefix(16) + "…")
                .font(.system(.body, design: .monospaced))
            Text(lastMessagePreview)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
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
    }
}
