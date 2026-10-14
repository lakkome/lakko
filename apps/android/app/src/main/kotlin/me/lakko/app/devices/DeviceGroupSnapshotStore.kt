// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

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
 * Per-account on-disk persistence for the **DeviceGroup** MLS
 * provider snapshot (Phase 2a3b4, ADR-0007).
 *
 * Mirror of:
 *
 *   * `apps/web/src/devices/device-group-snapshot-store.ts`
 *   * `apps/ios/Lakko/Devices/DeviceGroupSnapshotStore.swift`
 *
 * ## Why a separate snapshot store from [me.lakko.app.chat.IdentitySnapshotStore]
 *
 * Per ADR-0007 the runtime carries **two** MLS identities and
 * therefore **two** OpenMLS providers:
 *
 *   * **AccountIdentity** — deterministic from the OPAQUE
 *     `exportKey`, shared across every device the user logs in
 *     on. Its provider holds every conversation MLS group the
 *     user is a member of. Snapshot lives in
 *     [me.lakko.app.chat.IdentitySnapshotStore] under
 *     `noBackupFilesDir/lakko-mls-snapshots/`.
 *
 *   * **DeviceIdentity** — random per-device. Each device's
 *     leaf in the DeviceGroup (the per-user MLS group whose
 *     members are the user's own devices). Its provider holds
 *     **only** the DeviceGroup state. Snapshot lives **here**
 *     under `noBackupFilesDir/lakko-device-group-snapshots/`.
 *
 * Two providers means two snapshot stores, two wrap keys.
 * Conflating them would couple the AccountIdentity ratchet to
 * DeviceGroup pair / unpair operations — a layering violation
 * that complicates the `forgetDevice` blast radius.
 *
 * ## Threat model
 *
 * The DeviceGroup snapshot blob carries:
 *
 *   * The DeviceGroup MLS state (every device's leaf, the
 *     current ratchet head, every pending Remove if a peer
 *     hasn't yet acked it).
 *   * The DeviceIdentity's private signature key — derivable
 *     from the cached seed in [me.lakko.app.auth.DeviceIdentityStore],
 *     so an attacker who reads disk already has it; the snapshot
 *     doesn't make things worse, but we wrap it anyway for
 *     posture parity with the AccountIdentity snapshot.
 *
 * Same Keystore-resident wrap-key shape as
 * [me.lakko.app.chat.IdentitySnapshotStore] under
 * [WRAP_KEY_ALIAS] = `me.lakko.device-group-snapshot-wrap.v1`.
 *
 * ## Persistence layout
 *
 * ```text
 *   noBackupFilesDir/lakko-device-group-snapshots/
 *     <accountIdB64>.bin     12-byte IV | ciphertext | 16-byte tag
 * ```
 *
 * `noBackup` is critical for the same reason as the
 * AccountIdentity snapshot: MLS state is device-bound; a
 * Google-cloud-backup auto-restore must NOT carry it across
 * devices, or the restoring device's view of the DeviceGroup
 * diverges from every other device the moment any of them
 * commits a membership change.
 */
public object DeviceGroupSnapshotStore {

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
     *  [me.lakko.app.chat.IdentitySnapshotStore.WRAP_KEY_ALIAS]
     *  — see the file-level docstring for the rationale. */
    public const val WRAP_KEY_ALIAS: String = "me.lakko.device-group-snapshot-wrap.v1"

    private const val ANDROID_KEYSTORE = "AndroidKeyStore"

    /** Subdirectory under the app's no-backup files dir where
     *  the per-account DeviceGroup snapshot envelopes live.
     *  Distinct from `lakko-mls-snapshots` (AccountIdentity). */
    private const val ENVELOPE_DIR = "lakko-device-group-snapshots"

    /**
     * A second kind of envelope, in its own folder and under the same wrap
     * key. Correct because isolation here comes from the PATH: the key is one
     * per installation, not per account and not per kind, which is what the
     * comment on [WRAP_KEY_ALIAS] has always said.
     */
    public const val SYNC_STATE_DIR: String = "lakko-device-sync-state"

    /**
     * Persist [snapshotBytes] for [accountIdB64].
     *
     * Existing record (if any) is overwritten — same posture as
     * `IdentitySnapshotStore.putSnapshot`. Callers should
     * `.fill(0)` the input array after this call; the store
     * keeps no reference past the wrap.
     *
     * @throws java.io.IOException on disk-write failure.
     * @throws java.security.GeneralSecurityException on a
     *         non-recoverable Keystore failure.
     */
    public fun putSnapshot(
        context: Context,
        accountIdB64: String,
        snapshotBytes: ByteArray,
        dir: String = ENVELOPE_DIR,
    ) {
        me.lakko.app.account.AccountStoreGate.writeOrEnd(accountIdB64) {
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

            envelopeFile(context, accountIdB64, dir).writeBytes(out)
        }
    }

    /**
     * Read and unwrap the snapshot blob for [accountIdB64].
     *
     * Returns `null` when no envelope is on disk for that
     * account. Keystore failures (revoked / corrupted wrap
     * key) surface as [java.security.GeneralSecurityException].
     * A truncated file (smaller than just the IV header)
     * surfaces as `null` — same rationale as the
     * AccountIdentity sibling: callers treat it as "no
     * DeviceGroup on this device yet" and bootstrap a fresh
     * founder leaf.
     */
    public fun getSnapshot(
        context: Context,
        accountIdB64: String,
        dir: String = ENVELOPE_DIR,
    ): ByteArray? {
        val file = envelopeFile(context, accountIdB64, dir)
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
     * Called from `AuthRepository.forgetDevice` (alongside the
     * AccountIdentity snapshot wipe + DeviceIdentity seed wipe)
     * so a forget-device cycle leaves nothing per-account on
     * disk.
     */
    public fun deleteSnapshot(
        context: Context,
        accountIdB64: String,
        dir: String = ENVELOPE_DIR,
    ) {
        envelopeFile(context, accountIdB64, dir).delete()
    }

    // -----------------------------------------------------------------
    // Internal: wrap-key lifecycle (parallels IdentitySnapshotStore)
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
        // here. DeviceGroup snapshots are written on every
        // add-device / remove-device commit — relatively rare
        // compared to conversation provider snapshots, but
        // still bursty enough that StrongBox's per-second AES
        // throughput on some chipsets becomes a tail-latency
        // hazard. TEE protection is the right trade-off for a
        // key whose plaintext is the leaf signature material
        // and is rebuildable from
        // [me.lakko.app.auth.DeviceIdentityStore] anyway.
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
     * [me.lakko.app.chat.IdentitySnapshotStore]'s `envelopeFile`
     * shape under a different directory.
     */
    private fun envelopeFile(
        context: Context,
        accountIdB64: String,
        dirName: String = ENVELOPE_DIR,
    ): File {
        val dir = File(context.noBackupFilesDir, dirName)
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
