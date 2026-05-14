// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// SwiftUI per-thread chat screen.
///
/// Mirror of:
///
///   * `apps/web/src/chat/ChatRoute.tsx` (1.17f4b)
///   * `apps/android/.../ui/chat/ChatScreen.kt` (1.15f4b)
///
/// Renders the messages list (outgoing right-aligned, incoming
/// left-aligned, system rows centred as a faded separator) + a
/// composer with a multi-line `TextField` and a Send button.
/// Long-poll loop runs while the screen is on-screen via SwiftUI's
/// `.task` modifier (the standard auto-cancel-on-disappear shape).
public struct ChatScreen: View {

    @ObservedObject public var viewModel: ChatViewModel
    public var onBack: () -> Void

    public init(
        viewModel: ChatViewModel,
        onBack: @escaping () -> Void
    ) {
        self.viewModel = viewModel
        self.onBack = onBack
    }

    public var body: some View {
        VStack(spacing: 0) {
            messagesList
            Divider()
            composer
        }
        .navigationTitle(headline)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button(action: onBack) {
                    Label("Back", systemImage: "chevron.backward")
                }
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                Button(action: viewModel.onRefreshClicked) {
                    if viewModel.refreshing {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: "arrow.clockwise")
                    }
                }
                .disabled(viewModel.refreshing)
            }
        }
        .task {
            // Continuous foreground polling. SwiftUI's `.task`
            // ties the lifecycle to the view: the loop starts on
            // appear + cancels automatically on disappear. Mirror
            // of the Compose `LaunchedEffect(Unit) { while
            // (isActive) }` pattern from Android 1.15e7.
            while !Task.isCancelled {
                await viewModel.pollOnceLongPoll()
                await Task.yield()
            }
        }
    }

    private var headline: String {
        // For 1.14f3 we surface the first 16 chars of the hex
        // group id as the screen title. 1.14f4 will populate
        // the per-thread peer username (from
        // `AcceptedContact.request.fromPubkey`) — needs a
        // reverse-lookup keyed by pubkey, which lands when the
        // discovery side gains the inverse-of-OPRF endpoint.
        let prefix = viewModel.groupIdHex.prefix(16)
        return "Chat • \(prefix)"
    }

    @ViewBuilder
    private var messagesList: some View {
        ScrollViewReader { scrollProxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    if viewModel.messages.isEmpty {
                        emptyHint
                    } else {
                        ForEach(viewModel.messages) { message in
                            MessageRow(message: message)
                                .id(message.messageId)
                        }
                    }
                }
                .padding(16)
            }
            .onChange(of: viewModel.messages.count) { _, _ in
                // Scroll to the latest message whenever the list
                // grows. Mirror of the Android `rememberLazyListState`
                // + `LaunchedEffect(messages.size)` shape.
                if let last = viewModel.messages.last {
                    withAnimation {
                        scrollProxy.scrollTo(last.messageId, anchor: .bottom)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var emptyHint: some View {
        VStack(alignment: .leading, spacing: 8) {
            if viewModel.threadKnown {
                Text("Say hi 👋")
                    .font(.body)
                    .foregroundStyle(.secondary)
            } else {
                Text("Bootstrap pending")
                    .font(.body)
                    .foregroundStyle(.secondary)
                Text(
                    "The session is still rebuilding MLS state. " +
                    "If you just signed in, give it a moment; if it " +
                    "persists, the snapshot may be missing on this " +
                    "device — sending a message will surface a " +
                    "diagnostic error."
                )
                .font(.footnote)
                .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private var composer: some View {
        VStack(spacing: 8) {
            if let err = viewModel.sendError {
                Text(err)
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            HStack(spacing: 8) {
                TextField("Message", text: $viewModel.draft, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(1...4)
                    .disabled(viewModel.sending)
                Button(action: viewModel.onSendClicked) {
                    if viewModel.sending {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: "paperplane.fill")
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(
                    viewModel.sending ||
                    viewModel.draft.trimmingCharacters(
                        in: .whitespacesAndNewlines
                    ).isEmpty
                )
            }
        }
        .padding(12)
        .background(Color(.systemBackground))
    }
}

// MARK: - Message row

private struct MessageRow: View {
    let message: ChatMessage

    var body: some View {
        switch message.direction {
        case .outgoing:
            HStack {
                Spacer(minLength: 48)
                bubble
                    .background(Color.accentColor.opacity(0.9))
                    .foregroundStyle(.white)
                    .clipShape(RoundedRectangle(cornerRadius: 16))
            }
        case .incoming:
            HStack {
                bubble
                    .background(Color(.secondarySystemBackground))
                    .clipShape(RoundedRectangle(cornerRadius: 16))
                Spacer(minLength: 48)
            }
        case .system:
            HStack {
                Spacer()
                Text(message.text)
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                Spacer()
            }
        }
    }

    @ViewBuilder
    private var bubble: some View {
        Text(message.text)
            .font(.body)
            .padding(.vertical, 8)
            .padding(.horizontal, 12)
            .textSelection(.enabled)
    }
}
