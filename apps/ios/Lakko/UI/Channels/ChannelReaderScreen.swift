// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI
import UIKit

/// The channel as a reader sees it (UX spec section 5.4). A device that has
/// not subscribed yet sees a Subscribe button; subscribing generates (or
/// restores) an X25519 keypair, registers it on the channel, polls for a
/// seed envelope, opens it, then renders the posts with the recovered seed.
/// Pushed by ``ChannelDetailScreen`` for any channel this device does not
/// publish to.
struct ChannelReaderScreen: View {
    let channelId: String
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    /// The handle the channel was found by from the search field, so a
    /// subscribe can file the channel in the local list under it. Empty when
    /// the channel is already listed.
    var foundHandle: String = ""
    /// `docs/ux/feed.md` section 5, "Channel posts" — the newest readable post
    /// was on screen while the app was active: it is read, here and in the
    /// feed (the channel's count goes to zero).
    var onPostRead: ((_ cursor: UInt64) -> Void)?

    private enum Bootstrap: Equatable {
        /// No subscriber keys on this device: the Subscribe button shows.
        case notSubscribed
        case registering, polling, active, errorState
    }
    /// Per-post render state after the decrypt-by-epoch pass (ADR-0031).
    /// An absent entry means "still decoding".
    private enum PostDecodeState {
        /// Recovered plaintext.
        case text(String)
        /// Present epoch seed but AEAD/verify failed — wrong seed, tampered
        /// ciphertext, or another publisher's signing key.
        case unreadable
        /// The post's epoch is not in the retained `epoch -> seed` map (older
        /// than the retention window, or a gap). Never force-decrypted with a
        /// wrong seed (invariant #4) — surfaced as a placeholder + skipped.
        case olderKeyWindow
        /// ADR-0051 §4 — a metadata record (kept, never shown) or a malformed
        /// record (dropped). Not a post row at all.
        case hidden
    }
    @State private var bootstrap: Bootstrap = .notSubscribed
    @State private var bootstrapError: String?
    @State private var posts: [PostView] = []
    @State private var decodedByCursor: [UInt64: PostDecodeState] = [:]
    @State private var pollTask: Task<Void, Never>?
    /// Cursors of the post cards on screen.
    @State private var onScreen: Set<UInt64> = []

    private var sortedPosts: [PostView] {
        posts
            .filter { post in
                if case .hidden? = decodedByCursor[post.cursor] { return false }
                return true
            }
            .sorted { $0.cursor < $1.cursor }
    }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    description
                    statusBanner
                    postsSection
                }
                .padding(16)
            }
            .background(Color.lakkoBackground)
            .refreshable {
                if bootstrap == .active { await loadAndDecodePosts() }
            }
            .onChange(of: sortedPosts.last?.cursor) { _, cursor in
                guard let cursor else { return }
                withAnimation(.easeOut(duration: 0.15)) {
                    proxy.scrollTo(cursor, anchor: .bottom)
                }
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if bootstrap == .notSubscribed {
                subscribeBar
            }
        }
        .task {
            // Already subscribed on this device: resume without asking again.
            // The subscriber keypair outlives the channel list (it is a
            // Keychain item, the list a per-account file), so a channel
            // reached through Find with keys already on the device must be
            // listed here too, or it reads fine and never appears on the tab.
            if state.subscriberKeysByChannel[channelId] != nil {
                ensureListed()
                await runBootstrap()
            }
        }
        .onDisappear { pollTask?.cancel() }
        // A card coming on screen, or a post decrypting while its card is
        // already there.
        .onChange(of: onScreen) { _, _ in markNewestRead() }
        .onChange(of: newestReadableCursor) { _, _ in markNewestRead() }
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.didBecomeActiveNotification)) { _ in
            markNewestRead()
        }
    }

    /// ADR-0051 §5 — the header card: the shared name (else local, else the
    /// handle), `@handle`, the description when a record carried one, and a
    /// one-line status that reads the bootstrap while access is being set up.
    @ViewBuilder
    private var description: some View {
        ChannelHeaderCard(
            channelId: channelId,
            channel: state.channels.first(where: { $0.id == channelId }),
            foundHandle: foundHandle,
            isOwner: false,
            status: headerStatus
        )
        .padding(.bottom, 6)
    }

    private var headerStatus: String? {
        switch bootstrap {
        case .notSubscribed: return "Not subscribed"
        case .registering: return "Subscribing..."
        case .polling: return "Waiting to be let in..."
        case .active: return nil
        case .errorState: return "Not subscribed"
        }
    }

    /// One sentence on the sunken surface while access is being set up;
    /// nothing once posts are readable.
    @ViewBuilder
    private var statusBanner: some View {
        let line: (text: String, colour: Color, symbol: String)? = {
            switch bootstrap {
            case .notSubscribed, .active:
                return nil
            case .registering:
                return ("Subscribing...", Color.lakkoTextMuted, "arrow.triangle.2.circlepath")
            case .polling:
                return (
                    "Subscribed. Posts appear once the publisher shares access.",
                    Color.lakkoTextMuted, "clock"
                )
            case .errorState:
                return (bootstrapError ?? "Could not subscribe. Try again.",
                        Color.lakkoDanger, "exclamationmark.circle")
            }
        }()
        if let line {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: line.symbol)
                    .foregroundStyle(line.colour)
                Text(line.text)
                    .font(.subheadline)
                    .foregroundStyle(line.colour)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
                if bootstrap == .errorState {
                    Button("Retry") { Task { await runBootstrap() } }
                        .font(.subheadline)
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.lakkoSurfaceSunken)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
            .accessibilityElement(children: .combine)
        }
    }

    @ViewBuilder
    private var postsSection: some View {
        switch bootstrap {
        case .notSubscribed:
            Text("Posts appear here after you subscribe.")
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
                .frame(maxWidth: .infinity)
                .padding(.top, 40)
        case .active:
            if sortedPosts.isEmpty {
                Text("No posts yet.")
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(maxWidth: .infinity)
                    .padding(.top, 40)
            } else {
                ForEach(sortedPosts, id: \.cursor) { post in
                    ChannelPostCard(content: content(for: post), serverTsMs: post.serverTsMs)
                        .id(post.cursor)
                        .onAppear { onScreen.insert(post.cursor) }
                        .onDisappear { onScreen.remove(post.cursor) }
                }
            }
        case .registering, .polling, .errorState:
            EmptyView()
        }
    }

    /// The newest post this view could read (its text decrypted).
    private var newestReadableCursor: UInt64? {
        sortedPosts.last { post in
            if case .text? = decodedByCursor[post.cursor] { return true }
            return false
        }?.cursor
    }

    /// The newest readable post, when its card is on screen and the app is in
    /// front (as the feed's dwell requires), is read.
    private func markNewestRead() {
        guard let cursor = newestReadableCursor, onScreen.contains(cursor),
              UIApplication.shared.applicationState == .active
        else { return }
        onPostRead?(cursor)
    }

    private func content(for post: PostView) -> ChannelPostCard.Content {
        switch decodedByCursor[post.cursor] {
        case .text(let text): return .text(text)
        case .olderKeyWindow: return .unavailable(UnavailablePostReason.beforeAccess)
        case .unreadable: return .unavailable(UnavailablePostReason.notVerified)
        // Filtered out of `sortedPosts`; unreachable, kept exhaustive.
        case .hidden: return .decoding
        case .none: return .decoding
        }
    }

    /// The Subscribe button, in the composer's slot.
    private var subscribeBar: some View {
        VStack(spacing: 6) {
            Button {
                Task { await subscribe() }
            } label: {
                Text("Subscribe")
                    .font(.headline)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 12)
            }
            .buttonStyle(.borderedProminent)
            .tint(Color.lakkoAccent)
            Text("The publisher shares access with each subscriber; posts are readable once that lands.")
                .font(.caption)
                .foregroundStyle(Color.lakkoTextMuted)
                .multilineTextAlignment(.center)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity)
        .background(Color.lakkoSurface)
        .overlay(alignment: .top) {
            Rectangle().fill(Color.lakkoBorder).frame(height: 1)
        }
    }

    // MARK: - Actions

    /// Subscribe from the button: list the channel locally under the handle
    /// it was found by (when it is not listed yet), then run the same
    /// bootstrap a returning subscriber runs.
    private func subscribe() async {
        ensureListed()
        await runBootstrap()
    }

    /// Add this channel to the persisted list when it is not there yet.
    private func ensureListed() {
        guard !state.channels.contains(where: { $0.id == channelId }) else { return }
        state.addChannel(.init(
            id: channelId,
            localName: foundHandle,
            localDescription: "",
            addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000),
            handle: foundHandle
        ))
    }

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
        // A registration spends a `channel-subscribe` token (ADR-0059
        // section 4): a channel this device already follows with a key it
        // already holds is not registered again on every open; the re-sync
        // below re-registers when the current epoch's seed is missing.
        let alreadyFollowing = state.channels.contains(where: { $0.id == channelId })
            && state.subscriberKeysByChannel[channelId] != nil
        let kp = ensureKeypair()
        let pubHex = ChannelsComposer.bytesToHex(kp.publicKey)
        if !alreadyFollowing {
            do {
                _ = try await client.subscribe(
                    channelIdHex: channelId,
                    subscriberPubkeyHex: pubHex
                )
            } catch let token as AnonTokenUnavailable {
                NSLog("Channels: subscribe token: %@", String(describing: token))
                // ADR-0059 section 4 — a used-up `channel-subscribe` allowance
                // has its own line (UX spec section 7).
                if case .quotaExhausted = token {
                    bootstrapError = AnonTokenCopy.subscribeAllowanceUsed
                } else {
                    bootstrapError = token.errorDescription
                }
                bootstrap = .errorState
                return
            } catch {
                NSLog("Channels: subscribe failed: %@", String(describing: error))
                bootstrapError = "Could not subscribe. Try again."
                bootstrap = .errorState
                return
            }
        }

        // Catch-up (ADR-0031): pull the whole retained envelope set and open
        // each `(epoch, envelope)` into the per-channel `epoch -> seed` map,
        // so posts made under a rotation this device missed are still
        // decryptable.
        await catchUp(pubHex: pubHex)

        let seeds = state.subscribedSeeds(channelId: channelId)
        let currentEpoch = state.epochByChannel[channelId]
        let haveCurrent = currentEpoch.map { seeds[$0] != nil } ?? false

        // Re-sync: if the CURRENT epoch's seed is missing (offline past the
        // retention window, or nothing fanned out yet), a re-subscribe is
        // idempotent and makes the publisher reseal this device on its next
        // fanout. Beyond-retention posts stay the "older key window"
        // placeholder. At most one registration per open: a device that
        // just registered above has nothing to re-register, and a second
        // `channel-subscribe` token would be spent for nothing.
        if !haveCurrent && alreadyFollowing {
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
            // Keep polling while the screen is up: for the current epoch's
            // seed until it lands, and for posts published while reading.
            startPollingTask(pubHex: pubHex)
        }
    }

    /// Runs until the screen disappears. Before the current epoch's seed
    /// lands: catch up every 3 s (the publisher's fan-out is what we wait
    /// for). After it: reload the posts every 5 s so a post published while
    /// the reader is open appears, and re-check the epoch about every 30 s so
    /// a rotation is noticed without a fetch of the envelope set per tick.
    private func startPollingTask(pubHex: String) {
        pollTask?.cancel()
        pollTask = Task {
            var ticks = 0
            while !Task.isCancelled {
                let haveCurrent: Bool = {
                    guard let cur = state.epochByChannel[channelId] else { return false }
                    return state.subscribedSeeds(channelId: channelId)[cur] != nil
                }()
                try? await Task.sleep(
                    nanoseconds: haveCurrent ? Self.readerRefreshNanos : Self.seedWaitNanos)
                if Task.isCancelled { return }
                ticks += 1
                if !haveCurrent || ticks % Self.epochRecheckEveryTicks == 0 {
                    await catchUp(pubHex: pubHex)
                }
                if !state.subscribedSeeds(channelId: channelId).isEmpty {
                    bootstrap = .active
                    await loadAndDecodePosts()
                }
            }
        }
    }

    private static let seedWaitNanos: UInt64 = 3_000_000_000
    private static let readerRefreshNanos: UInt64 = 5_000_000_000
    private static let epochRecheckEveryTicks = 6

    /// ADR-0031 catch-up: refresh the current epoch, pull the whole retained
    /// envelope set, and open each into the per-channel `epoch -> seed` map.
    /// Best-effort — a transient network failure leaves the existing map
    /// intact so the poll loop retries. The singular `fetchEnvelope` is kept
    /// as a fallback to guarantee the current seed lands even if the plural
    /// set momentarily lags a just-committed reseal.
    private func catchUp(pubHex: String) async {
        guard let kp = state.subscriberKeysByChannel[channelId] else { return }

        // Current epoch first — the map key the re-sync check + the singular
        // fallback need.
        if let cur = try? await client.fetchEpoch(channelIdHex: channelId) {
            state.setEpoch(channelId: channelId, epoch: cur)
        }

        // Plural: the retained `[(epoch, envelope)]` set, each with its
        // concrete epoch. Open any epoch not already held.
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

        // Singular fallback: guarantee the current-epoch seed is present (the
        // plural set may lag a just-committed reseal).
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
            // Keep the shared cache current so the channel row shows the
            // last post's time.
            state.setPosts(channelId: channelId, posts: page.posts)
            var next: [UInt64: PostDecodeState] = [:]
            for post in page.posts {
                // Decode the clear header to learn the post's epoch, then pick
                // the seed matching THAT epoch (ADR-0031).
                guard let epoch = try? ChannelsComposer.postEpoch(
                    postWireB64Url: post.payloadB64
                ) else {
                    next[post.cursor] = .unreadable  // malformed wire
                    continue
                }
                guard let seed = seeds[epoch] else {
                    // No seed for this epoch — older than retention or a gap.
                    // Skip; never force-decrypt with a wrong seed (invariant #4).
                    next[post.cursor] = .olderKeyWindow
                    continue
                }
                do {
                    let plain = try ChannelsComposer.verifyAndDecrypt(
                        seed: seed,
                        channelIdHex: channelId,
                        postWireB64Url: post.payloadB64
                    )
                    // ADR-0051 §4 — a metadata record titles the channel
                    // (newest wins) and is never a post; a domain match that
                    // failed a bound is dropped, never rendered as text.
                    switch ChannelMetaRecord.classify(plain) {
                    case .text(let text):
                        next[post.cursor] = .text(text)
                    case .meta(let meta):
                        state.recordSharedMeta(channelId: channelId, meta: meta)
                        next[post.cursor] = .hidden
                    case .malformed:
                        next[post.cursor] = .hidden
                    }
                } catch {
                    next[post.cursor] = .unreadable
                }
            }
            decodedByCursor = next
        } catch {
            // Swallow — the status banner already shows the bootstrap state;
            // the posts section simply shows "no posts yet" until the next
            // refresh succeeds.
        }
    }
}
