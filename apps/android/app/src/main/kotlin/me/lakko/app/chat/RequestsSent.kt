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
 * sent, kept until the group appears (joined). A request cannot be withdrawn
 * and is never said to be refused: under 7 days it is "Waiting for an
 * admin.", after that "No answer yet." with `Ask again`. A device asks once
 * per group per 7 days. The web twin is `chat/requests-sent.ts`.
 */

/** A request may be asked again after this long (UX 6.2). */
public const val ASK_AGAIN_AFTER_MS: Long = 7L * 24 * 60 * 60 * 1000

/** One request this device sent. */
@Serializable
public data class SentRequest(
    /** The group id, lowercase hex. */
    val idHex: String,
    val name: String,
    val handle: String,
    val sentAtMs: Long,
    val avatar: me.lakko.app.profile.AvatarRefs? = null,
    /** The group's knock public key (base64) and version, for `Ask again`. */
    val knockKeyB64: String,
    val knockVersion: Int = 0,
)

/** The line a request shows (UX 6.2). Pure. */
public fun sentRequestWaiting(r: SentRequest, nowMs: Long): Boolean = nowMs - r.sentAtMs < ASK_AGAIN_AFTER_MS

/** Whether this device may ask again now. Pure. */
public fun mayAskAgain(r: SentRequest?, nowMs: Long): Boolean = r == null || nowMs - r.sentAtMs >= ASK_AGAIN_AFTER_MS

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
