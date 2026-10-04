// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// Channels, as screens of the Chats stack.
///
/// `docs/ux/navigation.md` section 1 — Channels is no longer a destination: a
/// channel is a row of the one conversation list, and these screens are pushed
/// from it. One screen per file under `UI/Channels/`:
///
/// - ``ChannelDetailScreen`` (this file) — the channel view: posts as cards,
///   the publish composer for owners, the reader flow for everyone else.
/// - ``NewChannelScreen`` — the create form, reached from the create menu.
/// - ``ChannelReaderScreen`` — subscribe, wait for access, read.
/// - ``ChannelManageScreen`` — subscribers, co-publishers and access refresh
///   for owners (``SubscribersPanel``, ``CoPublishersPanel``,
///   ``RotationPanel``).
/// - ``JoinCoPublisherScreen`` — the joiner side of a co-publisher link.
///
/// The screens share one `ChannelsUiState`, owned by the signed-in shell so
/// the conversation list and the pushed screens read one store; each child
/// receives it through `@ObservedObject`, so a publish in one view surfaces in
/// the list of another without manual rebroadcast.

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
    /// `docs/ux/feed.md` section 5 — the newest readable post was on screen
    /// in the subscriber view: it is read here and in the feed.
    var onPostRead: ((_ channelId: String, _ cursor: UInt64) -> Void)?

    /// Popping out of the channel once this device stops following it: the
    /// row it was opened from is gone, so there is nothing behind this screen
    /// to come back to.
    @Environment(\.dismiss) private var dismiss
    @State private var infoOpen = false

    var body: some View {
        Group {
            // ADR-0065 phase 2 (UX 16.6) — a channel this device owned under
            // keys a reset replaced: the notice and `Remove from this device`;
            // the composer, the rotation and the subscriber management are
            // gone (the server refuses the old owner key).
            if state.keysByChannel[channelId] != nil, state.orphanedByReset(channelId: channelId) {
                ChannelOrphanedView(channelId: channelId, state: state) { dismiss() }
            } else if state.keysByChannel[channelId] != nil {
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
                    foundHandle: foundHandle,
                    onPostRead: onPostRead.map { read in { read(channelId, $0) } }
                )
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            // `docs/ux/conversation-info.md` section 1 — the name is the ONE
            // way into this channel's info, the same way a chat's name is the
            // one way into its own. A plain `navigationTitle` cannot be
            // tapped, so the title is a button in the principal slot, which is
            // what `ChatScreen` already does.
            ToolbarItem(placement: .principal) {
                Button { infoOpen = true } label: {
                    Text(title)
                        .font(.headline)
                        .foregroundStyle(Color.lakkoText)
                        .lineLimit(1)
                }
                .buttonStyle(.plain)
                .accessibilityHint(ConversationInfoDestination.ofChannel(channelId: channelId)
                    .accessibilityHint)
            }
        }
        .sheet(isPresented: $infoOpen) {
            // Section 3 — a layer over the channel, which stays where it is
            // behind it.
            ChannelInfoSheet(
                channelId: channelId,
                state: state,
                client: client,
                accountIdentitySeed: accountIdentitySeed,
                foundHandle: foundHandle,
                onUnsubscribed: {
                    infoOpen = false
                    dismiss()
                },
                onDone: { infoOpen = false }
            )
        }
    }

    /// ADR-0051 §5 — the shared name, else the local name, else the handle.
    private var title: String {
        let channel = state.channels.first(where: { $0.id == channelId })
        return ChannelsFormatting.title(
            sharedName: channel?.sharedName ?? "",
            localName: channel?.localName ?? "",
            handle: channel?.handle ?? foundHandle)
    }
}

// MARK: - Header card (ADR-0051 §5, UX spec §3.2)

/// The card at the top of the channel view, the same shape as web's
/// `ChannelHead`: avatar, the name, `@handle` (never repeated when the name
/// IS the handle), the description when present, one status line.
struct ChannelHeaderCard: View {
    let channelId: String
    let channel: ChannelHandle?
    /// The handle the channel was found by when it is not listed yet.
    var foundHandle: String = ""
    let isOwner: Bool
    /// Overrides the role / handle status while access is being set up.
    var status: String? = nil

    private var handle: String { channel?.handle ?? foundHandle }

    private var title: String {
        ChannelsFormatting.title(
            sharedName: channel?.sharedName ?? "",
            localName: channel?.localName ?? "",
            handle: handle)
    }

    private var handleLine: String? {
        let bare = ChannelsFormatting.bareHandle(handle)
        return !bare.isEmpty && bare != title ? "@\(bare)" : nil
    }

    private var statusLine: String {
        status ?? ChannelsFormatting.headerStatus(title: title, handle: handle, isOwner: isOwner)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .center, spacing: 12) {
                InitialsAvatar(
                    name: title,
                    identity: channelId,
                    size: 48,
                    placeholderSymbol: "megaphone.fill"
                )
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(.headline)
                        .foregroundStyle(Color.lakkoText)
                        .lineLimit(2)
                    if let handleLine {
                        Text(handleLine)
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoTextMuted)
                            .lineLimit(1)
                    }
                    // The status is the handle when it is not already shown
                    // above; never the same line twice.
                    if statusLine != handleLine {
                        Text(statusLine)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                            .lineLimit(1)
                    }
                }
            }
            if let description = channel?.displayDescription.trimmingCharacters(in: .whitespacesAndNewlines),
               !description.isEmpty {
                Text(description)
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lakkoSurface)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous)
                .stroke(Color.lakkoBorder, lineWidth: 1)
        )
        .accessibilityElement(children: .combine)
    }
}

// MARK: - Orphaned by a key reset (ADR-0065 phase 2, UX 16.6)

/// The channel as the device that reset its account keys sees it: the header,
/// one notice stating what the reset did, and one control. The person creates
/// a new channel from the create menu as always; nothing is copied across,
/// because the subscribers of the old channel have no way to be told.
private struct ChannelOrphanedView: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let onRemoved: () -> Void
    @State private var confirming = false
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                ChannelHeaderCard(
                    channelId: channelId,
                    channel: state.channels.first(where: { $0.id == channelId }),
                    isOwner: true,
                    status: l10n.t("channels.keys_reset.row_sub")
                )
                HStack(alignment: .top, spacing: 12) {
                    Image(systemName: "exclamationmark.shield")
                        .foregroundStyle(Color.lakkoTextMuted)
                        .accessibilityHidden(true)
                    Text(l10n.t("channels.keys_reset.notice"))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoText)
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.lakkoSurfaceSunken)
                .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
                .accessibilityElement(children: .combine)
                Button(role: .destructive) { confirming = true } label: {
                    Text(l10n.t("channels.keys_reset.remove"))
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
            }
            .padding(16)
        }
        .background(Color.lakkoBackground)
        .confirmationDialog(
            l10n.t("channels.keys_reset.remove"), isPresented: $confirming, titleVisibility: .visible
        ) {
            Button(l10n.t("channels.keys_reset.remove"), role: .destructive) {
                state.removeOrphanedChannel(channelId)
                onRemoved()
            }
            Button(l10n.t("common.cancel"), role: .cancel) {}
        }
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
    @State private var publishError: UiText?
    @State private var decodedByCursor: [UInt64: String?] = [:]
    /// ADR-0051 §4 — cursors of rows that are metadata records (kept, never
    /// shown) or malformed records (dropped). Filtered out of ``posts``.
    @State private var hiddenCursors: Set<UInt64> = []
    @State private var loading = true
    @FocusState private var composerFocused: Bool
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    private var posts: [PostView] {
        (state.postsByChannel[channelId] ?? [])
            .filter { !hiddenCursors.contains($0.cursor) }
            .sorted { $0.cursor < $1.cursor }
    }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    ChannelHeaderCard(
                        channelId: channelId,
                        channel: state.channels.first(where: { $0.id == channelId }),
                        isOwner: true
                    )
                    .padding(.bottom, 6)
                    if posts.isEmpty {
                        Text(l10n.t(loading ? "common.loading" : "channels.posts.empty_owner"))
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
                    Text(l10n.t("common.manage"))
                }
            }
        }
        .task { await refresh() }
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
                    Text(publishError.text)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoDanger)
                    Spacer()
                    Button(l10n.t("common.retry")) { Task { await publish() } }
                        .font(.footnote)
                        .disabled(publishBusy)
                }
            }
            HStack(alignment: .bottom, spacing: 8) {
                TextField(l10n.t("channels.compose.placeholder"), text: $composeText, axis: .vertical)
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
                .accessibilityLabel(l10n.t(publishBusy ? "channels.compose.publishing" : "channels.compose.publish"))
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
            publishError = .key("channels.posts.error.load")
        }
    }

    private func publish() async {
        if publishBusy { return }
        let text = composeText.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.isEmpty { return }
        guard let keys = state.keysByChannel[channelId] else {
            publishError = .key("channels.publish.error.not_publisher")
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
            let identity = try Identity.fromSeed(seed: accountIdentitySeed)
            _ = try await client.publishPost(channelIdHex: channelId, payload: wire, accountIdentity: identity)
            composeText = ""
            // The author's own post goes into the feed now, not at the next sync.
            ChannelPostSync.requestSync()
            await refresh()
        } catch {
            NSLog("Channels: publish failed: %@", String(describing: error))
            publishError = ChannelsErrorCopy.text(
                for: error, prefixKey: "channels.publish.error.prefix",
                fallbackKey: "channels.publish.error.generic")
        }
    }

    private func decodeAll(posts: [PostView]) {
        guard let keys = state.keysByChannel[channelId] else {
            decodedByCursor = [:]
            hiddenCursors = []
            return
        }
        var next: [UInt64: String?] = [:]
        var hidden: Set<UInt64> = []
        for post in posts {
            do {
                let plain = try ChannelsComposer.verifyAndDecrypt(
                    seed: keys.seed,
                    channelIdHex: channelId,
                    postWireB64Url: post.payloadB64
                )
                // ADR-0051 §4 — a metadata record is kept (newest wins) and
                // never rendered; a domain match that failed a bound is dropped.
                switch ChannelMetaRecord.classify(plain) {
                case .text(let text):
                    next[post.cursor] = text
                case .meta(let meta):
                    state.recordSharedMeta(channelId: channelId, meta: meta)
                    hidden.insert(post.cursor)
                case .malformed:
                    hidden.insert(post.cursor)
                }
            } catch {
                next[post.cursor] = nil as String?
            }
        }
        decodedByCursor = next
        hiddenCursors = hidden
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
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            switch content {
            case .decoding:
                Text(l10n.t("common.loading"))
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
            locale: l10n.locale
        )
    }
}
