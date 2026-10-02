// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Encrypted **AccountIdentity seed** cache (ADR-0017, Part 1 —
 * phrase-seeded identity).
 *
 * Mirror of:
 *
 *   * `apps/web/src/auth/account-identity-seed-store.ts`
 *   * `apps/ios/Lakko/Auth/AccountIdentitySeedStore.swift`
 *
 * The AccountIdentity is the **account-level** Ed25519 + X25519
 * keypair shared across all of the user's devices and published in
 * the discovery identity bundle / KT log / recovery proofs — the key
 * contacts pin. Per ADR-0017 it is now derived from the user's
 * stable BIP39 **recovery-phrase seed**
 * (`RecoveryMnemonic.fromPhrase(phrase).toSeed()`, **64 bytes**), NOT
 * the password-derived OPAQUE `exportKey`, so a routine recover /
 * password-change preserves the identity (the phrase does not change
 * -> the seed does not change -> the pubkey does not churn).
 *
 * This store is the parallel of [ExportKeyStore] / [DeviceIdentityStore]:
 * it holds the BIP39 phrase seed under the **same**
 * [EncryptedSharedPreferences] posture (AES-256-GCM value encryption
 * + AES-256-SIV key encryption with the master key in the Android
 * Keystore). The wrapping master key is hardware-backed and never
 * leaves the Keystore sandbox, preserving invariant #7
 * (hardware-backed device keys).
 *
 * ## What's stored
 *
 * A single **64-byte** seed. The runtime AccountIdentity is rebuilt
 * deterministically by `Identity.fromSeed(seed)` — the same shape as
 * the `Identity.fromSeed(exportKey)` path the clients used before
 * this ADR, just with the BIP39 PBKDF2 seed bytes instead of the
 * OPAQUE-derived ones. `Identity::fromSeed` HKDF-expands arbitrary
 * input bytes under a pinned salt, so it is seed-agnostic
 * (ADR-0017 "Scope" section) — no `lakko-mls` / Rust-core change.
 *
 * ## Threat model
 *
 * This is the **highest-sensitivity** on-device secret: leaking the
 * AccountIdentity seed = full account-identity compromise (it is the
 * recovery factor itself). Compared to [ExportKeyStore]: the export
 * key can be rotated by a password change; this seed only changes if
 * the user changes their recovery phrase (a deliberate "rotate my
 * identity" action, out of scope for Part 1). It is wiped on
 * `forgetDevice` alongside the other per-account secrets.
 *
 * ## Storage layout
 *
 * `EncryptedSharedPreferences` named `"lakko-account-identity-seed"`.
 * Keys: `"account-identity-seed-v1:<accountIdB64>"`. Value:
 * base64-encoded 64 raw seed bytes.
 */
public object AccountIdentitySeedStore {

    private const val PREFS_NAME = "lakko-account-identity-seed"
    private const val KEY_PREFIX = "account-identity-seed-v1:"

    /** Fixed seed length pinned by the BIP39 PBKDF2 seed contract
     *  (`RecoveryMnemonic.toSeed()` returns 64 bytes). */
    public const val SEED_LENGTH: Int = 64

    /**
     * Persist `seed` (64 bytes) for `accountIdB64`. Overwrites any
     * prior row. Caller is responsible for zeroing `seed` after this
     * returns; the function does not retain a reference past return.
     *
     * @throws IllegalArgumentException when `seed.size != 64`.
     */
    public fun putAccountIdentitySeed(
        context: Context,
        accountIdB64: String,
        seed: ByteArray,
    ) {
        require(seed.size == SEED_LENGTH) {
            "AccountIdentity seed must be $SEED_LENGTH bytes (got ${seed.size})"
        }
        val encoded = Base64.encodeToString(seed, Base64.NO_WRAP)
        openPrefs(context)
            .edit()
            .putString(KEY_PREFIX + accountIdB64, encoded)
            .apply()
    }

    /**
     * Read the cached AccountIdentity seed for `accountIdB64`.
     * Returns `null` when absent (fresh device, post-`forgetDevice`,
     * never-seen account, or a device that logged in with only a
     * password before this seed was cached) or when the stored row is
     * malformed (wrong base64, wrong decoded length).
     *
     * The caller MUST NOT silently fall back to deriving the identity
     * from the export key or minting a fresh identity on a `null` —
     * per ADR-0017 the fresh-device fail-safe surfaces a
     * `NeedsRecoveryPhrase` state instead. Caller MUST zero the
     * returned bytes after use.
     */
    public fun getAccountIdentitySeed(
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
     * Erase the cached AccountIdentity seed for `accountIdB64`.
     * No-op when absent. Called from `forgetDevice` so the full-wipe
     * contract holds. After this the keys come back only from another
     * device, a passkey that restores keys, or the recovery phrase.
     */
    public fun deleteAccountIdentitySeed(
        context: Context,
        accountIdB64: String,
    ) {
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
