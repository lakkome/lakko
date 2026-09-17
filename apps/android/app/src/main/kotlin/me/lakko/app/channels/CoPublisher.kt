// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.util.Base64
import me.lakko.app.ds.DsClient
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.GroupInviteCapability
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.PublisherGroup
import uniffi.lakko_ffi_uniffi.channelIdFromPublisherGroupId
import uniffi.lakko_ffi_uniffi.composeGroupInviteCapability
import uniffi.lakko_ffi_uniffi.composeGroupJoinAttribution
import uniffi.lakko_ffi_uniffi.epochOfMessage
import uniffi.lakko_ffi_uniffi.parseWelcomeRendezvous
import uniffi.lakko_ffi_uniffi.publisherGroupOpenOrCreate
import uniffi.lakko_ffi_uniffi.sealSubscriberSeedEnvelope
import uniffi.lakko_ffi_uniffi.wrapWelcomeRendezvous

/**
 * ADR-0028 OQ3 — channel co-publisher management (owner + joiner side, Android).
 *
 * Kotlin mirror of `apps/web/src/channels/copublisher.ts` and
 * `apps/ios/Lakko/Channels/CoPublisher.swift`. A channel's PublisherGroup
 * (ADR-0010 Tier 1) is a vanilla MLS group, so adding a *co-publisher* reuses
 * the group join-ticket + external-join machinery (ADR-0027/0028) verbatim —
 * exactly what the OQ3 FFI surface on [PublisherGroup] exposes.
 *
 * **Reseal-after-add (load-bearing).** `approveExternalJoin` commits the Add,
 * which advances the PublisherGroup MLS epoch — so the channel content-key seed
 * (the epoch-keyed exporter output) CHANGES. Every existing subscriber holds the
 * OLD seed, so the owner MUST reseal the new seed to all subscribers (the
 * ADR-0010 / C-54 rotation fanout) or those subscribers can no longer decrypt.
 * [approve] does this via [resealSubscribers] — the same audited
 * `sealSubscriberSeedEnvelope` + atomic `POST /v1/channels/:id/rotate` fanout as
 * [RotationCron], but WITHOUT a second `rotate()` (the Add already advanced the
 * epoch; the reseal binds to the post-approve epoch the co-publisher joins at).
 *
 * The end-to-end correctness of the reseal under a real 2-publisher topology
 * needs a live 2-publisher e2e against a deployed channels backend — deferred
 * (no deployed environment), the same posture as every other live-channel gate.
 */
public object CoPublisher {
    /** Path prefix every `/g/` co-publisher invite URL starts with. */
    public const val URL_PREFIX: String = "/g/"

    /** Default origin Lakko shares its invite links under. */
    public const val DEFAULT_ORIGIN: String = "https://lakko.me"

    /** Errors surfaced by the co-publisher flow. */
    public sealed class CoPublisherException(msg: String) : Exception(msg) {
        public class BadChannelId(hex: String) :
            CoPublisherException("channel id hex is not valid: $hex")

        public class UnexpectedServerEpoch(epoch: UInt) :
            CoPublisherException("server epoch wraparound at $epoch — cannot reseal")

        public class MalformedSubscriberHex(hex: String) :
            CoPublisherException("subscriber pubkey hex is not valid: $hex")

        public class NotACoPublisherLink(link: String) :
            CoPublisherException("not a /g/ co-publisher link: $link")

        public object NotAChannelGroup :
            CoPublisherException("co-publisher link does not target a channel PublisherGroup")
    }

    // -----------------------------------------------------------------
    // Owner side
    // -----------------------------------------------------------------

    /** Open (or create) the persistent PublisherGroup handle for a channel. The
     *  caller owns the returned handle and MUST `.close()` it when done. */
    public fun openPublisherGroup(identity: Identity, channelIdHex: String): PublisherGroup {
        val channelId = hexToBytes(channelIdHex)
            ?: throw CoPublisherException.BadChannelId(channelIdHex)
        return try {
            publisherGroupOpenOrCreate(identity, channelId)
        } catch (e: Throwable) {
            // A channel minted under the retired v1 derivation refuses here and
            // cannot be repaired — surface the readable copy, not the raw string.
            throw LegacyChannelV1.mapOpenError(e)
        }
    }

    /** Mint a co-publisher invite link over the PublisherGroup's MLS `group_id`.
     *  A non-member opens it, requests to join, and the owner approves (reusing
     *  the ADR-0027/0028 external-join machinery). `0` = never expires. */
    public fun createCoPublisherLink(
        identity: Identity,
        publisherGroup: PublisherGroup,
        origin: String = DEFAULT_ORIGIN,
    ): String {
        val groupId = publisherGroup.groupId()
        val wire = composeGroupInviteCapability(identity.ed25519Seed(), groupId, 0uL)
        return "$origin$URL_PREFIX${base64UrlEncode(wire)}"
    }

    /** Poll the PublisherGroup's DS journal and apply incoming MLS messages
     *  (external-join proposals from prospective co-publishers + any co-publisher
     *  commits) via the role-gated path. Surfaces external-join proposals into
     *  the PublisherGroup's retained pending set. Non-MLS entries (a Welcome
     *  rendezvous) and our own already-merged commits throw and are skipped.
     *  Returns how many entries were applied (the caller snapshots the
     *  provider when that is non-zero — an applied commit advanced the
     *  group's epoch) and the number of pending requests after the poll. */
    public suspend fun pollJournal(
        ds: DsClient,
        identity: Identity,
        publisherGroup: PublisherGroup,
    ): JournalPoll {
        val groupId = publisherGroup.groupId()
        val raw = ds.fetchGroup(groupId, after = 0L, waitSecs = 0)
        var applied = 0
        for (m in raw.messages) {
            try {
                publisherGroup.processIncomingVerifyingRoles(identity, m.payload)
                applied += 1
            } catch (_: Throwable) {
                // Non-MLS entry (a Welcome rendezvous) or a commit we have already
                // merged (epoch mismatch) — skip; the FIFO journal catches up on
                // any genuinely-new commit on the next poll.
            }
        }
        return JournalPoll(applied = applied, pending = publisherGroup.pendingExternalJoins().size)
    }

    /** What one [pollJournal] pass did. */
    public data class JournalPoll(
        /** Journal entries the group accepted this pass. */
        val applied: Int,
        /** Pending co-publisher requests after the pass. */
        val pending: Int,
    )

    /** The pending co-publisher external-join requests — each a 32-byte joiner
     *  leaf signature key (the `proposalRef` for approve / reject). */
    public fun pending(publisherGroup: PublisherGroup): List<ByteArray> =
        publisherGroup.pendingExternalJoins()

    /** Approve a pending co-publisher. Commits the Add (advances the
     *  PublisherGroup epoch), broadcasts the commit + a `WelcomeRendezvous` to
     *  the journal (the not-yet-member joiner polls for it), then RESEALS the
     *  new-epoch channel seed to all subscribers — the Add changed the exporter
     *  output, so existing subscribers must get the new seed or they break. */
    public suspend fun approve(
        ds: DsClient,
        channelsClient: ChannelsClient,
        identity: Identity,
        publisherGroup: PublisherGroup,
        proposalRef: ByteArray,
        channelIdHex: String,
    ) {
        val groupId = publisherGroup.groupId()
        val added = publisherGroup.approveExternalJoin(identity, proposalRef)
        ds.publishGroup(groupId, added.commit)
        val rendezvous = wrapWelcomeRendezvous(proposalRef, added.welcome)
        ds.publishGroup(groupId, rendezvous)
        resealSubscribers(channelsClient, identity, publisherGroup, channelIdHex)
    }

    /** Reject a pending co-publisher — local drop, no commit, no epoch advance. */
    public fun reject(publisherGroup: PublisherGroup, proposalRef: ByteArray) {
        publisherGroup.rejectExternalJoin(proposalRef)
    }

    // -----------------------------------------------------------------
    // Joiner side — a prospective co-publisher requests + joins
    // -----------------------------------------------------------------

    /** Handles a [requestToCoPublish] returns so the caller can drive the poll
     *  loop ([pollForCoPublishWelcome]). */
    public data class CoPublishRequest(
        /** The PublisherGroup's MLS `group_id` (the journal channel to poll). */
        val groupId: ByteArray,
        /** The 32-char lowercase-hex channel id recovered from the `group_id`. */
        val channelIdHex: String,
        /** This device's leaf signature key — matches the `WelcomeRendezvous`
         *  the owner posts after approving. */
        val joinerKey: ByteArray,
    )

    /** ADR-0028 OQ3 — joiner side: request to become a co-publisher of a channel
     *  from a `/g/` co-publisher link. Decodes + verifies the capability,
     *  recovers the channel id from the PublisherGroup `group_id`, reads the
     *  group's current epoch from its DS journal head's public framing
     *  (`epochOfMessage`, no group secret), crafts an external-join proposal
     *  (ADR-0027), and posts it + a self-signed join-attribution (OQ2) to the
     *  journal. The caller then polls with [pollForCoPublishWelcome]. */
    public suspend fun requestToCoPublish(
        ds: DsClient,
        identity: Identity,
        link: String,
    ): CoPublishRequest {
        val idx = link.lastIndexOf(URL_PREFIX)
        if (idx < 0) throw CoPublisherException.NotACoPublisherLink(link)
        val slug = link.substring(idx + URL_PREFIX.length)
        val wire = base64UrlDecode(slug)
            ?: throw CoPublisherException.NotACoPublisherLink(link)
        val cap = GroupInviteCapability.decodeAndVerify(wire)
        val groupId = try {
            cap.groupId()
        } finally {
            cap.close()
        }
        val channelIdBytes = channelIdFromPublisherGroupId(groupId)
            ?: throw if (LegacyChannelV1.isLegacyV1PublisherGroupId(groupId)) {
                // A link minted under the retired v1 derivation — distinct from
                // a `/g/` link that simply targets a chat group, and it needs
                // its own copy.
                LegacyChannelException(LegacyChannelKind.INVITE_LINK)
            } else {
                CoPublisherException.NotAChannelGroup
            }
        val channelIdHex = ChannelsComposer.bytesToHex(channelIdBytes)
        // Read the current epoch from the journal head (public framing; a
        // non-member can do this). Empty / unreachable journal => epoch 0.
        var epoch: ULong = 0uL
        try {
            val raw = ds.fetchGroup(groupId, after = 0L, waitSecs = 0)
            for (m in raw.messages) {
                epochOfMessage(m.payload)?.let { epoch = it }
            }
        } catch (_: Throwable) {
            // keep epoch 0
        }
        val proposal = identity.createExternalJoinProposal(groupId, epoch)
        ds.publishGroup(groupId, proposal)
        // Self-signed join attribution (OQ2) — advisory; best-effort.
        try {
            val attribution = composeGroupJoinAttribution(identity.ed25519Seed(), wire)
            ds.publishGroup(groupId, attribution)
        } catch (_: Throwable) {
            // advisory only — ignore
        }
        return CoPublishRequest(
            groupId = groupId,
            channelIdHex = channelIdHex,
            joinerKey = identity.signaturePublicKey(),
        )
    }

    /** ADR-0028 OQ3 — joiner side: poll the PublisherGroup journal for the
     *  Welcome the owner posted after approving, join from it, and mint local
     *  publisher keys so this device can now publish to the channel. The joined
     *  group lands at the post-approve epoch — the same one the subscribers were
     *  resealed to — so the derived `channelSeed` matches. Returns the keys on
     *  success, or `null` if no Welcome has arrived yet (keep polling). */
    public suspend fun pollForCoPublishWelcome(
        ds: DsClient,
        identity: Identity,
        groupId: ByteArray,
        joinerKey: ByteArray,
        channelIdHex: String,
        // G-29 — reuse the persisted per-channel signing seed if this device
        // has co-published to the channel before (null on a first join).
        existingSigningSeed: ByteArray? = null,
    ): ChannelPublisherKeys? {
        val raw = ds.fetchGroup(groupId, after = 0L, waitSecs = 0)
        for (m in raw.messages) {
            val rendezvous = try {
                parseWelcomeRendezvous(m.payload)
            } catch (_: Throwable) {
                continue
            }
            if (!rendezvous.joinerSignatureKey.contentEquals(joinerKey)) continue
            // Join the PublisherGroup from the Welcome — this persists the joined
            // MLS state under the deterministic PublisherGroup `group_id` in the
            // identity's provider. The handle itself is throwaway (close it);
            // freshPublisherKeys re-opens the now-joined PublisherGroup by
            // channel id and derives the shared content-key seed + a fresh
            // per-device signing key so this device can publish.
            Group.joinFromWelcome(rendezvous.welcome, identity).close()
            return ChannelsComposer.freshPublisherKeys(
                identity, channelIdHex, existingSigningSeed)
        }
        return null
    }

    // -----------------------------------------------------------------
    // Reseal (mirror RotationCron steps 2-4, WITHOUT rotate())
    // -----------------------------------------------------------------

    /** Reseal the CURRENT (post-approve) channel seed to all subscribers — the
     *  [RotationCron] fanout WITHOUT advancing the MLS epoch (the Add already
     *  did). Binds the envelopes to the new server epoch the `/rotate` endpoint
     *  sets; a single seal failure aborts the whole batch (the atomic posture
     *  the server enforces). */
    private suspend fun resealSubscribers(
        channelsClient: ChannelsClient,
        identity: Identity,
        publisherGroup: PublisherGroup,
        channelIdHex: String,
    ) {
        val newSeed = publisherGroup.channelSeed(identity)
        val channelIdBytes = publisherGroup.channelId()
        val currentEpoch = channelsClient.fetchEpoch(channelIdHex)
        if (currentEpoch == UInt.MAX_VALUE) {
            throw CoPublisherException.UnexpectedServerEpoch(currentEpoch)
        }
        val newServerEpoch = currentEpoch + 1u
        val subscribers = channelsClient.listSubscribers(channelIdHex)
        val batch = ArrayList<RotateBatchEntry>(subscribers.size)
        for (subscriberHex in subscribers) {
            val subscriberBytes = hexToBytes(subscriberHex)
                ?: throw CoPublisherException.MalformedSubscriberHex(subscriberHex)
            val envelope = sealSubscriberSeedEnvelope(
                subscriberBytes,
                channelIdBytes,
                newServerEpoch,
                newSeed,
            )
            batch.add(
                RotateBatchEntry(
                    subscriber_pubkey_hex = subscriberHex,
                    envelope_b64 = Base64.encodeToString(envelope, Base64.NO_WRAP),
                ),
            )
        }
        channelsClient.rotateChannel(channelIdHex, batch)
    }

    // -----------------------------------------------------------------
    // helpers
    // -----------------------------------------------------------------

    private fun hexToBytes(hex: String): ByteArray? {
        if (hex.isEmpty() || hex.length % 2 != 0) return null
        val out = ByteArray(hex.length / 2)
        for (i in out.indices) {
            val byte = hex.substring(i * 2, i * 2 + 2).toIntOrNull(16) ?: return null
            out[i] = byte.toByte()
        }
        return out
    }

    private fun base64UrlEncode(bytes: ByteArray): String =
        Base64.encodeToString(bytes, Base64.NO_WRAP or Base64.URL_SAFE or Base64.NO_PADDING)

    private fun base64UrlDecode(slug: String): ByteArray? =
        runCatching {
            Base64.decode(slug, Base64.URL_SAFE or Base64.NO_PADDING)
        }.getOrNull()
}
