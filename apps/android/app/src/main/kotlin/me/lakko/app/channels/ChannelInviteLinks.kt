// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.util.Base64
import uniffi.lakko_ffi_uniffi.ChannelInviteSigner
import uniffi.lakko_ffi_uniffi.FfiChannelAccess
import uniffi.lakko_ffi_uniffi.FfiSubscriberInvite

/**
 * ADR-0087 sections 7 and 14.2 — a channel's subscriber invite links (`/g/`
 * preview kind 3), on both ends.
 *
 * A public channel's link carries its read key and needs no invite key. A
 * private channel's link carries a fresh invite seed, whose key is registered
 * at the channels service (the door: a subscribe needs a proof by a registered
 * key), and the channel's admission public key a ticket is sealed to.
 */
public object ChannelInviteLinks {

    /** Mint, upload to the group-invite ledger, and answer the link URL. */
    public suspend fun mint(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        channel: ChannelHandle,
        public: Boolean,
        /** Section 14.1 — a private channel's per-person invitation. */
        oneUse: Boolean = false,
        /** Section 14.2 — the owner's label for the link, or empty. */
        label: String = "",
        /** Section 14.2 — when the link stops working (unix ms), `0` never. */
        expiryMs: Long = 0L,
        /** Section 14.2 — the use limit (`0` one use, `0xFFFFFFFF` none);
         *  `null` follows [oneUse]. */
        maxUses: Long? = null,
    ): String = mintFull(context, state, client, channel, public, oneUse, label, expiryMs, maxUses).url

    /** [mint], and the invitation card that carries the same link in a chat
     *  (section 14). */
    public suspend fun mintFull(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        channel: ChannelHandle,
        public: Boolean,
        oneUse: Boolean = false,
        /** Section 14.2 — the owner's label for a link; empty for an
         *  invitation card (section 15.1 then reads `From an invitation`). */
        label: String = "",
        /** Section 14.2 — when the link stops working (unix ms), `0` never. */
        expiryMs: Long = 0L,
        /** Section 14.2 — the use limit (`0` one use, `0xFFFFFFFF` none);
         *  `null` follows [oneUse]. */
        maxUses: Long? = null,
    ): ChannelLinkMint {
        val uses = maxUses ?: if (oneUse && !public) 0L else UNLIMITED_USES
        val expiry = expiryMs.coerceAtLeast(0L)
        val publisher = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
        val account = state.boundAccount ?: throw IllegalStateException("no account bound")
        val shared = channel.sharedMeta()
        val name = shared?.name?.takeIf { it.isNotBlank() } ?: channel.localName.ifBlank { channel.handle }
        val description = shared?.description ?: channel.localDescription
        val avatar = shared?.fullRecord()?.avatar
        val channelId = ChannelsComposer.channelIdFromHex(channel.id)
        val inviteSeed: ByteArray? = if (public) null else uniffi.lakko_ffi_uniffi.generateChannelInviteSeed()
        try {
            var admissionVersionUsed: UShort = 0u
            val access = if (inviteSeed == null) {
                val readKey = if (state.joinedGroupIdOf(channel.id) == null) {
                    publisher.withIdentity(persist = false) { ChannelDiscovery.readKey(it, channel.id) }
                } else {
                    ChannelReadKeys.get(context, account, channel.id)
                        ?: throw IllegalStateException("this device does not hold the channel's read key")
                }
                FfiChannelAccess.Public(readKey)
            } else {
                // Security pass F1 (2026-11-15) - the seed this device holds.
                // Version 0 is minted by New channel (or a switch to private
                // made on this device). A device without one, owner or
                // co-publisher, waits for the owner account's register: it
                // never mints (a second owner device's seed would orphan
                // tickets sealed to the register's) and never marks the
                // service's rows as readers (any of them could be planted).
                // The pre-phase-B repair that minted here is gone (no channel
                // of that age is kept: nothing is deployed).
                val (version, admissionSeed) = admissionSeedFor(
                    reason = AdmissionSeedReason.USE,
                    held = { ChannelAdmissionKeys.current(context, account, channel.id) },
                    mint = { error("a use never mints") },
                    markRowsAsReaders = {},
                ) ?: throw ChannelLinkNeedsOwner()
                admissionVersionUsed = version
                try {
                    // The door (after the seed is known, so a refusal above
                    // registers nothing): the link's invite key, no expiry, no
                    // use limit (the ledger row carries the same rule).
                    ChannelInviteSigner.fromInviteSeed(inviteSeed).use { signer ->
                        ChannelPublisherSigners.derive(publisher, channel.id).use {
                            client.putInviteKey(channel.id, hex(signer.verifyingKey()), expiry, uses, it)
                        }
                    }
                    FfiChannelAccess.Private(
                        inviteSeed,
                        uniffi.lakko_ffi_uniffi.channelAdmissionPublicKey(admissionSeed),
                        version,
                    )
                } finally {
                    admissionSeed.fill(0)
                }
            }
            val mint = publisher.withIdentity(persist = false) { identity ->
                uniffi.lakko_ffi_uniffi.composeChannelSubscriberInviteCapability(
                    identity.ed25519Seed(),
                    channelId,
                    expiry.toULong(),
                    name,
                    description,
                    access,
                    avatar,
                    // Section 7.2: the link page reads "Anna invites you to";
                    // the core leaves out a name that is not valid or does not fit.
                    ownDisplayName(context),
                )
            }
            val discovery = me.lakko.app.discovery.DiscoveryClient()
            discovery.setSessionToken(me.lakko.app.auth.SessionTokens.provider?.current())
            var ledger: ByteArray? = null
            val url = discovery.use {
                CoPublisher.uploadCoPublisherLink(
                    mint.sealed,
                    mint.linkSecret,
                    it,
                    account,
                    maxUses = uses,
                    expiryMs = expiry,
                    register = LinkRegisterEntry(
                        kind = uniffi.lakko_ffi_uniffi.LinkRecordKind.CHANNEL,
                        targetId = channelId,
                        inviteSeed = inviteSeed ?: ByteArray(0),
                        admissionVersion = admissionVersionUsed,
                        // The owner's label, never the channel's name: every
                        // device labels a subscriber by it (`From link "..."`).
                        label = label.trim(),
                    ),
                    onUploaded = { ledger = it },
                )
            }
            return ChannelLinkMint(url, ChannelLinkMint.cardOf(ledger, mint.linkSecret, mint.sealed))
        } finally {
            inviteSeed?.fill(0)
        }
    }

    /**
     * Section 7.2 — Subscribe on a channel's link page. A public channel:
     * keep its read key; the channel opens subscribed and reads at once. A
     * private channel: register this device's subscriber key with the
     * invitation's proof and a sealed admission ticket; posts appear once a
     * publisher's device lets the key in. Answers the channel id (hex).
     */
    public suspend fun subscribeFromLink(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        channelId: ByteArray,
        name: String?,
        invite: FfiSubscriberInvite,
    ): String {
        val account = state.boundAccount ?: throw IllegalStateException("no account bound")
        val idHex = ChannelsComposer.bytesToHex(channelId)
        when (val access = invite.access) {
            is FfiChannelAccess.Public -> {
                ChannelReadKeys.put(context, account, idHex, access.readKey)
                addRow(state, idHex, name, invite.description)
                // The conversation screen registers the key (subscribeOnOpen).
            }
            is FfiChannelAccess.Private -> {
                val kp = ChannelSubscriptionSync.keyForSubscribing(state, idHex)
                val pubHex = ChannelsComposer.bytesToHex(kp.publicKey)
                ChannelInviteSigner.fromInviteSeed(access.inviteSeed).use { signer ->
                    val proof = signer.signProof(channelId, kp.publicKey)
                    val ticket = signer.sealTicket(access.admissionPublicKey, access.admissionVersion, channelId, kp.publicKey)
                    client.subscribeWithInvite(idHex, pubHex, ticket, proof)
                }
                addRow(state, idHex, name, invite.description)
                ChannelUnsubscribe.noteFollowing(idHex)
            }
        }
        return idHex
    }

    private fun addRow(state: ChannelsUiState, idHex: String, name: String?, description: String) {
        if (state.channels.value.any { it.id == idHex }) return
        state.addChannel(
            ChannelHandle(
                id = idHex,
                localName = name?.trim().orEmpty(),
                localDescription = description,
                addedAtMs = System.currentTimeMillis().toULong(),
            ),
        )
    }

    /** This account's own display name, or `null` (section 7.2). */
    private fun ownDisplayName(context: Context): String? {
        val b64 = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64 ?: return null
        val id = try {
            java.util.Base64.getDecoder().decode(b64)
        } catch (_: IllegalArgumentException) {
            return null
        }
        return try {
            me.lakko.app.profile.OwnProfileStore.load(context, id)?.displayName?.trim()?.takeIf { it.isNotEmpty() }
        } catch (_: Throwable) {
            null
        }
    }

    /** The discovery ledger's "no use limit". */
    public const val UNLIMITED_USES: Long = 0xFFFFFFFFL

    private fun hex(b: ByteArray): String = b.joinToString("") { "%02x".format(it) }
}

/**
 * ADR-0087 section 7 / UX 4.1 — this device's admission mark of a private
 * channel, the web access record's `admittedThroughSeq` and
 * `admissionMarkSet`: the service row number through which rows are readers
 * (they came in before the channel went private, or a let-in pass let them
 * in; 0 when none), and whether this device set it itself (it made the
 * channel private or minted its first admission seed). A device that did not
 * removes nobody and moves its mark only past rows it let in
 * ([ChannelSubscriberRows.letInVerdict]). A rotation reseals per key to the
 * rows at or below the mark ([ChannelReseal]). Only ever raised.
 */
public object ChannelAdmissionMark {
    private fun key(account: String, channelId: String): String = "admitted-through|$account|$channelId"
    private fun setKey(account: String, channelId: String): String = "admitted-mark-set|$account|$channelId"

    public fun get(context: Context, account: String, channelId: String): Long =
        ChannelKeyStore.encryptedPrefs(context).getLong(key(account, channelId), 0L)

    public fun isSet(context: Context, account: String, channelId: String): Boolean =
        ChannelKeyStore.encryptedPrefs(context).getBoolean(setKey(account, channelId), false)

    private fun seenKey(account: String, channelId: String): String = "rows-seen-through|$account|$channelId"
    private fun pendingKey(account: String, channelId: String): String = "admitted-pending|$account|$channelId"

    /** ADR-0090 security pass — the newest subscriber row seq this device
     *  has itself read from the channels service. */
    public fun seenThrough(context: Context, account: String, channelId: String): Long =
        ChannelKeyStore.encryptedPrefs(context).getLong(seenKey(account, channelId), 0L)

    /** Note rows read up to [seq] (only ever raised). */
    public fun noteSeen(context: Context, account: String, channelId: String, seq: Long) {
        if (seq <= seenThrough(context, account, channelId)) return
        ChannelKeyStore.encryptedPrefs(context).edit().putLong(seenKey(account, channelId), seq).apply()
    }

    /** A received owner mark above the rows this device has read, or 0. */
    public fun pending(context: Context, account: String, channelId: String): Long =
        ChannelKeyStore.encryptedPrefs(context).getLong(pendingKey(account, channelId), 0L)

    public fun setPending(context: Context, account: String, channelId: String, through: Long?) {
        val edit = ChannelKeyStore.encryptedPrefs(context).edit()
        if (through == null) edit.remove(pendingKey(account, channelId)) else edit.putLong(pendingKey(account, channelId), through)
        edit.apply()
    }

    /** Raise the mark to [through]; [markSet] records that this device set
     *  it itself (never cleared). */
    public fun raise(context: Context, account: String, channelId: String, through: Long, markSet: Boolean = false) {
        val current = get(context, account, channelId)
        val edit = ChannelKeyStore.encryptedPrefs(context).edit()
        if (through > current) edit.putLong(key(account, channelId), through)
        if (markSet) edit.putBoolean(setKey(account, channelId), true)
        edit.commit()
    }
}

/**
 * Section 7 — a private channel's admission seeds, by version: a random seed
 * minted by the owner (not derived from the account seed, so co-publishers
 * can hold it too), kept in the Keystore-backed channel preferences. The
 * owner mints version 0 and seals every version to each publisher
 * ([ChannelAdmissionSeedShare]); a publisher keeps the versions it is sent.
 * Older versions are kept so tickets from older links still open.
 */
public object ChannelAdmissionKeys {
    private fun key(account: String, channelId: String, version: UShort, seed: ByteArray): String =
        "admission|$account|$channelId|$version|${seedTag(seed)}"
    private fun legacyKey(account: String, channelId: String): String = "admission|$account|$channelId"

    /** A short tag of a seed, so two seeds of one version are stored apart. */
    private fun seedTag(seed: ByteArray): String =
        java.security.MessageDigest.getInstance("SHA-256").digest(seed).take(8).joinToString("") { "%02x".format(it) }

    /** ADR-0090 open question 4 - the owner account's admission-seed register
     *  (lakko-sync, carried between the account's devices); set by the
     *  session shell, `null` in a host test. */
    @Volatile
    public var register: ((channelId: String) -> List<Pair<UShort, ByteArray>>)? = null

    /** Write a seed to the register (and send it); set by the session shell. */
    @Volatile
    public var writeRegister: ((channelId: String, version: UShort, seed: ByteArray) -> Boolean)? = null

    /** Every held seed with its version, this device's and the register's
     *  (ADR-0090: two seeds of one version may both be held; a ticket of a
     *  version is tried against every one). The caller wipes each. */
    public fun all(context: Context, account: String, channelId: String): List<Pair<UShort, ByteArray>> {
        val prefs = ChannelKeyStore.encryptedPrefs(context)
        val prefix = "admission|$account|$channelId|"
        val out = ArrayList<Pair<UShort, ByteArray>>()
        for ((k, v) in prefs.all) {
            val version = when {
                k.startsWith(prefix) -> k.removePrefix(prefix).substringBefore('|').toIntOrNull()
                    ?.takeIf { it in 0..0xFFFF }?.toUShort()
                k == legacyKey(account, channelId) -> 0u.toUShort()
                else -> null
            } ?: continue
            val seed = (v as? String)?.let {
                try {
                    Base64.decode(it, Base64.NO_WRAP)
                } catch (_: IllegalArgumentException) {
                    null
                }
            } ?: continue
            out += version to seed
        }
        val fromRegister = try {
            register?.invoke(channelId).orEmpty()
        } catch (_: Throwable) {
            emptyList()
        }
        return mergeAdmissionSeeds(out, fromRegister)
    }

    /** The newest version (one of its seeds, this device's first), or `null`. */
    public fun current(context: Context, account: String, channelId: String): Pair<UShort, ByteArray>? {
        val all = all(context, account, channelId)
        val newest = all.lastOrNull() ?: return null
        all.filter { it !== newest }.forEach { it.second.fill(0) }
        return newest
    }

    public fun put(context: Context, account: String, channelId: String, version: UShort, seed: ByteArray) {
        ChannelKeyStore.encryptedPrefs(context).edit()
            .putString(key(account, channelId, version, seed), Base64.encodeToString(seed, Base64.NO_WRAP))
            .commit()
        // ADR-0101 section 7 - an owned channel's admission seeds are in the
        // owned-channels list: the next pass writes it.
        OwnedChannelsRuntime.channelChanged(channelId)
    }

    /** The newest version, minting version 0 when none is held (the owner);
     *  a minted seed goes to the register at once. */
    public fun currentOrMint(context: Context, account: String, channelId: String): Pair<UShort, ByteArray> {
        current(context, account, channelId)?.let { return it }
        val seed = uniffi.lakko_ffi_uniffi.generateChannelAdmissionSeed()
        put(context, account, channelId, 0u, seed)
        toRegister(channelId, 0u, seed)
        return 0u.toUShort() to seed
    }

    /** ADR-0090 - write a seed this owner device holds to the register. */
    public fun toRegister(channelId: String, version: UShort, seed: ByteArray): Boolean =
        try {
            writeRegister?.invoke(channelId, version, seed) ?: false
        } catch (_: Throwable) {
            false
        }

    /** ADR-0090 - every seed this device holds that the register lacks goes
     *  to it (an owner device's pass). */
    public fun syncToRegister(context: Context, account: String, channelId: String) {
        val inRegister = try {
            register?.invoke(channelId).orEmpty()
        } catch (_: Throwable) {
            return
        }
        val local = all(context, account, channelId)
        try {
            for ((version, seed) in local) {
                if (inRegister.any { (v, s) -> v == version && s.contentEquals(seed) }) continue
                toRegister(channelId, version, seed)
            }
        } finally {
            local.forEach { it.second.fill(0) }
            inRegister.forEach { it.second.fill(0) }
        }
    }
}

/**
 * ADR-0090 - this device's admission seeds and the register's, as one list:
 * a seed held in both is listed once, two seeds of one version both stay;
 * ascending by version, this device's seeds before the register's within a
 * version. Pure.
 */
internal fun mergeAdmissionSeeds(
    local: List<Pair<UShort, ByteArray>>,
    register: List<Pair<UShort, ByteArray>>,
): List<Pair<UShort, ByteArray>> {
    val out = ArrayList<Pair<UShort, ByteArray>>()
    for (entry in local + register) {
        if (out.any { it.first == entry.first && it.second.contentEquals(entry.second) }) continue
        out += entry
    }
    return out.withIndex().sortedWith(compareBy({ it.value.first }, { it.index })).map { it.value }
}

/** A co-publisher holds no admission seed of a private channel yet: the
 *  owner's device seals one to it once it has opened Lakko. */
public class ChannelLinkNeedsOwner : IllegalStateException("the owner has not shared the channel's admission key yet")

/**
 * What a channel link's error reads as (`ADR-0087 phase B`): no admission
 * seed on a co-publisher, offline only on a real network failure, else a
 * plain failure.
 */
public fun channelLinkErrorKey(cause: Throwable, failedKey: String = "channels.links.failed"): String {
    var c: Throwable? = cause
    val seen = HashSet<Throwable>()
    while (c != null && seen.add(c)) {
        if (c is ChannelLinkNeedsOwner) return "channels.links.needs_owner"
        if (c is java.io.IOException || c is java.nio.channels.UnresolvedAddressException) return "common.error.offline"
        c = c.cause
    }
    return failedKey
}

/** A minted channel link, and the card that carries it in a chat (`null`
 *  when no card could be composed: the link text alone still works). */
public class ChannelLinkMint(public val url: String, public val card: ByteArray?) {
    public companion object {
        public fun cardOf(ledgerId: ByteArray?, linkSecret: ByteArray, sealed: ByteArray): ByteArray? {
            if (ledgerId == null) return null
            return try {
                uniffi.lakko_ffi_uniffi.composeChannelInviteCard(ledgerId, linkSecret, sealed)
            } catch (_: Throwable) {
                null
            }
        }
    }
}
