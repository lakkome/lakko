// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.security.SecureRandom

/**
 * Encrypted **DeviceIdentity seed** cache (Phase 2a3b0,
 * ADR-0007).
 *
 * Mirror of:
 *
 *   * `apps/web/src/auth/device-identity-store.ts`
 *   * `apps/ios/Lakko/Auth/DeviceIdentityStore.swift`
 *
 * Caches the raw 32-byte random seed the DeviceIdentity
 * (per-device MLS leaf identity, distinct from the
 * deterministic AccountIdentity from OPAQUE exportKey) is built
 * from. Runtime DeviceIdentity is rebuilt deterministically by
 * `Identity.fromSeed(seed)` — same shape as the existing
 * `Identity::fromSeed(exportKey)` path, just with random input
 * bytes.
 *
 * See `docs/adr/0007-multi-device-identity.md` for the full
 * rationale.
 *
 * ## Threat model
 *
 * Compared to [ExportKeyStore]:
 *
 *   - **Export key** leak = account-wide compromise.
 *   - **DeviceIdentity seed** leak = THIS device's leaf
 *     material only. Surgical via DeviceGroup `Remove` (2a3c).
 *
 * Same EncryptedSharedPreferences posture as [PhraseStore] /
 * [ExportKeyStore] — AES-256-GCM value encryption with the
 * master key in the Android Keystore.
 *
 * ## Storage layout
 *
 * `EncryptedSharedPreferences` named `"lakko-device-identity"`.
 * Keys: `"device-identity-v1:<accountIdB64>"`. Value:
 * base64-encoded 32 raw seed bytes.
 */
public object DeviceIdentityStore {

    private const val PREFS_NAME = "lakko-device-identity"
    private const val KEY_PREFIX = "device-identity-v1:"
    private const val BINDING_PREFIX = "device-binding-v1:"

    /** Fixed seed length pinned by `Identity.fromSeed`'s
     *  32-byte Ed25519 seed contract. */
    public const val SEED_LENGTH: Int = 32

    /**
     * Persist `seed` (32 bytes) for `accountIdB64`. Overwrites
     * any prior row.
     *
     * @throws IllegalArgumentException when `seed.size != 32`.
     */
    public fun putDeviceIdentitySeed(
        context: Context,
        accountIdB64: String,
        seed: ByteArray,
    ) {
        require(seed.size == SEED_LENGTH) {
            "DeviceIdentity seed must be $SEED_LENGTH bytes (got ${seed.size})"
        }
        val encoded = Base64.encodeToString(seed, Base64.NO_WRAP)
        openPrefs(context)
            .edit()
            .putString(KEY_PREFIX + accountIdB64, encoded)
            .apply()
    }

    /**
     * Read the cached DeviceIdentity seed for `accountIdB64`.
     * Returns `null` when absent or when the stored row is
     * malformed (wrong base64, wrong decoded length).
     *
     * Caller MUST zero the returned bytes after use.
     */
    public fun getDeviceIdentitySeed(
        context: Context,
        accountIdB64: String,
    ): ByteArray? {
        val encoded = openPrefs(context)
            .getString(KEY_PREFIX + accountIdB64, null)
            ?: return null
        val decoded: ByteArray = try {
            Base64.decode(encoded, Base64.NO_WRAP)
        } catch (_: IllegalArgumentException) {
            return null
        }
        return if (decoded.size == SEED_LENGTH) decoded else null
    }

    /**
     * Erase the cached DeviceIdentity seed for `accountIdB64`.
     * No-op when absent. Called from `forgetDevice`.
     *
     * Note: deleting the seed alone does NOT remove the
     * device's leaf from the user's DeviceGroup — the other
     * devices need to see an MLS `Remove` commit to ratchet
     * past the gone leaf. The full revoke flow lands in 2a3c.
     */
    public fun deleteDeviceIdentitySeed(
        context: Context,
        accountIdB64: String,
    ) {
        openPrefs(context)
            .edit()
            .remove(KEY_PREFIX + accountIdB64)
            .remove(BINDING_PREFIX + accountIdB64)
            .apply()
    }

    /**
     * ADR-0050 — the persisted device binding wire bytes (public: the account
     * key's signature over this device's key), kept so `issued_at` stays stable
     * across restarts. `null` until the device is first bound.
     */
    public fun getDeviceBinding(context: Context, accountIdB64: String): ByteArray? {
        val encoded = openPrefs(context).getString(BINDING_PREFIX + accountIdB64, null)
            ?: return null
        return try {
            Base64.decode(encoded, Base64.NO_WRAP)
        } catch (_: IllegalArgumentException) {
            null
        }
    }

    /** Persist the ADR-0050 device binding beside the seed. */
    public fun putDeviceBinding(context: Context, accountIdB64: String, binding: ByteArray) {
        openPrefs(context)
            .edit()
            .putString(BINDING_PREFIX + accountIdB64, Base64.encodeToString(binding, Base64.NO_WRAP))
            .apply()
    }

    /**
     * ADR-0065 — drop the binding alone, keeping the seed: after a reset of
     * the account keys the stored binding was signed by the OLD account key,
     * and the next chat bootstrap mints one under the new key
     * ([SessionIdentities.deviceIdentity] also re-mints when it finds the
     * stored one names another account key, so this is belt and braces).
     */
    public fun deleteDeviceBinding(context: Context, accountIdB64: String) {
        openPrefs(context)
            .edit()
            .remove(BINDING_PREFIX + accountIdB64)
            .apply()
    }

    /**
     * Convenience: read the cached seed if present, otherwise
     * generate a fresh random 32-byte seed and persist it.
     * Returns the seed bytes in both branches. Idempotent.
     *
     * Called from the signup / login / recover handlers as
     * part of the "make sure this device has a DeviceIdentity"
     * hook.
     */
    public fun ensureDeviceIdentitySeed(
        context: Context,
        accountIdB64: String,
    ): ByteArray {
        val existing = getDeviceIdentitySeed(context, accountIdB64)
        if (existing != null) return existing
        val seed = ByteArray(SEED_LENGTH)
        SecureRandom().nextBytes(seed)
        putDeviceIdentitySeed(context, accountIdB64, seed)
        return seed
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
