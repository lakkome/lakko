// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json

/**
 * Publisher requests this account sent that have not landed (ADR-0087
 * section 15, ADR-0045 decision 2: a request has no deadline). Android twin
 * of `apps/web/src/channels/pending-publisher-requests.ts`.
 *
 * `Accept` on a publisher invitation knocks with a KeyPackage of the channel
 * publisher identity and waits for the owner's device. The owner may approve
 * minutes or days later, when its app next runs; the Welcome then lands in
 * this account's mailbox. The record is what lets the account mailbox loop
 * (`ContactsRepository`) join that Welcome whenever it arrives, after a
 * restart too, whether or not the card that sent it is still on screen.
 *
 * Per account, in a plain preferences file (no secret in it: the KeyPackage's
 * private key is in the publisher identity's provider snapshot, saved before
 * the knock). A record goes once the channel is joined, when the channel is
 * deleted (`410 channel-deleted`), and with the account wipe. A second
 * request for the same channel replaces the first.
 */
public object PendingPublisherRequests {
    private const val PREFS = "lakko.channels.pending-publisher"

    /** One request waiting for the owner's Welcome. */
    @Serializable
    public data class Request(
        val channelIdHex: String,
        /** The PublisherGroup the link named (lower-case hex). */
        val groupIdHex: String,
        /** base64 of this device's leaf key the knocked KeyPackage belongs
         *  to (ADR-0090: a bound device). */
        val joinerKeyB64: String,
        /** The channel's name the link carried, for its row once joined. */
        val name: String,
        val requestedAtMs: Long,
        /** ADR-0090 — base64 of the owner's account key (the capability's
         *  issuer): the Welcome must come from one of its devices. A record
         *  without it never lands (written before ADR-0090). */
        val ownerAccountKeyB64: String? = null,
    )

    private val json = Json { ignoreUnknownKeys = true }
    private val listSerializer = ListSerializer(Request.serializer())

    /** The stored form of [list]. */
    internal fun encode(list: List<Request>): String = json.encodeToString(listSerializer, list)

    /** [encode]'s reverse; an unreadable row reads as none. */
    internal fun decode(raw: String?): List<Request> {
        if (raw.isNullOrEmpty()) return emptyList()
        return try {
            json.decodeFromString(listSerializer, raw)
        } catch (_: Throwable) {
            emptyList()
        }
    }

    /** [list] with [record] in place of any earlier one for its channel. */
    internal fun withRecord(list: List<Request>, record: Request): List<Request> =
        list.filter { it.channelIdHex != record.channelIdHex } + record

    /** [list] without the request for [channelIdHex]. */
    internal fun without(list: List<Request>, channelIdHex: String): List<Request> =
        list.filter { it.channelIdHex != channelIdHex }

    private fun prefs(context: Context) = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** Every pending request of [account]. */
    public fun load(context: Context, account: String): List<Request> =
        decode(prefs(context).getString(account, null))

    /** When the pending request for [channelIdHex] was sent, `null` when
     *  none is pending (UX 15.2.2: `Ask again` after a day). */
    public fun sentAtMs(context: Context, account: String, channelIdHex: String): Long? =
        load(context, account).firstOrNull { it.channelIdHex == channelIdHex }?.requestedAtMs

    /** Whether a request for [channelIdHex] is pending. */
    public fun isPending(context: Context, account: String, channelIdHex: String): Boolean =
        load(context, account).any { it.channelIdHex == channelIdHex }

    private fun save(context: Context, account: String, list: List<Request>) {
        val ok = me.lakko.app.account.AccountStoreGate.writeOrEnd(account) {
            val editor = prefs(context).edit()
            if (list.isEmpty()) editor.remove(account) else editor.putString(account, encode(list))
            editor.commit()
        }
        if (!ok) throw IllegalStateException("pending publisher request not written")
    }

    /** Record a request, replacing an earlier one for the same channel.
     *  Throws when it cannot be written (the caller must not knock then). */
    public fun record(context: Context, account: String, record: Request) {
        save(context, account, withRecord(load(context, account), record))
    }

    /** Drop the request for a channel (joined, or the channel is gone). */
    public fun drop(context: Context, account: String, channelIdHex: String) {
        val list = load(context, account)
        val rest = without(list, channelIdHex)
        if (rest.size != list.size) save(context, account, rest)
    }

    /** The account wipe. */
    public fun forgetAccount(context: Context, account: String) {
        prefs(context).edit().remove(account).commit()
    }

    /**
     * Installed by the session shell: try one Welcome from the account
     * mailbox against this account's pending requests; `true` when it landed
     * one. Throws when a landed join could not be made durable (the mailbox
     * loop then holds its cursor). `null` outside a session.
     */
    @Volatile
    public var lander: (suspend (welcome: ByteArray) -> Boolean)? = null
}
