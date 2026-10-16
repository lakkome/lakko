// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import android.content.SharedPreferences
import java.util.Base64
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json

/**
 * On-device persistence for the user's accepted contacts — the Android
 * mirror of the iOS `AcceptedContactsStore` and the web `local-store`
 * accepted list.
 *
 * Closes (for the accepted list) the long-standing "No persistence yet"
 * gap on [ContactsRepository]: the accepted contacts (each carrying
 * `request.fromAccountId`) survive a relaunch. (A group-call tile is no
 * longer named through the account id: since ADR-0078 section 2 the SFU
 * knows no account and each peer announces itself, sealed and signed.)
 *
 * ## One list per ACCOUNT, never per device
 *
 * The list (with the local renames it carries) belongs to the signed-in
 * account. Until 2026-09-30 it lived under one device-wide key, so a
 * second account signed in on the same phone listed, messaged and
 * searched the first account's contacts as its own. Every entry is now
 * keyed on the owning account id, the way the stranger inbox
 * ([ContactsInboxStore]) already was:
 *
 *   * [bind] records the signed-in account. [AuthRepository] calls it at
 *     every point a session lands (cold-start restore and every
 *     authenticated landing) and [unbind] when the session ends.
 *   * [load] / [save] without an account act on the bound account. With
 *     no account bound a read is EMPTY and a write is refused — there is
 *     no shared fallback key.
 *   * A caller that already knows the account (backup compose / restore,
 *     the pairing state bundle, [ContactsRepository] itself) passes it,
 *     so a restore writes into the RESTORED account's list whatever is
 *     bound at the time.
 *   * Log out keeps the list for the next login of the same account;
 *     Forget device calls [delete] for the departing account.
 *
 * The accepted list is non-secret relationship metadata (peer pubkeys +
 * account_ids the user already chose to trust, which the contacts UI
 * already shows), JSON-encoded into plain `SharedPreferences`; it is
 * never sent to any server (invariant #1/#2 unaffected). The `ByteArray`
 * fields ride as Base64 strings in a serialisable DTO so the domain model
 * stays free of serialization annotations.
 *
 * Preference file `lakko-accepted-contacts`, key `accepted.v2:<accountIdB64>`
 * (standard base64 of the 16-byte account id). The pre-2026-09-30 global key
 * `accepted.v1` is never read; [delete] removes it if it is still there.
 */
public object AcceptedContactsStore {
    private const val PREFS = "lakko-accepted-contacts"
    private const val KEY_PREFIX = "accepted.v2:"

    /** The device-wide key the list used to live under. Never read. */
    private const val LEGACY_GLOBAL_KEY = "accepted.v1"

    private val json = Json { ignoreUnknownKeys = true }

    /** Where the encoded lists live. Production is `SharedPreferences`;
     *  host tests pass an in-memory map. */
    internal interface Backend {
        fun get(key: String): String?
        fun put(key: String, value: String)
        fun remove(key: String)
    }

    private class PrefsBackend(private val prefs: SharedPreferences) : Backend {
        override fun get(key: String): String? = prefs.getString(key, null)
        override fun put(key: String, value: String) {
            prefs.edit().putString(key, value).apply()
        }
        override fun remove(key: String) {
            prefs.edit().remove(key).apply()
        }
    }

    private fun backendOf(context: Context): Backend =
        PrefsBackend(context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE))

    // ---------------------------------------------------------------------
    // The bound account
    // ---------------------------------------------------------------------

    @Volatile
    private var boundAccount: String? = null

    /** The signed-in account the account-less calls act on, standard base64;
     *  `null` when no session is live. */
    public val boundAccountB64: String?
        get() = boundAccount

    /** Bind the signed-in account (from the session landing). */
    public fun bind(accountId: ByteArray) {
        boundAccount = Base64.getEncoder().encodeToString(accountId)
    }

    /** Drop the binding (session ended). The stored lists stay. */
    public fun unbind() {
        boundAccount = null
    }

    // ---------------------------------------------------------------------
    // Reads
    // ---------------------------------------------------------------------

    /** The bound account's accepted contacts; empty with no account bound,
     *  on first use, or on a decode failure (a forward-compat / corruption
     *  guard). */
    public fun load(context: Context): List<AcceptedContact> = load(backendOf(context))

    /** [accountId]'s accepted contacts, whatever is bound. */
    public fun load(context: Context, accountId: ByteArray): List<AcceptedContact> =
        loadFor(backendOf(context), Base64.getEncoder().encodeToString(accountId))

    /** [accountIdB64]'s accepted contacts, whatever is bound. */
    public fun load(context: Context, accountIdB64: String): List<AcceptedContact> =
        loadFor(backendOf(context), accountIdB64)

    internal fun load(backend: Backend): List<AcceptedContact> {
        val account = boundAccount ?: return emptyList()
        return loadFor(backend, account)
    }

    internal fun loadFor(backend: Backend, accountIdB64: String): List<AcceptedContact> {
        val raw = backend.get(keyOf(accountIdB64)) ?: return emptyList()
        return decode(raw) ?: emptyList()
    }

    // ---------------------------------------------------------------------
    // Writes
    // ---------------------------------------------------------------------

    /** Persist [accepted] for the bound account. Returns `false`, writing
     *  nothing, when no account is bound. */
    public fun save(context: Context, accepted: List<AcceptedContact>): Boolean =
        save(backendOf(context), accepted)

    /** Persist [accepted] for [accountId], whatever is bound. */
    public fun save(context: Context, accountId: ByteArray, accepted: List<AcceptedContact>) {
        saveFor(backendOf(context), Base64.getEncoder().encodeToString(accountId), accepted)
    }

    /** Persist [accepted] for [accountIdB64], whatever is bound. */
    public fun save(context: Context, accountIdB64: String, accepted: List<AcceptedContact>) {
        saveFor(backendOf(context), accountIdB64, accepted)
    }

    internal fun save(backend: Backend, accepted: List<AcceptedContact>): Boolean {
        val account = boundAccount ?: return false
        saveFor(backend, account, accepted)
        return true
    }

    /** Best-effort — an encode failure leaves the prior value in place
     *  rather than clearing it. */
    internal fun saveFor(backend: Backend, accountIdB64: String, accepted: List<AcceptedContact>) {
        val encoded = try {
            encode(accepted)
        } catch (_: Exception) {
            return
        }
        backend.put(keyOf(accountIdB64), encoded)
    }

    /** Forget device: drop [accountIdB64]'s list (and the retired global
     *  key, if a pre-2026-09-30 build left one). Other accounts' lists stay. */
    public fun delete(context: Context, accountIdB64: String) {
        deleteFor(backendOf(context), accountIdB64)
    }

    internal fun deleteFor(backend: Backend, accountIdB64: String) {
        backend.remove(keyOf(accountIdB64))
        backend.remove(LEGACY_GLOBAL_KEY)
    }

    // ---------------------------------------------------------------------
    // Keys + codec
    // ---------------------------------------------------------------------

    /** The key for one account. The id is re-encoded as standard base64 so
     *  a caller's URL-safe or unpadded spelling of the same id lands on the
     *  same key. */
    internal fun keyOf(accountIdB64: String): String = KEY_PREFIX + canonicalAccountB64(accountIdB64)

    /** [accountIdB64] re-encoded as standard padded base64 (the spelling
     *  [bind] records); unchanged when it does not decode. */
    public fun canonicalAccountB64(accountIdB64: String): String =
        try {
            Base64.getEncoder().encodeToString(decodeAnyBase64(accountIdB64))
        } catch (_: IllegalArgumentException) {
            accountIdB64
        }

    private fun decodeAnyBase64(s: String): ByteArray =
        try {
            Base64.getDecoder().decode(s)
        } catch (_: IllegalArgumentException) {
            Base64.getUrlDecoder().decode(s)
        }

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
        // ADR-0021 follow-up: user-chosen display name. Optional with a
        // default so a record without it decodes with `displayName == null`.
        val displayName: String? = null,
    )

    private fun b64(bytes: ByteArray): String = Base64.getEncoder().encodeToString(bytes)

    private fun unb64(s: String): ByteArray = Base64.getDecoder().decode(s)

    internal fun encode(accepted: List<AcceptedContact>): String =
        json.encodeToString(
            accepted.map { c ->
                Stored(
                    messageId = c.request.messageId,
                    fromPubkeyB64 = b64(c.request.fromPubkey),
                    fromAccountIdB64 = c.request.fromAccountId?.let { b64(it) },
                    fromPseudonymB64 = b64(c.request.fromPseudonym),
                    validAfterMs = c.request.validAfterMs.toLong(),
                    intro = c.request.intro,
                    createdAt = c.request.createdAt,
                    groupIdHex = c.groupIdHex,
                    displayName = c.displayName,
                )
            },
        )

    internal fun decode(raw: String): List<AcceptedContact>? =
        try {
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
                    displayName = s.displayName,
                )
            }
        } catch (_: Exception) {
            null
        }
}
