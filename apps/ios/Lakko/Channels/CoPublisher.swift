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
            return try publisherGroupOpenOrCreate(identity: identity, channelId: channelId)
        } catch {
            // A channel minted under the retired v1 derivation refuses here and
            // cannot be repaired — surface the readable copy, not the raw string.
            throw LegacyChannelV1.mapOpenError(error)
        }
    }

    /// Mint a co-publisher invite link over the PublisherGroup's MLS `group_id`.
    /// A non-member opens it, requests to join, and the owner approves (reusing
    /// the ADR-0027/0028 external-join machinery). `0` = never expires.
    ///
    /// Sealed links (ADR-0028 amendment of 2026-09-28): the capability is
    /// sealed under a fresh 12-byte secret that rides the fragment only; the
    /// sealed blob goes to the `/g/` ledger on `discovery` (a session-bearing
    /// client, the upload is `require_session`) and the short pointer link is
    /// returned. Only a TRANSPORT failure falls back to the inline link; a
    /// server refusal surfaces.
    public static func createCoPublisherLink(
        identity: Identity,
        publisherGroup: PublisherGroup,
        discovery: DiscoveryClient,
        origin: String = defaultOrigin
    ) async throws -> String {
        let groupId = try publisherGroup.groupId()
        let mint = try composeGroupInviteCapability(
            issuerSigningSeed: identity.ed25519Seed(),
            groupId: groupId,
            expiryMs: 0
        )
        do {
            let blobId = try await discovery.uploadGroupInvite(
                mint: mint, expiryMs: 0, maxUses: UInt32.max)
            return InviteLink.pointer(
                origin: origin, prefix: urlPrefix, blobId: blobId, secret: mint.linkSecret)
        } catch let error where InviteLink.uploadFallsBackInline(error) {
            NSLog("co-publisher link upload failed (network); sharing the inline link")
            return InviteLink.inline(
                origin: origin, prefix: urlPrefix, sealed: mint.sealed, secret: mint.linkSecret)
        }
    }

    /// Open a pasted `/g/` co-publisher link: read the secret from the
    /// fragment (none or malformed → ``CoPublisherError/damagedLink``, before
    /// any request), take the sealed blob inline or fetch it by id, and open
    /// + verify it. Returns the opened capability.
    static func openCoPublisherLink(
        _ link: String,
        discovery: DiscoveryClient
    ) async throws -> GroupInviteCapability {
        let (base, fragment) = InviteLink.split(link.trimmingCharacters(in: .whitespacesAndNewlines))
        guard let idx = base.range(of: urlPrefix, options: .backwards) else {
            throw CoPublisherError.notACoPublisherLink(link)
        }
        let slug = String(base[idx.upperBound...])
        let parsed: GroupInviteUrl.ParsedGroupInvite
        do {
            parsed = try GroupInviteUrl.parseGroupInviteSlug(slug)
        } catch {
            throw CoPublisherError.notACoPublisherLink(link)
        }
        guard let secret = InviteLink.secret(fromFragment: fragment) else {
            throw CoPublisherError.damagedLink
        }
        let sealed: Data
        switch parsed {
        case .inline(let bytes):
            sealed = bytes
        case .pointer(let blobId):
            guard let fetched = try await discovery.fetchGroupInvite(blobId: blobId) else {
                throw CoPublisherError.notACoPublisherLink(link)
            }
            sealed = fetched
        }
        do {
            return try GroupInviteCapability.openLink(sealed: sealed, linkSecret: secret)
        } catch {
            throw CoPublisherError.damagedLink
        }
    }

    // MARK: - ADR-0053 journal access

    /// The auth for a request to the PublisherGroup's journal (contract point
    /// 1): its current-epoch access signer, and a claim-on-open as the retry.
    public static func auth(
        ds: DsClient,
        identity: Identity,
        publisherGroup: PublisherGroup
    ) -> DsAuth? {
        guard let signer = try? publisherGroup.dsAccessSigner(identity: identity) else { return nil }
        return DsAuth(
            signer: signer,
            reclaim: { await claimJournal(ds: ds, identity: identity, publisherGroup: publisherGroup) }
        )
    }

    /// Claim-on-open (contract point 2) — register the current epoch's access
    /// key; the first open after `publisherGroupOpenOrCreate` is the claim at
    /// creation. Idempotent and unsigned; a failure is logged and retried on
    /// the next open.
    @discardableResult
    public static func claimJournal(
        ds: DsClient,
        identity: Identity,
        publisherGroup: PublisherGroup
    ) async -> DsRequestSigner? {
        guard let signer = try? publisherGroup.dsAccessSigner(identity: identity),
              let groupId = try? publisherGroup.groupId() else { return nil }
        do {
            try await ds.claimGroup(
                groupId: groupId, bodyJson: signer.groupClaimBodyJson(), provenBy: nil)
        } catch {
            NSLog("ADR-0053: PublisherGroup journal claim failed, retried on the next open: %@",
                  String(describing: error))
        }
        return signer
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
    /// already-merged commits throw and are skipped. A batch that moved the epoch re-registers the new key
    /// (heal-on-apply, point 4).
    public static func pollJournal(
        ds: DsClient,
        identity: Identity,
        publisherGroup: PublisherGroup,
        knockAfter: Int64 = 0,
        readKnocks: Bool = true
    ) async throws -> PollOutcome {
        let groupId = try publisherGroup.groupId()
        let signerBefore = try? publisherGroup.dsAccessSigner(identity: identity)
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
            // ADR-0058 section 6 — a co-publisher's attribution carries its
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
        await dsHealRegistration(
            ds: ds, groupId: groupId, before: signerBefore,
            after: try? publisherGroup.dsAccessSigner(identity: identity))
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
            register: { next, provenBy in
                try await ds.claimGroup(
                    groupId: groupId, bodyJson: next.groupClaimBodyJson(), provenBy: provenBy)
            },
            prepare: { added in added },
            rollback: {},
            publish: { added, signerN in
                _ = try await ds.publishGroup(
                    groupId: groupId, payload: added.commit, auth: DsAuth(signer: signerN))
                // The joiner's external-join proposal carried a KeyPackage under
                // `proposalRef` (its account signature key — a co-publisher joins
                // as its account), so its mailbox is the pseudonym of that key.
                // ADR-0058 — written with the `reply` grant its attribution
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
        link: String,
        discovery: DiscoveryClient = DiscoveryClient()
    ) async throws -> CoPublishRequest {
        let cap = try await openCoPublisherLink(link, discovery: discovery)
        // The PLAINTEXT capability the link opened to: what the attribution
        // carries (the ledger consume is not driven here; the co-publisher
        // link is minted with an unlimited ceiling).
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
        // ADR-0053 point 6 — the knock: a probe learns the registered epoch,
        // the proposal is built for it, a second knock carries the self-signed
        // attribution (OQ2, advisory) and the proposal. No journal read.
        // ADR-0058 section 6 — the `reply` grant the owner writes the Welcome
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
        _ = groupId
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
    /// The link has no fragment, a fragment that is not a link secret, or a
    /// sealed capability that does not open under it.
    case damagedLink
    /// No session token, so the ledger upload cannot be made.
    case notSignedIn

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
        case .damagedLink:
            return "co-publisher link is damaged"
        case .notSignedIn:
            return "not signed in: no session token for discovery"
        }
    }
}
