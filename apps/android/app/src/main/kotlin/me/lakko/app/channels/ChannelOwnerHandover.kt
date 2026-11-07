// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CancellationException
import uniffi.lakko_ffi_uniffi.Identity

/*
 * ADR-0074 section 3.5, "Channels" - after a rotation of the account keys
 * the owner key of every channel this account owns moves to the key the NEW
 * seed derives: `PUT /v1/channels/{id}/owner`, body = the new owner key, the
 * owner proof by the OLD seed's key.
 *
 * The handover is tried by the rotating device as a step of its owed record
 * and, idempotently, by every kept device once it holds the new keys: a
 * handover naming the key that already owns the channel changes nothing.
 *
 * Two identities then exist side by side on this device, and they are kept
 * apart on purpose:
 *
 *   - a channel's PublisherGroup is an MLS group and stays on the
 *     CONVERSATION identity ([ChannelPublisherIdentity], the predecessor
 *     after a rotation: Stage A moves no leaf);
 *   - OWNER requests are signed by the key the CURRENT account seed derives
 *     ([ChannelOwnerSuccession], consulted by [ChannelsClient.ownerSigner]).
 *
 * A co-publisher key this account holds in someone else's channel derives
 * from the old seed too and can only be replaced by that channel's owner;
 * the client asks nothing of them (a stated residual of the ADR).
 */

/** What one channel's handover came to. */
public enum class ChannelHandoverOutcome {
    /** The owner key is the new seed's (replaced now, or before). */
    OWNED,

    /** Neither the old nor the new key owns the channel: this account is
     *  not its owner (a co-publisher). Nothing to do. */
    NOT_OWNER,

    /** No verdict (offline, a limit, `5xx`): owed. */
    RETRY,
}

/** What follows the attempt signed by the OLD key. */
public enum class AfterOldOwnerKey {
    OWNED,

    /** `401 owner-proof-required` or `409 owner-changed`: the old key no
     *  longer owns the channel. Ask with the NEW key whether the handover
     *  already landed. */
    PROBE_WITH_NEW_KEY,

    RETRY,
}

private const val HANDOVER_HTTP_UNAUTHORIZED: Int = 401
private const val HANDOVER_HTTP_CONFLICT: Int = 409

/** Read the answer to the handover signed by the old key; `null` is `204`.
 *  `400 bad-owner-pubkey` is not a verdict about ownership: owed. */
public fun afterOldOwnerKey(error: Throwable?): AfterOldOwnerKey = when {
    error == null -> AfterOldOwnerKey.OWNED
    error is ChannelsApiException &&
        (error.status == HANDOVER_HTTP_UNAUTHORIZED || error.status == HANDOVER_HTTP_CONFLICT) ->
        AfterOldOwnerKey.PROBE_WITH_NEW_KEY
    else -> AfterOldOwnerKey.RETRY
}

/** Read the answer to the probe signed by the NEW key (naming itself). */
public fun afterNewOwnerKey(error: Throwable?): ChannelHandoverOutcome = when {
    error == null -> ChannelHandoverOutcome.OWNED
    error is ChannelsApiException && error.status == HANDOVER_HTTP_UNAUTHORIZED -> ChannelHandoverOutcome.NOT_OWNER
    else -> ChannelHandoverOutcome.RETRY
}

/**
 * Hand one channel over. [put] is `PUT /v1/channels/{id}/owner` naming the
 * NEW owner key, signed by the new key when `signWithNewKey`, else by the
 * old one.
 */
public suspend fun handOverChannelOwner(put: suspend (signWithNewKey: Boolean) -> Unit): ChannelHandoverOutcome {
    val first = attempt { put(false) }
    return when (afterOldOwnerKey(first)) {
        AfterOldOwnerKey.OWNED -> ChannelHandoverOutcome.OWNED
        AfterOldOwnerKey.RETRY -> ChannelHandoverOutcome.RETRY
        AfterOldOwnerKey.PROBE_WITH_NEW_KEY -> afterNewOwnerKey(attempt { put(true) })
    }
}

private suspend fun attempt(run: suspend () -> Unit): Throwable? =
    try {
        run()
        null
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (cause: Throwable) {
        cause
    }

/** Whether every channel has a verdict (nothing is owed). An empty list is
 *  settled: the account owns no channel on this device. */
public fun channelHandoversSettled(outcomes: Collection<ChannelHandoverOutcome>): Boolean =
    ChannelHandoverOutcome.RETRY !in outcomes

/**
 * Hand every channel in [channelIds] over; [handOverOne] is one channel's
 * [handOverChannelOwner]. `true` only when every channel has a verdict.
 * Every channel is attempted even after one failed.
 */
public suspend fun handOverChannels(
    channelIds: Collection<String>,
    handOverOne: suspend (channelIdHex: String) -> ChannelHandoverOutcome,
    log: (String) -> Unit = {},
): Boolean {
    val outcomes = channelIds.map { id -> handOverOne(id) }
    log(
        "channel owner handover: ${outcomes.count { it == ChannelHandoverOutcome.OWNED }} owned, " +
            "${outcomes.count { it == ChannelHandoverOutcome.NOT_OWNER }} not owner, " +
            "${outcomes.count { it == ChannelHandoverOutcome.RETRY }} owed",
    )
    return channelHandoversSettled(outcomes)
}

/**
 * Whether an owner signer asked of the identity with [passedKey] is derived
 * from the account's CURRENT identity instead: only when a rotation
 * installed a successor for exactly that (conversation) key. Pure.
 */
public fun ownerKeySucceeds(passedKey: ByteArray, conversationKey: ByteArray?): Boolean =
    conversationKey != null && passedKey.contentEquals(conversationKey)

/**
 * The account identity OWNER requests are signed by when it is not the one
 * the PublisherGroups run on (after a rotation of the account keys). Process
 * wide, like the publisher identity it stands beside; installed by
 * [ChannelPublisherIdentity.bind] and cleared by its `unbind`.
 */
internal object ChannelOwnerSuccession {
    private class Entry(val conversationKey: ByteArray, val current: Identity)

    private var entry: Entry? = null

    /** OWNER requests asked of the identity with [conversationKey] are
     *  signed by [current] from now on. Takes ownership of [current]. */
    @Synchronized
    fun install(conversationKey: ByteArray, current: Identity) {
        entry?.current?.close()
        entry = Entry(conversationKey.copyOf(), current)
    }

    @Synchronized
    fun clear() {
        entry?.current?.close()
        entry = null
    }

    /** Run [derive] on the identity owner requests of [passed] are signed
     *  by: the installed successor, else [passed] itself. */
    @Synchronized
    fun <T> withOwnerIdentity(passed: Identity, derive: (Identity) -> T): T {
        val held = entry
        return if (held != null && ownerKeySucceeds(passed.signaturePublicKey(), held.conversationKey)) {
            derive(held.current)
        } else {
            derive(passed)
        }
    }
}

/** The rotation's channel step over the device and the channels service. */
internal object KeyRotationChannels {
    private const val TAG = "KeyRotation"

    /**
     * Hand every channel this device publishes to from the key
     * [predecessorSeed] derives to the key [currentSeed] derives (both
     * 64-byte phrase seeds). `true` only when every channel has a verdict;
     * a channel list that cannot be read is NOT "no channels" and throws.
     */
    suspend fun handOverAll(
        context: Context,
        accountIdB64: String,
        predecessorSeed: ByteArray,
        currentSeed: ByteArray,
        /** ADR-0078 phase 6 — the lineage (oldest) seed the PublisherGroup ids
         *  derive from; `null` skips the owed handle rekeys. */
        lineageSeed: ByteArray? = null,
    ): Boolean {
        val channelIds = OrphanedChannelsStore.publisherChannelIds(
            ChannelPublisherKeyStore.encryptedPrefs(context),
            accountIdB64,
        ).sorted()
        if (channelIds.isEmpty()) return true
        if (lineageSeed != null) queueOwedHandleRekeys(context, accountIdB64, channelIds, predecessorSeed, lineageSeed)
        val settled = Identity.fromSeed(predecessorSeed).use { old ->
            Identity.fromSeed(currentSeed).use { new ->
                ChannelsClient().use { client ->
                    handOverChannels(
                        channelIds = channelIds,
                        handOverOne = { id ->
                            handOverChannelOwner { signWithNewKey ->
                                ChannelsClient.ownerSignerExact(new, id).use { naming ->
                                    val newOwnerKey = naming.verifyingKey()
                                    if (signWithNewKey) {
                                        client.replaceOwner(id, naming, newOwnerKey)
                                    } else {
                                        ChannelsClient.ownerSignerExact(old, id).use { signing ->
                                            client.replaceOwner(id, signing, newOwnerKey)
                                        }
                                    }
                                }
                            }
                        },
                        log = { line -> Log.i(TAG, line) },
                    )
                }
            }
        }
        // A role learned under the old owner key is not the new key's.
        ChannelPublisherRoles.shared.clear()
        return settled
    }

    /**
     * ADR-0087 section 3 + ADR-0074 — each owned channel's @name was written
     * under the handle key of the PREDECESSOR seed: keep that key as an owed
     * `rekey`, which the publisher loop signs with it to move the name to the
     * current seed's key. A channel with no @name drops the row there.
     * Idempotent: a row already owed is kept (it names the older key).
     */
    private fun queueOwedHandleRekeys(
        context: Context,
        accountIdB64: String,
        channelIds: List<String>,
        predecessorSeed: ByteArray,
        lineageSeed: ByteArray,
    ) {
        try {
            Identity.fromSeed(lineageSeed).use { lineage ->
                Identity.fromSeed(predecessorSeed).use { old ->
                    val oldAccountSeed = old.ed25519Seed()
                    try {
                        for (id in channelIds) {
                            val held = me.lakko.app.chat.GroupSettingsStore.owedRekeySeed(context, accountIdB64, id)
                            if (held != null) {
                                held.fill(0)
                                continue
                            }
                            val journal = uniffi.lakko_ffi_uniffi.publisherGroupIdFor(
                                lineage,
                                ChannelsClient.channelIdBytes(id),
                            )
                            val seed = me.lakko.app.chat.groupHandleSeed(oldAccountSeed, journal)
                            try {
                                me.lakko.app.chat.GroupSettingsStore.saveOwedRekeySeed(context, accountIdB64, id, seed)
                            } finally {
                                seed.fill(0)
                            }
                        }
                    } finally {
                        oldAccountSeed.fill(0)
                    }
                }
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            Log.w(TAG, "owed channel handle rekeys not recorded", e)
        }
    }
}
