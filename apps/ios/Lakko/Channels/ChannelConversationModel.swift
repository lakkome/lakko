// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore
import SwiftUI
import UIKit

/// ADR-0087 sections 12 to 13a — one channel as a conversation: its posts
/// (the chat envelope, opened under the seed of each post's epoch), the
/// publish path of the owner and publishers, the reader's subscribe, and the
/// reactions and comments under each post.
///
/// Owned by ``ChannelConversationScreen``; everything that must keep running
/// without the screen (rotation, letting subscribers in, the co-publisher
/// poll) lives in the app-wide loops instead, never here.
@MainActor
final class ChannelConversationModel: ObservableObject {

    // MARK: - Inputs

    let channelId: String
    let state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data
    private(set) weak var sync: DeviceGroupSyncRepository?
    /// ADR-0099 step 5 — the channels moderators removed.
    let removals: ChannelRemovals
    private var removalWatch: AnyCancellable?

    /// The handle and name a search card or a link carried, while the channel
    /// is not in the local list yet.
    var foundHandle: String = ""
    var foundName: String = ""
    /// A public channel's read key carried by a search record or an
    /// invitation, kept on the extras once subscribed.
    var foundReadKey: Data?

    // MARK: - Output

    enum Access: Equatable {
        /// This device publishes to the channel.
        case publisher
        /// Subscribed and reading.
        case reading
        /// Not subscribed: `Subscribe` shows.
        case notSubscribed
        case subscribing
        /// Section 7.2 — subscribed through a private invitation, waiting to
        /// be let in.
        case waiting
        /// The subscribe failed; the line says why.
        case failed(UiText)
    }

    @Published private(set) var access: Access = .notSubscribed
    @Published private(set) var timeline = ChannelTimeline.Folded(items: [], earlierUnavailable: false)
    @Published private(set) var loading = true
    /// The publish composer's state.
    @Published var draft: String = ""
    @Published private(set) var sending = false
    @Published private(set) var sendError: UiText?
    /// `docs/ux/media-messages.md` section 11 — the composer tray of this
    /// channel, kept while the app runs. Channel files live 45 days idle.
    let tray: ComposerTrayStore
    private var trayChanges: AnyCancellable?

    /// Section 13.1 — resolved reaction counts per post, as the service last
    /// answered them.
    @Published private(set) var countsByPost: [UInt64: [FfiReactionCount]] = [:]
    /// The service's comment count of each post (ADR-0087 phase B).
    @Published private(set) var commentCountByPost: [UInt64: UInt64] = [:]
    /// Reaction counts under each comment, by comment cursor.
    @Published private(set) var countsByComment: [UInt64: [FfiReactionCount]] = [:]
    /// Section 13.2 — the comment threads loaded so far.
    @Published private(set) var threads: [UInt64: CommentThread] = [:]
    /// UX 9.5 — a publisher removed this device: the name for the dialog.
    @Published private(set) var removedName: String?

    init(
        channelId: String, state: ChannelsUiState, client: ChannelsClient, accountIdentitySeed: Data,
        removals: ChannelRemovals = .shared
    ) {
        self.channelId = channelId
        self.state = state
        self.client = client
        self.accountIdentitySeed = accountIdentitySeed
        self.removals = removals
        self.access = Self.initialAccess(channelId: channelId, state: state)
        self.tray = ComposerTrays.shared.store(for: "channel:\(channelId)", idleDays: Self.fileIdleDays)
        trayChanges = tray.objectWillChange.sink { [weak self] _ in self?.objectWillChange.send() }
        // Every view of the model redraws when the channel is held as removed
        // or answers again.
        removalWatch = removals.objectWillChange.sink { [weak self] _ in self?.objectWillChange.send() }
    }

    func attach(sync: DeviceGroupSyncRepository) { self.sync = sync }

    private static func initialAccess(channelId: String, state: ChannelsUiState) -> Access {
        if state.keysByChannel[channelId] != nil { return .publisher }
        if state.subscriberKeysByChannel[channelId] != nil {
            return state.extras(channelId).waitingForAdmission ? .waiting : .reading
        }
        return .notSubscribed
    }

    var isPublisher: Bool { state.keysByChannel[channelId] != nil }

    /// ADR-0099 step 5 — set while the service answers `channel-removed` for
    /// this channel: the line the conversation shows instead of its write
    /// controls.
    var removal: ChannelRemovals.Status? { removals.status(channelId) }

    /// Whether composing, commenting and reacting are offered: not while the
    /// channel is held as removed (UX `reporting-and-moderation.md` 3).
    var writable: Bool { removal == nil }

    /// UX second-device 4a — the account owns or co-publishes this channel
    /// and this device holds no PublisherGroup for it: the line in place of
    /// the composer; `nil` otherwise.
    var readerDeviceRole: ChannelReaderDeviceRole? { state.readerDeviceRole(channelId) }

    var channelIdBytes: Data? { ChannelsClient.hexToData(channelId) }

    private var currentIdentity: Identity? { try? Identity.fromSeed(seed: accountIdentitySeed) }

    /// UX group-and-channel-privacy.md section 12 — the header line carries
    /// the subscriber count where this device can know it: the owner's and a
    /// publisher's (the count route answers only them, section 15.1), as the
    /// info screen reads it. Nothing changes on a failure.
    func refreshSubscriberCount() async {
        guard isPublisher, let identity = currentIdentity,
              let exact = try? await client.subscriberCount(channelIdHex: channelId, accountIdentity: identity)
        else { return }
        state.setServiceSubscriberCount(channelId: channelId, serviceCount: exact)
    }

    /// The identity a subscriber's per-write keys and comment author key
    /// derive from: the lineage one, so a rotation does not lose the
    /// reactions and comments already written (ADR-0087 section 13a).
    private var writerIdentity: Identity? {
        ChannelLineage.identity(accountIdB64: AcceptedContactsStore.boundAccountIdB64, currentSeed: accountIdentitySeed)
            ?? currentIdentity
    }

    // MARK: - Seeds

    /// The seed of every epoch this device holds for the channel.
    private func seeds() -> [UInt32: Data] {
        // A publisher's composer seed opens the one epoch it serves; every
        // other epoch reads from the seeds kept per epoch.
        state.readSeeds(channelId: channelId)
    }

    /// The seed `epoch` was sealed under, fetching a public channel's public
    /// seed envelope when this device holds none yet.
    private func seed(for epoch: UInt32) async -> Data? {
        if let held = seeds()[epoch] { return held }
        guard let readKey = state.extras(channelId).readKey ?? foundReadKey,
              let id = channelIdBytes,
              let wire = try? await client.fetchPublicSeed(channelIdHex: channelId, epoch: epoch),
              let opened = try? openPublicSeedEnvelope(readKey: readKey, channelId: id, expectedEpoch: epoch, wire: wire)
        else { return nil }
        state.setSubscribedSeed(channelId: channelId, epoch: epoch, seed: opened)
        return opened
    }

    // MARK: - Loading

    /// Most post pages fetched per refresh (100 a page; the service keeps 30
    /// days).
    static let maxPages = 10

    private var rawPosts: [PostView] = []
    private var decoded: [UInt64: ChannelDecodedPost] = [:]

    /// Fetch the posts, open them and fold the timeline. `quiet` says nothing
    /// when it fails; the next refresh tries again.
    /// `epochKnown`: a catch-up just read the epoch, so it is not read again
    /// (one read per resource per pass).
    func refresh(quiet: Bool = false, epochKnown: Bool = false) async {
        guard access == .publisher || access == .reading || state.extras(channelId).readKey != nil else {
            loading = false
            return
        }
        if !quiet { loading = true }
        defer { loading = false }
        do {
            if !epochKnown {
                let epoch = try await client.fetchEpoch(channelIdHex: channelId)
                state.setEpoch(channelId: channelId, epoch: epoch)
            }
            var after: UInt64 = 0
            var all: [PostView] = []
            for _ in 0..<Self.maxPages {
                let page = try await client.fetchPosts(channelIdHex: channelId, after: after, limit: 100)
                all.append(contentsOf: page.posts)
                guard !page.posts.isEmpty, page.nextAfter > after else { break }
                after = page.nextAfter
            }
            rawPosts = all
            state.setPosts(channelId: channelId, posts: all)
            await decodeAll()
            await refreshCounts()
        } catch {
            if Cancellation.endedByCancellation(error) { return }
            if ChannelsApiError.isChannelDeleted(error) {
                endDeleted()
                return
            }
            if ChannelsApiError.isChannelRemoved(error) {
                // ADR-0099 step 5 — nothing ends: the posts this device holds
                // stay readable.
                await showHeldPosts()
                return
            }
            NSLog("[channels] posts did not load")
        }
    }

    /// ADR-0087 section 12 (2026-11-06) — while the channel is open, read
    /// only the posts after the newest one held, a page at a time to the end;
    /// nothing else is read unless something arrived. Returns whether
    /// anything did. A failure keeps what is shown.
    @discardableResult
    func poll() async -> Bool {
        guard access == .publisher || access == .reading || state.extras(channelId).readKey != nil else {
            return false
        }
        var after = rawPosts.map(\.cursor).max() ?? 0
        var fresh: [PostView] = []
        do {
            for _ in 0..<Self.maxPages {
                let page = try await client.fetchPosts(channelIdHex: channelId, after: after, limit: 100)
                fresh.append(contentsOf: page.posts.filter { $0.cursor > after })
                guard !page.posts.isEmpty, page.nextAfter > after else { break }
                after = page.nextAfter
            }
        } catch {
            if ChannelsApiError.isChannelDeleted(error) { endDeleted() }
            return false
        }
        guard !fresh.isEmpty else {
            // UX 15.2.1 — earlier seeds arrived (a new publisher's hand-over,
            // a catch-up): the posts are opened again, so "Earlier posts
            // aren't available." goes once every earlier post opens.
            if Set(seeds().keys) != decodedSeedEpochs { await decodeAll() }
            return false
        }
        let held = Set(rawPosts.map(\.cursor))
        rawPosts.append(contentsOf: fresh.filter { !held.contains($0.cursor) })
        rawPosts.sort { $0.cursor < $1.cursor }
        state.setPosts(channelId: channelId, posts: rawPosts)
        await decodeAll()
        await refreshCounts()
        return true
    }

    /// The posts this device already holds for a channel the service no
    /// longer serves: those loaded this session and those in the post log
    /// the Feed keeps (with their wires), by cursor.
    nonisolated static func heldPosts(loaded: [PostView], logged: [PersistedChannelPost]) -> [PostView] {
        var byCursor: [UInt64: PostView] = [:]
        for post in logged {
            guard let wire = post.payloadB64 else { continue }
            byCursor[post.cursor] = PostView(
                cursor: post.cursor, serverTsMs: UInt64(clamping: post.serverTsMs), payloadB64: wire)
        }
        for post in loaded { byCursor[post.cursor] = post }
        return byCursor.values.sorted { $0.cursor < $1.cursor }
    }

    private func showHeldPosts() async {
        let logged = AcceptedContactsStore.boundAccountIdB64
            .flatMap { ChannelPostLogStore().get(accountIdB64: $0)?.channels[channelId] }?.posts ?? []
        let held = Self.heldPosts(loaded: rawPosts + (state.postsByChannel[channelId] ?? []), logged: logged)
        guard held.map(\.cursor) != rawPosts.map(\.cursor) else { return }
        rawPosts = held
        await decodeAll()
    }

    /// The epochs this device held a seed for at the last decode.
    private var decodedSeedEpochs: Set<UInt32> = []

    private func decodeAll() async {
        guard let id = channelIdBytes else { return }
        _ = id
        decodedSeedEpochs = Set(seeds().keys)
        var next: [UInt64: ChannelDecodedPost] = [:]
        for post in rawPosts {
            if let held = decoded[post.cursor], case .body = held.state {
                next[post.cursor] = held
                continue
            }
            guard let epoch = try? ChannelsComposer.postEpoch(postWireB64Url: post.payloadB64) else {
                next[post.cursor] = ChannelDecodedPost(
                    cursor: post.cursor, serverTsMs: post.serverTsMs, epoch: 0, state: .unreadable)
                continue
            }
            guard let seed = await seed(for: epoch) else {
                next[post.cursor] = ChannelDecodedPost(
                    cursor: post.cursor, serverTsMs: post.serverTsMs, epoch: epoch, state: .noKey)
                continue
            }
            let state: ChannelDecodedPost.State
            if let plain = try? ChannelsComposer.verifyAndDecrypt(
                seed: seed, channelIdHex: channelId, postWireB64Url: post.payloadB64) {
                switch ChannelMetaRecord.classify(plain) {
                case .text(let envelope): state = .body(ChannelPostBody.read(envelope))
                case .meta(let meta):
                    self.state.recordSharedMeta(channelId: channelId, meta: meta)
                    state = .hidden
                case .banList(let updatedAtMs, let authors):
                    self.state.recordBanList(channelId: channelId, updatedAtMs: updatedAtMs, authors: authors)
                    state = .hidden
                case .malformed: state = .hidden
                }
            } else {
                state = .unreadable
            }
            next[post.cursor] = ChannelDecodedPost(
                cursor: post.cursor, serverTsMs: post.serverTsMs, epoch: epoch, state: state)
        }
        decoded = next
        let before = timeline.items
        timeline = ChannelTimeline.fold(Array(next.values))
        noteDeletedPosts(before: before, after: timeline)
    }

    // MARK: - `docs/ux/message-actions.md` section 9 — a deleted post leaves

    /// A deleted post in its 2-second going: the item as it was drawn, and
    /// where it is in its going.
    struct HeldPost: Equatable {
        let item: ChannelTimelineItem
        var phase: DeletedRows.Phase
    }

    /// In memory only, never persisted.
    @Published private(set) var heldPosts: [UInt64: HeldPost] = [:]
    private var heldPostTasks: [UInt64: Task<Void, Never>] = [:]

    /// Set by the screen: the channel is the open screen.
    var screenVisible = false {
        didSet { if !screenVisible { endDeletedHolds() } }
    }

    /// The platform's Reduce Motion setting, as the screen last read it.
    var reduceMotion = false

    /// The items the timeline lays out: the posts, plus the deleted ones
    /// still in their going, in cursor order.
    var drawnItems: [ChannelTimelineItem] {
        guard !heldPosts.isEmpty else { return timeline.items }
        return (timeline.items + heldPosts.values.map(\.item)).sorted { $0.cursor < $1.cursor }
    }

    private func noteDeletedPosts(before: [ChannelTimelineItem], after: ChannelTimeline.Folded) {
        let drawnBefore = Set(before.map(\.cursor))
        let newly = drawnBefore.intersection(after.deleted)
        guard !newly.isEmpty else { return }
        let inFront = screenVisible && UIApplication.shared.applicationState == .active
        let entering = DeletedRows.enteringHold(
            drawnBefore: drawnBefore, deletedNow: newly,
            alreadyHeld: Set(heldPosts.keys), inFront: inFront)
        for item in before where entering.contains(item.cursor) {
            heldPosts[item.cursor] = HeldPost(item: item, phase: .holding)
            heldPostTasks[item.cursor]?.cancel()
            let cursor = item.cursor
            heldPostTasks[cursor] = Task { @MainActor [weak self] in
                try? await Task.sleep(nanoseconds: UInt64(DeletedRows.holdSeconds * 1_000_000_000))
                guard let self, !Task.isCancelled, self.heldPosts[cursor] != nil else { return }
                if !self.reduceMotion {
                    withAnimation(.easeInOut(duration: DeletedRows.collapseSeconds)) {
                        self.heldPosts[cursor]?.phase = .collapsing
                    }
                    try? await Task.sleep(nanoseconds: UInt64(DeletedRows.collapseSeconds * 1_000_000_000))
                    guard !Task.isCancelled, self.heldPosts[cursor] != nil else { return }
                }
                self.heldPosts[cursor] = nil
                self.heldPostTasks[cursor] = nil
            }
        }
    }

    /// A comment in its going, keyed by post and comment cursor.
    struct HeldCommentKey: Hashable {
        let post: UInt64
        let comment: UInt64
    }

    /// Section 9 — the comments this device deleted while the channel was
    /// in front of the person, in their going. In memory only.
    @Published private(set) var heldComments: [HeldCommentKey: DeletedRows.Phase] = [:]
    private var heldCommentTasks: [HeldCommentKey: Task<Void, Never>] = [:]

    /// Where a comment is in its going, `nil` when it is not held.
    func commentPhase(post: UInt64, comment: UInt64) -> DeletedRows.Phase? {
        heldComments[HeldCommentKey(post: post, comment: comment)]
    }

    /// A comment's delete landed: seen going when the channel is in front,
    /// else gone at once.
    private func commentDeleted(post: UInt64, comment: UInt64) {
        let key = HeldCommentKey(post: post, comment: comment)
        let inFront = screenVisible && UIApplication.shared.applicationState == .active
        guard inFront, heldComments[key] == nil else {
            if heldComments[key] == nil { threads[post]?.comments.removeAll { $0.cursor == comment } }
            return
        }
        heldComments[key] = .holding
        heldCommentTasks[key] = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(DeletedRows.holdSeconds * 1_000_000_000))
            guard let self, !Task.isCancelled, self.heldComments[key] != nil else { return }
            if !self.reduceMotion {
                withAnimation(.easeInOut(duration: DeletedRows.collapseSeconds)) {
                    self.heldComments[key] = .collapsing
                }
                try? await Task.sleep(nanoseconds: UInt64(DeletedRows.collapseSeconds * 1_000_000_000))
                guard !Task.isCancelled, self.heldComments[key] != nil else { return }
            }
            self.threads[post]?.comments.removeAll { $0.cursor == comment }
            self.heldComments[key] = nil
            self.heldCommentTasks[key] = nil
        }
    }

    /// The channel stopped being in front of the person: every post and
    /// comment in its going is gone when they come back.
    func endDeletedHolds() {
        for task in heldPostTasks.values { task.cancel() }
        heldPostTasks = [:]
        if !heldPosts.isEmpty { heldPosts = [:] }
        for task in heldCommentTasks.values { task.cancel() }
        heldCommentTasks = [:]
        if !heldComments.isEmpty {
            for key in heldComments.keys {
                threads[key.post]?.comments.removeAll { $0.cursor == key.comment }
            }
            heldComments = [:]
        }
    }

    /// ADR-0099 — the evidence item of a post as it was received (its wire),
    /// `nil` when this device no longer holds it.
    func postEvidence(cursor: UInt64) -> FfiEvidenceItem? {
        guard let id = channelIdBytes,
              let raw = rawPosts.first(where: { $0.cursor == cursor }),
              let wire = Data(base64UrlNoPadListing: raw.payloadB64) else { return nil }
        return .channelPost(channelId: id, post: wire)
    }

    /// ADR-0099 / UX 2.1 — a report on the channel from a post's menu (that
    /// post included) or from the channel's info (nothing included). Every
    /// post may be picked; `Unsubscribe` is the follow-up for a subscriber.
    func reportRequest(title: String, includingCursor: UInt64?, subscriber: Bool) -> ReportRequest? {
        guard let id = channelIdBytes, id.count == 16 else { return nil }
        let candidates = timeline.items.reversed().map {
            ReportCandidate(messageId: Int64(clamping: $0.cursor), authorName: nil, text: $0.text, createdAt: "")
        }
        return ReportRequest(
            target: .channel(channelId: id), targetName: title,
            conversationKind: .channel,
            initialMessageIds: includingCursor.map { [Int64(clamping: $0)] } ?? [],
            candidates: candidates,
            followUp: subscriber ? .unsubscribe : nil,
            candidateEvidence: { [weak self] messageId in
                guard messageId >= 0 else { return nil }
                return self?.postEvidence(cursor: UInt64(messageId))
            })
    }

    /// The epoch seed a readable post was opened under.
    func epochSeed(of item: ChannelTimelineItem) -> Data? { seeds()[item.epoch] }

    // MARK: - Subscribing (sections 5.1, 6, 7.2)

    /// Run when the screen opens: a reader resumes, a card's `Subscribe`
    /// subscribes.
    func start(subscribeOnOpen: Bool) async {
        access = Self.initialAccess(channelId: channelId, state: state)
        switch access {
        case .publisher:
            await refresh()
        case .reading, .waiting:
            ensureListed()
            let epochKnown = await catchUp()
            await refresh(epochKnown: epochKnown)
        case .notSubscribed, .subscribing, .failed:
            if let invite = PendingChannelInvites.take(channelId) {
                await subscribePrivate(
                    inviteSeed: invite.inviteSeed, admissionPublicKey: invite.admissionPublicKey,
                    admissionVersion: invite.admissionVersion)
            } else if subscribeOnOpen {
                await subscribe()
            } else {
                // A public channel opened before subscribing can be read
                // through its read key, if this device holds one.
                loading = false
            }
        }
    }

    /// `Subscribe`. A public channel reads at once (section 6); any other
    /// waits for its seed envelope (section 7.2).
    func subscribe() async {
        ensureListed()
        state.onSubscribedHere?(channelId)
        access = .subscribing
        // One key per account and channel: a key another device of the
        // account subscribed with is reused (the subscription register).
        let keys = state.subscriberKeysForSubscribe(channelId)
        if let readKey = foundReadKey {
            state.updateExtras(channelId) { $0.readKey = readKey }
        }
        do {
            _ = try await client.subscribe(
                channelIdHex: channelId, subscriberPubkeyHex: ChannelsComposer.bytesToHex(keys.publicKey))
        } catch let token as AnonTokenUnavailable {
            access = .failed(AnonTokenUnavailable.isQuotaExhausted(token)
                ? .key("tokens.subscribe_allowance_used") : .key("channels.subscribe.error.network"))
            return
        } catch {
            if Cancellation.endedByCancellation(error) { return }
            access = .failed(ChannelsErrorCopy.text(
                for: error, prefixKey: "channels.subscribe.error.prefix",
                fallbackKey: "channels.subscribe.error.generic"))
            return
        }
        await catchUp()
        access = .reading
        await refresh()
    }

    /// Section 7.2 — subscribe through a private invitation: the invite
    /// proof and the admission ticket, then wait to be let in.
    func subscribePrivate(inviteSeed: Data, admissionPublicKey: Data, admissionVersion: UInt16) async {
        ensureListed()
        state.onSubscribedHere?(channelId)
        access = .subscribing
        guard let id = channelIdBytes else { return }
        // One key per account and channel: a key another device of the
        // account subscribed with is reused (the subscription register).
        let keys = state.subscriberKeysForSubscribe(channelId)
        do {
            let signer = try ChannelInviteSigner.fromInviteSeed(inviteSeed: inviteSeed)
            let ticket = try signer.sealTicket(
                admissionPublicKey: admissionPublicKey, admissionVersion: admissionVersion,
                channelId: id, subscriberPk: keys.publicKey)
            let proof = try signer.signProof(channelId: id, subscriberPk: keys.publicKey)
            _ = try await client.subscribePrivate(
                channelIdHex: channelId, subscriberPubkeyHex: ChannelsComposer.bytesToHex(keys.publicKey),
                inviteProof: proof, sealedTicket: ticket)
        } catch let token as AnonTokenUnavailable {
            access = .failed(AnonTokenUnavailable.isQuotaExhausted(token)
                ? .key("tokens.subscribe_allowance_used") : .key("channels.subscribe.error.network"))
            return
        } catch let error as ChannelsApiError where error.status == 401 {
            access = .failed(.key("invite.link.invalid"))
            return
        } catch {
            if Cancellation.endedByCancellation(error) { return }
            access = .failed(ChannelsErrorCopy.text(
                for: error, prefixKey: "channels.subscribe.error.prefix",
                fallbackKey: "channels.subscribe.error.generic"))
            return
        }
        state.updateExtras(channelId) { $0.waitingForAdmission = true }
        let epochKnown = await catchUp()
        access = state.extras(channelId).waitingForAdmission ? .waiting : .reading
        await refresh(epochKnown: epochKnown)
    }

    /// ADR-0031 catch-up: open every retained seed envelope this device does
    /// not hold yet. A private channel's first one ends the wait.
    /// Returns whether it read the channel's epoch.
    @discardableResult
    func catchUp() async -> Bool {
        guard let kp = state.subscriberKeysByChannel[channelId] else { return false }
        let pubHex = ChannelsComposer.bytesToHex(kp.publicKey)
        var epochRead = false
        if let cur = try? await client.fetchEpoch(channelIdHex: channelId) {
            state.setEpoch(channelId: channelId, epoch: cur)
            epochRead = true
        }
        // Section 7.3 — a removed subscriber's envelope read answers `410
        // subscriber-removed`: the channel ends on this device (UX 9.5). One
        // read of the retained set carries it (no separate single read).
        var retained: [(epoch: UInt32, envelope: Data)]?
        do {
            retained = try await client.fetchEnvelopes(channelIdHex: channelId, subscriberPubkeyHex: pubHex)
        } catch let error as ChannelsApiError where error.code == "subscriber-removed" {
            endRemoved()
            return epochRead
        } catch where ChannelsApiError.isChannelDeleted(error) {
            endDeleted()
            return epochRead
        } catch {}
        if let set = retained {
            for (epoch, wire) in set where state.subscribedSeeds(channelId: channelId)[epoch] == nil {
                if let seed = try? ChannelsComposer.openSubscriberSeedEnvelope(
                    keys: kp, channelIdHex: channelId, epochId: epoch, wire: wire) {
                    state.setSubscribedSeed(channelId: channelId, epoch: epoch, seed: seed)
                }
            }
        }
        if !state.subscribedSeeds(channelId: channelId).isEmpty, state.extras(channelId).waitingForAdmission {
            state.updateExtras(channelId) { $0.waitingForAdmission = false }
            if access == .waiting { access = .reading }
        }
        await ChannelMemberCards.uploadIfNeeded(state: state, channelId: channelId)
        return epochRead
    }

    /// UX 9.5 — the row and its posts go, on every device of the person; the
    /// open channel shows the dialog (``removedName``).
    private func endRemoved() {
        let name = state.channels.first { $0.id == channelId }?.title ?? foundName
        removedName = name
        ChannelMemberCards.forgetUploads(channelId: channelId)
        state.clearSubscriberKeys(channelId)
        state.removeChannel(channelId)
        state.onRemovedHere?(channelId, nil)
    }

    /// The owner deleted the channel: it ends here as a removal does, with
    /// the open channel's dialog.
    func endDeleted() {
        removedName = state.channels.first { $0.id == channelId }?.title ?? foundName
        state.endDeleted(channelId)
    }

    private func ensureListed() {
        guard !state.channels.contains(where: { $0.id == channelId }) else { return }
        state.addChannel(.init(
            id: channelId,
            localName: foundName.isEmpty ? foundHandle : foundName,
            localDescription: "",
            addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000),
            handle: foundHandle))
    }

    // MARK: - Publishing (section 13)

    /// Publish the draft and every item in the tray as one post (section 11).
    func sendDraft() async {
        guard writable else { return }
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard tray.tray.canSend(hasText: !text.isEmpty), !sending else { return }
        let attachments = tray.tray.refs
        do {
            let envelope = try composeEnvelope(text: text, attachments: attachments)
            if await publish(envelope) {
                draft = ""
                _ = tray.takeForSend()
            }
        } catch {
            sendError = .key("channels.publish.error.generic")
        }
    }

    /// Publish one attachment as its own post (a voice message, a video note,
    /// a GIF), the media controller's port.
    func publishAttachment(_ ref: AttachmentRef) async throws {
        let envelope = try composeEnvelope(text: "", attachments: [ref])
        guard await publish(envelope) else { throw ChannelPublishError.failed }
    }

    /// ADR-0023 ops on a post, its server cursor the target.
    func edit(_ item: ChannelTimelineItem, text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let bytes = try? composeEditEnvelope(
            target: Int64(clamping: item.cursor), newText: trimmed) else { return }
        _ = await publish(bytes)
    }

    func delete(_ item: ChannelTimelineItem) async {
        guard let bytes = try? composeDeleteEnvelope(target: Int64(clamping: item.cursor)) else { return }
        if await publish(bytes) {
            // Section 13 — the post's own files go with it.
            BlobLifecycle.shared.delete(MessageBlobs.ids(of: item.attachments))
        }
    }

    @discardableResult
    private func publish(_ plaintext: Data) async -> Bool {
        guard let keys = state.keysByChannel[channelId], let identity = currentIdentity else {
            sendError = .key("channels.publish.error.not_publisher")
            return false
        }
        sending = true
        sendError = nil
        defer { sending = false }
        do {
            // The seed of an epoch the channel service has reached, never the
            // composer's seed under an epoch it does not serve yet (a
            // co-publisher that applied the owner's commit before the owner's
            // reseal landed). None held: no post now; a retry posts once the
            // reseal lands.
            guard let sealing = state.composerSeed(channelId: channelId) else {
                throw ChannelPublisherSeedUnavailable()
            }
            let counter = try state.nextCounter(channelId: channelId)
            let wire = try ChannelsComposer.compose(
                keys: ChannelPublisherKeys(seed: sealing.seed, signingKeySeed: keys.signingKeySeed),
                channelIdHex: channelId, epochId: sealing.epoch, messageCounter: counter, plaintext: plaintext)
            _ = try await client.publishPost(channelIdHex: channelId, payload: wire, accountIdentity: identity)
            ChannelPostSync.requestSync()
            await refresh(quiet: true)
            return true
        } catch {
            if Cancellation.endedByCancellation(error) { return false }
            NSLog("[channels] publish failed")
            sendError = ChannelsAPIPublishRefusal.isRefusal(error)
                ? .key("channels.post.not_publisher")
                : ChannelsErrorCopy.text(
                    for: error, prefixKey: "channels.publish.error.prefix",
                    fallbackKey: "channels.publish.error.generic")
            return false
        }
    }

    func clearSendError() { sendError = nil }

    /// ADR-0087 section 13 — a channel file's idle life (chosen there).
    static let fileIdleDays: UInt8 = 45

    // MARK: - Reactions (section 13.1, reactions spec 8.3 and 11)

    /// The emoji the reaction counts are resolved against: the bundled set
    /// and the recent ones.
    private lazy var candidateEmoji: [String] = {
        var out = RecentReactions.load(accountIdB64: AcceptedContactsStore.boundAccountIdB64)
        var seen = Set(out)
        for emoji in EmojiDataset.bundled.byEmoji.keys.sorted() where seen.insert(emoji).inserted { out.append(emoji) }
        return out
    }()

    /// The reactions this account holds on a post.
    func ownReactions(_ item: ChannelTimelineItem) -> [String] {
        guard let id = channelIdBytes, let sync else { return [] }
        return sync.channelReactions(channelId: id, postCursor: item.cursor)
    }

    /// One batched read of the counts of the posts in the timeline (50 a
    /// call), or of the posts at `only` (the newest 50 of them). A failure
    /// keeps the counts shown.
    func refreshCounts(only: Set<UInt64>? = nil) async {
        var items = timeline.items
        if let only { items = Array(items.filter { only.contains($0.cursor) }.suffix(50)) }
        guard !items.isEmpty else { return }
        var next = countsByPost
        var comments = commentCountByPost
        defer { commentCountByPost = comments }
        for chunk in stride(from: 0, to: items.count, by: 50).map({ Array(items[$0..<min($0 + 50, items.count)]) }) {
            guard let answer = try? await client.reactionCounts(
                channelIdHex: channelId, posts: chunk.map(\.cursor)) else { return }
            for post in answer {
                if let count = post.comments { comments[post.cursor] = count }
                // ADR-0092 section 4.3 — a poll post's votes, per option.
                if let item = chunk.first(where: { $0.cursor == post.cursor }), item.contentEnvelopeB64 != nil {
                    pollCounts[post.cursor] = pollCountsOf(item, voters: post.voters ?? 0, labels: post.votes)
                }
                guard let item = chunk.first(where: { $0.cursor == post.cursor }),
                      let key = reactionKey(item) else { continue }
                next[post.cursor] = key.resolveCounts(
                    postCursor: post.cursor, candidates: candidateEmoji, counts: post.labels)
            }
        }
        countsByPost = next
    }

    // MARK: - ADR-0092 section 4: polls in channels

    /// Votes per option of each poll post, read with the reaction counts.
    @Published private(set) var pollCounts: [UInt64: ChannelPollCounts] = [:]
    /// A vote on its way, or one that did not go out.
    @Published private(set) var pollSendState: [UInt64: BallotSendState] = [:]

    private func pollOf(_ item: ChannelTimelineItem) -> SignedPoll? {
        guard let b64 = item.contentEnvelopeB64,
              case .poll(let poll)? = ContentModel.content(PersistedContent(envelopeB64: b64)) else { return nil }
        return poll
    }

    private func pollCountsOf(_ item: ChannelTimelineItem, voters: UInt64, labels: [FfiReactionLabelCount])
        -> ChannelPollCounts? {
        guard let poll = pollOf(item), let key = reactionKey(item),
              let options = try? channelPollOptionLabels(
                key: key, postCursor: item.cursor, optionCount: UInt8(clamping: poll.body.options.count)),
              let tally = try? channelPollTallyFromCounts(optionLabels: options, labelCounts: labels, voters: voters)
        else { return nil }
        return ChannelPollCounts(
            counts: tally.counts.map { UInt32(clamping: $0) }, voters: UInt32(clamping: tally.voters), frozen: false)
    }

    private var pollConversation: FolderConversationRef? {
        channelIdBytes.map { FolderConversationRef(kind: .channel, id: $0) }
    }

    /// What the poll card of `item` shows: the frozen tally of a close, else
    /// the service's counts; the own choice from the synced register.
    func pollView(_ item: ChannelTimelineItem) -> PollView? {
        guard let poll = pollOf(item) else { return nil }
        let own = pollConversation.flatMap {
            sync?.pollChoice(conversation: $0, pollMessageId: Int64(clamping: item.cursor))
        }.map { [UInt8]($0.choices) }
        let counts = item.frozenTally.map {
            ChannelPollCounts(counts: $0.counts.map { UInt32(clamping: $0) }, voters: UInt32(clamping: $0.voters), frozen: true)
        } ?? pollCounts[item.cursor]
        return PollView.channel(
            poll: poll, counts: counts, closedByOp: item.closedByOp, own: own, nowMs: ChatRepository.nowMs())
    }

    /// Vote (or retract with no choices): the credential is fetched in the
    /// background the first time in a period; the own choice is recorded in
    /// the synced register once the service took the vote.
    func votePoll(_ item: ChannelTimelineItem, choices: [UInt8]) async {
        guard let poll = pollOf(item), let id = channelIdBytes, let key = reactionKey(item),
              let writer = writerIdentity, let account = AcceptedContactsStore.boundAccountIdB64 else { return }
        pollSendState[item.cursor] = .sending
        do {
            let options = try channelPollOptionLabels(
                key: key, postCursor: item.cursor, optionCount: UInt8(clamping: poll.body.options.count))
            let labels = choices.compactMap { Int($0) < options.count ? options[Int($0)] : nil }
            let credential = try await ChannelPollCredentials.shared.current(writer: writer, accountIdB64: account)
            try await client.sendPollVote(
                channelIdHex: channelId, channelId: id, postCursor: item.cursor, labels: labels, credential: credential)
            if let ref = pollConversation {
                _ = await sync?.setPollChoice(
                    conversation: ref, pollMessageId: Int64(clamping: item.cursor), choices: choices, setVersion: 0)
            }
            pollSendState[item.cursor] = nil
            await refreshCounts()
        } catch {
            NSLog("ADR-0092: channel vote not sent")
            pollSendState[item.cursor] = .failed
        }
    }

    /// A publisher's `Close poll`: freeze the tally this app reads now into
    /// the post (ADR-0092 section 4.3).
    func closePoll(_ item: ChannelTimelineItem) async {
        await refreshCounts()
        let counts = pollCounts[item.cursor]
        let tally = counts.flatMap {
            try? encodeChannelPollTally(tally: ChannelPollTally(
                voters: UInt64($0.voters), counts: $0.counts.map { UInt64($0) }))
        }
        guard let bytes = try? ContentFFI.composeClose(target: Int64(clamping: item.cursor), tally: tally) else { return }
        _ = await publish(bytes)
    }

    /// A publisher's tick, added item or edit on a checklist post (ADR-0092
    /// section 5: signed post ops, publishers only).
    func publishContentOp(_ bytes: Data) async {
        guard isPublisher else { return }
        _ = await publish(bytes)
    }

    /// A publisher's poll, checklist, location, contact card or article post.
    func publishContent(_ draft: ContentDraft, attachments: [AttachmentRef] = []) async {
        guard let content = draft.unsigned(),
              let bytes = try? composeContentEnvelope(
                content: content, attachments: attachments, parts: ContentModel.emptyParts) else {
            sendError = .key("channels.publish.error.generic")
            return
        }
        _ = await publish(bytes)
    }

    private func reactionKey(_ item: ChannelTimelineItem) -> ChannelReactionKey? {
        guard let seed = epochSeed(of: item), let id = channelIdBytes else { return nil }
        return try? ChannelReactionKey.derive(epochSeed: seed, channelId: id, epochId: item.epoch)
    }

    /// The chips under a post (reactions spec section 7): the service's
    /// counts, the own ones highlighted, an own reaction not counted yet
    /// counted once.
    func chips(_ item: ChannelTimelineItem) -> FfiChipRow {
        let own = Set(ownReactions(item))
        var sources: [FfiChipSource] = []
        var seen: Set<String> = []
        for count in countsByPost[item.cursor] ?? [] {
            let mine = own.contains(count.emoji)
            seen.insert(count.emoji)
            sources.append(FfiChipSource(
                emoji: count.emoji, count: UInt32(clamping: max(count.count, mine ? 1 : 0)),
                firstAt: 0, mine: mine, reactors: []))
        }
        for emoji in own where !seen.contains(emoji) {
            sources.append(FfiChipSource(emoji: emoji, count: 1, firstAt: 0, mine: true, reactors: []))
        }
        guard !sources.isEmpty,
              let row = try? reactionChips(sources: sources, conversation: .channel, allowed: allowed)
        else { return FfiChipRow(chips: [], overflowKinds: 0) }
        return row
    }

    var allowed: FfiAllowedReactions { state.extras(channelId).allowed }

    /// The reaction row of the message menu.
    func reactionRow(_ item: ChannelTimelineItem) -> ReactionRowModel {
        if case .off = allowed { return .empty }
        let own = ownReactions(item)
        let kinds = (countsByPost[item.cursor] ?? []).map(\.emoji)
        return ReactionRowModel(
            emoji: quickReactionRow(
                quick: QuickReactionSetting.shared.chosen,
                recent: RecentReactions.load(accountIdB64: AcceptedContactsStore.boundAccountIdB64),
                allowed: allowed, messageKinds: kinds),
            own: own,
            capReached: own.count >= ReactionRules.maxPerPerson,
            showsExpand: rowHasExpand(allowed: allowed))
    }

    /// Press `emoji` on a post: add it, or take it off. Shown at once; an add
    /// that finds no token waits for the next refill (section 13.1).
    func react(_ item: ChannelTimelineItem, emoji: String) async {
        guard writable else { return }
        guard let id = channelIdBytes, let sync, let writer = writerIdentity, let key = reactionKey(item) else { return }
        let own = ownReactions(item)
        let verdict = reactionTapVerdict(
            emoji: emoji, own: own, allowed: allowed,
            messageKinds: (countsByPost[item.cursor] ?? []).map(\.emoji))
        let adding: Bool
        switch verdict {
        case .add: adding = true
        case .remove: adding = false
        case .notAllowed, .capReached, .kindLimit: return
        }
        if adding { RecentReactions.push(emoji, accountIdB64: AcceptedContactsStore.boundAccountIdB64) }
        await sync.setChannelReaction(
            channelId: id, postCursor: item.cursor, emoji: emoji, reacted: adding, postAtMs: item.serverTsMs)
        objectWillChange.send()
        do {
            let label = try key.label(postCursor: item.cursor, emoji: emoji)
            let write = try ChannelReactionWriteKey(
                accountIdentity: writer, channelId: id, postCursor: item.cursor, label: label)
            if adding {
                try await client.addReaction(channelIdHex: channelId, postCursor: item.cursor, writeKey: write, label: label)
            } else {
                try await client.deleteReaction(channelIdHex: channelId, postCursor: item.cursor, writeKey: write, label: label)
            }
            PendingChannelReactionStore.remove(channelId: channelId, cursor: item.cursor, emoji: emoji)
        } catch {
            // Nothing is shown: the reaction stays drawn and an add goes out
            // with the next refill (section 13.1), also after a restart.
            if adding {
                PendingChannelReactionStore.add(.init(
                    channelIdHex: channelId, cursor: item.cursor, emoji: emoji, epoch: item.epoch))
            }
        }
        await refreshCounts()
    }

    /// Send again the adds that found no token (kept across restarts).
    func flushPendingReactions() async {
        guard let writer = writerIdentity else { return }
        await ChannelBackgroundChecks.flushReactions(
            channelId: channelId, seeds: seeds(), client: client, writer: writer)
    }

    // MARK: - Reactions on comments (section 13a, phase B)

    /// The reactions this account holds on a comment: the synced
    /// own-reactions register, the same on every device of the account.
    func ownCommentReactions(_ comment: Comment, of item: ChannelTimelineItem) -> [String] {
        guard let id = channelIdBytes, let sync else { return [] }
        return sync.channelCommentReactions(channelId: id, postCursor: item.cursor, commentCursor: comment.cursor)
    }

    private func commentReactionKey(_ item: ChannelTimelineItem) -> ChannelCommentReactionKey? {
        guard let seed = epochSeed(of: item), let id = channelIdBytes else { return nil }
        return try? ChannelCommentReactionKey.derive(epochSeed: seed, channelId: id, epochId: item.epoch)
    }

    /// One read of the counts of the comments loaded under a post.
    func refreshCommentCounts(_ item: ChannelTimelineItem) async {
        let cursors = (threads[item.cursor]?.comments ?? []).map(\.cursor).filter { $0 != 0 }
        guard !cursors.isEmpty, let key = commentReactionKey(item) else { return }
        var next = countsByComment
        for chunk in stride(from: 0, to: cursors.count, by: 50).map({ Array(cursors[$0..<min($0 + 50, cursors.count)]) }) {
            guard let answer = try? await client.commentReactionCounts(
                channelIdHex: channelId, postCursor: item.cursor, comments: chunk) else { return }
            for comment in answer {
                next[comment.cursor] = key.resolveCounts(
                    postCursor: item.cursor, commentCursor: comment.cursor,
                    candidates: candidateEmoji, counts: comment.labels)
            }
        }
        countsByComment = next
    }

    /// The chips under a comment: as under a post.
    func commentChips(_ comment: Comment, of item: ChannelTimelineItem) -> FfiChipRow {
        let own = Set(ownCommentReactions(comment, of: item))
        var sources: [FfiChipSource] = []
        var seen: Set<String> = []
        for count in countsByComment[comment.cursor] ?? [] {
            let mine = own.contains(count.emoji)
            seen.insert(count.emoji)
            sources.append(FfiChipSource(
                emoji: count.emoji, count: UInt32(clamping: max(count.count, mine ? 1 : 0)),
                firstAt: 0, mine: mine, reactors: []))
        }
        for emoji in own where !seen.contains(emoji) {
            sources.append(FfiChipSource(emoji: emoji, count: 1, firstAt: 0, mine: true, reactors: []))
        }
        guard !sources.isEmpty,
              let row = try? reactionChips(sources: sources, conversation: .channel, allowed: allowed)
        else { return FfiChipRow(chips: [], overflowKinds: 0) }
        return row
    }

    /// The reaction row of a comment's menu.
    func commentReactionRow(_ comment: Comment, of item: ChannelTimelineItem) -> ReactionRowModel {
        if case .off = allowed { return .empty }
        let own = ownCommentReactions(comment, of: item)
        return ReactionRowModel(
            emoji: quickReactionRow(
                quick: QuickReactionSetting.shared.chosen,
                recent: RecentReactions.load(accountIdB64: AcceptedContactsStore.boundAccountIdB64),
                allowed: allowed, messageKinds: (countsByComment[comment.cursor] ?? []).map(\.emoji)),
            own: own,
            capReached: own.count >= ReactionRules.maxPerPerson,
            showsExpand: rowHasExpand(allowed: allowed))
    }

    /// Press `emoji` on a comment: add it, or take it off.
    func reactComment(_ comment: Comment, of item: ChannelTimelineItem, emoji: String) async {
        guard writable else { return }
        guard comment.cursor != 0, let id = channelIdBytes, let sync, let writer = writerIdentity,
              let key = commentReactionKey(item) else { return }
        let own = ownCommentReactions(comment, of: item)
        let verdict = reactionTapVerdict(
            emoji: emoji, own: own, allowed: allowed,
            messageKinds: (countsByComment[comment.cursor] ?? []).map(\.emoji))
        let adding: Bool
        switch verdict {
        case .add: adding = true
        case .remove: adding = false
        case .notAllowed, .capReached, .kindLimit: return
        }
        if adding { RecentReactions.push(emoji, accountIdB64: AcceptedContactsStore.boundAccountIdB64) }
        // The register holds the per-comment limit too: a refused write
        // sends nothing.
        guard await sync.setCommentReaction(
            channelId: id, postCursor: item.cursor, commentCursor: comment.cursor, emoji: emoji,
            reacted: adding, postAtMs: item.serverTsMs) else { return }
        objectWillChange.send()
        do {
            let label = try key.label(postCursor: item.cursor, commentCursor: comment.cursor, emoji: emoji)
            let write = try ChannelCommentReactionWriteKey(
                accountIdentity: writer, channelId: id, postCursor: item.cursor,
                commentCursor: comment.cursor, label: label)
            if adding {
                try await client.addCommentReaction(
                    channelIdHex: channelId, postCursor: item.cursor, commentCursor: comment.cursor,
                    writeKey: write, label: label)
            } else {
                try await client.deleteCommentReaction(
                    channelIdHex: channelId, postCursor: item.cursor, commentCursor: comment.cursor,
                    writeKey: write, label: label)
            }
        } catch let error as ChannelsApiError where error.status == 404 {
            // The comment is gone.
            await sync.setCommentReaction(
                channelId: id, postCursor: item.cursor, commentCursor: comment.cursor, emoji: emoji,
                reacted: false, postAtMs: item.serverTsMs)
        } catch {
            NSLog("[channels] a comment reaction did not reach the service")
        }
        await refreshCommentCounts(item)
    }

    // MARK: - Comments (section 13.2)

    struct Comment: Equatable, Identifiable {
        let cursor: UInt64
        let writerKey: Data
        let authorKey: Data
        let name: String
        let text: String
        let replyTo: UInt64?
        let serverTsMs: UInt64
        /// This account wrote it.
        let own: Bool
        /// A comment that did not send: its text, kept with Retry.
        var failed: Bool = false
        var id: UInt64 { cursor }
    }

    struct CommentThread: Equatable {
        var comments: [Comment] = []
        var nextAfter: UInt64 = 0
        /// The last page was full: more may exist.
        var mayHaveMore = false
        var expanded = false
        var loading = false
        var failedToLoad = false
        /// Comments that arrived while the thread was open (section 13.2):
        /// shown behind `{n} new comments` until it is pressed.
        var arrived: [Comment] = []
        /// The service's comment count when this device last read the
        /// thread (nil before a read that knew it): what the open channel's
        /// tick compares a new count with (``commentRefresh``).
        var countAtRead: UInt64?
        var failedSends: [Comment] = []
        /// Rows the service returned so far, opened or not: a row that did
        /// not open and a banned author's row are still rows read.
        var rowsRead = 0
        /// The first page was read.
        var firstRead = false
    }

    /// Section 13.2 — what follows the first two comments of a collapsed
    /// thread.
    enum CollapsedMore: Equatable {
        /// Nothing (yet): the first page is not read, or every comment shows.
        case none
        /// `Show {n} more comments`.
        case count(Int)
        /// `Show more comments`: rows remain and the service gave no count.
        case unknown
        /// The quiet `Comment`: there are none.
        case comment
    }

    /// The rule behind ``CollapsedMore``: the shared one in
    /// `crates/lakko-ui-rules` (`comments`), so the three clients cannot
    /// disagree. The service's count (ADR-0087 phase B) counts every stored
    /// comment, including those this reader hides (a banned author's, one
    /// that does not open), so it never stands alone: nothing is said before
    /// the first page is read, the rows read are judged by themselves, and
    /// only the rows not read yet are taken from the service.
    nonisolated static func collapsedMore(
        visible: Int, rowsRead: Int, firstRead: Bool, mayHaveMore: Bool, serverCount: UInt64?
    ) -> CollapsedMore {
        switch collapsedComments(
            visible: UInt32(clamping: visible), rowsRead: UInt32(clamping: rowsRead),
            firstPageRead: firstRead, moreRows: mayHaveMore, serverCount: serverCount)
        {
        case .nothing: return .none
        case .count(let n): return .count(Int(n))
        case .unknown: return .unknown
        case .comment: return .comment
        }
    }

    /// ``collapsedMore(visible:rowsRead:firstRead:mayHaveMore:serverCount:)``
    /// for the post at `cursor`.
    func collapsedMore(_ cursor: UInt64) -> CollapsedMore {
        let thread = threads[cursor]
        return Self.collapsedMore(
            visible: visibleComments(cursor).count, rowsRead: thread?.rowsRead ?? 0,
            firstRead: thread?.firstRead ?? false, mayHaveMore: thread?.mayHaveMore ?? false,
            serverCount: commentCountByPost[cursor])
    }

    /// The comments of a post, the banned authors' hidden.
    func visibleComments(_ cursor: UInt64) -> [Comment] {
        let banned = Set(state.extras(channelId).bannedAuthors)
        return (threads[cursor]?.comments ?? []).filter { !banned.contains($0.authorKey) }
    }

    /// Load the first page of a post's comments when it comes on screen.
    func loadComments(_ item: ChannelTimelineItem, more: Bool = false) async {
        // A removed channel keeps the comments already loaded; nothing is asked.
        guard writable else { return }
        var thread = threads[item.cursor] ?? CommentThread()
        guard !thread.loading else { return }
        if !more, !thread.comments.isEmpty { return }
        thread.loading = true
        thread.failedToLoad = false
        threads[item.cursor] = thread
        do {
            let page = try await client.listComments(
                channelIdHex: channelId, postCursor: item.cursor, after: more ? thread.nextAfter : 0)
            var opened = more ? thread.comments : []
            for row in page.rows where !opened.contains(where: { $0.cursor == row.cursor }) {
                if let comment = open(row, item: item) { opened.append(comment) }
            }
            thread.comments = opened
            thread.rowsRead = (more ? thread.rowsRead : 0) + page.rows.count
            thread.firstRead = true
            thread.nextAfter = page.nextAfter
            thread.mayHaveMore = page.rows.count >= 20
            thread.countAtRead = commentCountByPost[item.cursor]
            if !more { thread.arrived = [] }
            if more { thread.expanded = true }
        } catch {
            thread.failedToLoad = true
        }
        thread.loading = false
        threads[item.cursor] = thread
        await refreshCommentCounts(item)
    }

    private func open(_ row: ChannelsClient.CommentRow, item: ChannelTimelineItem) -> Comment? {
        guard let seed = epochSeed(of: item), let id = channelIdBytes,
              let opened = try? openChannelComment(
                epochSeed: seed, channelId: id, epochId: item.epoch, postCursor: item.cursor,
                writerKey: row.writerKey, sealed: row.sealed)
        else { return nil }
        let ownKey = writerIdentity.flatMap { try? commentAuthorKey(accountIdentity: $0, channelId: id) }
        return Comment(
            cursor: row.cursor, writerKey: row.writerKey, authorKey: opened.authorKey,
            name: opened.comment.name, text: opened.comment.text, replyTo: opened.comment.replyTo,
            serverTsMs: row.serverTsMs, own: ownKey == opened.authorKey)
    }

    func expandComments(_ item: ChannelTimelineItem) {
        threads[item.cursor, default: CommentThread()].expanded = true
        showArrived(item)
    }

    func collapseComments(_ item: ChannelTimelineItem) {
        showArrived(item)
        threads[item.cursor]?.expanded = false
    }

    /// How many comments wait behind `{n} new comments`: never one this
    /// reader hides.
    func arrivedCount(_ cursor: UInt64) -> Int {
        let banned = Set(state.extras(channelId).bannedAuthors)
        return (threads[cursor]?.arrived ?? []).filter { !banned.contains($0.authorKey) }.count
    }

    /// `{n} new comments` was pressed: they join the thread.
    func showArrived(_ item: ChannelTimelineItem) {
        guard var thread = threads[item.cursor], !thread.arrived.isEmpty else { return }
        thread.comments = Self.merged(thread.comments, thread.arrived)
        thread.arrived = []
        threads[item.cursor] = thread
    }

    /// `existing` with `incoming` added, by cursor, oldest first. Pure.
    nonisolated static func merged(_ existing: [Comment], _ incoming: [Comment]) -> [Comment] {
        var byCursor: [UInt64: Comment] = [:]
        for comment in existing { byCursor[comment.cursor] = comment }
        for comment in incoming { byCursor[comment.cursor] = comment }
        return byCursor.values.sorted { $0.cursor < $1.cursor }
    }

    // MARK: - Keeping the threads on screen current (section 13.2)

    /// What the open channel's tick does for one post's thread. The rule
    /// written three times: web `commentRefresh` in `comment-thread.ts`,
    /// Android `commentRefresh` in `CommentCount.kt`, the same table in each
    /// client's tests.
    enum CommentRefresh: Equatable {
        /// Nothing to read.
        case none
        /// Read after the last row read: into a collapsed thread (the first
        /// two and the count follow), behind `{n} new comments` in an open
        /// one.
        case newer
        /// A collapsed thread lost comments (a removal): its first page is
        /// read again in place of what it held.
        case reread
        /// An open thread lost comments: nothing is read (the open thread
        /// does not move); the count is the new base.
        case rebase
    }

    /// The rule behind ``CommentRefresh``, driven by the service's comment
    /// count the tick reads with the reaction counts; with no count (an
    /// older service) an open thread reads on every tick. Pure.
    nonisolated static func commentRefresh(
        firstRead: Bool, expanded: Bool, morePages: Bool, rowsRead: Int,
        countAtRead: UInt64?, serverCount: UInt64?
    ) -> CommentRefresh {
        guard firstRead else { return .none }
        guard let serverCount else { return expanded && !morePages ? .newer : .none }
        let seen = countAtRead ?? UInt64(max(rowsRead, 0))
        if serverCount == seen { return .none }
        if serverCount > seen { return morePages ? .none : .newer }
        return expanded ? .rebase : .reread
    }

    /// One pass over the threads of the posts at `cursors` (those on
    /// screen), after the tick read their counts: a comment written
    /// elsewhere reaches a channel on screen within a tick.
    func refreshComments(_ cursors: Set<UInt64>) async {
        guard writable else { return }
        for item in timeline.items where cursors.contains(item.cursor) {
            guard let thread = threads[item.cursor], !thread.loading else { continue }
            let count = commentCountByPost[item.cursor]
            switch Self.commentRefresh(
                firstRead: thread.firstRead, expanded: thread.expanded, morePages: thread.mayHaveMore,
                rowsRead: thread.rowsRead, countAtRead: thread.countAtRead, serverCount: count)
            {
            case .none:
                continue
            case .rebase:
                threads[item.cursor]?.countAtRead = count
            case .reread:
                threads[item.cursor]?.comments = []
                await loadComments(item)
            case .newer:
                await readNewer(item, count: count)
            }
        }
    }

    /// Read the rows after the last one read: into the thread when it is
    /// collapsed, behind `{n} new comments` when it is open. A failure keeps
    /// what is shown; the next tick tries again.
    private func readNewer(_ item: ChannelTimelineItem, count: UInt64?) async {
        guard var thread = threads[item.cursor], !thread.loading else { return }
        thread.loading = true
        threads[item.cursor] = thread
        defer { threads[item.cursor]?.loading = false }
        guard let page = try? await client.listComments(
            channelIdHex: channelId, postCursor: item.cursor, after: thread.nextAfter) else { return }
        guard var current = threads[item.cursor] else { return }
        var fresh: [Comment] = []
        let known = Set(current.comments.map(\.cursor)).union(current.arrived.map(\.cursor))
        for row in page.rows where !known.contains(row.cursor) {
            if let comment = open(row, item: item) { fresh.append(comment) }
        }
        if current.expanded {
            // This account's own comments join the thread at once.
            current.comments = Self.merged(current.comments, fresh.filter(\.own))
            current.arrived = Self.merged(current.arrived, fresh.filter { !$0.own })
        } else {
            current.comments = Self.merged(current.comments, fresh)
        }
        current.rowsRead += page.rows.count
        current.nextAfter = max(current.nextAfter, page.nextAfter)
        current.mayHaveMore = page.rows.count >= 20
        current.countAtRead = count
        threads[item.cursor] = current
        await refreshCommentCounts(item)
    }

    /// Write a comment under a post (section 13.2): text and emoji only.
    @discardableResult
    func comment(_ item: ChannelTimelineItem, text: String, replyTo: UInt64?) async -> Bool {
        guard writable else { return false }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, (try? checkCommentText(text: trimmed)) != nil,
              let id = channelIdBytes, let seed = epochSeed(of: item), let writer = writerIdentity
        else { return false }
        let nonce = newChannelCommentNonce()
        do {
            let write = try ChannelCommentWriteKey(
                accountIdentity: writer, channelId: id, postCursor: item.cursor, commentNonce: nonce)
            let own = ProfileDirectory.shared.ownProfile
            let name = own?.displayName ?? ProfileDirectory.shared.ownUsername.map { "@\($0)" } ?? ""
            let sealed = try sealChannelComment(
                accountIdentity: writer, epochSeed: seed, channelId: id, epochId: item.epoch,
                postCursor: item.cursor, writerKey: write.publicKey(),
                comment: FfiComment(name: name, avatar: own?.avatar?.still.ffi, text: trimmed, replyTo: replyTo))
            await sync?.noteChannelComment(
                channelId: id, postCursor: item.cursor, nonce: nonce, postAtMs: item.serverTsMs, present: true)
            _ = try await client.addComment(
                channelIdHex: channelId, postCursor: item.cursor, writeKey: write, sealed: sealed)
            threads[item.cursor, default: CommentThread()].failedSends.removeAll { $0.text == trimmed }
            await reloadOpenThread(item)
            return true
        } catch {
            await sync?.noteChannelComment(
                channelId: id, postCursor: item.cursor, nonce: nonce, postAtMs: item.serverTsMs, present: false)
            threads[item.cursor, default: CommentThread()].failedSends.append(Comment(
                cursor: 0, writerKey: Data(), authorKey: Data(), name: "", text: trimmed,
                replyTo: replyTo, serverTsMs: 0, own: true, failed: true))
            return false
        }
    }

    private func reloadOpenThread(_ item: ChannelTimelineItem) async {
        let expanded = threads[item.cursor]?.expanded ?? false
        threads[item.cursor]?.comments = []
        await loadComments(item)
        if expanded { threads[item.cursor]?.expanded = true }
    }

    /// Delete a comment: its author by its write key, a publisher by proof.
    func deleteComment(_ comment: Comment, of item: ChannelTimelineItem) async {
        guard let id = channelIdBytes else { return }
        do {
            if comment.own, let writer = writerIdentity, let sync,
               let nonce = sync.channelCommentNonces(channelId: id, postCursor: item.cursor).first(where: { nonce in
                   (try? ChannelCommentWriteKey(
                    accountIdentity: writer, channelId: id, postCursor: item.cursor, commentNonce: nonce))?
                       .publicKey() == comment.writerKey
               }) {
                let write = try ChannelCommentWriteKey(
                    accountIdentity: writer, channelId: id, postCursor: item.cursor, commentNonce: nonce)
                try await client.deleteOwnComment(
                    channelIdHex: channelId, postCursor: item.cursor, commentCursor: comment.cursor, writeKey: write)
                await sync.noteChannelComment(
                    channelId: id, postCursor: item.cursor, nonce: nonce, postAtMs: item.serverTsMs, present: false)
            } else if isPublisher, let identity = currentIdentity {
                try await client.deleteCommentAsPublisher(
                    channelIdHex: channelId, postCursor: item.cursor, commentCursor: comment.cursor,
                    accountIdentity: identity)
            } else {
                return
            }
            commentDeleted(post: item.cursor, comment: comment.cursor)
        } catch {
            NSLog("[channels] a comment delete failed")
        }
    }

    /// Section 13.3 — `Ban from comments`: the author's key joins the
    /// channel's ban list, published like the metadata record. In a private
    /// channel too (maintainer, 2026-11-05): a comment names no subscriber
    /// key, so nothing links it to a row to remove.
    func ban(_ comment: Comment) async {
        let held = state.extras(channelId)
        var authors = held.bannedAuthors.filter { $0 != comment.authorKey }
        authors.append(comment.authorKey)
        let maxKeys = Int(banListMaxKeys())
        if authors.count > maxKeys { authors.removeFirst(authors.count - maxKeys) }
        await publishBanList(authors)
    }

    func unban(_ authorKey: Data) async {
        await publishBanList(state.extras(channelId).bannedAuthors.filter { $0 != authorKey })
    }

    private func publishBanList(_ authors: [Data]) async {
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        let stamp = max(now, state.extras(channelId).banUpdatedAtMs + 1)
        guard let bytes = try? composeBanList(list: FfiBanList(updatedAtMs: stamp, authors: authors)) else { return }
        if await publish(bytes) {
            state.recordBanList(channelId: channelId, updatedAtMs: stamp, authors: authors)
        }
    }
}

enum ChannelPublishError: Error { case failed }

/// Whether a publish failed because this device may no longer post
/// (section 13: "Only the owner and publishers can post here.").
enum ChannelsAPIPublishRefusal {
    static func isRefusal(_ error: Error) -> Bool { ChannelsApiError.isPublisherRefusal(error) }
}
