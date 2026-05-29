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
    /// 32-byte OPAQUE export key for the current session.
    /// `NewChannelScreen.create` rebuilds a transient
    /// `Identity` from it (`Identity.fromSeed(seed: exportKey)`)
    /// to drive the MLS PublisherGroup that produces the
    /// channel content-key seed (G-27).
    ///
    /// The host (`LakkoApp` / a future Channels tab) supplies
    /// this from `AuthRepository`'s in-memory `IdentitySecrets`
    /// — same shape `ChatRepository.bootstrap` already uses.
    private let exportKey: Data

    public init(
        client: ChannelsClient = ChannelsClient(),
        exportKey: Data
    ) {
        self.client = client
        self.exportKey = exportKey
    }

    public var body: some View {
        NavigationStack {
            ChannelsListScreen(
                state: state,
                client: client,
                exportKey: exportKey
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
    /// identity. See [`ChannelsRootScreen.exportKey`].
    let exportKey: Data

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
                    NewChannelScreen(
                        state: state,
                        client: client,
                        exportKey: exportKey
                    )
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
    /// OPAQUE export key from the active session. Used to
    /// build a transient `Identity` inside [`create`] —
    /// the MLS PublisherGroup founding member.
    let exportKey: Data
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
            // G-27: build a transient `Identity` from the
            // OPAQUE export key, hand it to the composer
            // so the channel content-key seed comes from a
            // real MLS PublisherGroup exporter (instead of
            // `newChannelSeedForTest`). The identity is
            // throwaway — `freshPublisherKeys` builds a
            // throwaway `Group` from it for the duration
            // of the call.
            // `Identity` releases its handle via ARC at
            // scope-exit — UniFFI's Swift surface has no
            // explicit `close()`.
            let identity = try Identity.fromSeed(seed: exportKey)
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
            try await client.deliverEnvelope(
                channelIdHex: channelId,
                subscriberPubkeyHex: pubkeyHex,
                envelope: wire
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
    @State private var bootstrap: Bootstrap = .idle
    @State private var bootstrapError: String?
    @State private var posts: [PostView] = []
    @State private var decodedByCursor: [UInt64: String?] = [:]
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

            if let entry = decodedByCursor[post.cursor] {
                if let text = entry {
                    Text(text)
                        .foregroundStyle(Theme.textPrimary)
                } else {
                    Text("[unreadable — wrong seed or tampered ciphertext]")
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

            // If a seed is already cached in memory, jump
            // straight to active.
            if state.subscribedSeedByChannel[channelId] != nil {
                bootstrap = .active
                await loadAndDecodePosts()
                return
            }

            bootstrap = .polling
            // First-attempt fetch in case the publisher
            // has already fanned out.
            if let b64 = try await client.fetchEnvelope(
                channelIdHex: channelId,
                subscriberPubkeyHex: pubHex
            ) {
                try await openAndCacheSeed(envelopeB64: b64)
                bootstrap = .active
                await loadAndDecodePosts()
                return
            }
            startPollingTask(pubHex: pubHex)
        } catch let err as ChannelsApiError {
            bootstrapError = "\(err.code) — \(err.detail)"
            bootstrap = .errorState
        } catch {
            bootstrapError = error.localizedDescription
            bootstrap = .errorState
        }
    }

    private func startPollingTask(pubHex: String) {
        pollTask?.cancel()
        pollTask = Task { [client] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 3_000_000_000)
                if Task.isCancelled { return }
                do {
                    if let b64 = try await client.fetchEnvelope(
                        channelIdHex: channelId,
                        subscriberPubkeyHex: pubHex
                    ) {
                        try await openAndCacheSeed(envelopeB64: b64)
                        bootstrap = .active
                        await loadAndDecodePosts()
                        return
                    }
                } catch {
                    // Transient — keep polling. A permanent
                    // failure (unknown-channel etc.) would
                    // have come from the initial register.
                }
            }
        }
    }

    private func openAndCacheSeed(envelopeB64: String) async throws {
        guard let kp = state.subscriberKeysByChannel[channelId] else {
            throw ChannelsComposerError.ffi("missing subscriber keypair")
        }
        guard let wire = Data(base64URLNoPadSubscriber: envelopeB64) else {
            throw ChannelsComposerError.badBase64(envelopeB64)
        }
        let epoch = try await client.fetchEpoch(channelIdHex: channelId)
        let seed = try ChannelsComposer.openSubscriberSeedEnvelope(
            keys: kp,
            channelIdHex: channelId,
            epochId: epoch,
            wire: wire
        )
        state.setSubscribedSeed(channelId: channelId, seed: seed)
        state.setEpoch(channelId: channelId, epoch: epoch)
    }

    private func loadAndDecodePosts() async {
        guard let seed = state.subscribedSeedByChannel[channelId] else {
            return
        }
        do {
            let page = try await client.fetchPosts(
                channelIdHex: channelId,
                after: 0
            )
            posts = page.posts
            var next: [UInt64: String?] = [:]
            for post in page.posts {
                do {
                    let plain = try ChannelsComposer.verifyAndDecrypt(
                        seed: seed,
                        channelIdHex: channelId,
                        postWireB64Url: post.payloadB64
                    )
                    next[post.cursor] = String(data: plain, encoding: .utf8)
                } catch {
                    next[post.cursor] = nil as String?
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
