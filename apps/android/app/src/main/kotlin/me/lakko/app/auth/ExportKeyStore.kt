// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Encrypted OPAQUE export-key cache for sign-in-with-passkey
 * (Phase 2a2c2-android).
 *
 * Mirror of:
 *
 *   * `apps/web/src/auth/export-key-store.ts`
 *   * `apps/ios/Lakko/Auth/ExportKeyStore.swift`
 *
 * Caches the raw 32-byte OPAQUE export key in
 * [EncryptedSharedPreferences], AES-256-GCM value encryption +
 * AES-256-SIV key encryption with the master key in the Android
 * Keystore. Same posture as [me.lakko.app.backup.PhraseStore] and
 * the iOS Keychain `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`
 * row — at-rest hardware-backed encryption, never leaves the
 * Keystore sandbox.
 *
 * ## Threat model
 *
 * The export key is the seed for the account's private material
 * on this device. Caching it widens the attack surface vs.
 * in-memory-only storage (Keystore compromise = full identity
 * compromise until the user rotates via recovery phrase). The
 * trade-off buys sign-in-with-passkey on a device that has
 * already seen the account. See the web-side docstring for the
 * full rationale.
 *
 * ## Per-account isolation
 *
 * One key per `accountIdB64`. `forgetDevice` drops the row.
 *
 * ## Storage layout
 *
 * `EncryptedSharedPreferences` named `"lakko-export-key"`. Keys:
 * `"export-key-v1:<accountIdB64>"`. Value: base64-encoded 32
 * raw export-key bytes.
 */
public object ExportKeyStore {

    private const val PREFS_NAME = "lakko-export-key"
    private const val KEY_PREFIX = "export-key-v1:"

    /** Fixed export-key length pinned by the OPAQUE ciphersuite. */
    public const val EXPORT_KEY_LENGTH: Int = 32

    /**
     * Persist `exportKey` (32 bytes) for `accountIdB64`.
     * Overwrites any prior row. Caller is responsible for
     * zeroing `exportKey` after this returns; the function does
     * not retain a reference past return.
     *
     * @throws IllegalArgumentException when `exportKey.size != 32`.
     */
    public fun putExportKey(
        context: Context,
        accountIdB64: String,
        exportKey: ByteArray,
    ) {
        require(exportKey.size == EXPORT_KEY_LENGTH) {
            "export key must be $EXPORT_KEY_LENGTH bytes (got ${exportKey.size})"
        }
        val encoded = Base64.encodeToString(exportKey, Base64.NO_WRAP)
        openPrefs(context)
            .edit()
            .putString(KEY_PREFIX + accountIdB64, encoded)
            .apply()
    }

    /**
     * Read the cached export key for `accountIdB64`. Returns
     * `null` when absent (fresh device, post-`forgetDevice`,
     * never-logged-in account, etc.) or when the stored row is
     * malformed (wrong base64, wrong decoded length).
     *
     * Caller MUST zero the returned bytes after use.
     */
    public fun getExportKey(context: Context, accountIdB64: String): ByteArray? {
        val encoded = openPrefs(context)
            .getString(KEY_PREFIX + accountIdB64, null)
            ?: return null
        val decoded: ByteArray = try {
            Base64.decode(encoded, Base64.NO_WRAP)
        } catch (_: IllegalArgumentException) {
            return null
        }
        return if (decoded.size == EXPORT_KEY_LENGTH) decoded else null
    }

    /**
     * Erase the cached export key for `accountIdB64`. No-op when
     * absent. Called from `forgetDevice`.
     */
    public fun deleteExportKey(context: Context, accountIdB64: String) {
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
