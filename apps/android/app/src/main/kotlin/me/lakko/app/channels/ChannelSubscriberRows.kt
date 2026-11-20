// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * ADR-0087 sections 7 and 15.1 — a private channel's subscriber rows as a
 * publisher holding the admission seed sees them: each row's sealed ticket
 * opened under the channel's admission seeds, naming the invite key the
 * person came by. A row whose ticket does not open, or names a key the
 * channel does not hold, is not let in and is removed (the publisher's
 * device is the boundary; the service is the door).
 */
public object ChannelSubscriberRows {
    private const val TAG = "ChannelSubscriberRows"

    /** One subscriber row with what its ticket says. */
    public data class Row(
        val pubkeyHex: String,
        /** The invite key the ticket names (hex), or `null` with no valid
         *  ticket. */
        val inviteKeyHex: String?,
    )

    /** The invite key [ticket] names, under any held admission seed version. */
    public fun inviteKeyOf(context: Context, account: String, channelId: String, subscriberHex: String, ticket: ByteArray?): String? {
        if (ticket == null) return null
        val channelBytes = ChannelsComposer.channelIdFromHex(channelId)
        val subscriber = hexToBytes(subscriberHex) ?: return null
        val wanted = uniffi.lakko_ffi_uniffi.channelTicketAdmissionVersion(ticket)
        val seeds = ChannelAdmissionKeys.all(context, account, channelId)
            .sortedByDescending { (version, _) -> if (version == wanted) Int.MAX_VALUE else version.toInt() }
        for ((_, seed) in seeds) {
            try {
                return uniffi.lakko_ffi_uniffi.openChannelAdmissionTicket(seed, channelBytes, subscriber, ticket)
                    .joinToString("") { "%02x".format(it) }
            } catch (_: Throwable) {
                // Another version's seed.
            } finally {
                seed.fill(0)
            }
        }
        return null
    }

    /** Every row, with what its ticket names. */
    public suspend fun read(context: Context, state: ChannelsUiState, client: ChannelsClient, channelId: String): List<Row> {
        val account = state.boundAccount ?: return emptyList()
        val pub = state.publisher ?: return emptyList()
        val out = ArrayList<Row>()
        var after = 0L
        while (true) {
            val page = ChannelPublisherSigners.derive(pub, channelId).use { client.subscriberPage(channelId, it, after) }
            for (r in page.rows) out += Row(r.pubkeyHex, inviteKeyOf(context, account, channelId, r.pubkeyHex, r.sealedTicket))
            if (page.rows.isEmpty() || page.nextAfter <= after) break
            after = page.nextAfter
        }
        return out
    }

    /**
     * The let-in pass of a private channel: the keys whose ticket names a key
     * the channel holds. A row with no valid ticket is removed. `null` when
     * this device holds no admission seed (a co-publisher before the owner
     * sealed one to it): the owner's device lets people in.
     */
    public suspend fun admissible(context: Context, state: ChannelsUiState, client: ChannelsClient, channelId: String): List<String>? {
        val account = state.boundAccount ?: return null
        if (ChannelAdmissionKeys.all(context, account, channelId).isEmpty()) return null
        val pub = state.publisher ?: return null
        val live = ChannelPublisherSigners.derive(pub, channelId).use { client.listInviteKeys(channelId, it) }.toSet()
        val rows = read(context, state, client, channelId)
        val keep = ArrayList<String>()
        for (row in rows) {
            if (row.inviteKeyHex != null && row.inviteKeyHex in live) {
                keep += row.pubkeyHex
            } else if (row.inviteKeyHex == null) {
                try {
                    ChannelPublisherSigners.derive(pub, channelId).use { client.removeSubscriber(channelId, row.pubkeyHex, it) }
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    Log.i(TAG, "a row with no valid ticket was not removed yet")
                }
            }
        }
        return keep
    }

    /** Section 7.3 — `Remove`: the row goes and a rotation follows at once, so
     *  the removed key gets no seed of the next epoch. */
    public suspend fun remove(state: ChannelsUiState, client: ChannelsClient, channelId: String, pubkeyHex: String) {
        val pub = state.publisher ?: throw IllegalStateException("no publisher identity")
        ChannelPublisherSigners.derive(pub, channelId).use { client.removeSubscriber(channelId, pubkeyHex, it) }
        ChannelPublisherLoop.peek(state)?.rotateNow(channelId)
    }

    private fun hexToBytes(hex: String): ByteArray? {
        if (hex.length % 2 != 0) return null
        return try {
            ByteArray(hex.length / 2) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
        } catch (_: NumberFormatException) {
            null
        }
    }
}

/**
 * ADR-0087 section 9.5 — a subscriber removed from a private channel: the
 * service answers `410 subscriber-removed` for its key. Noticed app-wide
 * (the session shell asks every few minutes) and on the open channel; the
 * channel ends on this device as an unsubscribe does, the notice "You are no
 * longer in {name}." is filed, and an open channel shows the dialog.
 */
public object ChannelRemovalWatch {
    private val _removed = MutableStateFlow<Map<String, String>>(emptyMap())

    /** Channels removed this session (id -> name), for an open screen's dialog. */
    public val removed: StateFlow<Map<String, String>> = _removed.asStateFlow()

    /** Whether [cause] is the service saying this key was removed. */
    public fun isRemoval(cause: Throwable): Boolean =
        cause is ChannelsApiException && cause.code == "subscriber-removed"

    /** Section 15 (phase B) — whether [cause] is the service saying the
     *  owner deleted the channel (`410 channel-deleted`). */
    public fun isDeleted(cause: Throwable): Boolean {
        var c: Throwable? = cause
        val seen = HashSet<Throwable>()
        while (c != null && seen.add(c)) {
            if (c is ChannelsApiException && c.status == 410 && c.code == "channel-deleted") return true
            c = c.cause
        }
        return false
    }

    /** Whether [cause] ends the channel on this device: a removal or a
     *  deletion. */
    public fun isEnd(cause: Throwable): Boolean = isRemoval(cause) || isDeleted(cause)

    /**
     * Section 9 / 15 — the channel ended for this account (deleted by its
     * owner, or this account is no longer a publisher): it goes from every
     * device of the account, as an unsubscribe does, and the notice is
     * filed. An open channel shows the dialog.
     */
    public fun endEverywhere(
        state: ChannelsUiState,
        channelId: String,
        name: String,
        onNotice: ((String, String) -> Unit)?,
    ) {
        ChannelUnsubscribe.leave(state, channelId, ChannelsClient.DEFAULT_BASE_URL, tellDevices = true)
        ChannelMemberCards.forgetUploads(channelId)
        RemovedChannels.noteAnswered(channelId)
        _removed.update { it + (channelId to name) }
        (onNotice ?: notice)?.invoke(channelId, name)
    }

    /** End [channelId] on this device after a removal. */
    public fun ended(state: ChannelsUiState, channelId: String, name: String, onNotice: (String, String) -> Unit) {
        state.clearSubscriberKeys(channelId)
        state.removeChannel(channelId)
        ChannelMemberCards.forgetUploads(channelId)
        RemovedChannels.noteAnswered(channelId)
        me.lakko.app.feed.requestChannelPostSync()
        _removed.update { it + (channelId to name) }
        onNotice(channelId, name)
    }

    /** Ask the service about every channel this device only reads. */
    public suspend fun checkAll(
        state: ChannelsUiState,
        client: ChannelsClient,
        onNotice: (channelId: String, name: String) -> Unit,
    ) {
        val publishing = state.keysByChannel.value.keys
        for ((channelId, keys) in state.subscriberKeysByChannel.value) {
            if (channelId in publishing) continue
            val channel = state.channels.value.firstOrNull { it.id == channelId } ?: continue
            // ADR-0089 step 5 — a removed channel is asked once per period.
            if (!RemovedChannels.mayAsk(channelId)) continue
            try {
                client.fetchEnvelope(channelId, ChannelsComposer.bytesToHex(keys.publicKey))
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                val name = channel.sharedName.ifBlank { channel.localName }
                if (isDeleted(e)) {
                    endEverywhere(state, channelId, name, onNotice)
                } else if (isRemoval(e)) {
                    ended(state, channelId, name, onNotice)
                }
            }
        }
    }

    public fun clear() {
        _removed.value = emptyMap()
    }

    private val answered = java.util.Collections.synchronizedSet(HashSet<String>())

    /** The open channel's dialog was answered: it is not shown again, and the
     *  Chats notice says nothing about it (UX 9). In memory only. */
    public fun acknowledge(channelId: String) {
        _removed.update { it - channelId }
        answered += channelId.lowercase()
    }

    /** Whether the in-channel dialog for [channelId] was answered. */
    public fun wasAnswered(channelId: String): Boolean = channelId.lowercase() in answered

    /** Where an in-channel removal files its notice (the session shell sets
     *  it to the membership-end notice). */
    @Volatile
    public var notice: ((channelId: String, name: String) -> Unit)? = null
}
