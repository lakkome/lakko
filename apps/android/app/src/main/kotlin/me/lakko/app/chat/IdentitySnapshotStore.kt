// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Per-account on-disk persistence for the MLS provider snapshot
 * blob produced by [uniffi.lakko_ffi_uniffi.Identity.snapshotStorage].
 *
 * ## Threat model
 *
 * The snapshot blob is the most sensitive bytes any Lakko
 * process touches — it carries every group's ratchet state, the
 * leaf-secret material, and the init-key private bytes. A
 * dump of `noBackupFilesDir/lakko-mls-snapshots/<acct>.bin`
 * with the raw blob would let an attacker decrypt every prior
 * message in every group the user is part of. The blob must
 * be wrapped under a Keystore-resident key — same posture as
 * [me.lakko.app.auth.DeviceKeyStore] from 1.15d3.
 *
 * ## Why a separate wrap key alias from `DeviceKeyStore`
 *
 * `DeviceKeyStore` wraps the **device root key** — a 32-byte
 * value that the app keeps in memory after unwrap. Once the
 * device root key is unwrapped at boot, it's available in
 * heap for the app to use as a KEK for further on-disk
 * material.
 *
 * Pre-1.15f3 we could have wrapped the MLS snapshot under the
 * unwrapped device root key directly (AES-256-GCM in app
 * code). 1.15f3 takes a different approach: a separate
 * Keystore-resident wrap key under
 * [WRAP_KEY_ALIAS] = `me.lakko.mls-snapshot-wrap.v1`,
 * mirroring [me.lakko.app.auth.DeviceKeyStore.WRAP_KEY_ALIAS]'s
 * shape verbatim. Two reasons:
 *
 *   1. **Hardware-isolation parity**: the device-root-key wrap
 *      already lives in StrongBox-or-TEE; making the MLS
 *      snapshot the **same kind of citizen** (its own Keystore
 *      alias, not "app-layer AES over heap bytes") keeps the
 *      threat-model story uniform — both wrap keys are equally
 *      hard for a root-compromised OS to exfiltrate.
 *   2. **Independent rotation**: a future hardware-binding
 *      audit may want to rotate the MLS-snapshot wrap without
 *      touching the device root key (or vice versa). Versioned
 *      aliases (`…v1` → `…v2`) make that clean.
 *
 * Mirror of `apps/web`'s `chat/mls-store.ts` posture: dedicated
 * IndexedDB store for the snapshot, wrapped via the same
 * non-extractable CryptoKey lineage as the device root key.
 *
 * ## Persistence layout
 *
 * ```text
 *   noBackupFilesDir/lakko-mls-snapshots/
 *     <accountIdB64>.bin     12-byte IV | ciphertext | 16-byte tag
 * ```
 *
 * Same on-disk shape as `DeviceKeyStore`. `noBackup` is
 * critical: MLS state is device-bound; an auto-restore from
 * Google's cloud backup must NOT carry it between devices,
 * even if the user opts in — the recipient peer's view of the
 * conversation would diverge from the restored sender's view
 * the moment any of them sends a new message.
 *
 * ## Host-JVM tests
 *
 * The class loads the `AndroidKeyStore` provider, which only
 * exists on a real device or emulator — instrumented coverage
 * lives with 1.15f5. The pure-bytes path (the file-layout
 * helpers) doesn't need Keystore, so it could be tested in
 * isolation if we ever extract them; for 1.15f3 the helpers
 * are inlined for clarity.
 */
public object IdentitySnapshotStore {

    /** AES-256-GCM in [Cipher] notation. */
    private const val AES_GCM_NOPAD = "AES/GCM/NoPadding"

    /** GCM IV is 96 bits per NIST SP 800-38D — also the default
     *  Keystore Cipher generates when not given an explicit IV. */
    private const val GCM_IV_LEN = 12

    /** GCM auth tag length, in bits. 128 is the maximum and the
     *  recommended value. */
    private const val GCM_TAG_BITS = 128

    /** AES key size, in bits. */
    private const val AES_KEY_BITS = 256

    /** Keystore alias for the wrap key. Versioned so a future
     *  rotation has somewhere to go. Distinct from
     *  [me.lakko.app.auth.DeviceKeyStore.WRAP_KEY_ALIAS] —
     *  see the file-level docstring for the rationale. */
    public const val WRAP_KEY_ALIAS: String = "me.lakko.mls-snapshot-wrap.v1"

    private const val ANDROID_KEYSTORE = "AndroidKeyStore"

    /** Subdirectory under the app's no-backup files dir where
     *  the per-account snapshot envelopes live. */
    private const val ENVELOPE_DIR = "lakko-mls-snapshots"

    /**
     * Persist [snapshotBytes] for [accountIdB64].
     *
     * Existing record (if any) is overwritten — same posture as
     * `DeviceKeyStore.putDeviceKey`. The caller is responsible
     * for `.fill(0)`'ing the input array after this call; the
     * store keeps no reference past the wrap.
     *
     * @throws java.io.IOException on disk-write failure.
     * @throws java.security.GeneralSecurityException on a
     *         non-recoverable Keystore failure.
     */
    public fun putSnapshot(
        context: Context,
        accountIdB64: String,
        snapshotBytes: ByteArray,
    ) {
        if (snapshotBytes.isEmpty()) {
            throw IllegalArgumentException("snapshotBytes must be non-empty")
        }
        val wrap = getOrCreateWrapKey()
        val cipher = Cipher.getInstance(AES_GCM_NOPAD)
        cipher.init(Cipher.ENCRYPT_MODE, wrap)
        val iv = cipher.iv
        check(iv.size == GCM_IV_LEN) {
            "unexpected GCM IV length from Keystore: ${iv.size}"
        }
        val ciphertext = cipher.doFinal(snapshotBytes)

        val out = ByteArray(GCM_IV_LEN + ciphertext.size)
        System.arraycopy(iv, 0, out, 0, GCM_IV_LEN)
        System.arraycopy(ciphertext, 0, out, GCM_IV_LEN, ciphertext.size)

        envelopeFile(context, accountIdB64).writeBytes(out)
    }

    /**
     * Read and unwrap the snapshot blob for [accountIdB64].
     *
     * Returns `null` when no envelope is on disk for that
     * account. Keystore failures (revoked / corrupted wrap
     * key) surface as [java.security.GeneralSecurityException].
     * A truncated file (smaller than just the IV header)
     * surfaces as `null` — same rationale as
     * `DeviceKeyStore.getDeviceKey`: callers treat it as "no
     * snapshot on this device" and re-bootstrap from the
     * discovery directory's freshly-uploaded KeyPackage.
     */
    public fun getSnapshot(context: Context, accountIdB64: String): ByteArray? {
        val file = envelopeFile(context, accountIdB64)
        if (!file.exists()) return null
        val blob = file.readBytes()
        if (blob.size <= GCM_IV_LEN) return null
        val iv = blob.copyOfRange(0, GCM_IV_LEN)
        val ciphertext = blob.copyOfRange(GCM_IV_LEN, blob.size)

        val wrap = getOrCreateWrapKey()
        val cipher = Cipher.getInstance(AES_GCM_NOPAD)
        cipher.init(
            Cipher.DECRYPT_MODE,
            wrap,
            GCMParameterSpec(GCM_TAG_BITS, iv),
        )
        return cipher.doFinal(ciphertext)
    }

    /**
     * Erase the snapshot for [accountIdB64]. No-op if the
     * envelope file is absent. The Keystore wrap key is left
     * intact — other accounts on the same device may still
     * need it; rotating the master would require re-wrapping
     * every surviving envelope.
     *
     * Called from `AuthRepository.forgetDevice` (alongside
     * `DeviceKeyStore.deleteDeviceKey`) so a forget-device
     * cycle leaves nothing per-account on disk.
     */
    public fun deleteSnapshot(context: Context, accountIdB64: String) {
        envelopeFile(context, accountIdB64).delete()
        leafKeyFile(context, accountIdB64).delete()
    }

    /**
     * ADR-0050 — record the leaf signature key of the identity the snapshot on
     * disk was taken under. Public bytes, so a plaintext sidecar. A snapshot
     * taken under another leaf identity (a pre-ADR-0050 account-key snapshot)
     * must not be restored into this device's identity: `restoreStorage`
     * replaces the whole provider with groups signed by a key this identity
     * does not hold. The only writer of this store is the device itself: the
     * account backup carries no MLS state (2026-10-02), so nothing restored
     * from a backup ever lands here.
     *
     * Called after [putSnapshot] by every writer that owns the live identity;
     * a snapshot written without it reads as "another identity".
     */
    public fun putSnapshotLeafKey(context: Context, accountIdB64: String, leafKey: ByteArray) {
        val encoded = android.util.Base64.encodeToString(leafKey, android.util.Base64.NO_WRAP)
        val file = leafKeyFile(context, accountIdB64)
        if (file.exists() && runCatching { file.readText() }.getOrNull() == encoded) return
        file.writeText(encoded)
    }

    /** The leaf key [putSnapshotLeafKey] recorded, or `null` when none was
     *  (or the sidecar is unreadable). */
    public fun getSnapshotLeafKey(context: Context, accountIdB64: String): ByteArray? {
        val file = leafKeyFile(context, accountIdB64)
        if (!file.exists()) return null
        return try {
            android.util.Base64.decode(file.readText().trim(), android.util.Base64.NO_WRAP)
        } catch (_: Throwable) {
            null
        }
    }

    private fun leafKeyFile(context: Context, accountIdB64: String): File {
        val envelope = envelopeFile(context, accountIdB64)
        return File(envelope.parentFile, envelope.nameWithoutExtension + ".leaf")
    }

    // -----------------------------------------------------------------
    // Internal: wrap-key lifecycle (parallels DeviceKeyStore)
    // -----------------------------------------------------------------

    private fun getOrCreateWrapKey(): SecretKey {
        val ks = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        ks.getKey(WRAP_KEY_ALIAS, null)?.let { existing ->
            return existing as SecretKey
        }
        return generateWrapKey()
    }

    private fun generateWrapKey(): SecretKey {
        // We deliberately do NOT call setIsStrongBoxBacked
        // here. The MLS snapshot is **frequently** read/written
        // (every send + every poll mutates provider storage in
        // 1.15f4's persistence wire-up); StrongBox has limited
        // per-second AES throughput on some chipsets and the
        // throughput hit isn't worth the marginal additional
        // hardware-isolation for a key that already enjoys TEE
        // protection. Device-root-key in `DeviceKeyStore` is
        // touched ~once per app start — different access
        // pattern, different threat-model trade-off.
        val builder = KeyGenParameterSpec.Builder(
            WRAP_KEY_ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setKeySize(AES_KEY_BITS)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setRandomizedEncryptionRequired(true)
            .setUserAuthenticationRequired(false)

        val kg = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        kg.init(builder.build())
        return kg.generateKey()
    }

    /**
     * Per-account envelope file path. Mirror of
     * `DeviceKeyStore.envelopeFile` shape (separate directory).
     */
    private fun envelopeFile(context: Context, accountIdB64: String): File {
        val dir = File(context.noBackupFilesDir, ENVELOPE_DIR)
        if (!dir.exists() && !dir.mkdirs()) {
            // Don't throw here — the eventual writeBytes call
            // will surface the real I/O error with a useful
            // stack frame.
        }
        // Replace base64 `/` with `_` so the segment is
        // filesystem-safe on every Android storage backend.
        val safe = accountIdB64.replace("/", "_")
        return File(dir, "$safe.bin")
    }
}
