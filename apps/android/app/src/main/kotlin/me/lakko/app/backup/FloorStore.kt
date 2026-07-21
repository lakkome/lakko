// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Backup-head version-floor store (ADR-0037 §6, G-11).
 *
 * Mirror of `apps/web/src/backup/floor-store.ts`.
 *
 * The monotonic rollback floor: the highest `backup_version` this device has
 * ever minted or restored, plus the envelope hash it committed. On a later
 * restore a **returning** client refuses any head below this floor outright
 * (hard, no freshness window) — the auth-assigned version never regresses, so
 * the floor is trustworthy against a storage-tier replay of an older head.
 *
 * ## Why device-key-encrypted (not plain SharedPreferences)
 *
 * The floor is integrity-critical: an attacker who could lower it would defeat
 * the returning-client rollback defence. Sealing it under
 * [EncryptedSharedPreferences] (AES-256-GCM value encryption + AES-256-SIV key
 * encryption with the master key in the Android Keystore) means a device-image
 * attacker who has not also broken the Keystore isolation boundary can neither
 * read nor tamper the floor. Same substrate + threat model as
 * [me.lakko.app.contacts.PeerPinStore] and the recovery-phrase
 * [PhraseStore].
 *
 * ## Storage layout
 *
 * `EncryptedSharedPreferences` named `"lakko-backup-floor"`. Keys:
 * `"floor-v1:<accountIdB64>"`. Value is a `|`-separated record
 * `"<version>|<envelopeHashB64>"` where `<version>` is the decimal `ULong`
 * `backup_version` and `<envelopeHashB64>` is the base64 (standard alphabet, no
 * wrap) of the 32-byte `BLAKE3` envelope hash. Base64 standard alphabet never
 * emits `|`, so the fields split cleanly.
 */
public object FloorStore {

    private const val PREFS_NAME = "lakko-backup-floor"
    private const val KEY_PREFIX = "floor-v1:"

    /** Raw `BLAKE3` envelope-hash length pinned by `backupEnvelopeHash`. */
    private const val ENVELOPE_HASH_LEN = 32

    /**
     * Persist (or advance) the version floor for `accountIdB64`.
     *
     * **Monotone.** If a higher (or equal) floor is already stored, this is a
     * no-op — the floor never regresses, mirroring the auth-side counter. A
     * deliberate stale-override restore therefore never lowers a returning
     * client's protection. Returns `true` when a write happened, `false` on the
     * monotone no-op.
     */
    public fun putFloor(
        context: Context,
        accountIdB64: String,
        floor: BackupFloor,
    ): Boolean {
        val existing = getFloor(context, accountIdB64)
        // Never lower an existing, cleanly-read floor (ULong comparison). A
        // corrupt / absent existing floor is overwritten by this fresh, valid one
        // — the write heals the anchor.
        if (existing is FloorRead.Present && existing.floor.version >= floor.version) {
            return false
        }
        val value = "${floor.version}|" +
            Base64.encodeToString(floor.envelopeHash, Base64.NO_WRAP)
        openPrefs(context)
            .edit()
            .putString(KEY_PREFIX + accountIdB64, value)
            .apply()
        return true
    }

    /**
     * Read the version floor for `accountIdB64`.
     *
     * Returns a discriminated [FloorRead] (F5a):
     * - [FloorRead.Absent] — no stored entry (fresh device — the zero-anchor
     *   path). Genuine absence stays fail-safe: a first-ever restore is NEVER
     *   [FloorRead.Corrupt].
     * - [FloorRead.Present] — a valid, parsed floor.
     * - [FloorRead.Corrupt] — an entry EXISTS but is undecryptable / wrong shape
     *   / bad base64 (a device-key rotation, keystore loss, or an attacker who
     *   tampered the row). Distinct from [FloorRead.Absent]: the anchor once
     *   existed and is now untrustworthy, so the restore gate must NOT silently
     *   fall to the weaker freshness path — it routes a corrupt anchor to
     *   [RestoreVerdict.StaleOverride].
     *
     * Total — never throws. A store that cannot even be opened (e.g. an
     * unrecoverable master-key rotation) means an anchor that once existed is now
     * unreadable, so it is surfaced as [FloorRead.Corrupt], never [FloorRead.Absent].
     */
    public fun getFloor(context: Context, accountIdB64: String): FloorRead {
        val storageKey = KEY_PREFIX + accountIdB64
        // `contains` answers "is there a stored entry" WITHOUT decrypting the
        // value (EncryptedSharedPreferences encrypts the key deterministically via
        // AES-SIV), so an undecryptable value does not masquerade as absent. A
        // store that fails to open at all is treated as a corrupt anchor.
        val prefs = try {
            openPrefs(context)
        } catch (_: Throwable) {
            return FloorRead.Corrupt
        }
        val present = try {
            prefs.contains(storageKey)
        } catch (_: Throwable) {
            return FloorRead.Corrupt
        }
        if (!present) return FloorRead.Absent
        // The entry exists → any read / parse failure below is a CORRUPT anchor,
        // never Absent.
        val raw = try {
            prefs.getString(storageKey, null)
        } catch (_: Throwable) {
            return FloorRead.Corrupt
        } ?: return FloorRead.Corrupt
        val parts = raw.split('|', limit = 2)
        if (parts.size != 2) return FloorRead.Corrupt
        val version = parts[0].toULongOrNull() ?: return FloorRead.Corrupt
        val hash = try {
            Base64.decode(parts[1], Base64.NO_WRAP)
        } catch (_: IllegalArgumentException) {
            return FloorRead.Corrupt
        }
        if (hash.size != ENVELOPE_HASH_LEN) return FloorRead.Corrupt
        return FloorRead.Present(BackupFloor(version = version, envelopeHash = hash))
    }

    /**
     * Erase the floor for `accountIdB64`. No-op when absent — used by
     * `forgetDevice` cleanup alongside [PhraseStore.deletePhrase].
     */
    public fun deleteFloor(context: Context, accountIdB64: String) {
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

/**
 * The outcome of reading the floor (F5a). Distinguishes a genuinely absent
 * anchor (fresh device) from an anchor that EXISTS but is untrustworthy (a
 * device-key rotation or an attacker who tampered the stored row). The restore
 * gate must never silently demote a corrupt anchor to the weaker fresh-install
 * path — it routes it to [RestoreVerdict.StaleOverride] instead. Mirror of the
 * web `FloorRead` discriminated union.
 */
public sealed interface FloorRead {
    /** No stored entry — a first-ever restore on this device (zero-anchor path). */
    public data object Absent : FloorRead

    /** A valid, parsed floor. */
    public data class Present(val floor: BackupFloor) : FloorRead

    /** An entry exists but decrypt / parse / shape-check failed. */
    public data object Corrupt : FloorRead
}

/** The decrypted floor a returning client compares against (ADR-0037 §6). */
public data class BackupFloor(
    /** Highest `backup_version` this device has minted / restored. */
    val version: ULong,
    /** The 32-byte `BLAKE3` envelope hash the floor's head committed. */
    val envelopeHash: ByteArray,
) {
    // ByteArray needs structural equals/hashCode for value-semantics.
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is BackupFloor) return false
        return version == other.version && envelopeHash.contentEquals(other.envelopeHash)
    }

    override fun hashCode(): Int = 31 * version.hashCode() + envelopeHash.contentHashCode()
}
