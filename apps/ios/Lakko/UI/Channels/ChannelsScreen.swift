// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import SwiftUI

/// Channels UI for the iOS client — Phase 4 G4-2 iOS
/// port of the web `apps/web/src/routes/Channels.tsx`.
///
/// Three SwiftUI screens behind one file (small enough
/// that splitting hurts more than it helps; matches the
/// web shape so a parallel review reads naturally):
///
/// - [`ChannelsListScreen`] — list of locally-tracked
///   channels.
/// - [`NewChannelScreen`] — create form.
/// - [`ChannelDetailScreen`] — server epoch + posts list
///   + real signed encrypted publish via UniFFI.
///
/// The screens are driven by a shared
/// `ChannelsUiState` instance owned by the entry-point
/// view (typically [`ChannelsRootScreen`]); each child
/// view receives it through `@ObservedObject` so a
/// publish in one view surfaces in the list of another
/// without manual rebroadcast.

// ============================================================================
// Theme — palette mirrors apps/web/src/routes/Channels.tsx
// ============================================================================

private enum Theme {
    static let background = Color(red: 0x0F / 255, green: 0x0F / 255, blue: 0x0F / 255)
    static let surface = Color(red: 0x1A / 255, green: 0x1A / 255, blue: 0x1A / 255)
    static let border = Color(red: 0x2A / 255, green: 0x2A / 255, blue: 0x2A / 255)
    static let textPrimary = Color(red: 0xFA / 255, green: 0xFA / 255, blue: 0xFA / 255)
    static let textSecondary = Color(red: 0xA1 / 255, green: 0xA1 / 255, blue: 0xA1 / 255)
    static let accent = Color(red: 0x3B / 255, green: 0x82 / 255, blue: 0xF6 / 255)
    static let destructive = Color(red: 0xDC / 255, green: 0x26 / 255, blue: 0x26 / 255)
}

// ============================================================================
// ChannelsRootScreen — entry point + NavigationStack host
// ============================================================================

public struct ChannelsRootScreen: View {
    @StateObject private var state = ChannelsUiState()
    private let client: ChannelsClient

    public init(client: ChannelsClient = ChannelsClient()) {
        self.client = client
    }

    public var body: some View {
        NavigationStack {
            ChannelsListScreen(state: state, client: client)
        }
        .preferredColorScheme(.dark)
    }
}

// ============================================================================
// ChannelsListScreen
// ============================================================================

struct ChannelsListScreen: View {
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient

    var body: some View {
        ZStack {
            Theme.background.ignoresSafeArea()
            if state.channels.isEmpty {
                emptyState
            } else {
                listContent
            }
        }
        .navigationTitle("Channels")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                NavigationLink {
                    NewChannelScreen(state: state, client: client)
                } label: {
                    Image(systemName: "plus")
                        .foregroundStyle(Theme.accent)
                }
            }
        }
    }

    private var emptyState: some View {
        VStack(spacing: 16) {
            Image(systemName: "antenna.radiowaves.left.and.right")
                .font(.system(size: 48))
                .foregroundStyle(Theme.textSecondary)
            Text("No channels yet.\nCreate one to start broadcasting.")
                .font(.body)
                .foregroundStyle(Theme.textSecondary)
                .multilineTextAlignment(.center)
        }
        .padding()
    }

    private var listContent: some View {
        List {
            ForEach(state.channels) { channel in
                NavigationLink {
                    ChannelDetailScreen(
                        channelId: channel.id,
                        state: state,
                        client: client
                    )
                } label: {
                    channelRow(channel)
                }
                .listRowBackground(Theme.surface)
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .background(Theme.background)
    }

    private func channelRow(_ channel: ChannelHandle) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(channel.localName.isEmpty ? "(unnamed)" : channel.localName)
                .font(.headline)
                .foregroundStyle(Theme.textPrimary)
            if !channel.localDescription.isEmpty {
                Text(channel.localDescription)
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                    .lineLimit(2)
            }
            Text("\(channel.id.prefix(8))…\(channel.id.suffix(8))")
                .font(.caption.monospaced())
                .foregroundStyle(Theme.textSecondary)
        }
        .padding(.vertical, 4)
    }
}

// ============================================================================
// NewChannelScreen
// ============================================================================

struct NewChannelScreen: View {
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    @Environment(\.dismiss) private var dismiss

    @State private var name: String = ""
    @State private var description: String = ""
    @State private var busy = false
    @State private var errorMessage: String?

    var body: some View {
        ZStack {
            Theme.background.ignoresSafeArea()
            Form {
                Section {
                    TextField("Name", text: $name)
                        .listRowBackground(Theme.surface)
                        .foregroundStyle(Theme.textPrimary)
                    TextField("Description (optional)", text: $description, axis: .vertical)
                        .lineLimit(2...4)
                        .listRowBackground(Theme.surface)
                        .foregroundStyle(Theme.textPrimary)
                } footer: {
                    Text("Name + description are local-only — never sent to the server.")
                        .foregroundStyle(Theme.textSecondary)
                }
                if let error = errorMessage {
                    Section {
                        Text(error)
                            .foregroundStyle(.white)
                            .listRowBackground(Theme.destructive)
                    }
                }
            }
            .scrollContentBackground(.hidden)
        }
        .navigationTitle("New channel")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(busy ? "Creating…" : "Create") {
                    Task { await create() }
                }
                .disabled(busy || name.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
    }

    private func create() async {
        if busy { return }
        busy = true
        defer { busy = false }
        errorMessage = nil
        let id = ChannelsComposer.freshChannelIdHex()
        do {
            _ = try await client.createChannel(channelIdHex: id)
            let keys = ChannelsComposer.freshPublisherKeys()
            state.addChannel(.init(
                id: id,
                localName: name.trimmingCharacters(in: .whitespaces),
                localDescription: description.trimmingCharacters(in: .whitespaces),
                addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000)
            ))
            state.setKeys(channelId: id, keys: keys)
            dismiss()
        } catch let err as ChannelsApiError {
            errorMessage = "Server rejected: \(err.code) — \(err.detail)"
        } catch {
            errorMessage = "Network error: \(error.localizedDescription)"
        }
    }
}

// ============================================================================
// ChannelDetailScreen
// ============================================================================

struct ChannelDetailScreen: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient

    @State private var composeText: String = ""
    @State private var publishBusy = false
    @State private var publishError: String?
    @State private var decodedByCursor: [UInt64: String?] = [:]
    @State private var loading = true

    var body: some View {
        ZStack {
            Theme.background.ignoresSafeArea()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    metaSection
                    if state.keysByChannel[channelId] != nil {
                        composerSection
                    } else {
                        subscriberPlaceholderSection
                    }
                    postsSection
                }
                .padding()
            }
        }
        .navigationTitle(localNameOrFallback)
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await refresh()
        }
    }

    private var localNameOrFallback: String {
        let channel = state.channels.first(where: { $0.id == channelId })
        let n = channel?.localName ?? ""
        return n.isEmpty ? "(channel)" : n
    }

    private var metaSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(channelId)
                .font(.caption.monospaced())
                .foregroundStyle(Theme.textSecondary)
            if let desc = state.channels.first(where: { $0.id == channelId })?
                .localDescription, !desc.isEmpty {
                Text(desc)
                    .foregroundStyle(Theme.textSecondary)
            }
            HStack {
                Text("Server epoch:")
                    .foregroundStyle(Theme.textSecondary)
                Text("\(state.epochByChannel[channelId] ?? 0)")
                    .font(.body.monospaced())
                    .foregroundStyle(Theme.textPrimary)
            }
        }
    }

    private var composerSection: some View {
        VStack(spacing: 10) {
            TextField("Write a post…", text: $composeText, axis: .vertical)
                .lineLimit(2...6)
                .padding(10)
                .background(Theme.surface)
                .foregroundStyle(Theme.textPrimary)
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .disabled(publishBusy)
            HStack {
                Spacer()
                Button(publishBusy ? "Publishing…" : "Publish") {
                    Task { await publish() }
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.accent)
                .disabled(
                    publishBusy
                        || composeText.trimmingCharacters(in: .whitespaces).isEmpty
                )
            }
            if let error = publishError {
                Text(error)
                    .font(.footnote)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.destructive)
                    .foregroundStyle(.white)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
            }
        }
    }

    private var subscriberPlaceholderSection: some View {
        Text(
            "No local publisher keys for this channel. Subscriber-side view + HPKE seed-envelope reception arrive in a follow-up sub-sprint."
        )
        .font(.footnote)
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface)
        .foregroundStyle(Theme.textSecondary)
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    private var postsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Posts")
                .font(.headline)
                .foregroundStyle(Theme.textPrimary)
            let posts = state.postsByChannel[channelId] ?? []
            if posts.isEmpty {
                Text(loading ? "Loading…" : "No posts yet. Publish the first one above.")
                    .foregroundStyle(Theme.textSecondary)
            } else {
                ForEach(posts, id: \.cursor) { post in
                    postRow(post)
                }
            }
        }
    }

    private func postRow(_ post: PostView) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("cursor \(post.cursor)")
                Spacer()
                Text(isoTimestamp(ms: post.serverTsMs))
            }
            .font(.caption.monospaced())
            .foregroundStyle(Theme.textSecondary)
            decodedTextView(for: post)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    @ViewBuilder
    private func decodedTextView(for post: PostView) -> some View {
        if let entry = decodedByCursor[post.cursor] {
            if let text = entry {
                Text(text)
                    .foregroundStyle(Theme.textPrimary)
            } else {
                Text(
                    "[unreadable — wrong seed, tampered ciphertext, or another publisher's signing key]"
                )
                .font(.footnote)
                .italic()
                .foregroundStyle(Theme.textSecondary)
            }
        } else {
            Text("Decoding…")
                .font(.footnote)
                .italic()
                .foregroundStyle(Theme.textSecondary)
        }
    }

    // MARK: - Actions

    private func refresh() async {
        loading = true
        defer { loading = false }
        do {
            async let postsTask = client.fetchPosts(channelIdHex: channelId, after: 0)
            async let epochTask = client.fetchEpoch(channelIdHex: channelId)
            let postsResult = try await postsTask
            let epoch = try await epochTask
            state.setPosts(channelId: channelId, posts: postsResult.posts)
            state.setEpoch(channelId: channelId, epoch: epoch)
            decodeAll(posts: postsResult.posts)
        } catch let err as ChannelsApiError {
            publishError = "Fetch failed: \(err.code) — \(err.detail)"
        } catch {
            publishError = "Network error: \(error.localizedDescription)"
        }
    }

    private func publish() async {
        if publishBusy { return }
        let text = composeText.trimmingCharacters(in: .whitespaces)
        if text.isEmpty { return }
        guard let keys = state.keysByChannel[channelId] else {
            publishError = "No publisher keys for this channel."
            return
        }
        publishBusy = true
        defer { publishBusy = false }
        publishError = nil
        do {
            let epoch = state.epochByChannel[channelId] ?? 0
            let counter = state.nextCounter(channelId: channelId)
            let wire = try ChannelsComposer.compose(
                keys: keys,
                channelIdHex: channelId,
                epochId: epoch,
                messageCounter: counter,
                plaintext: Data(text.utf8)
            )
            _ = try await client.publishPost(channelIdHex: channelId, payload: wire)
            composeText = ""
            await refresh()
        } catch let err as ChannelsApiError {
            publishError = "Server rejected: \(err.code) — \(err.detail)"
        } catch let err as ChannelsComposerError {
            publishError = "Compose error: \(err)"
        } catch {
            publishError = "Error: \(error.localizedDescription)"
        }
    }

    private func decodeAll(posts: [PostView]) {
        guard let keys = state.keysByChannel[channelId] else {
            decodedByCursor = [:]
            return
        }
        var next: [UInt64: String?] = [:]
        for post in posts {
            do {
                let plain = try ChannelsComposer.verifyAndDecrypt(
                    seed: keys.seed,
                    channelIdHex: channelId,
                    postWireB64Url: post.payloadB64
                )
                next[post.cursor] = String(data: plain, encoding: .utf8)
            } catch {
                next[post.cursor] = nil as String?
            }
        }
        decodedByCursor = next
    }

    private func isoTimestamp(ms: UInt64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(ms) / 1000.0)
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: date)
    }
}
