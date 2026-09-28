// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.security.SecureRandom

/**
 * Per-device push pseudonym — 32 random bytes generated on first
 * push registration and persisted in `EncryptedSharedPreferences`
 * for the lifetime of the device.
 *
 * Mirror of:
 *
 *   * `apps/web/src/push/device-id.ts` (1.20b)
 *   * `apps/ios/Lakko/Push/DevicePushId.swift` (1.20c)
 *
 * ## Why a fresh random value
 *
 * The push server sees `(device_pseudonym, provider) → token`.
 * `/push/notify` is gone (ADR-0054: a wake is a consequence of a
 * signed DS publish). To minimise what the server can link:
 *
 *   * Deriving from `identity_pubkey` would let the server
 *     correlate identity uploads with push registrations the
 *     moment a peer publishes a contact request.
 *   * **Fresh random 32 bytes, persisted in
 *     EncryptedSharedPreferences**: the server sees opaque
 *     randomness; clearing the store (or running
 *     ``DevicePushId.clear()``) rotates the pseudonym.
 *
 * ## Storage layout
 *
 * `EncryptedSharedPreferences` named `"lakko-push"`, AES-256-GCM
 * value encryption, AES-256-SIV key encryption, master key in the
 * Android Keystore. Mirror of the iOS `Keychain` posture: the
 * value is at-rest-encrypted with hardware-backed keys; an
 * attacker with raw disk access cannot read it without unlocking
 * the device.
 *
 * Key: `"device-id-v1"` (schema-versioned for a possible future
 * migration), value: base64-encoded 32-byte pseudonym.
 */
public object DevicePushId {

    private const val PREFS_NAME = "lakko-push"
    private const val KEY_PSEUDONYM = "device-id-v1"
    private const val PSEUDONYM_LEN = 32

    /**
     * Return the per-device push pseudonym, generating one and
     * persisting it on first call. Subsequent calls return the
     * same bytes for the lifetime of the EncryptedSharedPreferences
     * entry.
     *
     * @throws Throwable on Keystore / EncryptedSharedPreferences
     *         failure. Callers treat as "push not available" and
     *         fall back to foreground polling.
     */
    public fun getOrCreate(context: Context): ByteArray {
        val prefs = openPrefs(context)
        prefs.getString(KEY_PSEUDONYM, null)?.let { encoded ->
            val bytes = decodeOrNull(encoded)
            if (bytes != null && bytes.size == PSEUDONYM_LEN) {
                return bytes
            }
            // Corrupted entry — drop + regen.
            prefs.edit().remove(KEY_PSEUDONYM).apply()
        }
        val fresh = ByteArray(PSEUDONYM_LEN).also { SecureRandom().nextBytes(it) }
        prefs.edit()
            .putString(KEY_PSEUDONYM, Base64.encodeToString(fresh, Base64.NO_WRAP))
            .apply()
        return fresh
    }

    /** Read-without-create — `null` when no entry exists. Used
     *  by the unsubscribe path: skip the server call when we
     *  have nothing to unregister. */
    public fun peek(context: Context): ByteArray? {
        val prefs = openPrefs(context)
        val encoded = prefs.getString(KEY_PSEUDONYM, null) ?: return null
        return decodeOrNull(encoded)?.takeIf { it.size == PSEUDONYM_LEN }
    }

    /** Drop the persisted pseudonym. Called by `forgetDevice` +
     *  the unsubscribe-from-push orchestrator. */
    public fun clear(context: Context) {
        openPrefs(context).edit().remove(KEY_PSEUDONYM).apply()
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

    private fun decodeOrNull(encoded: String): ByteArray? {
        return try {
            Base64.decode(encoded, Base64.NO_WRAP)
        } catch (_: IllegalArgumentException) {
            null
        }
    }
}
