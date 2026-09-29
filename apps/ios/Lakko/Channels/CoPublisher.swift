// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0028 OQ3 — channel co-publisher management (owner side, iOS).
///
/// Swift mirror of `apps/web/src/channels/copublisher.ts` (owner half). A
/// channel's PublisherGroup (ADR-0010 Tier 1) is a vanilla MLS group, so adding
/// a *co-publisher* reuses the group join-ticket + external-join machinery
/// (ADR-0027/0028) verbatim — exactly what the OQ3 FFI surface on
/// `PublisherGroup` exposes.
///
/// **Reseal-after-add (load-bearing).** `approveExternalJoin` commits the Add,
/// which advances the PublisherGroup MLS epoch — so the channel content-key seed
/// (the epoch-keyed exporter output) CHANGES. Every existing subscriber holds the
/// OLD seed, so the owner MUST reseal the new seed to all subscribers (the
/// ADR-0010 / C-54 rotation fanout) or those subscribers can no longer decrypt.
/// `approve` does this via `resealSubscribers` — the same audited
/// `sealSubscriberSeedEnvelope` + atomic `POST /v1/channels/:id/rotate` fanout as
/// `RotationCron`, but WITHOUT a second `rotate()` (the Add already advanced the
/// epoch; the reseal binds to the post-approve epoch the co-publisher joins at).
///
/// The end-to-end correctness of the reseal under a real 2-publisher topology
/// needs a live 2-publisher e2e against a deployed channels backend — deferred
/// (no deployed environment), the same posture as every other live-channel gate.
public enum CoPublisher {
    /// Path prefix every `/g/` co-publisher invite URL starts with.
    public static let urlPrefix = "/g/"
    /// Default origin Lakko shares its invite links under: the web origin of
    /// the stack this build talks to (`ApiEndpoints.webOrigin`).
    public static var defaultOrigin: String { ApiEndpoints.webOrigin }

    /// Open (or create) the persistent PublisherGroup handle for a channel.
    public static func openPublisherGroup(
        identity: Identity,
        channelIdHex: String
    ) throws -> PublisherGroup {
        guard let channelId = Self.hexToData(channelIdHex) else {
            throw CoPublisherError.badChannelId(channelIdHex)
        }
        do {
            if let existing = try publisherGroupLoad(identity: identity, channelId: channelId) {
                return existing
            }
            let created = try publisherGroupOpenOrCreate(identity: identity, channelId: channelId)
            markFounded(created)
            return created
        } catch {
            // A channel minted under the retired v1 derivation refuses here and
            // cannot be repaired — surface the readable copy, not the raw string.
            throw LegacyChannelV1.mapOpenError(error)
        }
    }

    /// ADR-0057 obligation 7 — this device founded the PublisherGroup: its
    /// next claim may reset a journal the same root claimed before (a channel
    /// whose local PublisherGroup was lost and founded again).
    static func markFounded(_ group: PublisherGroup) {
        guard let groupId = try? group.groupId() else { return }
        JournalFoundingMarks.shared.mark(journalIdHex: DsClient.pseudonymHex(groupId))
    }

    /// Mint a co-publisher invite link over the PublisherGroup's MLS `group_id`.
    /// A non-member opens it, requests to join, and the owner approves (reusing
    /// the ADR-0027/0028 external-join machinery). `0` = never expires.
    ///
    /// ADR-0028 amendment 2026-09-28 — the capability is sealed under a 12-byte
    /// link secret that rides the fragment. A co-publisher link was never put
    /// on the discovery ledger, so it keeps the inline shape:
    /// `<origin>/g/<b64url sealed>#<secret>`.
    public static func createCoPublisherLink(
        identity: Identity,
        publisherGroup: PublisherGroup,
        origin: String = defaultOrigin
    ) throws -> String {
        let groupId = try publisherGroup.groupId()
        let mint = try composeGroupInviteCapability(
            issuerSigningSeed: identity.ed25519Seed(),
            groupId: groupId,
            expiryMs: 0
        )
        return InviteLinkSecret.link(
            base: "\(origin)\(urlPrefix)\(Self.base64UrlEncode(mint.sealed))",
            secret: mint.linkSecret)
    }

    // MARK: - ADR-0053 / ADR-0057 journal access

    /// The auth for a request to the PublisherGroup's journal (contract point
    /// 1): its current-epoch access signer, and the root's lazy claim as the
    /// retry.
    public static func auth(
        ds: DsClient,
        identity: Identity,
        publisherGroup: PublisherGroup
    ) -> DsAuth? {
        guard let signer = try? publisherGroup.dsAccessSigner(identity: identity) else { return nil }
        return DsAuth(
            signer: signer,
            reclaim: { await lazyClaim(ds: ds, identity: identity, publisherGroup: publisherGroup) }
        )
    }

    /// The PublisherGroup journal's root: the channel owner's ACCOUNT identity
    /// (channels run on the account identity).
    static func rootSigner(identity: Identity, publisherGroup: PublisherGroup) throws
        -> DsStatementSignerProtocol {
        try dsJournalRootSigner(accountIdentity: identity, journalId: try publisherGroup.groupId())
    }

    /// Claim on open. A PublisherGroup this device just founded claims with
    /// the root and resets on a `409` (obligation 7); otherwise the claim is
    /// the idempotent re-send and a refusal is left alone. Best effort.
    ///
    /// Only the CHANNEL OWNER claims. A co-publisher's root is derived from
    /// its own account seed, so its claim would squat the journal against the
    /// owner; a co-publisher device (``CoPublishedJournals``) never claims, and
    /// when the journal is still unclaimed it waits for the owner.
    @discardableResult
    public static func claimJournal(
        ds: DsStatementSending,
        identity: Identity,
        publisherGroup: PublisherGroup,
        marks: JournalFoundingMarks = .shared,
        coPublished: CoPublishedJournals = .shared
    ) async -> DsRequestSigner? {
        guard let signer = try? publisherGroup.dsAccessSigner(identity: identity),
              let groupId = try? publisherGroup.groupId() else { return nil }
        let hex = DsClient.pseudonymHex(groupId)
        guard !coPublished.contains(journalIdHex: hex) else {
            NSLog("ADR-0057: co-publisher device; the channel owner claims the journal")
            return signer
        }
        guard let root = try? rootSigner(identity: identity, publisherGroup: publisherGroup)
        else { return nil }
        do {
            if marks.isMarked(journalIdHex: hex) {
                try await JournalOwnership.claimOrReset(
                    root: root, journalId: groupId, epoch: signer.epoch(),
                    epochKey: signer.verifyingKey(), ds: ds)
                marks.clear(journalIdHex: hex)
            } else {
                try await JournalOwnership.claim(
                    root: root, journalId: groupId, epoch: signer.epoch(),
                    epochKey: signer.verifyingKey(), ds: ds)
            }
        } catch DsStatementError.conflict {
            // Already claimed by this root at another chain; commits register
            // under the root.
        } catch {
            NSLog("ADR-0057: PublisherGroup journal claim failed, retried on the next open: %@",
                  String(describing: error))
        }
        return signer
    }

    /// The root's lazy claim after a `401` (obligation 2) — the channel
    /// owner's only: a co-publisher device never claims (its root would squat
    /// the journal), it waits. An already-claimed journal (`409`) is left
    /// alone.
    static func lazyClaim(
        ds: DsStatementSending,
        identity: Identity,
        publisherGroup: PublisherGroup,
        coPublished: CoPublishedJournals = .shared
    ) async -> DsRequestSigner? {
        guard let signer = try? publisherGroup.dsAccessSigner(identity: identity),
              let groupId = try? publisherGroup.groupId() else { return nil }
        let outcome = await JournalOwnership.lazyClaim(
            isOwner: !coPublished.contains(journalIdHex: DsClient.pseudonymHex(groupId)),
            root: { try rootSigner(identity: identity, publisherGroup: publisherGroup) },
            journalId: groupId, epoch: signer.epoch(), epochKey: signer.verifyingKey(), ds: ds)
        switch outcome {
        case .claimed: return signer
        case .alreadyClaimed, .notOwner, .failed: return nil
        }
    }

    /// Register the epoch a PublisherGroup commit moves to under the root
    /// (obligation 5). Throws: a refused registration aborts the commit.
    static func registerEpoch(
        ds: DsStatementSending,
        identity: Identity,
        publisherGroup: PublisherGroup,
        next: DsGroupSigner
    ) async throws {
        try await JournalOwnership.registerEpoch(
            authority: .root(try rootSigner(identity: identity, publisherGroup: publisherGroup)),
            journalId: try publisherGroup.groupId(), epoch: next.epoch(),
            epochKey: next.verifyingKey(), revoke: [], ds: ds)
    }

    /// What one ``pollJournal(ds:identity:publisherGroup:knockAfter:)`` did.
    public struct PollOutcome: Equatable {
        /// Pending co-publisher requests after the poll.
        public let pendingCount: Int
        /// Where the next knock read starts.
        public let knockCursor: Int64
    }

    /// Poll the PublisherGroup's DS journal (co-publisher commits) and its
    /// KNOCKS (external-join proposals from prospective co-publishers, ADR-0053
    /// point 7). Both reads are signed. Journal rows go through the role-gated
    /// path; knock rows go through the core's knock gate
    /// (`PublisherGroup.processKnock`), which refuses anything that is not a
    /// stranger's external-join proposal for this group before processing it —
    /// a refused row is dropped and the cursor still advances. Our own
    /// already-merged commits throw and are skipped. (ADR-0057: no
    /// heal-on-apply; the committer registered the epoch.)
    public static func pollJournal(
        ds: DsClient,
        identity: Identity,
        publisherGroup: PublisherGroup,
        knockAfter: Int64 = 0,
        readKnocks: Bool = true
    ) async throws -> PollOutcome {
        let groupId = try publisherGroup.groupId()
        let raw = try await ds.fetchGroup(
            groupId: groupId, after: 0, waitSecs: 0,
            auth: auth(ds: ds, identity: identity, publisherGroup: publisherGroup))
        for m in raw.messages {
            do {
                _ = try publisherGroup.processIncomingVerifyingRoles(
                    identity: identity, bytes: m.payload)
            } catch {
                // A commit we have already merged (epoch mismatch) — skip; the
                // FIFO journal catches up on any genuinely-new commit on the next
                // poll.
            }
        }
        var knockCursor = knockAfter
        // `readKnocks` is the caller's floor (``KnockPollThrottle``): the
        // journal poll runs every few seconds, the knock read far less often.
        if readKnocks {
            let knocks = try await ds.fetchKnocks(
                groupId: groupId, after: knockAfter, waitSecs: 0,
                auth: auth(ds: ds, identity: identity, publisherGroup: publisherGroup))
            for row in knocks.messages {
                knockCursor = max(knockCursor, row.id)
                _ = try? publisherGroup.processKnock(identity: identity, bytes: row.payload)
            }
            // ADR-0060 section 6 — a co-publisher's attribution carries its
            // `reply` grant, the only key its closed mailbox takes the Welcome
            // under. A co-publisher knocks with its account identity, so the
            // joiner key IS its account key.
            for row in knocks.messages {
                guard let attr = try? parseGroupJoinAttribution(bytes: row.payload),
                      attr.groupId == groupId else { continue }
                await MainActor.run {
                    _ = ReachCenter.shared.keep(
                        attr.replyGrant, carrier: .request,
                        fromAccountKeyB64: base64Encode(attr.joinerSignatureKey), scopeHex: nil)
                }
            }
        }
        return PollOutcome(
            pendingCount: try publisherGroup.pendingExternalJoins().count,
            knockCursor: knockCursor)
    }

    /// The pending co-publisher external-join requests — each a 32-byte joiner
    /// leaf signature key (the `proposalRef` for approve / reject).
    public static func pending(publisherGroup: PublisherGroup) throws -> [Data] {
        try publisherGroup.pendingExternalJoins()
    }

    /// Approve a pending co-publisher. Commits the Add (advances the
    /// PublisherGroup epoch) after registering the new epoch's access key
    /// (ADR-0053 point 3), broadcasts the commit to the journal, sends the
    /// Welcome to the joiner's own mailbox (point 8: a non-member cannot read
    /// the claimed journal), then RESEALS the new-epoch channel seed to all
    /// subscribers — the Add changed the exporter output, so existing
    /// subscribers must get the new seed or they break.
    ///
    /// The PublisherGroup approve is a direct eager-merge commit with no
    /// ADR-0040 outbox and no reset path: a failed registration throws after
    /// the merge exactly as a failed publish always did, leaving the handle
    /// ahead of the journal until the panel is reopened.
    public static func approve(
        ds: DsClient,
        channelsClient: ChannelsClient,
        identity: Identity,
        publisherGroup: PublisherGroup,
        proposalRef: Data,
        channelIdHex: String
    ) async throws {
        let groupId = try publisherGroup.groupId()
        try await DirectCommitSteps(
            currentSigner: { try publisherGroup.dsAccessSigner(identity: identity) },
            create: {
                try publisherGroup.approveExternalJoin(identity: identity, proposalRef: proposalRef)
            },
            register: { next in
                try await registerEpoch(
                    ds: ds, identity: identity, publisherGroup: publisherGroup, next: next)
            },
            prepare: { added in added },
            rollback: {},
            publish: { added, signerN in
                _ = try await ds.publishGroup(
                    groupId: groupId, payload: added.commit, auth: DsAuth(signer: signerN))
                // The joiner's external-join proposal carried a KeyPackage under
                // `proposalRef` (its account signature key — a co-publisher joins
                // as its account), so its mailbox is the pseudonym of that key.
                // ADR-0060 — written with the `reply` grant its attribution
                // carried; the mailbox is closed to anything else.
                let mailbox = try IdentityPseudonym.fromIdentity(pubkey: proposalRef).asBytes()
                let auth = await MainActor.run {
                    ReachCenter.shared.writeAuth(toAccountKey: proposalRef)
                }
                _ = try await ds.publishMailbox(
                    pseudonym: mailbox,
                    payload: wrapEnvelope(kind: .welcome, body: added.welcome),
                    writeAuth: auth)
            }
        ).run()
        try await resealSubscribers(
            channelsClient: channelsClient,
            identity: identity,
            publisherGroup: publisherGroup,
            channelIdHex: channelIdHex
        )
    }

    /// Reject a pending co-publisher — no commit, no epoch advance. The
    /// retained request set is part of the PublisherGroup's persisted MLS
    /// storage, so the core takes the identity to write the durable copy and
    /// the rejection survives a restart.
    public static func reject(
        identity: Identity,
        publisherGroup: PublisherGroup,
        proposalRef: Data
    ) throws {
        try publisherGroup.rejectExternalJoin(identity: identity, proposalRef: proposalRef)
    }

    // MARK: - Joiner side — a prospective co-publisher requests + joins

    /// Handles a `requestToCoPublish` returns so the caller can drive the poll
    /// loop (`pollForCoPublishWelcome`).
    public struct CoPublishRequest {
        /// The PublisherGroup's MLS `group_id` (the journal channel to poll).
        public let groupId: Data
        /// The 32-char lowercase-hex channel id recovered from the `group_id`.
        public let channelIdHex: String
        /// This device's leaf signature key — names the mailbox the owner sends
        /// the Welcome to after approving (ADR-0053).
        public let joinerKey: Data
    }

    /// ADR-0028 OQ3 — joiner side: request to become a co-publisher of a channel
    /// from a `/g/` co-publisher link. Decodes + verifies the capability,
    /// recovers the channel id from the PublisherGroup `group_id` (the link
    /// conveys only the `group_id`), then knocks (ADR-0053): a probe learns the
    /// registered epoch, an external-join proposal (ADR-0027) is built for it,
    /// and a second knock carries it with a self-signed join-attribution (OQ2).
    /// The caller then polls with `pollForCoPublishWelcome`.
    public static func requestToCoPublish(
        ds: DsClient,
        identity: Identity,
        link: String
    ) async throws -> CoPublishRequest {
        // ADR-0028 amendment 2026-09-28 — `<origin>/g/<b64url sealed>#<secret>`:
        // the capability is sealed under the fragment's secret.
        let parts = InviteLinkSecret.split(link)
        guard let idx = parts.base.range(of: urlPrefix, options: .backwards) else {
            throw CoPublisherError.notACoPublisherLink(parts.base)
        }
        let slug = String(parts.base[idx.upperBound...])
        guard let sealed = Self.base64UrlDecode(slug) else {
            throw CoPublisherError.notACoPublisherLink(parts.base)
        }
        guard let secret = InviteLinkSecret.read(parts.fragment) else {
            throw CoPublisherError.linkDamaged
        }
        let cap: GroupInviteCapability
        do {
            cap = try GroupInviteCapability.openLink(sealed: sealed, linkSecret: secret)
        } catch {
            throw CoPublisherError.linkDamaged
        }
        let wire = cap.wire()
        let groupId = cap.groupId()
        guard let channelIdBytes = channelIdFromPublisherGroupId(groupId: groupId) else {
            // Two distinct reasons the id does not resolve, needing different
            // copy: a link minted under the retired v1 derivation (recognisable
            // by its 41-byte shape) versus a `/g/` link targeting a chat group.
            if LegacyChannelV1.isLegacyV1PublisherGroupId(groupId) {
                throw LegacyChannelError(kind: .inviteLink)
            }
            throw CoPublisherError.notAChannelGroup
        }
        let channelIdHex = ChannelsComposer.bytesToHex(channelIdBytes)
        // ADR-0057 — this device asks to CO-publish, so it is not the channel
        // owner: it must never claim the PublisherGroup journal.
        CoPublishedJournals.shared.mark(journalIdHex: DsClient.pseudonymHex(groupId))
        // ADR-0053 point 6 — the knock: a probe learns the registered epoch,
        // the proposal is built for it, a second knock carries the self-signed
        // attribution (OQ2, advisory) and the proposal. No journal read.
        // ADR-0060 section 6 — the `reply` grant the owner writes the Welcome
        // with, registered before the knock.
        let replyGrant = try await ReachCenter.shared.newReplyGrant(
            purpose: ReplyPurpose.groupJoin(groupId))
        try await DsKnockJoin(
            knock: { payloads in
                try await ds.knock(groupId: groupId, payloads: payloads).epoch
            },
            createProposal: { epoch in
                try identity.createExternalJoinProposal(groupId: groupId, epoch: epoch)
            },
            attribution: {
                try? composeGroupJoinAttribution(
                    joinerSigningSeed: identity.ed25519Seed(),
                    capabilityBytes: wire,
                    replyGrant: replyGrant)
            }
        ).run()
        return CoPublishRequest(
            groupId: groupId,
            channelIdHex: channelIdHex,
            joinerKey: identity.signaturePublicKey()
        )
    }

    /// ADR-0028 OQ3 — joiner side: look for the Welcome the owner sent after
    /// approving, join from it, and mint local publisher keys so this device
    /// can now publish to the channel. The joined group lands at the
    /// post-approve epoch — the same one the subscribers were resealed to — so
    /// the derived `channelSeed` matches. Returns the keys on success, or `nil`
    /// if no Welcome has arrived yet (keep polling).
    ///
    /// ADR-0053 point 8 — the owner sends the Welcome to the mailbox named by
    /// this device's `joinerKey`, which it reads signed under `identity` (the
    /// key the mailbox is named after). The whole mailbox is scanned on each
    /// attempt: other Welcomes there (chat adds) do not open under this
    /// identity's co-publisher KeyPackage and are skipped.
    public static func pollForCoPublishWelcome(
        ds: DsClient,
        identity: Identity,
        groupId: Data,
        joinerKey: Data,
        channelIdHex: String,
        // G-29 — reuse the persisted per-channel signing seed if this device
        // has co-published to the channel before (nil on a first join).
        existingSigningSeed: Data? = nil
    ) async throws -> ChannelPublisherKeys? {
        let mailbox = try IdentityPseudonym.fromIdentity(pubkey: joinerKey).asBytes()
        let signer = dsMailboxSigner(identity: identity)
        var candidates: [Data] = []
        var after: Int64 = 0
        for _ in 0..<Self.maxMailboxPages {
            let page = try await ds.fetchMailbox(
                pseudonym: mailbox, after: after, waitSecs: 0, auth: DsAuth(signer: signer))
            for row in page.messages {
                if case .welcome(let body) = parseEnvelope(row.payload) { candidates.append(body) }
            }
            guard let last = page.messages.last, page.hasMore, last.id > after else { break }
            after = last.id
        }
        // Join the PublisherGroup from the Welcome — this persists the joined
        // MLS state under the deterministic PublisherGroup `group_id` in the
        // identity's provider. The handle itself is throwaway (ARC frees it);
        // `freshPublisherKeys` re-opens the now-joined PublisherGroup by
        // channel id and derives the shared content-key seed + a fresh
        // per-device signing key so this device can publish. Newest first,
        // and a Welcome sealed to a KeyPackage this identity does not hold is
        // skipped rather than ending the wait.
        var joined = false
        for welcome in candidates.reversed() {
            if (try? Group.joinFromWelcome(welcome: welcome, identity: identity)) != nil {
                joined = true
                break
            }
        }
        guard joined else { return nil }
        // ADR-0057 — a co-publisher never claims the journal (idempotent with
        // the mark set when the request was sent).
        CoPublishedJournals.shared.mark(journalIdHex: DsClient.pseudonymHex(groupId))
        return try ChannelsComposer.freshPublisherKeys(
            identity: identity,
            channelIdHex: channelIdHex,
            existingSigningSeed: existingSigningSeed)
    }

    /// Bound on the mailbox pages one Welcome look-up reads.
    static let maxMailboxPages = 50

    /// Reseal the CURRENT (post-approve) channel seed to all subscribers — the
    /// `RotationCron` fanout WITHOUT advancing the MLS epoch (the Add already
    /// did). Binds the envelopes to the new server epoch the `/rotate` endpoint
    /// sets; a single seal failure aborts the whole batch (the atomic posture the
    /// server enforces).
    private static func resealSubscribers(
        channelsClient: ChannelsClient,
        identity: Identity,
        publisherGroup: PublisherGroup,
        channelIdHex: String
    ) async throws {
        let newSeed = try publisherGroup.channelSeed(identity: identity)
        let channelIdBytes = try publisherGroup.channelId()
        let currentEpoch = try await channelsClient.fetchEpoch(channelIdHex: channelIdHex)
        let newServerEpoch = currentEpoch &+ 1
        if newServerEpoch < currentEpoch {
            throw CoPublisherError.unexpectedServerEpoch(currentEpoch)
        }
        let subscribers = try await channelsClient.listSubscribers(channelIdHex: channelIdHex)
        var batch: [RotateBatchEntry] = []
        batch.reserveCapacity(subscribers.count)
        for subscriberHex in subscribers {
            guard let subscriberBytes = Self.hexToData(subscriberHex) else {
                throw CoPublisherError.malformedSubscriberHex(subscriberHex)
            }
            let envelope = try sealSubscriberSeedEnvelope(
                subscriberPub: subscriberBytes,
                channelId: channelIdBytes,
                epochId: newServerEpoch,
                seed: newSeed
            )
            batch.append(
                RotateBatchEntry(
                    subscriberPubkeyHex: subscriberHex,
                    envelopeB64: envelope.base64EncodedString()
                )
            )
        }
        _ = try await channelsClient.rotateChannel(channelIdHex: channelIdHex, envelopes: batch)
    }

    // MARK: - helpers

    static func hexToData(_ hex: String) -> Data? {
        guard hex.count % 2 == 0, !hex.isEmpty else { return nil }
        var out = Data(capacity: hex.count / 2)
        var idx = hex.startIndex
        while idx < hex.endIndex {
            let next = hex.index(idx, offsetBy: 2)
            guard let b = UInt8(hex[idx..<next], radix: 16) else { return nil }
            out.append(b)
            idx = next
        }
        return out
    }

    static func base64UrlEncode(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    static func base64UrlDecode(_ slug: String) -> Data? {
        var s = slug
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let remainder = s.count % 4
        if remainder != 0 {
            s.append(String(repeating: "=", count: 4 - remainder))
        }
        return Data(base64Encoded: s)
    }
}

/// Errors surfaced by the co-publisher owner flow.
public enum CoPublisherError: Error, LocalizedError {
    case badChannelId(String)
    case unexpectedServerEpoch(UInt32)
    case malformedSubscriberHex(String)
    case notACoPublisherLink(String)
    case notAChannelGroup
    /// ADR-0028 amendment 2026-09-28 — the link's secret is missing or the
    /// capability does not open under it.
    case linkDamaged

    public var errorDescription: String? {
        switch self {
        case .badChannelId(let hex):
            return "channel id hex is not valid: \(hex)"
        case .unexpectedServerEpoch(let epoch):
            return "unexpected server epoch \(epoch) — cannot reseal"
        case .malformedSubscriberHex(let hex):
            return "subscriber pubkey hex is not valid: \(hex)"
        case .notACoPublisherLink(let link):
            return "not a /g/ co-publisher link: \(link)"
        case .notAChannelGroup:
            return "co-publisher link does not target a channel PublisherGroup"
        case .linkDamaged:
            return inviteLinkDamagedMessage
        }
    }
}

/// ADR-0057 — PublisherGroup journals this device joined as a CO-publisher.
/// Only the channel owner claims a PublisherGroup journal: a co-publisher's
/// root is derived from its own account seed, so its claim would squat the
/// journal against the owner. A PublisherGroup exposes no role table, so the
/// device remembers that it asked to co-publish. Plain `UserDefaults`: a
/// journal id is a public DS address.
public final class CoPublishedJournals {
    public static let shared = CoPublishedJournals()

    private let defaults: UserDefaults
    private static let key = "me.lakko.ds.co-published.v1"

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func mark(journalIdHex: String) {
        var set = Set(defaults.stringArray(forKey: Self.key) ?? [])
        guard set.insert(journalIdHex.lowercased()).inserted else { return }
        defaults.set(Array(set).sorted(), forKey: Self.key)
    }

    public func contains(journalIdHex: String) -> Bool {
        (defaults.stringArray(forKey: Self.key) ?? []).contains(journalIdHex.lowercased())
    }
}
