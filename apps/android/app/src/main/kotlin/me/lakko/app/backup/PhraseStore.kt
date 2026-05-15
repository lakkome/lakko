// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Encrypted recovery-phrase cache for the auto-backup flow
 * (Phase 2a1c).
 *
 * Mirror of:
 *
 *   * `apps/web/src/backup/phrase-store.ts`
 *   * `apps/ios/Lakko/Backup/PhraseStore.swift`
 *
 * Stores the canonical recovery phrase bytes (UTF-8,
 * NFKD-normalised, single-space-separated) in
 * [`EncryptedSharedPreferences`], AES-256-GCM value encryption +
 * AES-256-SIV key encryption with the master key in the Android
 * Keystore. Same posture as [`DevicePushId`] and the iOS
 * Keychain `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`
 * row — at-rest hardware-backed encryption, never leaves the
 * Keystore sandbox.
 *
 * ## Per-account isolation
 *
 * One key per `accountIdB64`. Multi-account on the same device
 * (currently unsupported in Phase-1 UX but the data model
 * tolerates it) gets distinct phrases per account. Forget-device
 * drops the row.
 *
 * ## Threat model
 *
 * - **At-rest confidentiality**: Keystore-backed master key
 *   guards the AES-256-GCM that wraps the value. The bytes
 *   never leave the EncryptedSharedPreferences sandbox.
 * - **Process-memory exposure**: phrase bytes briefly live in
 *   ByteArray on read; same window as the OPAQUE export key.
 * - **Cross-device migration**: EncryptedSharedPreferences is
 *   local-device only. Recovery on a fresh device uses the
 *   recovery-phrase entry flow — that's the only way the
 *   phrase moves between devices.
 *
 * ## Storage layout
 *
 * `EncryptedSharedPreferences` named `"lakko-backup-phrase"`.
 * Keys: `"phrase-v1:<accountIdB64>"`. Value: base64-encoded UTF-8
 * phrase bytes.
 */
public object PhraseStore {

    private const val PREFS_NAME = "lakko-backup-phrase"
    private const val KEY_PREFIX = "phrase-v1:"

    /**
     * Persist `phraseBytes` for `accountIdB64`. Overwrites any
     * prior row. Caller is responsible for zeroing `phraseBytes`
     * after this returns; the function does not retain a
     * reference past return.
     */
    public fun putPhrase(
        context: Context,
        accountIdB64: String,
        phraseBytes: ByteArray,
    ) {
        val encoded = Base64.encodeToString(phraseBytes, Base64.NO_WRAP)
        openPrefs(context)
            .edit()
            .putString(KEY_PREFIX + accountIdB64, encoded)
            .apply()
    }

    /**
     * Read the cached phrase for `accountIdB64`. Returns the raw
     * UTF-8 bytes the caller hands to `sealBackup`, or `null`
     * when no row exists.
     */
    public fun getPhrase(context: Context, accountIdB64: String): ByteArray? {
        val encoded = openPrefs(context).getString(KEY_PREFIX + accountIdB64, null) ?: return null
        return try {
            Base64.decode(encoded, Base64.NO_WRAP)
        } catch (_: IllegalArgumentException) {
            null
        }
    }

    /**
     * Erase the cached phrase for `accountIdB64`. No-op when
     * absent. Called from the `forgetDevice` cleanup.
     */
    public fun deletePhrase(context: Context, accountIdB64: String) {
        openPrefs(context).edit().remove(KEY_PREFIX + accountIdB64).apply()
    }

    // -----------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------

    private fun openPrefs(context: Context): android.content.SharedPreferences {
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
