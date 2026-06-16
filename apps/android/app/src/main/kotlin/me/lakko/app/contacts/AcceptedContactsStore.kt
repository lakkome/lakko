// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import android.util.Base64
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json

/**
 * On-device persistence for the user's accepted contacts — the Android
 * mirror of the iOS `AcceptedContactsStore` and the web `local-store`
 * accepted list.
 *
 * Closes (for the accepted list) the long-standing "No persistence yet"
 * gap on [ContactsRepository] and unblocks ADR-0021 group-call roster
 * name resolution: the SFU sends a per-call `peer_tag` derived from each
 * peer's account_id, and the client re-derives the same over its
 * contacts' account_ids to resolve a name — which needs the accepted
 * contacts (each carrying `request.fromAccountId`) to survive a relaunch.
 *
 * The accepted list is non-secret relationship metadata (peer pubkeys +
 * account_ids the user already chose to trust, which the contacts UI
 * already shows), JSON-encoded into plain `SharedPreferences`; it is
 * never sent to any server (invariant #1/#2 unaffected). The `ByteArray`
 * fields ride as Base64 strings in a serialisable DTO so the domain model
 * stays free of serialization annotations.
 */
object AcceptedContactsStore {
    private const val PREFS = "lakko-accepted-contacts"
    private const val KEY = "accepted.v1"
    private val json = Json { ignoreUnknownKeys = true }

    @Serializable
    private data class Stored(
        val messageId: Long,
        val fromPubkeyB64: String,
        val fromAccountIdB64: String?,
        val fromPseudonymB64: String,
        val validAfterMs: Long,
        val intro: String?,
        val createdAt: String,
        val groupIdHex: String?,
    )

    private fun b64(bytes: ByteArray): String = Base64.encodeToString(bytes, Base64.NO_WRAP)

    private fun unb64(s: String): ByteArray = Base64.decode(s, Base64.NO_WRAP)

    /** Load the persisted accepted contacts (empty on first launch or a
     *  decode failure — a forward-compat / corruption guard). */
    fun load(context: Context): List<AcceptedContact> {
        val raw = prefs(context).getString(KEY, null) ?: return emptyList()
        return try {
            json.decodeFromString<List<Stored>>(raw).map { s ->
                AcceptedContact(
                    request = IncomingContactRequest(
                        messageId = s.messageId,
                        fromPubkey = unb64(s.fromPubkeyB64),
                        fromAccountId = s.fromAccountIdB64?.let { unb64(it) },
                        fromPseudonym = unb64(s.fromPseudonymB64),
                        validAfterMs = s.validAfterMs.toULong(),
                        intro = s.intro,
                        createdAt = s.createdAt,
                    ),
                    groupIdHex = s.groupIdHex,
                )
            }
        } catch (e: Exception) {
            emptyList()
        }
    }

    /** Persist the accepted contacts. Best-effort — an encode failure
     *  leaves the prior value in place rather than clearing it. */
    fun save(context: Context, accepted: List<AcceptedContact>) {
        val stored = accepted.map { c ->
            Stored(
                messageId = c.request.messageId,
                fromPubkeyB64 = b64(c.request.fromPubkey),
                fromAccountIdB64 = c.request.fromAccountId?.let { b64(it) },
                fromPseudonymB64 = b64(c.request.fromPseudonym),
                validAfterMs = c.request.validAfterMs.toLong(),
                intro = c.request.intro,
                createdAt = c.request.createdAt,
                groupIdHex = c.groupIdHex,
            )
        }
        prefs(context).edit().putString(KEY, json.encodeToString(stored)).apply()
    }

    private fun prefs(context: Context) =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
