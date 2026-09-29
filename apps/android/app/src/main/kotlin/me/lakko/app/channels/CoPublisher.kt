// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.util.Base64
import me.lakko.app.chat.GroupNotAcceptingRequests
import me.lakko.app.chat.JournalHeadEpoch
import me.lakko.app.chat.runLinkJoinKnock
import me.lakko.app.contacts.KIND_WELCOME
import me.lakko.app.contacts.ParsedEnvelope
import me.lakko.app.contacts.parseEnvelope
import me.lakko.app.contacts.wrapEnvelope
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.FfiDsJournalSigner
import me.lakko.app.ds.healRegistration
import me.lakko.app.ds.registerBeforePublish
import uniffi.lakko_ffi_uniffi.IdentityPseudonym
import uniffi.lakko_ffi_uniffi.dsMailboxSigner
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.GroupInviteCapability
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.PublisherGroup
import uniffi.lakko_ffi_uniffi.channelIdFromPublisherGroupId
import uniffi.lakko_ffi_uniffi.composeGroupInviteCapability
import uniffi.lakko_ffi_uniffi.composeGroupJoinAttribution
import uniffi.lakko_ffi_uniffi.publisherGroupOpenOrCreate
import uniffi.lakko_ffi_uniffi.sealSubscriberSeedEnvelope

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

    /** Default origin Lakko shares its invite links under: the web origin of
     *  the stack this build talks to ([me.lakko.app.config.ApiEndpoints.webOrigin]). */
    public val DEFAULT_ORIGIN: String
        get() = me.lakko.app.config.ApiEndpoints.webOrigin

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
        // The 2026-09-28 security pass — the capability travels sealed under a
        // link secret that rides in the fragment.
        val mint = composeGroupInviteCapability(identity.ed25519Seed(), groupId, 0uL)
        return "$origin$URL_PREFIX${base64UrlEncode(mint.sealed)}#${base64UrlEncode(mint.linkSecret)}"
    }

    /** Poll the PublisherGroup's DS journal and apply incoming MLS messages
     *  (external-join proposals from prospective co-publishers + any co-publisher
     *  commits) via the role-gated path. Surfaces external-join proposals into
     *  the PublisherGroup's retained pending set, reading both the journal and
     *  (ADR-0053) the knock queue. Non-MLS rows and our own already-merged
     *  commits throw and are skipped.
     *  Returns how many entries were applied (the caller snapshots the
     *  provider when that is non-zero — an applied commit advanced the
     *  group's epoch) and the number of pending requests after the poll. */
    public suspend fun pollJournal(
        ds: DsClient,
        identity: Identity,
        publisherGroup: PublisherGroup,
    ): JournalPoll {
        val groupId = publisherGroup.groupId()
        // ADR-0053 — the journal is claimed: every read carries a proof under
        // the current epoch's key, and a poll that advanced the epoch
        // re-registers the reached key (heal on apply).
        val signerBefore = signerOf(publisherGroup, identity)
        val raw = ds.fetchGroup(groupId, after = 0L, waitSecs = 0, signer = signerBefore)
        var applied = 0
        for (m in raw.messages) {
            try {
                publisherGroup.processIncomingVerifyingRoles(identity, m.payload)
                applied += 1
            } catch (_: Throwable) {
                // A commit we have already merged (epoch mismatch) or a non-MLS
                // row — skip; the FIFO journal catches up on any genuinely-new
                // commit on the next poll.
            }
        }
        healRegistration(signerBefore, signerOf(publisherGroup, identity))?.let { (next, current) ->
            try {
                ds.claimGroup(groupId, next, provenBy = current)
            } catch (e: kotlin.coroutines.cancellation.CancellationException) {
                throw e
            } catch (_: Throwable) {
                // Best effort: someone registered first, or this device lags.
            }
        }
        // ADR-0053 client contract point 7 — what prospective co-publishers
        // knocked: their external-join proposal (and attribution) never enter
        // the journal. Read from the start: the quarantine de-duplicates, and a
        // proposal for a past epoch is refused by the epoch check.
        try {
            val knocks = ds.fetchKnocks(groupId, after = 0L, waitSecs = 0, signer = signerOf(publisherGroup, identity))
            // Through the core's knock gate only (see `KnockGate.kt`): a
            // knock is an unauthenticated write, so a knocked commit or app
            // message must be refused before it is processed, never applied.
            // Attribution rows, stale proposals and garbage are refused too.
            val drained = me.lakko.app.chat.drainKnockRows(knocks.messages, 0L) { bytes ->
                publisherGroup.processKnock(identity, bytes)
            }
            // ADR-0058 §6 — keep each joiner's reply key from its attribution.
            for (row in knocks.messages) {
                val attr = try {
                    uniffi.lakko_ffi_uniffi.parseGroupJoinAttribution(row.payload)
                } catch (_: Throwable) {
                    continue
                }
                if (!attr.groupId.contentEquals(groupId)) continue
                if (attr.replyGrant.`class` != uniffi.lakko_ffi_uniffi.FfiReachClass.REPLY) continue
                joinerReplyGrants[me.lakko.app.reach.reachHex(attr.joinerSignatureKey)] = attr.replyGrant
            }
            applied += drained.accepted.count {
                it.second is uniffi.lakko_ffi_uniffi.ProcessedMessage.ExternalJoinProposal
            }
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (_: Throwable) {
            // The knock read is best effort; the next poll retries.
        }
        return JournalPoll(applied = applied, pending = publisherGroup.pendingExternalJoins().size)
    }

    /** ADR-0053 — the PublisherGroup journal's signer at its current epoch,
     *  or `null` when the handle cannot derive one. */
    public fun signerOf(publisherGroup: PublisherGroup, identity: Identity): DsJournalSigner? =
        try {
            FfiDsJournalSigner(publisherGroup.dsAccessSigner(identity))
        } catch (_: Throwable) {
            null
        }

    /**
     * ADR-0053 client contract point 2 — claim the PublisherGroup journal at
     * its current epoch (at creation this is the first registration; on every
     * later open an idempotent re-send). Best effort: a failure is logged and
     * the next open retries.
     */
    public suspend fun claimJournal(ds: DsClient, identity: Identity, publisherGroup: PublisherGroup) {
        val signer = signerOf(publisherGroup, identity) ?: return
        try {
            ds.claimGroup(publisherGroup.groupId(), signer)
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (e: Throwable) {
            android.util.Log.w("CoPublisher", "ADR-0053: publisher journal claim did not land", e)
        }
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
     *  PublisherGroup epoch), registers the new epoch's journal key and
     *  broadcasts the commit (ADR-0053), sends the Welcome to the joiner's own
     *  mailbox (the not-yet-member joiner polls it), then RESEALS the
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
        val signerBeforeCommit = signerOf(publisherGroup, identity)
        val added = publisherGroup.approveExternalJoin(identity, proposalRef)
        // ADR-0053 client contract point 3 — register epoch N+1's key proven
        // under N's before the commit is published. A refusal throws: the
        // caller's `withIdentity` then persists nothing, so the durable group
        // stays at N and nothing reached the wire.
        val commitSigner = registerBeforePublish(
            before = signerBeforeCommit,
            after = signerOf(publisherGroup, identity),
        ) { next, current -> ds.registerNextEpoch(groupId, next, current) }
        ds.publishGroup(groupId, added.commit, signer = commitSigner)
        // ADR-0053 client contract point 8 — the Welcome goes to the joiner's
        // own mailbox (the pseudonym of the key its proposal carried), in the
        // same envelope a direct add uses; nothing is written to the journal
        // for a non-member to read.
        val joinerMailbox = IdentityPseudonym.fromIdentity(proposalRef).use { it.asBytes() }
        // ADR-0058 §6 — written with the reply key the joiner's attribution
        // carried: its mailbox is claimed and closed.
        val reach = joinerReplyGrants[me.lakko.app.reach.reachHex(proposalRef)]
            ?.let { me.lakko.app.ds.DsReachAuth(me.lakko.app.ds.reachSigner(it.seed)) }
        ds.publishMailbox(joinerMailbox, wrapEnvelope(KIND_WELCOME, added.welcome), reach = reach)
        resealSubscribers(channelsClient, identity, publisherGroup, channelIdHex)
    }

    /** Reject a pending co-publisher — no commit, no epoch advance, nothing on
     *  the journal. The retained request set lives in the provider's MLS
     *  storage, so the drop is written there through [identity]; the caller
     *  persists the provider snapshot afterwards or the request is back after
     *  a restart. */
    public fun reject(identity: Identity, publisherGroup: PublisherGroup, proposalRef: ByteArray) {
        publisherGroup.rejectExternalJoin(identity, proposalRef)
    }

    // -----------------------------------------------------------------
    // Joiner side — a prospective co-publisher requests + joins
    // -----------------------------------------------------------------

    /** When this process last sent a co-publisher request, or `0`. */
    @Volatile
    private var lastCoPublishRequestMs: Long = 0L

    /**
     * ADR-0053 — a co-publisher's Welcome now lands in the account mailbox
     * the contacts inbox also reads, and the inbox cannot open it (it is sealed
     * to the channel identity's KeyPackage, not a chat one). While a request
     * is recent the inbox therefore stays quiet about a Welcome it could not
     * join, instead of telling the user a chat failed to open.
     */
    public fun awaitingCoPublishWelcome(nowMs: Long = System.currentTimeMillis()): Boolean =
        lastCoPublishRequestMs != 0L && nowMs - lastCoPublishRequestMs < CO_PUBLISH_WELCOME_WINDOW_MS

    private const val CO_PUBLISH_WELCOME_WINDOW_MS: Long = 60L * 60L * 1000L

    /** Handles a [requestToCoPublish] returns so the caller can drive the poll
     *  loop ([pollForCoPublishWelcome]). */
    public data class CoPublishRequest(
        /** The PublisherGroup's MLS `group_id` (the journal channel to poll). */
        val groupId: ByteArray,
        /** The 32-char lowercase-hex channel id recovered from the `group_id`. */
        val channelIdHex: String,
        /** This identity's signature key — the one its proposal carried, whose
         *  mailbox the owner's Welcome is sent to. */
        val joinerKey: ByteArray,
    )

    /** ADR-0028 OQ3 — joiner side: request to become a co-publisher of a channel
     *  from a `/g/` co-publisher link. Decodes + verifies the capability,
     *  recovers the channel id from the PublisherGroup `group_id`, and knocks
     *  (ADR-0053 §4): an empty knock learns the epoch, a second carries a
     *  self-signed join-attribution (OQ2) and the external-join proposal
     *  (ADR-0027) for it. The journal is never read. The caller then polls
     *  with [pollForCoPublishWelcome]. */
    public suspend fun requestToCoPublish(
        ds: DsClient,
        identity: Identity,
        link: String,
    ): CoPublishRequest {
        val idx = link.lastIndexOf(URL_PREFIX)
        if (idx < 0) throw CoPublisherException.NotACoPublisherLink(link)
        val (slug, fragment) = me.lakko.app.discovery.splitLinkToken(link.substring(idx + URL_PREFIX.length))
        val sealed = base64UrlDecode(slug)
            ?: throw CoPublisherException.NotACoPublisherLink(link)
        val secret = fragment?.let(::base64UrlDecode)
            ?: throw CoPublisherException.NotACoPublisherLink(link)
        val cap = try {
            GroupInviteCapability.openLink(sealed, secret)
        } catch (_: Throwable) {
            throw CoPublisherException.NotACoPublisherLink(link)
        }
        val wire = cap.wire()
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
        // ADR-0053 client contract point 6 — the PublisherGroup journal is
        // claimed, so a prospective co-publisher never reads it: an empty knock
        // learns the epoch, a second knock carries the self-signed attribution
        // (OQ2, advisory) and the proposal built for that epoch. The Welcome
        // arrives in this identity's own mailbox ([pollForCoPublishWelcome]).
        // A group whose owner has not claimed it yet refuses with
        // [GroupNotAcceptingRequests]; nothing was stored.
        val replyGrant = registerReplyKey(ds, identity)
        runLinkJoinKnock(
            knock = { payloads ->
                ds.knock(groupId, payloads).epoch
            },
            buildProposal = { epoch -> identity.createExternalJoinProposal(groupId, epoch) },
            // ADR-0058 §6 — the reply key the owner writes the Welcome with,
            // registered on this identity's own mailbox before the knock.
            buildAttribution = { composeGroupJoinAttribution(identity.ed25519Seed(), wire, replyGrant) },
        )
        lastCoPublishRequestMs = System.currentTimeMillis()
        return CoPublishRequest(
            groupId = groupId,
            channelIdHex = channelIdHex,
            joinerKey = identity.signaturePublicKey(),
        )
    }

    /** ADR-0028 OQ3 — joiner side: read this identity's own mailbox (ADR-0053)
     *  for the Welcome the owner sent after approving, join from it, and mint local
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
        // ADR-0053 client contract point 8 — the owner's approve sends the
        // Welcome to THIS identity's mailbox (the pseudonym of the key the
        // proposal carried), never to the journal. The mailbox is claimed, so
        // the read is signed under the identity key. Paged to the end: the
        // Welcome is among the newest rows.
        val mailboxSigner = dsMailboxSigner(identity)
        val pseudonym = mailboxSigner.mailboxPseudonym()
        val signer = FfiDsJournalSigner(mailboxSigner)
        val candidates = ArrayList<ByteArray>()
        JournalHeadEpoch.scan<Unit>(
            fetchPage = { after, limit ->
                val raw = ds.fetchMailbox(pseudonym, after = after, limit = limit, waitSecs = 0, signer = signer)
                JournalHeadEpoch.Page(
                    rows = raw.messages.map { JournalHeadEpoch.Row(it.id, it.payload) },
                    hasMore = raw.hasMore,
                )
            },
            visit = { row ->
                val env = parseEnvelope(row.payload)
                if (env is ParsedEnvelope.Welcome) candidates += env.body
                null
            },
        )
        // Join the PublisherGroup from the Welcome — this persists the joined
        // MLS state under the deterministic PublisherGroup `group_id` in the
        // identity's provider. The handle itself is throwaway (close it);
        // freshPublisherKeys re-opens the now-joined PublisherGroup by
        // channel id and derives the shared content-key seed + a fresh
        // per-device signing key so this device can publish. Newest first,
        // and a Welcome this identity cannot open (a chat Welcome sharing
        // the mailbox, or one sealed to a KeyPackage it no longer holds) is
        // skipped rather than ending the wait.
        var joined = false
        for (welcome in candidates.asReversed()) {
            try {
                Group.joinFromWelcome(welcome, identity).close()
                joined = true
                break
            } catch (_: Throwable) {
                // try an older candidate
            }
        }
        if (!joined) return null
        return ChannelsComposer.freshPublisherKeys(
            identity, channelIdHex, existingSigningSeed)
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
    // ADR-0058 §6 — reply keys
    // -----------------------------------------------------------------

    /** Each pending joiner's reply grant, by its leaf key (hex). SECRET;
     *  process memory only — a restart re-reads the retained knocks. */
    private val joinerReplyGrants =
        java.util.concurrent.ConcurrentHashMap<String, uniffi.lakko_ffi_uniffi.FfiReachGrant>()

    /**
     * A `reply` key on [identity]'s own mailbox, registered (with its seven-day
     * expiry) before the request that carries it leaves. [identity] is the
     * channel publisher identity whose mailbox the owner's Welcome goes to.
     */
    private suspend fun registerReplyKey(
        ds: DsClient,
        identity: Identity,
    ): uniffi.lakko_ffi_uniffi.FfiReachGrant {
        val scope = ByteArray(me.lakko.app.reach.REPLY_SCOPE_LEN).also { java.security.SecureRandom().nextBytes(it) }
        val grant = uniffi.lakko_ffi_uniffi.deriveReachGrant(
            identity.ed25519Seed(),
            uniffi.lakko_ffi_uniffi.FfiReachClass.REPLY,
            scope,
            0u,
        )
        val mailboxSigner = dsMailboxSigner(identity)
        ds.putMailboxReach(
            pseudonym = mailboxSigner.mailboxPseudonym(),
            signer = FfiDsJournalSigner(mailboxSigner),
            add = listOf(
                me.lakko.app.ds.DsReachKeyAdd(
                    uniffi.lakko_ffi_uniffi.reachVerifyingKey(grant.seed),
                    me.lakko.app.reach.replyExpiry(System.currentTimeMillis()),
                ),
            ),
            remove = emptyList(),
        )
        return grant
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
