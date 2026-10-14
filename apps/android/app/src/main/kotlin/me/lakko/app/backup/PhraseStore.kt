// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import me.lakko.app.account.AccountStoreGate

/**
 * The recovery phrase on a device that holds the account's keys, at rest.
 *
 * Mirror of:
 *
 *   * `apps/web/src/backup/phrase-store.ts`
 *   * `apps/ios/Lakko/Backup/PhraseStore.swift`
 *
 * Stores the canonical recovery phrase bytes (UTF-8, 12 lower-case words,
 * single-space-separated) in [`EncryptedSharedPreferences`], AES-256-GCM
 * value encryption + AES-256-SIV key encryption with the master key in the
 * Android Keystore. Same posture as the other per-account secrets here:
 * at-rest hardware-backed encryption, never leaves the Keystore sandbox.
 *
 * Every landing path that gives the device keys writes it
 * ([DevicePhrase]); it is read by the encrypted backup, the local
 * three-word check, the hand-over to a new device and "Show recovery
 * phrase" in Settings.
 *
 * ## Per-account isolation
 *
 * One key per `accountIdB64`. Multi-account on the same device
 * (currently unsupported in Phase-1 UX but the data model
 * tolerates it) gets distinct phrases per account. Forget-device
 * drops the row, and every write takes the wipe mark of the work that
 * makes it, so a landing that was out while the account was forgotten
 * cannot write the row back ([putPhrase]). Deletes are not gated.
 *
 * ## Threat model
 *
 * - **At-rest confidentiality**: Keystore-backed master key
 *   guards the AES-256-GCM that wraps the value. The bytes
 *   never leave the EncryptedSharedPreferences sandbox.
 * - **Process-memory exposure**: phrase bytes briefly live in
 *   ByteArray on read; same window as the OPAQUE export key.
 * - **Cross-device migration**: EncryptedSharedPreferences is
 *   local-device only. The phrase reaches another device only as
 *   typed words, as its entropy inside the end-to-end encrypted
 *   another-device transfer, or sealed under a passkey's own secret
 *   when the person turned that on.
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
     * Persist `phraseBytes` as `accountIdB64`'s phrase. Overwrites any prior
     * row. `false`, and nothing written, when the account's data was wiped
     * after [since] was taken (a Forget device that ran while the landing
     * that writes this was out): the wipe removed this row, and a late write
     * would put a forgotten account's phrase back
     * ([me.lakko.app.account.AccountStoreGate.writeUnlessWipedSince]).
     *
     * A sign-out refuses nothing here, nor does a wipe that came before
     * [since]: a sign-in writes the phrase BEFORE its account lands, so an
     * account still sealed from an earlier sign-out, or forgotten earlier and
     * now signing back in, must take the write.
     *
     * Caller is responsible for zeroing `phraseBytes` after this returns;
     * the function does not retain a reference past return.
     */
    public fun putPhrase(
        context: Context,
        accountIdB64: String,
        phraseBytes: ByteArray,
        since: AccountStoreGate.WipeMark,
    ): Boolean = putRowUnlessWipedSince(accountIdB64, since) {
        putRow(context, KEY_PREFIX + accountIdB64, phraseBytes)
    }

    /**
     * [putPhrase] for a guest account's row, which is kept under a key of
     * its own ([me.lakko.app.auth.guestPhraseKey]) and gated by the account
     * it belongs to.
     */
    public fun putGuestPhrase(
        context: Context,
        accountIdB64: String,
        phraseBytes: ByteArray,
        since: AccountStoreGate.WipeMark,
    ): Boolean = putRowUnlessWipedSince(accountIdB64, since) {
        putRow(context, KEY_PREFIX + me.lakko.app.auth.guestPhraseKey(accountIdB64), phraseBytes)
    }

    /** The guest row of `accountIdB64` ([putGuestPhrase]), or `null`. */
    public fun getGuestPhrase(context: Context, accountIdB64: String): ByteArray? =
        getPhrase(context, me.lakko.app.auth.guestPhraseKey(accountIdB64))

    /** Erase the guest row of `accountIdB64`. No-op when absent. */
    public fun deleteGuestPhrase(context: Context, accountIdB64: String) {
        deletePhrase(context, me.lakko.app.auth.guestPhraseKey(accountIdB64))
    }

    /** The gate of [putPhrase] and [putGuestPhrase] over the row write
     *  [put] (host tests drive it without the Keystore). */
    internal inline fun putRowUnlessWipedSince(
        accountIdB64: String,
        since: AccountStoreGate.WipeMark,
        put: () -> Unit,
    ): Boolean = AccountStoreGate.writeUnlessWipedSince(accountIdB64, since) {
        put()
        true
    } ?: false

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

    private fun putRow(context: Context, key: String, phraseBytes: ByteArray) {
        val encoded = Base64.encodeToString(phraseBytes, Base64.NO_WRAP)
        openPrefs(context)
            .edit()
            .putString(key, encoded)
            .apply()
    }

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
