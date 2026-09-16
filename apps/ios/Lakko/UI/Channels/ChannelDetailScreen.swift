// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// The channel view (UX spec section 5.4): posts as cards, newest at the
/// bottom. A device holding publisher keys gets the publish composer and a
/// Manage button; everyone else gets the reader flow (``ChannelReaderScreen``)
/// with a Subscribe button until they subscribe.
struct ChannelDetailScreen: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    /// ADR-0028 OQ3 — the canonical AccountIdentity seed, threaded to the
    /// manage screen so the co-publisher panel can open the channel's
    /// PublisherGroup + sign.
    let accountIdentitySeed: Data
    /// The handle the channel was found by when opened from the search field
    /// (a channel not yet in the local list). Empty otherwise.
    var foundHandle: String = ""

    var body: some View {
        Group {
            if state.keysByChannel[channelId] != nil {
                ChannelOwnerView(
                    channelId: channelId,
                    state: state,
                    client: client,
                    accountIdentitySeed: accountIdentitySeed
                )
            } else {
                ChannelReaderScreen(
                    channelId: channelId,
                    state: state,
                    client: client,
                    foundHandle: foundHandle
                )
            }
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }

    private var title: String {
        let channel = state.channels.first(where: { $0.id == channelId })
        if let name = channel?.localName, !name.isEmpty { return name }
        if let handle = channel?.handle, !handle.isEmpty { return "@\(handle)" }
        if !foundHandle.isEmpty { return "@\(foundHandle)" }
        return "Channel"
    }
}

// MARK: - Owner view

/// The channel as its publisher sees it: the posts, the composer, Manage.
private struct ChannelOwnerView: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data

    @State private var composeText: String = ""
    @State private var publishBusy = false
    @State private var publishError: String?
    @State private var decodedByCursor: [UInt64: String?] = [:]
    @State private var loading = true
    @FocusState private var composerFocused: Bool

    private var posts: [PostView] {
        (state.postsByChannel[channelId] ?? []).sorted { $0.cursor < $1.cursor }
    }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    description
                    if posts.isEmpty {
                        Text(loading ? "Loading..." : "No posts yet. Your first post appears here.")
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoTextMuted)
                            .frame(maxWidth: .infinity)
                            .padding(.top, 40)
                    } else {
                        ForEach(posts, id: \.cursor) { post in
                            ChannelPostCard(content: content(for: post), serverTsMs: post.serverTsMs)
                                .id(post.cursor)
                        }
                    }
                }
                .padding(16)
            }
            .background(Color.lakkoBackground)
            .scrollDismissesKeyboard(.interactively)
            .refreshable { await refresh() }
            .onChange(of: posts.last?.cursor) { _, cursor in
                guard let cursor else { return }
                withAnimation(.easeOut(duration: 0.15)) {
                    proxy.scrollTo(cursor, anchor: .bottom)
                }
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) { composer }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                NavigationLink {
                    ChannelManageScreen(
                        channelId: channelId,
                        state: state,
                        client: client,
                        accountIdentitySeed: accountIdentitySeed
                    )
                } label: {
                    Text("Manage")
                }
            }
        }
        .task { await refresh() }
    }

    /// The description only. The channel id and the server-side key
    /// generation are bookkeeping, not something a reader acts on.
    @ViewBuilder
    private var description: some View {
        if let desc = state.channels.first(where: { $0.id == channelId })?.localDescription,
           !desc.isEmpty {
            Text(desc)
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.bottom, 6)
        }
    }

    private func content(for post: PostView) -> ChannelPostCard.Content {
        guard let entry = decodedByCursor[post.cursor] else { return .decoding }
        if let text = entry { return .text(text) }
        return .unavailable(UnavailablePostReason.notVerified)
    }

    /// The publish composer, in the chat composer's shape (design system
    /// section 3): a growing field on the sunken surface and a 40 pt accent
    /// send circle, on the elevated surface with a hairline top.
    private var composer: some View {
        VStack(spacing: 8) {
            if let publishError {
                HStack(spacing: 8) {
                    Text(publishError)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoDanger)
                    Spacer()
                    Button("Retry") { Task { await publish() } }
                        .font(.footnote)
                        .disabled(publishBusy)
                }
            }
            HStack(alignment: .bottom, spacing: 8) {
                TextField("Write a post", text: $composeText, axis: .vertical)
                    .lineLimit(1...5)
                    .focused($composerFocused)
                    .foregroundStyle(Color.lakkoText)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 9)
                    .background(
                        RoundedRectangle(cornerRadius: 20, style: .continuous)
                            .fill(Color.lakkoSurfaceSunken)
                    )
                    .disabled(publishBusy)
                Button {
                    Task { await publish() }
                } label: {
                    ZStack {
                        Circle().fill(canPublish ? Color.lakkoAccent : Color.lakkoSurfaceSunken)
                        if publishBusy {
                            ProgressView().controlSize(.small)
                                .tint(Color.lakkoAccentForeground)
                        } else {
                            Image(systemName: "paperplane.fill")
                                .foregroundStyle(
                                    canPublish ? Color.lakkoAccentForeground : Color.lakkoTextMuted)
                        }
                    }
                    .frame(width: 40, height: 40)
                }
                .buttonStyle(.plain)
                .disabled(!canPublish)
                .accessibilityLabel(publishBusy ? "Publishing" : "Publish")
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Color.lakkoSurface)
        .overlay(alignment: .top) {
            Rectangle().fill(Color.lakkoBorder).frame(height: 1)
        }
    }

    private var canPublish: Bool {
        !publishBusy && !composeText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    // MARK: Actions

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
        } catch {
            NSLog("Channels: refresh failed: %@", String(describing: error))
            publishError = "Could not load posts. Pull to try again."
        }
    }

    private func publish() async {
        if publishBusy { return }
        let text = composeText.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.isEmpty { return }
        guard let keys = state.keysByChannel[channelId] else {
            publishError = "This device cannot publish here."
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
        } catch {
            NSLog("Channels: publish failed: %@", String(describing: error))
            publishError = "Could not publish."
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
}

// MARK: - Post card

/// One post: the text (or "Not available" with its reason) and the time in
/// the corner. Elevated surface, radius 12, hairline border.
struct ChannelPostCard: View {
    enum Content: Equatable {
        case decoding
        case text(String)
        case unavailable(String)
    }

    let content: Content
    let serverTsMs: UInt64

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            switch content {
            case .decoding:
                Text("Loading...")
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            case .text(let text):
                Text(text)
                    .font(.body)
                    .foregroundStyle(Color.lakkoText)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            case .unavailable(let reason):
                UnavailablePostView(reason: reason)
            }
            HStack {
                Spacer()
                Text(timeLabel)
                    .font(.caption)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lakkoSurface)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous)
                .stroke(Color.lakkoBorder, lineWidth: 1)
        )
    }

    private var timeLabel: String {
        ChannelsFormatting.relativeLabel(
            ms: Int64(clamping: serverTsMs),
            nowMs: Int64(Date().timeIntervalSince1970 * 1000),
            calendar: .current,
            locale: .current
        )
    }
}
