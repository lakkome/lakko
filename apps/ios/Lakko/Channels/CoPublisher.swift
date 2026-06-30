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
        return try publisherGroupOpenOrCreate(identity: identity, channelId: channelId)
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
}

/// Errors surfaced by the co-publisher owner flow.
public enum CoPublisherError: Error, LocalizedError {
    case badChannelId(String)
    case unexpectedServerEpoch(UInt32)
    case malformedSubscriberHex(String)

    public var errorDescription: String? {
        switch self {
        case .badChannelId(let hex):
            return "channel id hex is not valid: \(hex)"
        case .unexpectedServerEpoch(let epoch):
            return "unexpected server epoch \(epoch) — cannot reseal"
        case .malformedSubscriberHex(let hex):
            return "subscriber pubkey hex is not valid: \(hex)"
        }
    }
}
