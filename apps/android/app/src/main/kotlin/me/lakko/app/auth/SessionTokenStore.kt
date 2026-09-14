// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Encrypted cache of the auth service's bearer **session token** for the
 * active account, so a process restart can re-attach to the live session
 * without a password round-trip.
 *
 * Mirror of `apps/web/src/auth/session-token-store.ts` (Phase 1.13c), which
 * persists the same token under the web device key for the same reason.
 * Before this store existed the Android client held the token in memory only,
 * so every relaunch landed in a `Session` state with no identity secrets:
 * the chat bootstrap never ran and the Contacts panel asked the user to log
 * out and back in.
 *
 * ## What the token grants, and what it does not
 *
 * Granted: bearer-authenticated writes to `services/discovery` and the other
 * token-gated endpoints for this account until the server-side expiry
 * (a 30-day TTL today). NOT granted: any message key, MLS state, or the
 * AccountIdentity seed — those live in their own stores. A leaked token is an
 * ACCESS credential, not an E2EE one (invariant #2 is unaffected).
 *
 * ## Storage posture
 *
 * [EncryptedSharedPreferences] with the master key in the Android Keystore —
 * the same posture as [ExportKeyStore] and [AccountIdentitySeedStore], so the
 * at-rest protection is hardware-backed (invariant #7). Writes use `commit()`
 * rather than `apply()`: a logout must be durable before the process can die,
 * or the next launch would silently re-attach to a session the user ended.
 *
 * ## Layout
 *
 * `EncryptedSharedPreferences` named `"lakko-session-token"`. Keys:
 * `"session-token-v1:<accountIdB64>"` (the token string) and
 * `"session-expiry-v1:<accountIdB64>"` (unix milliseconds).
 */
public object SessionTokenStore {

    private const val PREFS_NAME = "lakko-session-token"
    private const val TOKEN_PREFIX = "session-token-v1:"
    private const val EXPIRY_PREFIX = "session-expiry-v1:"

    /**
     * Persist [token] + [expiresAtMs] for [accountIdB64], replacing any prior
     * row. Throws when the write did not commit, so a caller can log it; the
     * consequence of a failed write is only that the next launch lands signed
     * out (fail closed).
     */
    public fun putSessionToken(
        context: Context,
        accountIdB64: String,
        token: String,
        expiresAtMs: Long,
    ) {
        require(token.isNotBlank()) { "session token must be non-blank" }
        val committed = openPrefs(context)
            .edit()
            .putString(TOKEN_PREFIX + accountIdB64, token)
            .putLong(EXPIRY_PREFIX + accountIdB64, expiresAtMs)
            .commit()
        check(committed) { "session token write did not commit" }
    }

    /**
     * Read the persisted token for [accountIdB64]. Returns `null` when absent
     * or when either half of the row is missing. Expiry is NOT evaluated here —
     * that is the caller's decision ([decideSessionRestore]), so it can be
     * tested without a Keystore.
     */
    public fun getSessionToken(context: Context, accountIdB64: String): PersistedSessionToken? {
        val prefs = openPrefs(context)
        val token = prefs.getString(TOKEN_PREFIX + accountIdB64, null) ?: return null
        if (!prefs.contains(EXPIRY_PREFIX + accountIdB64)) return null
        val expiresAtMs = prefs.getLong(EXPIRY_PREFIX + accountIdB64, 0L)
        return PersistedSessionToken(token = token, expiresAtMs = expiresAtMs)
    }

    /** Erase the row for [accountIdB64]. No-op when absent. Synchronous. */
    public fun deleteSessionToken(context: Context, accountIdB64: String) {
        openPrefs(context)
            .edit()
            .remove(TOKEN_PREFIX + accountIdB64)
            .remove(EXPIRY_PREFIX + accountIdB64)
            .commit()
    }

    private fun openPrefs(context: Context): SharedPreferences {
        val master = MasterKey.Builder(context.applicationContext)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        return EncryptedSharedPreferences.create(
            context.applicationContext,
            PREFS_NAME,
            master,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }
}

/** A persisted bearer token and its server-stated expiry. */
public data class PersistedSessionToken(
    val token: String,
    /** Unix milliseconds; the token is unusable at or after this instant. */
    val expiresAtMs: Long,
) {
    /** Never print the bearer token into a log line. */
    override fun toString(): String = "PersistedSessionToken(token=<redacted>, expiresAtMs=$expiresAtMs)"
}
