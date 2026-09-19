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
    /// Default origin Lakko shares its invite links under.
    public static let defaultOrigin = "https://lakko.me"

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
    public static func createCoPublisherLink(
        identity: Identity,
        publisherGroup: PublisherGroup,
        origin: String = defaultOrigin
    ) throws -> String {
        let groupId = try publisherGroup.groupId()
        let wire = try composeGroupInviteCapability(
            issuerSigningSeed: identity.ed25519Seed(),
            groupId: groupId,
            expiryMs: 0
        )
        return "\(origin)\(urlPrefix)\(Self.base64UrlEncode(wire))"
    }

    /// Poll the PublisherGroup's DS journal and apply incoming MLS messages
    /// (external-join proposals from prospective co-publishers + any co-publisher
    /// commits) via the role-gated path. Surfaces external-join proposals into
    /// the PublisherGroup's retained pending set. Non-MLS entries (a Welcome
    /// rendezvous) and our own already-merged commits throw and are skipped.
    /// Returns the number of pending requests after the poll.
    public static func pollJournal(
        ds: DsClient,
        identity: Identity,
        publisherGroup: PublisherGroup
    ) async throws -> Int {
        let groupId = try publisherGroup.groupId()
        let raw = try await ds.fetchGroup(groupId: groupId, after: 0, waitSecs: 0)
        for m in raw.messages {
            do {
                _ = try publisherGroup.processIncomingVerifyingRoles(
                    identity: identity, bytes: m.payload)
            } catch {
                // Non-MLS entry (a Welcome rendezvous) or a commit we have already
                // merged (epoch mismatch) — skip; the FIFO journal catches up on
                // any genuinely-new commit on the next poll.
            }
        }
        return try publisherGroup.pendingExternalJoins().count
    }

    /// The pending co-publisher external-join requests — each a 32-byte joiner
    /// leaf signature key (the `proposalRef` for approve / reject).
    public static func pending(publisherGroup: PublisherGroup) throws -> [Data] {
        try publisherGroup.pendingExternalJoins()
    }

    /// Approve a pending co-publisher. Commits the Add (advances the
    /// PublisherGroup epoch), broadcasts the commit + a `WelcomeRendezvous` to the
    /// journal (the not-yet-member joiner polls for it), then RESEALS the
    /// new-epoch channel seed to all subscribers — the Add changed the exporter
    /// output, so existing subscribers must get the new seed or they break.
    public static func approve(
        ds: DsClient,
        channelsClient: ChannelsClient,
        identity: Identity,
        publisherGroup: PublisherGroup,
        proposalRef: Data,
        channelIdHex: String
    ) async throws {
        let groupId = try publisherGroup.groupId()
        let added = try publisherGroup.approveExternalJoin(
            identity: identity, proposalRef: proposalRef)
        _ = try await ds.publishGroup(groupId: groupId, payload: added.commit)
        let rendezvous = try wrapWelcomeRendezvous(
            joinerSignatureKey: proposalRef, welcome: added.welcome)
        _ = try await ds.publishGroup(groupId: groupId, payload: rendezvous)
        try await resealSubscribers(
            channelsClient: channelsClient,
            identity: identity,
            publisherGroup: publisherGroup,
            channelIdHex: channelIdHex
        )
    }

    /// Reject a pending co-publisher — local drop, no commit, no epoch advance.
    public static func reject(
        publisherGroup: PublisherGroup,
        proposalRef: Data
    ) throws {
        try publisherGroup.rejectExternalJoin(proposalRef: proposalRef)
    }

    // MARK: - Joiner side — a prospective co-publisher requests + joins

    /// Handles a `requestToCoPublish` returns so the caller can drive the poll
    /// loop (`pollForCoPublishWelcome`).
    public struct CoPublishRequest {
        /// The PublisherGroup's MLS `group_id` (the journal channel to poll).
        public let groupId: Data
        /// The 32-char lowercase-hex channel id recovered from the `group_id`.
        public let channelIdHex: String
        /// This device's leaf signature key — matches the `WelcomeRendezvous`
        /// the owner posts after approving.
        public let joinerKey: Data
    }

    /// ADR-0028 OQ3 — joiner side: request to become a co-publisher of a channel
    /// from a `/g/` co-publisher link. Decodes + verifies the capability,
    /// recovers the channel id from the PublisherGroup `group_id` (the link
    /// conveys only the `group_id`), reads the group's current epoch from its DS
    /// journal head's public framing (`epochAfterMessage` over the paged journal, no group secret), crafts
    /// an external-join proposal (ADR-0027), and posts it + a self-signed
    /// join-attribution (OQ2) to the journal. The caller then polls with
    /// `pollForCoPublishWelcome`.
    public static func requestToCoPublish(
        ds: DsClient,
        identity: Identity,
        link: String
    ) async throws -> CoPublishRequest {
        guard let idx = link.range(of: urlPrefix, options: .backwards) else {
            throw CoPublisherError.notACoPublisherLink(link)
        }
        let slug = String(link[idx.upperBound...])
        guard let wire = Self.base64UrlDecode(slug) else {
            throw CoPublisherError.notACoPublisherLink(link)
        }
        let cap = try GroupInviteCapability.decodeAndVerify(wire: wire)
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
        // Read the current epoch from the journal head (public framing; a
        // non-member can do this). Empty / unreachable journal => epoch 0.
        // Paged to the head and read with `epochAfterMessage`: see
        // `JournalHeadEpoch` for the two ways the single-page read went stale.
        let epoch = (try? await JournalHeadEpoch.read(
            fetchPage: { after, limit in
                let raw = try await ds.fetchGroup(
                    groupId: groupId, after: after, limit: limit, waitSecs: 0)
                return .init(
                    rows: raw.messages.map { .init(id: $0.id, payload: $0.payload) },
                    hasMore: raw.hasMore)
            },
            epochAfter: { epochAfterMessage(bytes: $0) }
        )) ?? 0
        let proposal = try identity.createExternalJoinProposal(groupId: groupId, epoch: epoch)
        _ = try await ds.publishGroup(groupId: groupId, payload: proposal)
        // Self-signed join attribution (OQ2) — advisory; best-effort.
        if let attribution = try? composeGroupJoinAttribution(
            joinerSigningSeed: identity.ed25519Seed(),
            capabilityBytes: wire
        ) {
            _ = try? await ds.publishGroup(groupId: groupId, payload: attribution)
        }
        return CoPublishRequest(
            groupId: groupId,
            channelIdHex: channelIdHex,
            joinerKey: identity.signaturePublicKey()
        )
    }

    /// ADR-0028 OQ3 — joiner side: poll the PublisherGroup journal for the
    /// Welcome the owner posted after approving, join from it, and mint local
    /// publisher keys so this device can now publish to the channel. The joined
    /// group lands at the post-approve epoch — the same one the subscribers were
    /// resealed to — so the derived `channelSeed` matches. Returns the keys on
    /// success, or `nil` if no Welcome has arrived yet (keep polling).
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
        // Paged to the end: the Welcome is the newest row, and a single-page
        // read never reached it on a journal longer than one page (see
        // `JournalHeadEpoch.scan`).
        var candidates: [Data] = []
        let _: Never? = try await JournalHeadEpoch.scan(
            fetchPage: { after, limit in
                let raw = try await ds.fetchGroup(
                    groupId: groupId, after: after, limit: limit, waitSecs: 0)
                return .init(
                    rows: raw.messages.map { .init(id: $0.id, payload: $0.payload) },
                    hasMore: raw.hasMore)
            },
            visit: { row in
                guard let rendezvous = try? parseWelcomeRendezvous(bytes: row.payload),
                      rendezvous.joinerSignatureKey == joinerKey else { return nil }
                candidates.append(rendezvous.welcome)
                return nil
            }
        )
        // Join the PublisherGroup from the Welcome — this persists the joined
        // MLS state under the deterministic PublisherGroup `group_id` in the
        // identity's provider. The handle itself is throwaway (ARC frees it);
        // `freshPublisherKeys` re-opens the now-joined PublisherGroup by
        // channel id and derives the shared content-key seed + a fresh
        // per-device signing key so this device can publish. Newest first,
        // and a Welcome sealed to a KeyPackage this device no longer holds is
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
        }
    }
}
