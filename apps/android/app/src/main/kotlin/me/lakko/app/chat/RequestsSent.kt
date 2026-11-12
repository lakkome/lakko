// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json

/*
 * group-and-channel-privacy.md section 6.2 — the requests to join this device
 * sent, kept until the group appears (joined): from a public group's card in
 * search, and from a group's invite link (`/g/`). A request cannot be
 * withdrawn and is never said to be refused: under a day it is "Waiting for
 * an admin.", after that "No answer yet." with `Ask again`. A device asks once
 * per group per day (amended 2026-11-12, it was 7 days). The same day governs
 * the invite page and a channel publisher request (section 15.2.2). The web
 * twin is `chat/requests-sent.ts`.
 */

/** A request may be asked again after this long (UX 6.2, 15.2.2): one day. */
public const val ASK_AGAIN_AFTER_MS: Long = 24L * 60 * 60 * 1000

/** The one rule: a request sent at [sentAtMs] still waits at [nowMs] (no
 *  `Ask again` yet). Every caller (the Requests sent list, the invite page,
 *  the publisher invitation card) goes through it. Pure. */
public fun requestStillWaiting(sentAtMs: Long, nowMs: Long): Boolean = nowMs - sentAtMs < ASK_AGAIN_AFTER_MS

/** One request this device sent. */
@Serializable
public data class SentRequest(
    /** The group id, lowercase hex. */
    val idHex: String,
    val name: String,
    val handle: String,
    val sentAtMs: Long,
    val avatar: me.lakko.app.profile.AvatarRefs? = null,
    /** A request from search: the group's knock public key (base64) and
     *  version, for `Ask again`. Empty for a link request. */
    val knockKeyB64: String = "",
    val knockVersion: Int = 0,
    /** A request from an invite link: the opened capability's plaintext wire
     *  and the ledger id (base64), for `Ask again`; no link secret. Empty for
     *  a request from search. */
    val capabilityB64: String = "",
    val blobIdB64: String = "",
)

/** Whether [r] was sent through an invite link (its `Ask again` goes through
 *  the link again). */
public val SentRequest.fromLink: Boolean get() = capabilityB64.isNotEmpty()

/** The line a request shows (UX 6.2). Pure. */
public fun sentRequestWaiting(r: SentRequest, nowMs: Long): Boolean = requestStillWaiting(r.sentAtMs, nowMs)

/** Whether this device may ask again now. Pure. */
public fun mayAskAgain(r: SentRequest?, nowMs: Long): Boolean = r == null || !requestStillWaiting(r.sentAtMs, nowMs)

private val sentJson = Json { ignoreUnknownKeys = true }

@Serializable
private data class SentRows(val r: List<SentRequest> = emptyList())

internal fun encodeSentRequests(rows: List<SentRequest>): String = sentJson.encodeToString(SentRows(rows))

internal fun decodeSentRequests(raw: String?): List<SentRequest> = try {
    if (raw.isNullOrEmpty()) emptyList() else sentJson.decodeFromString<SentRows>(raw).r
} catch (_: Exception) {
    emptyList()
}

/** The per-account store, newest first through [rows]. */
public object RequestsSentStore {
    private const val PREFS = "lakko-requests-sent"
    private val mutableRows = MutableStateFlow<List<SentRequest>>(emptyList())
    private var bound: String? = null

    /** The bound account's requests, newest first. */
    public val rows: StateFlow<List<SentRequest>> = mutableRows.asStateFlow()

    private fun prefs(context: Context) = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** Load [accountIdB64]'s rows (once per account). */
    @Synchronized
    public fun open(context: Context, accountIdB64: String) {
        if (bound == accountIdB64) return
        bound = accountIdB64
        mutableRows.value = decodeSentRequests(prefs(context).getString(accountIdB64, null)).sortedByDescending { it.sentAtMs }
    }

    @Synchronized
    private fun save(context: Context, rows: List<SentRequest>) {
        val account = bound ?: return
        mutableRows.value = rows.sortedByDescending { it.sentAtMs }
        me.lakko.app.account.AccountStoreGate.write(account) {
            prefs(context).edit().putString(account, encodeSentRequests(rows)).apply()
        }
    }

    public fun record(context: Context, r: SentRequest) {
        save(context, mutableRows.value.filter { it.idHex != r.idHex } + r)
    }

    public fun of(idHex: String): SentRequest? = mutableRows.value.firstOrNull { it.idHex == idHex.lowercase() }

    /** The group appeared in Chats (joined). */
    public fun drop(context: Context, idHex: String) {
        if (mutableRows.value.none { it.idHex == idHex.lowercase() }) return
        save(context, mutableRows.value.filter { it.idHex != idHex.lowercase() })
    }

    /** Forget device. */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        prefs(context).edit().remove(accountIdB64).apply()
        if (bound == accountIdB64) {
            bound = null
            mutableRows.value = emptyList()
        }
    }
}
