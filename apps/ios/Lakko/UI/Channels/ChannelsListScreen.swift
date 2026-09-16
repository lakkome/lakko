// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// Channels UI for the iOS client (UX spec section 5.4). One screen per
/// file under `UI/Channels/`:
///
/// - ``ChannelsListScreen`` (this file, with ``ChannelsRootScreen``) — the
///   tab root: the list, search, the add menu, the empty state.
/// - ``NewChannelScreen`` — the create form.
/// - ``ChannelDetailScreen`` — the channel view: posts as cards, the publish
///   composer for owners, the reader flow for everyone else.
/// - ``ChannelReaderScreen`` — subscribe, wait for access, read.
/// - ``ChannelManageScreen`` — subscribers, co-publishers and access refresh
///   for owners (``SubscribersPanel``, ``CoPublishersPanel``,
///   ``RotationPanel``).
/// - ``JoinCoPublisherScreen`` — the joiner side of a co-publisher link.
///
/// The screens share one `ChannelsUiState` owned by the root view; each child
/// receives it through `@ObservedObject` so a publish in one view surfaces in
/// the list of another without manual rebroadcast.

// MARK: - ChannelsRootScreen

public struct ChannelsRootScreen: View {
    @StateObject private var state = ChannelsUiState()
    private let client: ChannelsClient
    /// ADR-0017: the 64-byte canonical AccountIdentity (phrase) seed for the
    /// current session. `NewChannelScreen.create` rebuilds a transient
    /// `Identity` from it (`Identity.fromSeed(seed: accountIdentitySeed)`) to
    /// drive the MLS PublisherGroup that produces the channel content-key seed
    /// (G-27). The host (`LakkoApp`) supplies it from `AuthRepository`'s
    /// in-memory `IdentitySecrets`.
    private let accountIdentitySeed: Data
    /// G4-5 — bearer token provider for the session-token-gated `@handle`
    /// registration on channel create. Read lazily per call so a token
    /// refresh is picked up without rebuilding the view.
    private let sessionTokenProvider: () -> String?

    public init(
        client: ChannelsClient = ChannelsClient(),
        accountIdentitySeed: Data,
        sessionTokenProvider: @escaping () -> String? = { nil }
    ) {
        self.client = client
        self.accountIdentitySeed = accountIdentitySeed
        self.sessionTokenProvider = sessionTokenProvider
    }

    /// Rendered inside the Channels tab's `NavigationStack`; every other
    /// channel screen pushes onto that stack. Follows the app's colour
    /// scheme like every other tab (UX spec section 4).
    public var body: some View {
        ChannelsListScreen(
            state: state,
            client: client,
            accountIdentitySeed: accountIdentitySeed,
            sessionTokenProvider: sessionTokenProvider
        )
    }
}

// MARK: - Routes

/// The screens the list pushes that are not a channel row: the two add-menu
/// flows and a channel found by handle from the search field.
enum ChannelsRoute: Hashable, Identifiable {
    case newChannel
    case joinCoPublisher
    /// A channel resolved from a typed `@handle` (or a pasted id); the reader
    /// screen offers Subscribe.
    case found(channelId: String, handle: String)

    var id: String {
        switch self {
        case .newChannel: return "new-channel"
        case .joinCoPublisher: return "join-co-publisher"
        case .found(let channelId, _): return "found-\(channelId)"
        }
    }
}

// MARK: - ChannelsListScreen

struct ChannelsListScreen: View {
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    /// Forwarded to ``NewChannelScreen`` and ``JoinCoPublisherScreen`` so
    /// they can build the MLS PublisherGroup identity.
    let accountIdentitySeed: Data
    /// G4-5 — forwarded to ``NewChannelScreen`` for the session-token-gated
    /// `@handle` registration on create.
    var sessionTokenProvider: () -> String? = { nil }

    /// UX spec section 5.1 vocabulary: the search field filters the list;
    /// a query that matches no local channel can be looked up as a handle
    /// (the former "Find & subscribe" box, folded into search).
    @State private var searchText: String = ""
    @State private var findBusy = false
    @State private var findError: String?
    @State private var route: ChannelsRoute?

    var body: some View {
        content
            .navigationTitle("Channels")
            .navigationBarTitleDisplayMode(.large)
            .searchable(text: $searchText, prompt: "Search or enter a @handle")
            .onChange(of: searchText) { _, _ in findError = nil }
            // Receiving is automatic elsewhere; here the pull gesture re-reads
            // each channel's posts and, for the owner, its subscriber count.
            .refreshable { await refreshAll() }
            .task { await refreshAll() }
            .navigationDestination(item: $route) { route in
                destination(for: route)
            }
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Button {
                            route = .newChannel
                        } label: {
                            Label("New channel", systemImage: "plus")
                        }
                        Button {
                            route = .joinCoPublisher
                        } label: {
                            Label("Join as co-publisher", systemImage: "person.badge.plus")
                        }
                    } label: {
                        Image(systemName: "plus")
                    }
                    .accessibilityLabel("Add")
                }
            }
    }

    @ViewBuilder
    private func destination(for route: ChannelsRoute) -> some View {
        switch route {
        case .newChannel:
            NewChannelScreen(
                state: state,
                client: client,
                accountIdentitySeed: accountIdentitySeed,
                sessionTokenProvider: sessionTokenProvider
            )
        case .joinCoPublisher:
            JoinCoPublisherScreen(
                state: state,
                accountIdentitySeed: accountIdentitySeed
            )
        case .found(let channelId, let handle):
            ChannelDetailScreen(
                channelId: channelId,
                state: state,
                client: client,
                accountIdentitySeed: accountIdentitySeed,
                foundHandle: handle
            )
        }
    }

    // MARK: Rows

    /// One row's rendered data, computed once per body pass so the search
    /// filter and the row read the same strings.
    private struct RowData: Identifiable {
        let channel: ChannelHandle
        let model: ChannelsFormatting.RowModel
        var id: String { channel.id }
    }

    private var rows: [RowData] {
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        return state.channels.map { channel in
            let posts = state.postsByChannel[channel.id] ?? []
            return RowData(
                channel: channel,
                model: ChannelsFormatting.rowModel(
                    name: channel.localName,
                    description: channel.localDescription,
                    handle: channel.handle,
                    subscriberCount: state.subscriberCountByChannel[channel.id],
                    newestPostMs: ChannelsFormatting.newestPostMs(posts.map(\.serverTsMs)),
                    nowMs: nowMs,
                    calendar: .current,
                    locale: .current
                )
            )
        }
        .filter { ChannelsFormatting.matches(query: searchText, row: $0.model) }
    }

    private var query: String {
        searchText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    @ViewBuilder
    private var content: some View {
        let visible = rows
        if state.channels.isEmpty && query.isEmpty {
            // Inside a scroll view so the pull gesture works on the empty list too.
            ScrollView {
                emptyState
                    .padding(.top, 80)
            }
        } else {
            List {
                ForEach(visible) { row in
                    NavigationLink {
                        ChannelDetailScreen(
                            channelId: row.channel.id,
                            state: state,
                            client: client,
                            accountIdentitySeed: accountIdentitySeed
                        )
                    } label: {
                        ChannelRow(channel: row.channel, model: row.model)
                    }
                    .listRowSeparator(.hidden)
                    .listRowInsets(EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
                }
                if !query.isEmpty {
                    if visible.isEmpty {
                        Text("No results")
                            .foregroundStyle(Color.lakkoTextMuted)
                            .frame(maxWidth: .infinity)
                            .listRowSeparator(.hidden)
                    }
                    Section {
                        findRow
                    } footer: {
                        if let findError {
                            Text(findError)
                                .foregroundStyle(Color.lakkoDanger)
                        }
                    }
                }
            }
            .listStyle(.plain)
        }
    }

    /// Look the query up as a channel handle (G4-5: a typed `@handle`, resolved
    /// through OPRF discovery, or a pasted channel id used verbatim) and open
    /// it for reading.
    private var findRow: some View {
        Button {
            Task { await onFind() }
        } label: {
            HStack(spacing: 12) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(Color.lakkoAccent)
                    .frame(width: 40, height: 40)
                VStack(alignment: .leading, spacing: 2) {
                    Text(findBusy ? "Looking up..." : "Find \(shownHandle)")
                        .foregroundStyle(Color.lakkoText)
                    Text("Open a channel you are not subscribed to yet")
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }
        }
        .disabled(findBusy)
        .listRowSeparator(.hidden)
        .listRowInsets(EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
    }

    private var shownHandle: String {
        query.hasPrefix("@") ? query : "@\(query)"
    }

    private func onFind() async {
        if findBusy { return }
        let raw = query
        if raw.isEmpty { return }
        findBusy = true
        findError = nil
        defer { findBusy = false }
        do {
            if let idHex = try await ChannelDiscovery.resolveChannelIdOrHandle(raw) {
                let handle = String(raw.drop(while: { $0 == "@" }))
                route = .found(channelId: idHex, handle: handle)
            } else {
                findError = "No channel is called \(shownHandle). Check the spelling."
            }
        } catch {
            NSLog("Channels: lookup failed: %@", String(describing: error))
            findError = "Could not look that up. Try again."
        }
    }

    /// UX spec section 5.1: one sentence and one action.
    private var emptyState: some View {
        ContentUnavailableView {
            Label("No channels yet", systemImage: "megaphone")
        } description: {
            Text("Channels you create or subscribe to appear here.")
        } actions: {
            Button("Create a channel") { route = .newChannel }
                .buttonStyle(.borderedProminent)
        }
    }

    // MARK: Refresh

    /// Re-read each channel's posts (for the last-post time) and, where this
    /// device holds publisher keys, its subscriber count. Best effort: a
    /// failure leaves the cached values in place.
    private func refreshAll() async {
        for channel in state.channels {
            if let page = try? await client.fetchPosts(channelIdHex: channel.id, after: 0) {
                state.setPosts(channelId: channel.id, posts: page.posts)
            }
            if state.keysByChannel[channel.id] != nil,
               let subscribers = try? await client.listSubscribers(channelIdHex: channel.id) {
                state.setSubscriberCount(channelId: channel.id, count: subscribers.count)
            }
        }
    }
}

// MARK: - ChannelRow

/// The same shape as a conversation row: a 40 pt avatar, the name, the time
/// of the last post on the right, the handle and subscriber count below.
private struct ChannelRow: View {
    let channel: ChannelHandle
    let model: ChannelsFormatting.RowModel

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            InitialsAvatar(
                name: model.title,
                // The channel id is hashed for the tint only, never shown.
                identity: channel.id,
                size: 40,
                placeholderSymbol: "megaphone.fill"
            )
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(model.title)
                        .font(.body)
                        .fontWeight(.medium)
                        .foregroundStyle(Color.lakkoText)
                        .lineLimit(1)
                    Spacer(minLength: 8)
                    if let timeLabel = model.timeLabel {
                        Text(timeLabel)
                            .font(.caption)
                            .foregroundStyle(Color.lakkoTextMuted)
                            .lineLimit(1)
                            .fixedSize()
                    }
                }
                if !model.secondLine.isEmpty {
                    Text(model.secondLine)
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .lineLimit(1)
                }
            }
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityText)
    }

    private var accessibilityText: String {
        var parts = [model.title]
        if let handle = model.handleLabel { parts.append(handle) }
        if let subs = model.subscribersLabel { parts.append(subs) }
        if let time = model.timeLabel { parts.append("last post \(time)") }
        return parts.joined(separator: ", ")
    }
}
