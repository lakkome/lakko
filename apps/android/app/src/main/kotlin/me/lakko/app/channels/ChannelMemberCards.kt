// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.util.Log
import java.util.concurrent.ConcurrentHashMap
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import me.lakko.app.reach.GrantVenue
import me.lakko.app.reach.ReachKind
import me.lakko.app.reach.ReachManager
import me.lakko.app.reach.acceptReceivedGrant
import me.lakko.app.reach.reachHex
import uniffi.lakko_ffi_uniffi.FfiProfile
import uniffi.lakko_ffi_uniffi.FfiReachClass
import uniffi.lakko_ffi_uniffi.FfiReachGrant

/*
 * ADR-0060 section 3 - channels list their members to their members. The
 * Android twin of `apps/web/src/channels/member-cards.ts`.
 *
 * Every member (subscribers and publishers alike) uploads a member card: its
 * profile (v3) sealed under the channel's CURRENT epoch seed, stored under
 * the subscriber key it registered, and signed by that key
 * (`x-lakko-member-card-sig`). The server keeps ciphertext only. A member
 * lists the cards, opens each with the seed of the epoch it names (opening
 * verifies the card's account binding), and reads the profile.
 *
 * A card may carry the author's `channel` reach grant. A reader keeps such a
 * grant ONLY from a card, filed under the account key the card's binding
 * verified under, and only after the reach check showed the grant is
 * registered on that account's mailbox. A card without the grant withdraws
 * one held from before. A card whose claimed account id the directory
 * publishes a DIFFERENT key for names someone else and is dropped; an account
 * id the directory cannot resolve does not drop it (the binding verified).
 *
 * The pure decisions are in `MemberCardRules.kt`; the reader runs over
 * [MemberCardPorts] so its open / refuse paths are host-testable.
 */

/** One row of a channel's member list. */
public data class ChannelMemberRow(
    /** Stable row key: the author's account key hex, else the subscriber key. */
    val key: String,
    val name: String,
    /** Shown only when the member lets this channel's members find them (the
     *  card carried a `channel` grant this device verified and keeps). */
    val username: String?,
    val isSelf: Boolean,
)

/** What reading the member list came back with. */
public data class ChannelMembersResult(
    val rows: List<ChannelMemberRow>,
    /** Cards this device could not open (no seed for their epoch, broken, or
     *  naming someone else). */
    val unreadable: Int,
    /** More pages exist than were read. */
    val truncated: Boolean,
)

/** An opened card: the account key its binding verified under, and the
 *  profile envelope it carries. */
public class OpenedCard(public val identityPk: ByteArray, public val profile: ByteArray)

/** What reading the member list asks the outside world. */
public interface MemberCardPorts {
    public suspend fun listPage(channelIdHex: String, after: String?): MemberCardPage

    /** `memberCardEpoch`; throws on a malformed card. */
    public fun cardEpoch(card: ByteArray): UInt

    /** `openMemberCard`; throws when the card does not open or its binding
     *  does not verify. */
    public fun openCard(seed: ByteArray, card: ByteArray, channelId: ByteArray, subscriberPk: ByteArray): OpenedCard

    /** `decodeProfileEnvelope`; throws on a malformed profile. */
    public fun decodeProfile(bytes: ByteArray): FfiProfile

    /** The account key hex the directory publishes for [accountId], `null`
     *  for an unknown account or when the directory could not answer. */
    public suspend fun resolveAccountKeyHex(accountId: ByteArray): String?

    /** The DS reach check of [accountKey]'s mailbox with [grant]; `null`
     *  when it could not answer. */
    public suspend fun reachCheck(accountKey: ByteArray, grant: FfiReachGrant): Boolean?

    public suspend fun fileChannelGrant(accountKey: ByteArray, grant: FfiReachGrant, channelIdHex: String)

    public suspend fun withdrawChannelGrant(accountKey: ByteArray, channelIdHex: String)

    public fun holdsChannelGrant(accountKey: ByteArray, channelIdHex: String): Boolean

    /** This account's key hex, `null` without a reach session. */
    public val ownAccountKeyHex: String?

    /** A reach session is bound. */
    public val reachAvailable: Boolean
}

private fun hexToBytes(hex: String): ByteArray =
    ByteArray(hex.length / 2) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }

/**
 * Read a channel's member list: every card this device can open, as rows,
 * filing or withdrawing the `channel` grants the cards carry on the way. Rows
 * are deduplicated per account (a member with several devices holds several
 * subscriber keys). Throws only when the first page cannot be read.
 */
public suspend fun loadChannelMembers(
    channelIdHex: String,
    seedForEpoch: (UInt) -> ByteArray?,
    ownSubscriberPkHex: String?,
    ports: MemberCardPorts,
    maxPages: Int = 10,
): ChannelMembersResult {
    val channelId = hexToBytes(channelIdHex)
    val rows = mutableListOf<ChannelMemberRow>()
    val seen = HashSet<String>()
    var unreadable = 0
    var after: String? = null
    var truncated = false
    for (page in 0 until maxPages) {
        val res = ports.listPage(channelIdHex, after)
        for (m in res.members) {
            val pkHex = reachHex(m.subscriberPk)
            val opened: OpenedCard
            val profile: FfiProfile
            try {
                val seed = seedForEpoch(ports.cardEpoch(m.card))
                if (seed == null) {
                    unreadable += 1
                    continue
                }
                opened = ports.openCard(seed, m.card, channelId, m.subscriberPk)
                profile = ports.decodeProfile(opened.profile)
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                unreadable += 1
                continue
            }
            val row = readCard(channelIdHex, pkHex, ownSubscriberPkHex, opened.identityPk, profile, ports)
            if (row == null) {
                unreadable += 1
                continue
            }
            if (!seen.add(row.key)) continue
            rows += row
        }
        after = res.nextAfter
        if (after == null) break
        if (page == maxPages - 1) truncated = true
    }
    // The viewer first, then everyone else in name order.
    val sorted = rows.sortedWith(compareBy<ChannelMemberRow> { !it.isSelf }.thenBy { it.name })
    return ChannelMembersResult(sorted, unreadable, truncated)
}

private suspend fun readCard(
    channelIdHex: String,
    pkHex: String,
    ownSubscriberPkHex: String?,
    identityPk: ByteArray,
    profile: FfiProfile,
    ports: MemberCardPorts,
): ChannelMemberRow? {
    val identityPkHex = reachHex(identityPk)
    val isOwnCard = ownSubscriberPkHex != null && pkHex == ownSubscriberPkHex
    // The card is filed under the account key its binding verified under,
    // never under the account id the profile claims. A claimed account id the
    // directory publishes a different key for is a card naming someone else.
    val accountId = profile.accountId
    if (!isOwnCard && accountId != null) {
        val published = ports.resolveAccountKeyHex(accountId)
        if (published != null && published != identityPkHex) return null
    }
    val senderHex = if (isOwnCard) null else identityPkHex
    val own = ports.ownAccountKeyHex
    val isSelf = isOwnCard || (senderHex != null && own != null && senderHex == own)
    val channelGrant = profile.grants.firstOrNull { it.`class` == FfiReachClass.CHANNEL }
    var username: String? = null
    if (ports.reachAvailable && !isSelf) {
        val verified: Boolean? = if (channelGrant != null && senderHex != null) {
            try {
                ports.reachCheck(identityPk, channelGrant)
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }
        val decision = cardGrantDecision(
            isOwnCard = isOwnCard,
            senderAccountKeyHex = senderHex,
            ownAccountKeyHex = own,
            carriesChannelGrant = channelGrant != null,
            grantVerified = verified,
        )
        if (decision == CardGrantDecision.FILE && channelGrant != null && own != null &&
            acceptReceivedGrant(
                kind = ReachKind.CHANNEL,
                venue = GrantVenue.CHANNEL_CARD,
                senderKeyHex = senderHex,
                selfKeyHex = own,
                otherParticipantKeyHex = null,
                senderIsMember = true,
            )
        ) {
            ports.fileChannelGrant(identityPk, channelGrant, channelIdHex)
        } else if (decision == CardGrantDecision.WITHDRAW && senderHex != null) {
            ports.withdrawChannelGrant(identityPk, channelIdHex)
        }
        // UX 6.2: the username goes with the permission - shown only while the
        // card carries the grant AND this device keeps it for this channel.
        if (channelGrant != null && senderHex != null && ports.holdsChannelGrant(identityPk, channelIdHex)) {
            username = profile.username
        }
    }
    return ChannelMemberRow(
        // Every device of the viewer's own account collapses into one row.
        key = if (isSelf) "self" else senderHex ?: "pk:$pkHex",
        name = memberRowName(profile.displayName, isSelf),
        username = username,
        isSelf = isSelf,
    )
}

/** How an own-card upload ended. */
public enum class OwnCardOutcome {
    /** Uploaded now. */
    UPLOADED,

    /** Already uploaded for this epoch with this content. */
    UNCHANGED,

    /** The channel does not know this key: register it and try again. */
    NOT_REGISTERED,

    /** The server holds a newer card than this epoch. */
    STALE,
    FAILED,
}

/** The production member-card wiring: the channels client, the core, the
 *  directory, the DS and the reach manager. */
public object ChannelMemberCards {
    private const val LOG_TAG = "ChannelMemberCards"

    /** `channelIdHex|subscriberPkHex` -> the card this process uploaded
     *  (memory only: a restart re-uploads once). */
    private val uploaded = ConcurrentHashMap<String, UploadedCard>()

    /** Per-channel serialisation of the own card's upload. */
    private val uploadLocks = ConcurrentHashMap<String, Mutex>()

    /** Account id hex -> account key hex the directory returned (a network
     *  failure is not cached). */
    private val accountKeys = ConcurrentHashMap<String, String>()

    /** `accountKeyHex|channelIdHex|grantHex` -> reach check result. */
    private val verifiedGrants = ConcurrentHashMap<String, Boolean>()

    /** The own card's inputs. */
    public class OwnCardInput(
        public val channelIdHex: String,
        public val keys: ChannelSubscriberKeys,
        public val epoch: UInt,
        public val seed: ByteArray,
        public val displayName: String?,
        public val username: String?,
        public val updatedAtMs: Long,
        public val accountId: ByteArray?,
        public val usernameCertificate: ByteArray?,
    )

    /** Upload this device's card for [input]'s epoch when one is due. Never
     *  throws (a cancellation is let through). */
    public suspend fun ensureOwnCard(
        reach: ReachManager,
        client: ChannelsClient,
        input: OwnCardInput,
    ): OwnCardOutcome {
        val lock = uploadLocks.getOrPut(input.channelIdHex) { Mutex() }
        return lock.withLock { uploadOwnCard(reach, client, input) }
    }

    private suspend fun uploadOwnCard(reach: ReachManager, client: ChannelsClient, input: OwnCardInput): OwnCardOutcome =
        try {
            val channelId = hexToBytes(input.channelIdHex)
            val reachAvailable = reach.ownAccountKey() != null
            val content = cardContent(
                displayName = input.displayName,
                username = input.username,
                channelAllowed = reach.channelVisible(input.channelIdHex),
                reachAvailable = reachAvailable,
            )
            var grant: FfiReachGrant? = null
            if (content.includeGrant) {
                reach.raiseChannelVersion(input.channelIdHex, input.epoch.toInt())
                grant = reach.grant(ReachKind.CHANNEL, channelId)
            }
            val pkHex = reachHex(input.keys.publicKey)
            val key = "${input.channelIdHex}|$pkHex"
            val fingerprint = cardFingerprint(content, input.updatedAtMs, grant?.version)
            if (!cardUploadDue(uploaded[key], input.epoch, fingerprint)) {
                OwnCardOutcome.UNCHANGED
            } else {
                val profile = uniffi.lakko_ffi_uniffi.composeProfileEnvelope(
                    FfiProfile(
                        displayName = content.displayName,
                        username = content.username,
                        updatedAtMs = input.updatedAtMs.coerceAtLeast(0L).toULong(),
                        accountId = input.accountId,
                        usernameCertificate = if (content.username == null) null else input.usernameCertificate,
                        grants = listOfNotNull(grant),
                    ),
                )
                // The card is bound to the account: without a reach session
                // there is no account seed to sign with, and a reader drops an
                // unbound card.
                val card = reach.sealOwnMemberCard(input.seed, input.epoch, channelId, input.keys.publicKey, profile)
                profile.fill(0)
                if (card == null) {
                    OwnCardOutcome.FAILED
                } else {
                    val signature = uniffi.lakko_ffi_uniffi.ChannelSubscriberKeypair.fromSecret(input.keys.secret)
                        .use { it.memberCardRequestSignature(channelId, card) }
                    try {
                        client.putMemberCard(
                            channelIdHex = input.channelIdHex,
                            subscriberPubkeyHex = pkHex,
                            card = card,
                            signatureHeaderName = uniffi.lakko_ffi_uniffi.memberCardSigHeaderName(),
                            signature = signature,
                        )
                        uploaded[key] = UploadedCard(input.epoch, fingerprint)
                        OwnCardOutcome.UPLOADED
                    } catch (err: ChannelsApiException) {
                        when (err.code) {
                            "unknown-subscriber" -> OwnCardOutcome.NOT_REGISTERED
                            "member-card-stale" -> OwnCardOutcome.STALE
                            else -> {
                                Log.w(LOG_TAG, "member card upload refused (${err.code})")
                                OwnCardOutcome.FAILED
                            }
                        }
                    }
                }
            }
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "member card upload failed: ${cause.javaClass.simpleName}")
            OwnCardOutcome.FAILED
        }

    /**
     * Keep this device's card current (web `useOwnMemberCard`): upload when
     * due, and when the channel does not know the key, register it once and
     * try again.
     */
    public suspend fun keepOwnCardCurrent(reach: ReachManager, client: ChannelsClient, input: OwnCardInput) {
        val outcome = ensureOwnCard(reach, client, input)
        if (outcome != OwnCardOutcome.NOT_REGISTERED) return
        try {
            client.subscribe(input.channelIdHex, reachHex(input.keys.publicKey))
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            return
        }
        ensureOwnCard(reach, client, input)
    }

    /**
     * The own-card inputs for [channelIdHex] at [epoch] with [seed], from the
     * bound account's own profile and the subscriber key this device
     * registered, or `null` when either is missing.
     */
    public fun ownCardInput(
        context: android.content.Context,
        state: ChannelsUiState,
        channelIdHex: String,
        epoch: UInt,
        seed: ByteArray,
    ): OwnCardInput? {
        val keys = state.subscriberKeysByChannel.value[channelIdHex] ?: return null
        val accountIdB64 = state.boundAccount ?: return null
        val accountId = try {
            java.util.Base64.getDecoder().decode(accountIdB64)
        } catch (_: IllegalArgumentException) {
            return null
        }
        val own = me.lakko.app.profile.OwnProfileStore.load(context, accountId)
        val username = own?.username?.takeIf { it.isNotEmpty() }
        return OwnCardInput(
            channelIdHex = channelIdHex,
            keys = keys,
            epoch = epoch,
            seed = seed,
            displayName = own?.displayName,
            username = username,
            updatedAtMs = own?.updatedAtMs ?: 0L,
            accountId = accountId.takeIf { it.size == 16 },
            usernameCertificate = username?.let { ReachManager.get(context).usernameCertificate() },
        )
    }

    /** A reader's own card, for the newest epoch it holds the seed of. Best
     *  effort. */
    public suspend fun refreshReaderCard(
        context: android.content.Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        channelIdHex: String,
    ) {
        val epoch = state.epochByChannel.value[channelIdHex] ?: return
        val seed = state.subscribedSeeds(channelIdHex)[epoch] ?: return
        val input = ownCardInput(context, state, channelIdHex, epoch, seed) ?: return
        keepOwnCardCurrent(ReachManager.get(context), client, input)
    }

    /**
     * A publisher's own card (web `ensureCardKey`): publishers upload a card
     * like anyone else, under a subscriber key registered on the channel.
     * Registers one when this device holds none, then uploads for the
     * current epoch. Best effort.
     */
    public suspend fun refreshPublisherCard(
        context: android.content.Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        channelIdHex: String,
    ) {
        val epoch = state.epochByChannel.value[channelIdHex] ?: return
        val seed = state.keysByChannel.value[channelIdHex]?.seed ?: return
        if (state.subscriberKeysByChannel.value[channelIdHex] == null) {
            val fresh = ChannelsComposer.freshSubscriberKeys()
            state.setSubscriberKeys(channelIdHex, fresh)
            try {
                client.subscribe(channelIdHex, reachHex(fresh.publicKey))
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                // The card upload re-registers on `unknown-subscriber`.
            }
        }
        val input = ownCardInput(context, state, channelIdHex, epoch, seed) ?: return
        keepOwnCardCurrent(ReachManager.get(context), client, input)
    }

    /** The seed a member card of [epoch] opens with: the publisher's current
     *  seed for the current epoch, else a subscriber seed this device holds. */
    public fun seedForEpoch(state: ChannelsUiState, channelIdHex: String, epoch: UInt): ByteArray? {
        val current = state.epochByChannel.value[channelIdHex]
        val publisherSeed = state.keysByChannel.value[channelIdHex]?.seed
        if (publisherSeed != null && epoch == current) return publisherSeed
        return state.subscribedSeeds(channelIdHex)[epoch]
    }

    /** Forget what this process uploaded for [channelIdHex]: the server
     *  removed the card with the subscription, so a later subscription
     *  uploads afresh. */
    public fun forgetUploads(channelIdHex: String) {
        uploaded.keys.removeAll { it.startsWith("$channelIdHex|") }
    }

    /** The production [MemberCardPorts]. */
    public fun ports(reach: ReachManager, client: ChannelsClient): MemberCardPorts = object : MemberCardPorts {
        override suspend fun listPage(channelIdHex: String, after: String?): MemberCardPage =
            client.listMemberCards(channelIdHex, after)

        override fun cardEpoch(card: ByteArray): UInt = uniffi.lakko_ffi_uniffi.memberCardEpoch(card)

        override fun openCard(seed: ByteArray, card: ByteArray, channelId: ByteArray, subscriberPk: ByteArray): OpenedCard {
            val opened = uniffi.lakko_ffi_uniffi.openMemberCard(seed, card, channelId, subscriberPk)
            return OpenedCard(opened.identityPk, opened.profile)
        }

        override fun decodeProfile(bytes: ByteArray): FfiProfile =
            uniffi.lakko_ffi_uniffi.decodeProfileEnvelope(bytes)

        override suspend fun resolveAccountKeyHex(accountId: ByteArray): String? {
            val idHex = reachHex(accountId)
            accountKeys[idHex]?.let { return it }
            return try {
                val key = me.lakko.app.discovery.DiscoveryClient().use { d ->
                    d.identityFetch(accountId)?.signature_pubkey
                }?.let { java.util.Base64.getDecoder().decode(it) } ?: return null
                reachHex(key).also { accountKeys[idHex] = it }
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                // Not cached: a network failure is not "unknown account".
                null
            }
        }

        override suspend fun reachCheck(accountKey: ByteArray, grant: FfiReachGrant): Boolean? {
            val cacheKey = try {
                "${reachHex(accountKey)}|${reachHex(uniffi.lakko_ffi_uniffi.encodeReachGrant(grant))}"
            } catch (_: Throwable) {
                return null
            }
            verifiedGrants[cacheKey]?.let { return it }
            return try {
                // ADR-0074 3.5a rule 1: the person's CURRENT mailbox.
                val mailbox = reach.mailboxOf(accountKey)
                val ok = me.lakko.app.ds.DsClient().use { ds ->
                    ds.checkReach(mailbox, me.lakko.app.ds.reachSigner(grant.seed))
                }
                verifiedGrants[cacheKey] = ok
                ok
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                null
            }
        }

        override suspend fun fileChannelGrant(accountKey: ByteArray, grant: FfiReachGrant, channelIdHex: String) {
            reach.fileHeld(accountKey, grant, channelIdHex)
        }

        override suspend fun withdrawChannelGrant(accountKey: ByteArray, channelIdHex: String) {
            reach.withdrawHeld(accountKey, ReachKind.CHANNEL, channelIdHex)
        }

        override fun holdsChannelGrant(accountKey: ByteArray, channelIdHex: String): Boolean =
            reach.holdsHeld(accountKey, ReachKind.CHANNEL, channelIdHex)

        override val ownAccountKeyHex: String? = reach.ownAccountKey()?.let(::reachHex)

        override val reachAvailable: Boolean = reach.ownAccountKey() != null
    }
}
