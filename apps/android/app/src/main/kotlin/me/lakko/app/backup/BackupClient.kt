// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import me.lakko.app.storage.StorageClient
import uniffi.lakko_ffi_uniffi.BackupContactRecord
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.ParsedBackupPayload
import uniffi.lakko_ffi_uniffi.buildBackupPayload
import uniffi.lakko_ffi_uniffi.openBackup
import uniffi.lakko_ffi_uniffi.parseBackupPayload
import uniffi.lakko_ffi_uniffi.sealBackup

/**
 * `BackupClient` — orchestrates the encrypted-backup upload and
 * restore flows on Android (Phase 2a1c).
 *
 * Mirror of:
 *
 *   * `apps/web/src/backup/client.ts`
 *   * `apps/ios/Lakko/Backup/BackupClient.swift`
 *
 * Glues four layers:
 *
 *   1. [PhraseStore] — `EncryptedSharedPreferences`-backed
 *      recovery-phrase cache.
 *   2. UniFFI surface for `lakko-backup` — `buildBackupPayload`,
 *      `sealBackup`, `openBackup`, `parseBackupPayload`.
 *   3. [StorageClient] — the three `/backup` HTTP routes.
 *   4. Identity material from [`Identity.fromSeed`] — the
 *      Ed25519 signing seed that lands inside the payload.
 *
 * MLS group snapshots are reserved in the v1 payload but
 * emitted empty — the DeviceGroup-side snapshot publishing
 * path lands in 2a3.
 */
public class BackupClient(
    private val storage: StorageClient = StorageClient(),
) : AutoCloseable {

    override fun close() {
        storage.close()
    }

    /** Bind / rebind the bearer token for `/backup` routes. */
    public fun setSessionToken(token: String?) {
        storage.setSessionToken(token)
    }

    // -----------------------------------------------------------------
    // Upload
    // -----------------------------------------------------------------

    /** Compose + seal + PUT in one step. Caller has the phrase
     *  on hand right now (signup just minted it, recover just
     *  consumed it). No phrase-cache hop. */
    public suspend fun uploadNow(
        accountId: ByteArray,
        ed25519Seed: ByteArray,
        phraseBytes: ByteArray,
        contacts: List<BackupContactRecord> = emptyList(),
        /** Phase 2a3d3: whole-storage MLS snapshot from
         *  `Identity.snapshotStorage()`. Empty `ByteArray` on
         *  signup baseline; recover-time + scheduled auto-backup
         *  paths pass the current snapshot. */
        wholeStorageSnapshot: ByteArray = ByteArray(0),
    ) {
        val createdAtMs = System.currentTimeMillis().toULong()
        val payload = buildBackupPayload(
            accountId = accountId,
            createdAtMs = createdAtMs,
            ed25519Seed = ed25519Seed,
            contacts = contacts,
            wholeStorageSnapshot = wholeStorageSnapshot,
        )
        val envelope = sealBackup(phrase = phraseBytes, payloadBytes = payload)
        storage.putBackup(envelope)
    }

    /** Read the phrase from the [PhraseStore] cache and run
     *  [uploadNow] with it. Returns a discriminated
     *  [UploadResult] so the orchestration layer can branch on
     *  `CacheMiss` (the user needs to run recovery to enable
     *  backups on this device). */
    public suspend fun uploadFromCache(
        context: Context,
        accountId: ByteArray,
        accountIdB64: String,
        ed25519Seed: ByteArray,
        contacts: List<BackupContactRecord> = emptyList(),
        wholeStorageSnapshot: ByteArray = ByteArray(0),
    ): UploadResult {
        val cached = PhraseStore.getPhrase(context, accountIdB64)
            ?: return UploadResult.CacheMiss
        try {
            uploadNow(
                accountId = accountId,
                ed25519Seed = ed25519Seed,
                phraseBytes = cached,
                contacts = contacts,
                wholeStorageSnapshot = wholeStorageSnapshot,
            )
            return UploadResult.Uploaded
        } catch (cause: Throwable) {
            return UploadResult.TransportFailed(cause)
        } finally {
            // Best-effort scrub of the local copy. The
            // EncryptedSharedPreferences row stays intact —
            // future auto-backups can re-read.
            cached.fill(0)
        }
    }

    // -----------------------------------------------------------------
    // Restore
    // -----------------------------------------------------------------

    /** Fetch + decrypt + parse the server-stored backup. Returns
     *  `null` when no blob has been uploaded yet; throws on
     *  transport / wrong-phrase / decode failures (caller
     *  distinguishes via the thrown type). */
    public suspend fun restore(phraseBytes: ByteArray): ParsedBackupPayload? {
        val envelope = storage.getBackup() ?: return null
        val payloadBytes = openBackup(phrase = phraseBytes, wire = envelope)
        return parseBackupPayload(payloadBytes)
    }

    /** Cache the phrase under EncryptedSharedPreferences. Called
     *  from the post-signup / post-recover bootstrap path. */
    public fun cachePhrase(
        context: Context,
        accountIdB64: String,
        phraseBytes: ByteArray,
    ) {
        PhraseStore.putPhrase(context, accountIdB64, phraseBytes)
    }

    /** Wipe the cached phrase. Called from the `forgetDevice`
     *  cleanup. No-op when absent. */
    public fun wipePhrase(context: Context, accountIdB64: String) {
        PhraseStore.deletePhrase(context, accountIdB64)
    }
}

/** Discriminated outcome of [BackupClient.uploadFromCache]. */
public sealed interface UploadResult {
    public data object Uploaded : UploadResult
    public data object CacheMiss : UploadResult
    public data class TransportFailed(val cause: Throwable) : UploadResult
}

/** Derive the AccountIdentity Ed25519 signing seed from the
 *  identity seed (ADR-0017: the BIP39 phrase seed, NOT the password
 *  exportKey — so the backup is signed with the stable account
 *  identity that survives a password change, matching web/iOS).
 *  Uses the same `Identity.fromSeed` deterministic flow every
 *  identity-bearing operation uses. Helper so the auth-side bootstrap
 *  can skip threading the seed through the public surface. */
public fun ed25519SeedFromIdentitySeed(identitySeed: ByteArray): ByteArray {
    return Identity.fromSeed(identitySeed).use { it.ed25519Seed() }
}
