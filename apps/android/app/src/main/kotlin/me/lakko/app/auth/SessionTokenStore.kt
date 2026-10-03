// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Encrypted cache of the auth service's **session pair** for the active
 * account (ADR-0065 section 3: the short-lived access token, the refresh
 * credential and the key generation they were issued under), so a process
 * restart can re-attach to the live session without a password round-trip.
 *
 * Mirror of `apps/web/src/auth/session-token-store.ts`, which persists the
 * same pair under the web device key for the same reason. Before this store
 * existed the Android client held the token in memory only, so every
 * relaunch landed in a `Session` state with no identity secrets.
 *
 * ## What the pair grants, and what it does not
 *
 * Granted: bearer-authenticated requests to the token-gated services for
 * this account until the access token's expiry (15 minutes), and a refresh
 * of that token until the refresh credential's (30 days, sliding). NOT
 * granted: any message key, MLS state, or the AccountIdentity seed - those
 * live in their own stores. A leaked pair is an ACCESS credential, not an
 * E2EE one (invariant #2 is unaffected), and a reset of the account keys
 * revokes it (the generation).
 *
 * ## Storage posture
 *
 * [EncryptedSharedPreferences] with the master key in the Android Keystore -
 * the same posture as [ExportKeyStore] and [AccountIdentitySeedStore], so the
 * at-rest protection is hardware-backed (invariant #7). Writes use `commit()`
 * rather than `apply()`: a logout must be durable before the process can die,
 * or the next launch would silently re-attach to a session the user ended.
 *
 * ## Layout
 *
 * `EncryptedSharedPreferences` named `"lakko-session-token"`. Keys, all
 * suffixed with the account:
 * `"session-token-v1:"` / `"session-expiry-v1:"` (the access token and its
 * unix-millisecond expiry), `"refresh-token-v1:"` / `"refresh-expiry-v1:"`
 * (the refresh credential and its expiry) and `"key-generation-v1:"`. A row
 * missing any of the five reads as absent (flag day: no pre-pair row is
 * restored).
 */
public object SessionTokenStore {

    private const val PREFS_NAME = "lakko-session-token"
    private const val TOKEN_PREFIX = "session-token-v1:"
    private const val EXPIRY_PREFIX = "session-expiry-v1:"
    private const val REFRESH_PREFIX = "refresh-token-v1:"
    private const val REFRESH_EXPIRY_PREFIX = "refresh-expiry-v1:"
    private const val GENERATION_PREFIX = "key-generation-v1:"

    /**
     * Persist [pair] for [accountIdB64], replacing any prior row. Throws when
     * the write did not commit, so a caller can log it; the consequence of a
     * failed write is only that the next launch lands signed out (fail
     * closed).
     */
    public fun putSessionPair(
        context: Context,
        accountIdB64: String,
        pair: SessionPair,
    ) {
        require(pair.accessToken.isNotBlank()) { "session token must be non-blank" }
        require(pair.refreshToken.isNotBlank()) { "refresh token must be non-blank" }
        val committed = openPrefs(context)
            .edit()
            .putString(TOKEN_PREFIX + accountIdB64, pair.accessToken)
            .putLong(EXPIRY_PREFIX + accountIdB64, pair.accessExpiresAtMs)
            .putString(REFRESH_PREFIX + accountIdB64, pair.refreshToken)
            .putLong(REFRESH_EXPIRY_PREFIX + accountIdB64, pair.refreshExpiresAtMs)
            .putLong(GENERATION_PREFIX + accountIdB64, pair.keyGeneration)
            .commit()
        check(committed) { "session pair write did not commit" }
    }

    /**
     * Read the persisted pair for [accountIdB64]. Returns `null` when absent
     * or when any part of the row is missing. Expiry is NOT evaluated here -
     * that is the caller's decision ([decideSessionRestore]), so it can be
     * tested without a Keystore.
     */
    public fun getSessionPair(context: Context, accountIdB64: String): SessionPair? {
        val prefs = openPrefs(context)
        val token = prefs.getString(TOKEN_PREFIX + accountIdB64, null) ?: return null
        val refresh = prefs.getString(REFRESH_PREFIX + accountIdB64, null) ?: return null
        for (key in listOf(EXPIRY_PREFIX, REFRESH_EXPIRY_PREFIX, GENERATION_PREFIX)) {
            if (!prefs.contains(key + accountIdB64)) return null
        }
        return SessionPair(
            accessToken = token,
            accessExpiresAtMs = prefs.getLong(EXPIRY_PREFIX + accountIdB64, 0L),
            refreshToken = refresh,
            refreshExpiresAtMs = prefs.getLong(REFRESH_EXPIRY_PREFIX + accountIdB64, 0L),
            keyGeneration = prefs.getLong(GENERATION_PREFIX + accountIdB64, 1L),
        )
    }

    /** Erase the row for [accountIdB64]. No-op when absent. Synchronous. */
    public fun deleteSessionToken(context: Context, accountIdB64: String) {
        openPrefs(context)
            .edit()
            .remove(TOKEN_PREFIX + accountIdB64)
            .remove(EXPIRY_PREFIX + accountIdB64)
            .remove(REFRESH_PREFIX + accountIdB64)
            .remove(REFRESH_EXPIRY_PREFIX + accountIdB64)
            .remove(GENERATION_PREFIX + accountIdB64)
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
