// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json
import me.lakko.app.auth.MarkerStorage

/*
 * ADR-0101 section 7 - a channel this account took back with its recovery
 * key after a key reset, until its PublisherGroup is founded again and its
 * type is known again; and the returned invitations that did not verify.
 * Mirror of `apps/web/src/channels/recovered-channels.ts` and
 * `returned-invitations.ts`.
 *
 * The reset lost everything the owner's devices kept about the channel: the
 * admission seeds, the read key, the type. Who reads the channel is still on
 * the channels service. So at recovery the owner's device records the
 * service's highest subscriber row (`throughSeq`) and treats every row up to
 * it as let in: the re-founding's reseal seals the new epoch seed to each of
 * those keys, and every later rotation does too. A fresh admission seed is
 * minted for invitations made from now on. Cost, stated: a row the service
 * itself added before the recovery is let in too.
 */

/** One recovered channel. */
@Serializable
public data class RecoveredChannel(
    /** The highest subscriber row the service listed at recovery. */
    val throughSeq: Long,
    /** A private channel retaken with its listed admission seeds: every row
     *  is judged by its ticket (mark 0), never let in for an unknown type. */
    val judgedByTickets: Boolean = false,
)

/** Which subscriber rows stay let in when a channel is retaken from the
 *  owned-channels list (ADR-0101 section 7 item 8, web `retakenChannel`). */
public enum class RetakeAdmission {
    /** A private channel with listed admission seeds: every row is judged
     *  again by its ticket under those seeds; a row with no valid ticket is
     *  removed. */
    TICKETS,

    /** Every row the service lists at the retake stays a reader (a public
     *  channel, or an entry with nothing to judge a row with). */
    EVERYONE_LISTED,
}

/** The rule above, from the list entry alone. */
public fun retakeAdmission(entry: OwnedEntry): RetakeAdmission = when {
    entry.readKey != null -> RetakeAdmission.EVERYONE_LISTED
    entry.admission.isNotEmpty() -> RetakeAdmission.TICKETS
    else -> RetakeAdmission.EVERYONE_LISTED
}

/** The admission mark a re-founding keeps: the recovery's mark for a
 *  recovered channel, else 0 (a channel this device simply lost judges every
 *  row again). */
public fun refoundAdmittedThrough(recovered: RecoveredChannel?): Long = recovered?.throughSeq ?: 0L

/** The highest subscriber row of a listing ([held] when higher or none). */
public fun highestSeq(seqs: List<Long>, held: Long = 0L): Long = seqs.fold(held) { m, s -> if (s > m) s else m }

/** A recovered channel's type, once its metadata record says. */
public enum class RecoveredVisibility { PUBLIC, PRIVATE }

/** The type a recovered channel takes from its newest metadata record:
 *  public / private when the record says, `null` while none is held. */
public fun recoveredVisibility(metaListed: Boolean?): RecoveredVisibility? = when (metaListed) {
    null -> null
    true -> RecoveredVisibility.PUBLIC
    false -> RecoveredVisibility.PRIVATE
}

private val json = Json { ignoreUnknownKeys = true }

/** The recovered channels, per signed-in account: channel id hex -> mark. */
public class RecoveredChannelsStore(private val storage: MarkerStorage) {
    private val serializer = MapSerializer(String.serializer(), RecoveredChannel.serializer())

    public fun read(account: String): Map<String, RecoveredChannel> {
        val raw = runCatching { storage.get(KEY_PREFIX + account) }.getOrNull() ?: return emptyMap()
        return runCatching { json.decodeFromString(serializer, raw) }.getOrElse { emptyMap() }
            .filterValues { it.throughSeq >= 0L }
    }

    /** Record one (a later recovery of the same channel raises the mark). */
    public fun mark(account: String, channelIdHex: String, throughSeq: Long, judgedByTickets: Boolean = false) {
        val all = read(account).toMutableMap()
        val held = all[channelIdHex]
        all[channelIdHex] = RecoveredChannel(
            maxOf(throughSeq, held?.throughSeq ?: 0L),
            judgedByTickets = judgedByTickets || held?.judgedByTickets == true,
        )
        save(account, all)
    }

    /** The type is settled: forget it. */
    public fun clear(account: String, channelIdHex: String) {
        val all = read(account).toMutableMap()
        if (all.remove(channelIdHex) == null) return
        save(account, all)
    }

    private fun save(account: String, all: Map<String, RecoveredChannel>) {
        try {
            me.lakko.app.account.AccountStoreGate.write(account) {
                if (all.isEmpty()) storage.remove(KEY_PREFIX + account)
                else storage.set(KEY_PREFIX + account, json.encodeToString(serializer, all))
            }
        } catch (_: Throwable) {
            // The access record carries the mark too.
        }
    }

    public companion object {
        public const val KEY_PREFIX: String = "channel-recovered:"

        public fun onDevice(context: Context): RecoveredChannelsStore =
            RecoveredChannelsStore(me.lakko.app.auth.ResetReturnStorage.plain(context))
    }
}

/** One returned invitation to show. */
public data class ShownInvitation(val channelIdHex: String, val cardB64: String)

/** The invitations to show: every held one whose channel the account does
 *  not read yet, in channel id order. */
public fun invitationsToShow(held: Map<String, String>, reading: Set<String>): List<ShownInvitation> =
    held.keys.filter { it !in reading }.sorted().map { ShownInvitation(it, held.getValue(it)) }

/**
 * ADR-0101 section 7 - a channel return whose old card binding does NOT
 * verify under this account's previous key is shown as the ordinary channel
 * invitation card, above the chat list, for the person to decide on. It is
 * never accepted silently and never dropped; it leaves once the account reads
 * the channel. Kept per account in the ENCRYPTED file: channel id -> standard
 * base64 of the invitation card (it carries a link secret). The newest card
 * per channel wins.
 */
public class ReturnedInvitationsStore(private val storage: MarkerStorage) {
    private val serializer = MapSerializer(String.serializer(), String.serializer())

    public fun read(account: String): Map<String, String> {
        val raw = runCatching { storage.get(KEY_PREFIX + account) }.getOrNull() ?: return emptyMap()
        return runCatching { json.decodeFromString(serializer, raw) }.getOrElse { emptyMap() }
            .filter { (id, card) -> CHANNEL_HEX.matches(id) && card.isNotEmpty() }
    }

    /** Keep one (replacing the held card of that channel). Throws when
     *  storage refuses, which keeps the return held for the next pass. */
    public fun keep(account: String, channelIdHex: String, cardB64: String) {
        val held = read(account) + (channelIdHex to cardB64)
        me.lakko.app.account.AccountStoreGate.writeOrEnd(account) {
            storage.set(KEY_PREFIX + account, json.encodeToString(serializer, held))
        }
        bump()
    }

    /** Drop the invitations of channels the account now reads. */
    public fun drop(account: String, channelIdsHex: Collection<String>) {
        val held = read(account)
        val next = held - channelIdsHex.toSet()
        if (next.size == held.size) return
        try {
            me.lakko.app.account.AccountStoreGate.write(account) {
                if (next.isEmpty()) storage.remove(KEY_PREFIX + account)
                else storage.set(KEY_PREFIX + account, json.encodeToString(serializer, next))
            }
        } catch (_: Throwable) {
            // Shown once more; harmless.
        }
        bump()
    }

    public companion object {
        public const val KEY_PREFIX: String = "channel-return-invitations:"
        private val CHANNEL_HEX = Regex("^[0-9a-f]{32}$")
        private val mutableVersion = MutableStateFlow(0L)

        /** Bumped on every change, for the chat list to re-read. */
        public val version: StateFlow<Long> = mutableVersion.asStateFlow()

        private fun bump() {
            mutableVersion.value = mutableVersion.value + 1
        }

        public fun onDevice(context: Context): ReturnedInvitationsStore =
            ReturnedInvitationsStore(me.lakko.app.auth.ResetReturnStorage.secret(context))
    }
}
