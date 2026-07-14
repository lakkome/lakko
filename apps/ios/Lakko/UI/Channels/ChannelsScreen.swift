// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
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
    /// ADR-0017: the 64-byte canonical AccountIdentity (phrase) seed
    /// for the current session. `NewChannelScreen.create` rebuilds a
    /// transient `Identity` from it
    /// (`Identity.fromSeed(seed: accountIdentitySeed)`) to drive the
    /// MLS PublisherGroup that produces the channel content-key seed
    /// (G-27). Was the OPAQUE export key pre-ADR-0017.
    ///
    /// The host (`LakkoApp` / a future Channels tab) supplies this
    /// from `AuthRepository`'s in-memory `IdentitySecrets` — same shape
    /// `ChatRepository.bootstrap` already uses.
    private let accountIdentitySeed: Data
    /// G4-5 — bearer token provider for the session-token-gated
    /// `@handle` registration on channel create (`/discovery/channel/register`).
    /// Read lazily per call (the host supplies
    /// `{ authRepository.identitySecrets?.sessionToken }`) so a token
    /// refresh is picked up without rebuilding the view. Defaults to a
    /// `nil`-returning closure so a preview / standalone host that never
    /// claims a handle still compiles.
    private let sessionTokenProvider: () -> String?
    /// Return to the caller's prior destination (Home). Wired by the
    /// session router; a `Back` toolbar button in
    /// [`ChannelsListScreen`] invokes it. Defaults to a no-op so a
    /// preview / standalone host need not supply one.
    private let onBack: () -> Void

    public init(
        client: ChannelsClient = ChannelsClient(),
        accountIdentitySeed: Data,
        sessionTokenProvider: @escaping () -> String? = { nil },
        onBack: @escaping () -> Void = {}
    ) {
        self.client = client
        self.accountIdentitySeed = accountIdentitySeed
        self.sessionTokenProvider = sessionTokenProvider
        self.onBack = onBack
    }

    public var body: some View {
        NavigationStack {
            ChannelsListScreen(
                state: state,
                client: client,
                accountIdentitySeed: accountIdentitySeed,
                sessionTokenProvider: sessionTokenProvider,
                onBack: onBack
            )
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
    /// Forwarded down to [`NewChannelScreen`] so the
    /// create-action can build the MLS PublisherGroup
    /// identity. See [`ChannelsRootScreen.accountIdentitySeed`].
    let accountIdentitySeed: Data
    /// G4-5 — forwarded to [`NewChannelScreen`] for the
    /// session-token-gated `@handle` registration on create.
    var sessionTokenProvider: () -> String? = { nil }
    /// Return to Home — the session router owns the destination
    /// toggle; this screen is presented in place of Home, so it
    /// carries its own back affordance (same idiom as
    /// `DevicesScreen` / `ContactsScreen`).
    var onBack: () -> Void = {}

    // G4-5: subscribe entry point. Accepts EITHER a typed `@handle`
    // (resolved to a channel id via OPRF discovery) OR a pasted 32-char
    // hex channel id (used verbatim). Both land on the same subscribe
    // screen. Mirror of the web `ChannelsList` "Find & subscribe" box.
    @State private var findInput: String = ""
    @State private var findBusy = false
    @State private var findError: String?
    /// Non-`nil` after a successful resolve → drives the push to
    /// [`ChannelSubscribeScreen`] via `navigationDestination(item:)`
    /// (iOS 17). Set back to `nil` by SwiftUI when the user pops back.
    @State private var subscribeTarget: String?

    var body: some View {
        ZStack {
            Theme.background.ignoresSafeArea()
            VStack(spacing: 0) {
                findSubscribeBar
                if state.channels.isEmpty {
                    emptyState
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    listContent
                }
            }
        }
        .navigationTitle("Channels")
        .navigationDestination(item: $subscribeTarget) { idHex in
            ChannelSubscribeScreen(
                channelId: idHex,
                state: state,
                client: client
            )
        }
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button("Back", action: onBack)
                    .foregroundStyle(Theme.accent)
            }
            ToolbarItem(placement: .topBarLeading) {
                NavigationLink {
                    JoinCoPublisherScreen(
                        state: state,
                        accountIdentitySeed: accountIdentitySeed
                    )
                } label: {
                    Image(systemName: "person.badge.plus")
                        .foregroundStyle(Theme.accent)
                }
            }
            ToolbarItem(placement: .topBarTrailing) {
                NavigationLink {
                    NewChannelScreen(
                        state: state,
                        client: client,
                        accountIdentitySeed: accountIdentitySeed,
                        sessionTokenProvider: sessionTokenProvider
                    )
                } label: {
                    Image(systemName: "plus")
                        .foregroundStyle(Theme.accent)
                }
            }
        }
    }

    // G4-5: "Find & subscribe" — one field accepting a `@handle` OR a
    // pasted hex channel id. On submit it resolves the input and pushes
    // the subscribe screen for the resolved id.
    private var findSubscribeBar: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                TextField("@handle or channel id", text: $findInput)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
                    .foregroundStyle(Theme.textPrimary)
                    .padding(9)
                    .background(Theme.surface)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                    .onSubmit { Task { await onFind() } }
                Button(findBusy ? "Finding…" : "Find") {
                    Task { await onFind() }
                }
                .disabled(
                    findBusy
                        || findInput.trimmingCharacters(in: .whitespaces).isEmpty
                )
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                .background(Theme.surface)
                .foregroundStyle(Theme.textPrimary)
                .clipShape(RoundedRectangle(cornerRadius: 8))
            }
            if let findError {
                Text(findError)
                    .font(.footnote)
                    .foregroundStyle(Theme.destructive)
            }
        }
        .padding(12)
    }

    private func onFind() async {
        if findBusy { return }
        let raw = findInput.trimmingCharacters(in: .whitespacesAndNewlines)
        if raw.isEmpty { return }
        findBusy = true
        findError = nil
        defer { findBusy = false }
        do {
            if let idHex = try await ChannelDiscovery.resolveChannelIdOrHandle(raw) {
                subscribeTarget = idHex
            } else {
                let shown = raw.hasPrefix("@") ? raw : "@\(raw)"
                findError = "No channel found for \(shown)."
            }
        } catch {
            findError = "Lookup failed: \(error.localizedDescription)"
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
                        client: client,
                        accountIdentitySeed: accountIdentitySeed
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
    /// ADR-0017: the canonical AccountIdentity (phrase) seed from the
    /// active session. Used to build a transient `Identity` inside
    /// [`create`] — the MLS PublisherGroup founding member. Was the
    /// OPAQUE export key pre-ADR-0017.
    let accountIdentitySeed: Data
    /// G4-5 — bearer token provider for the session-token-gated
    /// `@handle` registration (`/discovery/channel/register`). Read
    /// lazily so a token refresh is picked up; `nil` → cannot claim a
    /// handle (an empty handle skips discovery entirely, so create still
    /// works).
    var sessionTokenProvider: () -> String? = { nil }
    @Environment(\.dismiss) private var dismiss

    @State private var name: String = ""
    @State private var description: String = ""
    /// Optional `@handle` claimed via OPRF discovery (G4-5) so others
    /// can subscribe by handle instead of the raw hex channel id.
    @State private var handle: String = ""
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
                Section {
                    TextField("@handle (optional)", text: $handle)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                        .listRowBackground(Theme.surface)
                        .foregroundStyle(Theme.textPrimary)
                } footer: {
                    Text("Public @handle — others can find + subscribe to this channel by @handle instead of the raw id. First-come-first-served; leave blank to skip.")
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
            // G4-5: claim the optional `@handle` FIRST (before the
            // channel exists on the channels service) so a
            // first-come-first-served collision (409) aborts cleanly
            // without leaving a half-created channel. An empty handle
            // skips discovery entirely — the raw hex channel id still
            // works. The mapping is `@handle -> channel_id`; the OPRF
            // hash is all the server ever sees.
            let wantedHandle = handle.trimmingCharacters(in: .whitespaces)
            if !wantedHandle.isEmpty {
                guard let token = sessionTokenProvider() else {
                    errorMessage =
                        "Sign in again to claim a @handle (or leave it blank)."
                    return
                }
                do {
                    _ = try await ChannelDiscovery.registerChannelHandle(
                        handle: wantedHandle,
                        channelIdHex: id,
                        sessionToken: token
                    )
                } catch let DiscoveryClientError.http(status, _, _) where status == 409 {
                    let clean = wantedHandle.hasPrefix("@")
                        ? String(wantedHandle.dropFirst())
                        : wantedHandle
                    errorMessage =
                        "The handle @\(clean) is already taken — pick another (or leave it blank)."
                    return
                } catch {
                    errorMessage =
                        "Could not register the handle: \(error.localizedDescription)"
                    return
                }
            }

            _ = try await client.createChannel(channelIdHex: id)
            // G-27: build a transient `Identity` from the
            // canonical AccountIdentity (phrase) seed (ADR-0017 —
            // was the OPAQUE export key), hand it to the composer
            // so the channel content-key seed comes from a
            // real MLS PublisherGroup exporter (instead of
            // `newChannelSeedForTest`). The identity is
            // throwaway — `freshPublisherKeys` builds a
            // throwaway `Group` from it for the duration
            // of the call.
            // `Identity` releases its handle via ARC at
            // scope-exit — UniFFI's Swift surface has no
            // explicit `close()`.
            let identity = try Identity.fromSeed(seed: accountIdentitySeed)
            let keys = try ChannelsComposer.freshPublisherKeys(
                identity: identity,
                channelIdHex: id
            )
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
        } catch let err as ChannelsComposerError {
            errorMessage = "Composer: \(err)"
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
    /// ADR-0028 OQ3 — the canonical AccountIdentity seed, threaded so the
    /// co-publisher panel can open the channel's PublisherGroup + sign.
    let accountIdentitySeed: Data

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
                        SubscribersPanelView(
                            channelId: channelId,
                            state: state,
                            client: client
                        )
                        CoPublishersPanelView(
                            channelId: channelId,
                            client: client,
                            accountIdentitySeed: accountIdentitySeed
                        )
                        RotationPanelView(
                            channelId: channelId,
                            client: client,
                            accountIdentitySeed: accountIdentitySeed
                        )
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
        VStack(alignment: .leading, spacing: 12) {
            Text(
                "No local publisher keys for this channel — you cannot publish to it from this device."
            )
            .font(.footnote)
            .foregroundStyle(Theme.textSecondary)

            NavigationLink {
                ChannelSubscribeScreen(
                    channelId: channelId,
                    state: state,
                    client: client
                )
            } label: {
                Text("Read as a subscriber →")
                    .font(.footnote)
                    .foregroundStyle(Theme.accent)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface)
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

// ============================================================================
// SubscribersPanelView — publisher-side fan-out widget
// (embedded inside ChannelDetailScreen when the user holds
//  publisher keys for this channel)
// ============================================================================

/// Auto-fanout poll cadence — every 5 s the publisher
/// panel re-fetches the subscriber list, diffs against
/// the already-delivered set, and auto-seals an envelope
/// for each new pubkey. Same cadence the subscribe-screen
/// poll uses on the receive side.
private let subscribersPollIntervalNs: UInt64 = 5_000_000_000

struct SubscribersPanelView: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient

    @State private var subscribers: [String]?
    @State private var busyPk: String?
    @State private var error: String?
    private enum Status { case delivered, auto, failed }
    @State private var statusByPk: [String: Status] = [:]
    @State private var pollTask: Task<Void, Never>?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Subscribers")
                    .font(.subheadline.bold())
                    .foregroundStyle(Theme.textPrimary)
                Text("(\(subscribers?.count.description ?? "…"))")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                Spacer()
                Button("Refresh") {
                    Task { await refresh() }
                }
                .font(.caption)
                .foregroundStyle(Theme.textSecondary)
            }

            if let error {
                Text(error)
                    .font(.caption)
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.destructive)
                    .foregroundStyle(.white)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
            }

            if let subs = subscribers {
                if subs.isEmpty {
                    Text(
                        "No subscribers yet. Anyone with the channel id can open the subscribe screen and register their X25519 public key."
                    )
                    .font(.caption)
                    .foregroundStyle(Theme.textSecondary)
                } else {
                    ForEach(subs, id: \.self) { pk in
                        subscriberRow(pk)
                    }
                }
            } else {
                Text("Loading subscribers…")
                    .font(.caption)
                    .foregroundStyle(Theme.textSecondary)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .task {
            await refresh()
            startAutoFanoutLoop()
        }
        .onDisappear { pollTask?.cancel() }
    }

    private func subscriberRow(_ pk: String) -> some View {
        HStack(alignment: .center, spacing: 10) {
            Text(pk)
                .font(.caption2.monospaced())
                .foregroundStyle(Theme.textSecondary)
                .lineLimit(1)
                .truncationMode(.middle)

            if let st = statusByPk[pk] {
                let (label, color): (String, Color) = {
                    switch st {
                    case .delivered: return ("delivered", Theme.accent)
                    case .auto:      return ("auto-delivered", Theme.accent)
                    case .failed:    return ("failed", Theme.destructive)
                    }
                }()
                Text(label)
                    .font(.caption2)
                    .foregroundStyle(color)
            }

            Spacer(minLength: 4)

            Button {
                Task { await deliverTo(pk) }
            } label: {
                Text(busyPk == pk ? "Sealing…" : "Deliver seed")
                    .font(.caption2)
            }
            .buttonStyle(.borderedProminent)
            .tint(Theme.accent)
            .controlSize(.mini)
            .disabled(busyPk == pk)
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.background)
        .clipShape(RoundedRectangle(cornerRadius: 6))
    }

    private func refresh() async {
        do {
            subscribers = try await client.listSubscribers(
                channelIdHex: channelId
            )
        } catch let err as ChannelsApiError {
            error = "List failed: \(err.code) — \(err.detail)"
        } catch {
            self.error = "List failed: \(error.localizedDescription)"
        }
    }

    private enum DeliverTrigger { case manual, auto }

    private func deliverTo(
        _ pubkeyHex: String,
        trigger: DeliverTrigger = .manual
    ) async {
        // Manual delivery is one-at-a-time (single busy
        // spinner across the panel). The auto path
        // ignores `busyPk` so a slow manual click on one
        // subscriber does not stall the polling loop's
        // progress on others.
        if trigger == .manual && busyPk != nil { return }
        guard let keys = state.keysByChannel[channelId] else {
            error = "No publisher keys on this device."
            return
        }
        if trigger == .manual {
            busyPk = pubkeyHex
            error = nil
        }
        defer { if trigger == .manual { busyPk = nil } }
        do {
            guard let subPub = Data(channelHex: pubkeyHex) else {
                if trigger == .manual { error = "Bad subscriber pubkey hex." }
                statusByPk[pubkeyHex] = .failed
                return
            }
            let epoch = state.epochByChannel[channelId] ?? 0
            let wire = try ChannelsComposer.sealSubscriberSeedEnvelope(
                subscriberPub: subPub,
                channelIdHex: channelId,
                epochId: epoch,
                seed: keys.seed
            )
            // ADR-0031: file the envelope under the exact epoch we
            // sealed it for, so a catching-up subscriber lands it at
            // the right key in its `epoch -> seed` map.
            try await client.deliverEnvelope(
                channelIdHex: channelId,
                subscriberPubkeyHex: pubkeyHex,
                envelope: wire,
                epoch: epoch
            )
            statusByPk[pubkeyHex] = trigger == .auto ? .auto : .delivered
        } catch let err as ChannelsApiError {
            if trigger == .manual {
                error = "Deliver failed: \(err.code) — \(err.detail)"
            }
            statusByPk[pubkeyHex] = .failed
        } catch let err as ChannelsComposerError {
            if trigger == .manual { error = "Seal failed: \(err)" }
            statusByPk[pubkeyHex] = .failed
        } catch {
            if trigger == .manual {
                self.error = "Error: \(error.localizedDescription)"
            }
            statusByPk[pubkeyHex] = .failed
        }
    }

    /// Start the auto-fanout polling loop. Every
    /// `subscribersPollIntervalNs`: refresh the list,
    /// then auto-deliver to any pubkey we have not
    /// successfully delivered to yet. Cancellable via
    /// `pollTask?.cancel()` in `.onDisappear`.
    private func startAutoFanoutLoop() {
        pollTask?.cancel()
        pollTask = Task {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: subscribersPollIntervalNs)
                if Task.isCancelled { return }
                await refresh()
                guard let list = subscribers else { continue }
                // Sequence the auto-deliveries — UniFFI
                // ChannelSigningKey / ChannelSubscriberKeypair
                // calls each hit the Rust runtime
                // serially anyway, and serial avoids a
                // burst of N parallel seals on a fresh
                // subscriber storm.
                for pk in list {
                    if Task.isCancelled { return }
                    let st = statusByPk[pk]
                    if st == nil || st == .failed {
                        await deliverTo(pk, trigger: .auto)
                    }
                }
            }
        }
    }
}

// ============================================================================
// ChannelSubscribeScreen — subscriber-side route
//   Generates (or restores) an X25519 keypair on first
//   visit, registers it on the channel, polls for a seed
//   envelope, opens it, then renders decoded posts using
//   the recovered seed.
// ============================================================================

struct ChannelSubscribeScreen: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient

    private enum Bootstrap {
        case idle, registering, polling, active, errorState
    }
    /// Per-post render state after the decrypt-by-epoch pass
    /// (ADR-0031). An absent entry means "still decoding".
    private enum PostDecodeState {
        /// Recovered plaintext.
        case text(String)
        /// Present epoch seed but AEAD/verify failed — wrong
        /// seed, tampered ciphertext, or another publisher's
        /// signing key.
        case unreadable
        /// The post's epoch is not in the retained `epoch ->
        /// seed` map (older than the retention window, or a
        /// gap). Never force-decrypted with a wrong seed
        /// (invariant #4) — surfaced as a placeholder + skipped.
        case olderKeyWindow
    }
    @State private var bootstrap: Bootstrap = .idle
    @State private var bootstrapError: String?
    @State private var posts: [PostView] = []
    @State private var decodedByCursor: [UInt64: PostDecodeState] = [:]
    @State private var pollTask: Task<Void, Never>?

    var body: some View {
        ZStack {
            Theme.background.ignoresSafeArea()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    metaSection
                    keypairSection
                    statusSection
                    postsSection
                }
                .padding()
            }
        }
        .navigationTitle("Subscribe")
        .navigationBarTitleDisplayMode(.inline)
        .task { await runBootstrap() }
        .onDisappear { pollTask?.cancel() }
    }

    private var metaSection: some View {
        Text(channelId)
            .font(.caption.monospaced())
            .foregroundStyle(Theme.textSecondary)
    }

    private var keypairSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let kp = state.subscriberKeysByChannel[channelId] {
                Text("Your subscriber public key (registered on this channel):")
                    .font(.caption)
                    .foregroundStyle(Theme.textSecondary)
                Text(ChannelsComposer.bytesToHex(kp.publicKey))
                    .font(.caption2.monospaced())
                    .foregroundStyle(Theme.textPrimary)
                    .textSelection(.enabled)
                    .lineLimit(2)
                    .truncationMode(.middle)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    @ViewBuilder
    private var statusSection: some View {
        let (text, color): (String, Color) = {
            switch bootstrap {
            case .idle, .registering:
                return ("Registering your key on the channel…", Theme.textSecondary)
            case .polling:
                return (
                    "Registered. Waiting for the publisher to fan out a seed envelope… (auto-refreshes every 3 s)",
                    Theme.textSecondary
                )
            case .active:
                return ("Seed envelope opened. You can read posts on this channel.", Theme.accent)
            case .errorState:
                return (bootstrapError ?? "Unknown error.", Theme.destructive)
            }
        }()
        Text(text)
            .font(.footnote)
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.surface)
            .foregroundStyle(color)
            .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    private var postsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Posts")
                .font(.headline)
                .foregroundStyle(Theme.textPrimary)

            if bootstrap == .active && !posts.isEmpty {
                ForEach(posts, id: \.cursor) { p in
                    postRow(p)
                }
            } else {
                Text(
                    bootstrap == .active
                        ? "No posts yet on this channel."
                        : (bootstrap == .polling
                            ? "Waiting for the publisher to deliver your seed."
                            : "Bootstrapping the subscribe flow…")
                )
                .foregroundStyle(Theme.textSecondary)
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

            switch decodedByCursor[post.cursor] {
            case .text(let text):
                Text(text)
                    .foregroundStyle(Theme.textPrimary)
            case .olderKeyWindow:
                Text("[message from an older key window — no longer available]")
                    .font(.footnote)
                    .italic()
                    .foregroundStyle(Theme.textSecondary)
            case .unreadable:
                Text("[unreadable — wrong seed or tampered ciphertext]")
                    .font(.footnote)
                    .italic()
                    .foregroundStyle(Theme.textSecondary)
            case .none:
                Text("Decoding…")
                    .font(.footnote)
                    .italic()
                    .foregroundStyle(Theme.textSecondary)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    // MARK: - Actions

    private func ensureKeypair() -> ChannelSubscriberKeys {
        if let existing = state.subscriberKeysByChannel[channelId] {
            return existing
        }
        let fresh = ChannelsComposer.freshSubscriberKeys()
        state.setSubscriberKeys(channelId: channelId, keys: fresh)
        return fresh
    }

    private func runBootstrap() async {
        bootstrap = .registering
        bootstrapError = nil
        let kp = ensureKeypair()
        let pubHex = ChannelsComposer.bytesToHex(kp.publicKey)
        do {
            _ = try await client.subscribe(
                channelIdHex: channelId,
                subscriberPubkeyHex: pubHex
            )
        } catch let err as ChannelsApiError {
            bootstrapError = "\(err.code) — \(err.detail)"
            bootstrap = .errorState
            return
        } catch {
            bootstrapError = error.localizedDescription
            bootstrap = .errorState
            return
        }

        // Catch-up (ADR-0031): pull the whole retained envelope
        // set and open each `(epoch, envelope)` into the
        // per-channel `epoch -> seed` map, so posts made under a
        // rotation this device missed are still decryptable.
        await catchUp(pubHex: pubHex)

        let seeds = state.subscribedSeeds(channelId: channelId)
        let currentEpoch = state.epochByChannel[channelId]
        let haveCurrent = currentEpoch.map { seeds[$0] != nil } ?? false

        // Re-sync: if the CURRENT epoch's seed is missing (offline
        // past the retention window, or nothing fanned out yet), a
        // re-subscribe is idempotent and makes the publisher
        // reseal this device on its next fanout. Beyond-retention
        // posts stay the "older key window" placeholder.
        if !haveCurrent {
            _ = try? await client.subscribe(
                channelIdHex: channelId,
                subscriberPubkeyHex: pubHex
            )
        }

        if seeds.isEmpty {
            // No seed at any epoch yet — wait for the first fanout.
            bootstrap = .polling
            startPollingTask(pubHex: pubHex)
        } else {
            // We can read at least one epoch's posts now.
            bootstrap = .active
            await loadAndDecodePosts()
            // Keep polling until the current epoch's seed lands so
            // brand-new posts become readable too.
            if !haveCurrent {
                startPollingTask(pubHex: pubHex)
            }
        }
    }

    private func startPollingTask(pubHex: String) {
        pollTask?.cancel()
        pollTask = Task {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 3_000_000_000)
                if Task.isCancelled { return }
                await catchUp(pubHex: pubHex)
                let seeds = state.subscribedSeeds(channelId: channelId)
                if !seeds.isEmpty {
                    bootstrap = .active
                    await loadAndDecodePosts()
                }
                // Stop once we hold the current epoch's seed — new
                // posts are now readable; older ones stay the
                // placeholder. A permanent failure (unknown-channel
                // etc.) would already have surfaced from register.
                if let cur = state.epochByChannel[channelId],
                    seeds[cur] != nil {
                    return
                }
            }
        }
    }

    /// ADR-0031 catch-up: refresh the current epoch, pull the
    /// whole retained envelope set, and open each into the
    /// per-channel `epoch -> seed` map. Best-effort — a transient
    /// network failure leaves the existing map intact so the poll
    /// loop retries. The singular `fetchEnvelope` is kept as a
    /// fallback to guarantee the current seed lands even if the
    /// plural set momentarily lags a just-committed reseal.
    private func catchUp(pubHex: String) async {
        guard let kp = state.subscriberKeysByChannel[channelId] else { return }

        // Current epoch first — the map key the re-sync check
        // + the singular fallback need.
        if let cur = try? await client.fetchEpoch(channelIdHex: channelId) {
            state.setEpoch(channelId: channelId, epoch: cur)
        }

        // Plural: the retained `[(epoch, envelope)]` set, each with
        // its concrete epoch. Open any epoch not already held.
        if let set = try? await client.fetchEnvelopes(
            channelIdHex: channelId,
            subscriberPubkeyHex: pubHex
        ) {
            for (epoch, wire) in set {
                if state.subscribedSeeds(channelId: channelId)[epoch] != nil {
                    continue
                }
                if let seed = try? ChannelsComposer.openSubscriberSeedEnvelope(
                    keys: kp,
                    channelIdHex: channelId,
                    epochId: epoch,
                    wire: wire
                ) {
                    state.setSubscribedSeed(
                        channelId: channelId, epoch: epoch, seed: seed
                    )
                }
            }
        }

        // Singular fallback: guarantee the current-epoch seed is
        // present (the plural set may lag a just-committed reseal).
        if let cur = state.epochByChannel[channelId],
            state.subscribedSeeds(channelId: channelId)[cur] == nil {
            if let b64 = (try? await client.fetchEnvelope(
                channelIdHex: channelId,
                subscriberPubkeyHex: pubHex
            )) ?? nil,
                let wire = Data(base64URLNoPadSubscriber: b64),
                let seed = try? ChannelsComposer.openSubscriberSeedEnvelope(
                    keys: kp,
                    channelIdHex: channelId,
                    epochId: cur,
                    wire: wire
                ) {
                state.setSubscribedSeed(
                    channelId: channelId, epoch: cur, seed: seed
                )
            }
        }
    }

    private func loadAndDecodePosts() async {
        let seeds = state.subscribedSeeds(channelId: channelId)
        if seeds.isEmpty { return }
        do {
            let page = try await client.fetchPosts(
                channelIdHex: channelId,
                after: 0
            )
            posts = page.posts
            var next: [UInt64: PostDecodeState] = [:]
            for post in page.posts {
                // Decode the clear header to learn the post's epoch,
                // then pick the seed matching THAT epoch (ADR-0031).
                guard let epoch = try? ChannelsComposer.postEpoch(
                    postWireB64Url: post.payloadB64
                ) else {
                    next[post.cursor] = .unreadable  // malformed wire
                    continue
                }
                guard let seed = seeds[epoch] else {
                    // No seed for this epoch — older than retention
                    // or a gap. Skip; never force-decrypt with a
                    // wrong seed (invariant #4).
                    next[post.cursor] = .olderKeyWindow
                    continue
                }
                do {
                    let plain = try ChannelsComposer.verifyAndDecrypt(
                        seed: seed,
                        channelIdHex: channelId,
                        postWireB64Url: post.payloadB64
                    )
                    if let text = String(data: plain, encoding: .utf8) {
                        next[post.cursor] = .text(text)
                    } else {
                        next[post.cursor] = .unreadable
                    }
                } catch {
                    next[post.cursor] = .unreadable
                }
            }
            decodedByCursor = next
        } catch {
            // Swallow — the status section already shows
            // the bootstrap state; the posts section will
            // simply show "no posts yet" until the next
            // refresh succeeds.
        }
    }

    private func isoTimestamp(ms: UInt64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(ms) / 1000.0)
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: date)
    }
}

// MARK: - hex / base64url helpers shared by the iOS UI

private extension Data {
    /// Decode 64 lowercase hex chars (subscriber pubkey
    /// wire form) into 32 bytes. Returns `nil` on any
    /// failure.
    init?(channelHex hex: String) {
        let chars = Array(hex)
        guard chars.count % 2 == 0 else { return nil }
        var out = Data(capacity: chars.count / 2)
        var i = 0
        while i < chars.count {
            guard let byte = UInt8(String(chars[i...(i + 1)]), radix: 16)
            else { return nil }
            out.append(byte)
            i += 2
        }
        self = out
    }

    /// Decode a base64url-no-pad string into `Data`.
    /// Local copy of `ChannelsComposer`'s private helper,
    /// re-implemented here so the subscribe screen does
    /// not have to reach into composer internals.
    init?(base64URLNoPadSubscriber s: String) {
        var std = s
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let pad = (4 - (std.count % 4)) % 4
        std.append(String(repeating: "=", count: pad))
        guard let data = Data(base64Encoded: std) else { return nil }
        self = data
    }
}

/// ADR-0028 OQ3 — owner-side co-publisher management panel. Holds a long-lived
/// PublisherGroup + Identity (the approve path needs the same handle the poll
/// loop populated the pending set on). Opens both on appear, polls the journal
/// every 5 s, and surfaces create-link + approve/reject.
struct CoPublishersPanelView: View {
    let channelId: String
    let client: ChannelsClient
    let accountIdentitySeed: Data

    @State private var identity: Identity?
    @State private var publisherGroup: PublisherGroup?
    @State private var link: String?
    @State private var pending: [Data] = []
    @State private var busy = false
    @State private var error: String?
    @State private var pollTask: Task<Void, Never>?

    private let ds = DsClient()

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Co-publishers")
                .font(.headline)
                .foregroundStyle(Theme.textPrimary)
            Text("Invite another device or person to publish to this channel.")
                .font(.caption)
                .foregroundStyle(Theme.textSecondary)

            Button {
                Task { await createLink() }
            } label: {
                Text("Create co-publisher link")
            }
            .disabled(busy || publisherGroup == nil)

            if let link {
                Text(link)
                    .font(.caption.monospaced())
                    .foregroundStyle(Theme.textSecondary)
                    .textSelection(.enabled)
            }

            if !pending.isEmpty {
                Text("Requests (\(pending.count))")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textPrimary)
                ForEach(pending, id: \.self) { ref in
                    HStack {
                        Text(Self.shortHex(ref))
                            .font(.caption.monospaced())
                            .foregroundStyle(Theme.textSecondary)
                        Spacer()
                        Button("Approve") { Task { await approve(ref) } }
                            .disabled(busy)
                        Button(role: .destructive) {
                            Task { await reject(ref) }
                        } label: { Text("Reject") }
                            .disabled(busy)
                    }
                }
            }

            if let error {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        }
        .padding()
        .background(Theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .task { await onAppearLoad() }
        .onDisappear { teardown() }
    }

    private func onAppearLoad() async {
        do {
            let id = try Identity.fromSeed(seed: accountIdentitySeed)
            let pg = try CoPublisher.openPublisherGroup(identity: id, channelIdHex: channelId)
            identity = id
            publisherGroup = pg
        } catch {
            self.error = "Could not open the channel group: \(error.localizedDescription)"
            return
        }
        startPollLoop()
    }

    private func teardown() {
        pollTask?.cancel()
        pollTask = nil
    }

    private func startPollLoop() {
        pollTask?.cancel()
        pollTask = Task { @MainActor in
            while !Task.isCancelled {
                await refreshPending()
                try? await Task.sleep(nanoseconds: 5 * 1_000_000_000)
            }
        }
    }

    private func refreshPending() async {
        guard let identity, let publisherGroup else { return }
        do {
            _ = try await CoPublisher.pollJournal(
                ds: ds, identity: identity, publisherGroup: publisherGroup)
            pending = try CoPublisher.pending(publisherGroup: publisherGroup)
        } catch {
            // Best-effort — keep the last good pending list.
        }
    }

    private func createLink() async {
        guard let identity, let publisherGroup else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            link = try CoPublisher.createCoPublisherLink(
                identity: identity, publisherGroup: publisherGroup)
        } catch {
            self.error = "Could not create the link: \(error.localizedDescription)"
        }
    }

    private func approve(_ ref: Data) async {
        guard let identity, let publisherGroup else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            try await CoPublisher.approve(
                ds: ds, channelsClient: client, identity: identity,
                publisherGroup: publisherGroup, proposalRef: ref, channelIdHex: channelId)
            await refreshPending()
        } catch {
            self.error = "Approve failed: \(error.localizedDescription)"
        }
    }

    private func reject(_ ref: Data) async {
        guard let publisherGroup else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            try CoPublisher.reject(publisherGroup: publisherGroup, proposalRef: ref)
            await refreshPending()
        } catch {
            self.error = "Reject failed: \(error.localizedDescription)"
        }
    }

    private static func shortHex(_ data: Data) -> String {
        data.prefix(8).map { String(format: "%02x", $0) }.joined() + "…"
    }
}

// ============================================================================
// JoinCoPublisherScreen — joiner-side route
// ============================================================================

/// ADR-0028 OQ3 — joiner side: paste a `/g/` co-publisher link, request to
/// join, poll until the owner approves, then mint local publisher keys + open
/// the channel. Mirror of the web `onJoinAsCoPublisher` flow
/// (`apps/web/src/routes/Channels.tsx`).
struct JoinCoPublisherScreen: View {
    @ObservedObject var state: ChannelsUiState
    let accountIdentitySeed: Data
    @Environment(\.dismiss) private var dismiss

    @State private var link: String = ""
    @State private var localName: String = ""
    @State private var busy = false
    @State private var status: String?
    @State private var errorMessage: String?

    private let ds = DsClient()

    /// Number of poll attempts (every 3 s) before giving up — mirrors the web
    /// 60 x 3 s window.
    private static let pollAttempts = 60
    private static let pollIntervalNs: UInt64 = 3 * 1_000_000_000

    var body: some View {
        ZStack {
            Theme.background.ignoresSafeArea()
            Form {
                Section {
                    TextField("Paste a /g/ co-publisher link", text: $link, axis: .vertical)
                        .lineLimit(2...4)
                        .listRowBackground(Theme.surface)
                        .foregroundStyle(Theme.textPrimary)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                    TextField("Local name (optional)", text: $localName)
                        .listRowBackground(Theme.surface)
                        .foregroundStyle(Theme.textPrimary)
                } footer: {
                    Text(
                        "The channel owner shares a co-publisher link. After they approve your request, this device can publish to the channel."
                    )
                    .foregroundStyle(Theme.textSecondary)
                }
                if let status {
                    Section {
                        Text(status)
                            .foregroundStyle(Theme.textSecondary)
                            .listRowBackground(Theme.surface)
                    }
                }
                if let errorMessage {
                    Section {
                        Text(errorMessage)
                            .foregroundStyle(.white)
                            .listRowBackground(Theme.destructive)
                    }
                }
            }
            .scrollContentBackground(.hidden)
        }
        .navigationTitle("Join as co-publisher")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(busy ? "Requesting…" : "Request") {
                    Task { await join() }
                }
                .disabled(busy || link.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
    }

    private func join() async {
        if busy { return }
        let trimmed = link.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return }
        busy = true
        errorMessage = nil
        status = "Requesting…"
        defer {
            busy = false
            status = nil
        }
        do {
            let identity = try Identity.fromSeed(seed: accountIdentitySeed)
            let request = try await CoPublisher.requestToCoPublish(
                ds: ds, identity: identity, link: trimmed)
            status = "Waiting for the channel owner to approve…"
            var keys: ChannelPublisherKeys?
            for _ in 0..<Self.pollAttempts where keys == nil {
                keys = try await CoPublisher.pollForCoPublishWelcome(
                    ds: ds,
                    identity: identity,
                    groupId: request.groupId,
                    joinerKey: request.joinerKey,
                    channelIdHex: request.channelIdHex
                )
                if keys == nil {
                    try? await Task.sleep(nanoseconds: Self.pollIntervalNs)
                }
            }
            guard let keys else {
                errorMessage =
                    "Timed out waiting for approval — try again once the owner has approved."
                return
            }
            let name = localName.trimmingCharacters(in: .whitespaces)
            state.addChannel(.init(
                id: request.channelIdHex,
                localName: name.isEmpty ? "Co-published channel" : name,
                localDescription: "",
                addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000)
            ))
            state.setKeys(channelId: request.channelIdHex, keys: keys)
            dismiss()
        } catch {
            errorMessage = "Co-publish failed: \(error.localizedDescription)"
        }
    }
}

// ============================================================================
// RotationPanelView — scheduled content-key rotation (ADR-0010 / C-54)
//   Embedded inside ChannelDetailScreen when the local device holds
//   publisher keys for this channel. Starts the `RotationCron`
//   heartbeat while the screen is visible + offers a manual
//   "Rotate now".
// ============================================================================

/// ADR-0010 §"PublisherGroup" — the *scheduled* half of content-key
/// rotation (the membership-change half advances automatically on every
/// MLS epoch bump). The pure `channelShouldRotate` policy (default 24 h /
/// 1000 posts) decides *when*; [`RotationCron`] does *how* — advance the
/// PublisherGroup epoch, derive the new seed, and reseal it to every
/// subscriber via the atomic `POST /v1/channels/:id/rotate` fanout (the
/// same audited path `CoPublisher.approve` reuses after adding a
/// co-publisher).
///
/// **Fail-closed (invariant #4).** A rotation that fails part-way never
/// exposes plaintext or hands out a bad key: the reseal batch is atomic
/// server-side, a single seal failure aborts the whole batch, and a
/// failed fanout simply leaves subscribers unable to decrypt the *next*
/// post until the next successful reseal — an availability effect, never
/// a downgrade. The `RotationCron` already carries these semantics; this
/// view only drives its lifecycle + surfaces the error.
struct RotationPanelView: View {
    let channelId: String
    let client: ChannelsClient
    /// Canonical AccountIdentity (phrase) seed — used to open the
    /// channel's persistent PublisherGroup (same handle
    /// `CoPublishersPanelView` opens).
    let accountIdentitySeed: Data

    @StateObject private var model = RotationPanelModel()

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Key rotation")
                .font(.headline)
                .foregroundStyle(Theme.textPrimary)
            Text(
                "Rotates the channel content key on a schedule (every 24 h "
                    + "or 1000 posts) and reseals it to every subscriber — "
                    + "backward secrecy for past keys."
            )
            .font(.caption)
            .foregroundStyle(Theme.textSecondary)

            Text(model.status)
                .font(.caption.monospaced())
                .foregroundStyle(Theme.textSecondary)

            Button {
                Task { await model.rotateNow() }
            } label: {
                Text(model.busy ? "Rotating…" : "Rotate now")
            }
            .buttonStyle(.borderedProminent)
            .tint(Theme.accent)
            .controlSize(.small)
            .disabled(model.busy || !model.ready)

            if let error = model.error {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .task {
            await model.load(
                channelId: channelId,
                client: client,
                accountIdentitySeed: accountIdentitySeed
            )
        }
        .onDisappear { model.teardown() }
    }
}

/// Reactive owner of the rotation lifecycle for one channel-detail
/// screen. A reference type (`ObservableObject`) so the long-lived
/// `RotationCron` callbacks can capture it safely — a SwiftUI `View`
/// struct value cannot be mutated from an escaping closure stored on the
/// cron actor.
@MainActor
final class RotationPanelModel: ObservableObject {
    /// Human-readable last-rotated / cadence line.
    @Published var status: String = "Loading rotation state…"
    /// Surfaced when opening the group or a manual rotation fails.
    @Published var error: String?
    /// `true` while a manual "Rotate now" is in flight.
    @Published var busy = false
    /// `true` once the PublisherGroup + cron are ready (enables the
    /// button).
    @Published var ready = false

    private var channelIdHex = ""
    private var client = ChannelsClient()
    private var identity: Identity?
    private var publisherGroup: PublisherGroup?
    private var cron: RotationCron?
    private var cronTask: Task<Void, Never>?

    /// Open the channel's PublisherGroup once, then (re)start the
    /// periodic heartbeat. Idempotent across screen re-appears: the
    /// group is opened only on the first call; the heartbeat is
    /// restarted every time (it is cancelled in [`teardown`] on
    /// disappear).
    func load(
        channelId: String,
        client: ChannelsClient,
        accountIdentitySeed: Data
    ) async {
        self.channelIdHex = channelId
        self.client = client
        if cron == nil {
            do {
                let id = try Identity.fromSeed(seed: accountIdentitySeed)
                let pg = try CoPublisher.openPublisherGroup(
                    identity: id, channelIdHex: channelId)
                identity = id
                publisherGroup = pg
                cron = makeCron(identity: id, publisherGroup: pg)
                ready = true
            } catch {
                self.error =
                    "Could not open the channel group: \(error.localizedDescription)"
                return
            }
        }
        refreshStatus()
        startPeriodic()
    }

    /// User-initiated rotation — forces a rotation regardless of the
    /// schedule (e.g. after suspecting a subscriber compromise). Drives
    /// the same `runOnce` path the heartbeat uses; a synthetic
    /// "never rotated" state + a 1 ms interval satisfies
    /// `channelShouldRotate` so the gate always fires, then `runOnce`
    /// stamps + persists a fresh post-rotation state via the context's
    /// `saveState`.
    func rotateNow() async {
        guard let cron else { return }
        if busy { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            _ = try await cron.runOnce(
                policy: ChannelRotationPolicy(intervalMs: 1, maxPosts: 1),
                state: ChannelRotationState(lastRotationMs: 0, postsSinceRotation: 0)
            )
            // The `onRotated` callback refreshes the status line on
            // success; refresh here too so a no-subscriber rotation
            // (still bumps the epoch) is reflected immediately.
            refreshStatus()
        } catch {
            self.error = "Rotate failed: \(error.localizedDescription)"
        }
    }

    /// Cancel the heartbeat when the screen disappears. The
    /// PublisherGroup + cron handles are retained so a re-appear
    /// restarts the heartbeat without re-opening the group.
    func teardown() {
        cronTask?.cancel()
        cronTask = nil
    }

    // MARK: - internals

    private func makeCron(
        identity: Identity,
        publisherGroup: PublisherGroup
    ) -> RotationCron {
        let channelId = self.channelIdHex
        let context = RotationCron.Context(
            publisherGroup: publisherGroup,
            identity: identity,
            channelIdHex: channelId,
            channelsClient: client,
            saveState: { newState in
                ChannelRotationStateStore.save(channelId: channelId, state: newState)
            },
            onRotated: { [weak self] epoch in
                Task { @MainActor in self?.handleRotated(epoch) }
            }
        )
        return RotationCron(context: context)
    }

    private func startPeriodic() {
        guard let cron else { return }
        cronTask?.cancel()
        let channelId = self.channelIdHex
        cronTask = cron.start(
            policy: channelRotationPolicyDefault(),
            loadState: {
                ChannelRotationStateStore.loadOrSeed(
                    channelId: channelId,
                    nowMs: UInt64(Date().timeIntervalSince1970 * 1000)
                )
            },
            onError: { [weak self] err in
                Task { @MainActor in self?.handleError(err) }
            }
        )
    }

    private func handleRotated(_ epoch: UInt32) {
        error = nil
        refreshStatus()
    }

    private func handleError(_ err: Error) {
        // Heartbeat errors are transient (the next tick retries from the
        // new local state); surface the latest so a persistent failure
        // is visible without spamming.
        error = "Rotation check failed: \(err.localizedDescription)"
    }

    private func refreshStatus() {
        let cadence = "auto every 24 h or 1000 posts"
        guard let state = ChannelRotationStateStore.load(channelId: channelIdHex),
            state.lastRotationMs > 0
        else {
            status = "Not rotated yet · \(cadence)"
            return
        }
        let ago = Self.relativeAgo(sinceMs: state.lastRotationMs)
        status = "Last rotated \(ago) · \(state.postsSinceRotation) posts since · \(cadence)"
    }

    private static func relativeAgo(sinceMs: UInt64) -> String {
        let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
        let deltaS = nowMs >= sinceMs ? (nowMs - sinceMs) / 1000 : 0
        switch deltaS {
        case 0..<60: return "just now"
        case 60..<3600: return "\(deltaS / 60)m ago"
        case 3600..<86_400: return "\(deltaS / 3600)h ago"
        default: return "\(deltaS / 86_400)d ago"
        }
    }
}

/// Per-channel persistence for the rotation state (`lastRotationMs` +
/// `postsSinceRotation`). Non-secret operational metadata — no key
/// material — so plain `UserDefaults` is appropriate (invariant #7
/// governs key material only; the channel list itself is likewise not
/// SQLCipher-backed on iOS today).
enum ChannelRotationStateStore {
    private static let prefix = "lakko.channels.rotation-state.v1."

    private struct Persisted: Codable {
        let lastRotationMs: UInt64
        let postsSinceRotation: UInt32
    }

    static func load(channelId: String) -> ChannelRotationState? {
        guard
            let data = UserDefaults.standard.data(forKey: prefix + channelId),
            let p = try? JSONDecoder().decode(Persisted.self, from: data)
        else { return nil }
        return ChannelRotationState(
            lastRotationMs: p.lastRotationMs,
            postsSinceRotation: p.postsSinceRotation
        )
    }

    static func save(channelId: String, state: ChannelRotationState) {
        let p = Persisted(
            lastRotationMs: state.lastRotationMs,
            postsSinceRotation: state.postsSinceRotation
        )
        guard let data = try? JSONEncoder().encode(p) else { return }
        UserDefaults.standard.set(data, forKey: prefix + channelId)
    }

    /// Return the persisted state, or seed a genesis
    /// `after_rotation(now)` snapshot when absent — so the first
    /// scheduled check on an existing channel does not fire an
    /// immediate rotation (it lands ~one interval later instead).
    static func loadOrSeed(channelId: String, nowMs: UInt64) -> ChannelRotationState {
        if let existing = load(channelId: channelId) { return existing }
        let seeded = channelRotationStateAfterRotation(nowMs: nowMs)
        save(channelId: channelId, state: seeded)
        return seeded
    }
}
