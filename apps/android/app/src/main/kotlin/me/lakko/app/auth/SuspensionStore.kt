// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * ADR-0099 section 4 - the suspended mode of an account, kept on disk so it
 * survives a restart: the `s1` restricted session and the `r2` refresh
 * credential a `403 account-suspended` handed this device.
 *
 * The same posture as [SessionTokenStore] ([EncryptedSharedPreferences],
 * master key in the Android Keystore, `commit()` writes). The fresh-auth
 * grant a sign-in's refusal carries is NOT kept here: it lives in memory
 * only, like every fresh-auth proof.
 *
 * Removed by sign-out, by the suspension ending, and by the account wipe
 * (`AccountLocalWipe`, step "suspension").
 */
public object SuspensionStore {

    private const val PREFS_NAME = "lakko-suspension"
    private const val RESTRICTED = "restricted-v1:"
    private const val RESTRICTED_EXPIRY = "restricted-expiry-v1:"
    private const val REFRESH = "refresh-v1:"
    private const val REFRESH_EXPIRY = "refresh-expiry-v1:"
    private const val GENERATION = "key-generation-v1:"

    /** Persist [credentials] for their account (the grant is left out). */
    public fun put(context: Context, credentials: SuspendedCredentials) {
        val acct = credentials.accountIdB64
        val committed = openPrefs(context)
            .edit()
            .putString(RESTRICTED + acct, credentials.restrictedToken)
            .putLong(RESTRICTED_EXPIRY + acct, credentials.restrictedExpiresAtMs)
            .putString(REFRESH + acct, credentials.refreshToken)
            .putLong(REFRESH_EXPIRY + acct, credentials.refreshExpiresAtMs)
            .putLong(GENERATION + acct, credentials.keyGeneration)
            .commit()
        check(committed) { "suspension record write did not commit" }
    }

    /** The record of [accountIdB64], or `null`. */
    public fun get(context: Context, accountIdB64: String): SuspendedCredentials? {
        val prefs = openPrefs(context)
        val restricted = prefs.getString(RESTRICTED + accountIdB64, null) ?: return null
        val refresh = prefs.getString(REFRESH + accountIdB64, null) ?: return null
        for (key in listOf(RESTRICTED_EXPIRY, REFRESH_EXPIRY, GENERATION)) {
            if (!prefs.contains(key + accountIdB64)) return null
        }
        return SuspendedCredentials(
            accountIdB64 = accountIdB64,
            restrictedToken = restricted,
            restrictedExpiresAtMs = prefs.getLong(RESTRICTED_EXPIRY + accountIdB64, 0L),
            refreshToken = refresh,
            refreshExpiresAtMs = prefs.getLong(REFRESH_EXPIRY + accountIdB64, 0L),
            keyGeneration = prefs.getLong(GENERATION + accountIdB64, 1L),
        )
    }

    /** Whether the account this device is signed in to (the session marker)
     *  is in suspended mode. Read where the in-memory switch may not be set
     *  yet (a process a push woke). `false` when anything is unreadable. */
    public fun isSuspendedHere(context: Context): Boolean = try {
        val acct = SessionStore(context.applicationContext).load()?.accountIdB64
        acct != null && get(context, acct) != null
    } catch (_: Throwable) {
        false
    }

    /** Erase the record of [accountIdB64]. Synchronous. */
    public fun delete(context: Context, accountIdB64: String) {
        openPrefs(context)
            .edit()
            .remove(RESTRICTED + accountIdB64)
            .remove(RESTRICTED_EXPIRY + accountIdB64)
            .remove(REFRESH + accountIdB64)
            .remove(REFRESH_EXPIRY + accountIdB64)
            .remove(GENERATION + accountIdB64)
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
