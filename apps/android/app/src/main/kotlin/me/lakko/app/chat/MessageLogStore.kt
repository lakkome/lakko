// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Persistent plaintext-message log on Android (Phase 2a3f1).
 *
 * Mirror of:
 *
 *   * `apps/ios/Lakko/Chat/MessageLogStore.swift`
 *   * `apps/web/src/chat/message-log-store.ts` (forthcoming)
 *
 * Persists the per-account `{ groupIdHex → (cursor, messages) }`
 * map the in-memory [ChatRepository] keeps in
 * [GroupRecord]. Without this layer the timeline vanishes on
 * every app launch — MLS state survives via
 * [IdentitySnapshotStore] but cannot decrypt already-consumed
 * wire bytes (forward secrecy intentionally wipes chain keys).
 *
 * ## Threat model
 *
 * The stored bytes ARE plaintext messages — text + attachment
 * refs. Wrapped under a Keystore-resident AES-256-GCM key with
 * the same posture as [me.lakko.app.devices.DeviceGroupSnapshotStore]:
 *
 *   * dedicated alias [WRAP_KEY_ALIAS], versioned so a future
 *     rotation has somewhere to go;
 *   * `noBackupFilesDir` so a Google-cloud-backup auto-restore
 *     does NOT carry the timeline across devices.
 *
 * ## On-disk layout
 *
 * ```text
 *   noBackupFilesDir/lakko-message-logs/
 *     <accountIdB64>.bin     12-byte IV | ciphertext | 16-byte tag
 * ```
 *
 * Plaintext is the JSON-encoded [MessageLogPayload]. Format
 * version field (`version: 1`) keeps older blobs decryptable
 * across migrations.
 */
public object MessageLogStore {

    private const val AES_GCM_NOPAD = "AES/GCM/NoPadding"
    private const val GCM_IV_LEN = 12
    private const val GCM_TAG_BITS = 128
    private const val AES_KEY_BITS = 256
    private const val ANDROID_KEYSTORE = "AndroidKeyStore"

    /** Keystore alias for the wrap key. Versioned so a future
     *  rotation has somewhere to go. Distinct from the
     *  MLS-snapshot wrap key + the DeviceGroup-snapshot wrap
     *  key — every store gets its own alias for the same
     *  rotational-independence reason. */
    public const val WRAP_KEY_ALIAS: String = "me.lakko.message-log-wrap.v1"

    /** Subdirectory under the app's no-backup files dir where
     *  per-account message-log envelopes live. */
    private const val ENVELOPE_DIR = "lakko-message-logs"

    /** Strict JSON: refuse to lose data on a schema mismatch,
     *  emit predictable output for golden tests. */
    private val json: Json = Json {
        ignoreUnknownKeys = false
        prettyPrint = false
        explicitNulls = true
    }

    /**
     * Persist [payload] for [accountIdB64]. Overwrites any
     * prior envelope. Best-effort — Keystore / FS failures
     * surface as `false` so the caller doesn't crash the
     * mutation flow.
     */
    public fun putLog(
        context: Context,
        accountIdB64: String,
        payload: MessageLogPayload,
    ): Boolean {
        val plaintext = try {
            json.encodeToString(MessageLogPayload.serializer(), payload).toByteArray(Charsets.UTF_8)
        } catch (_: Throwable) {
            return false
        }
        val wrap = try {
            getOrCreateWrapKey()
        } catch (_: Throwable) {
            return false
        }
        return try {
            val cipher = Cipher.getInstance(AES_GCM_NOPAD)
            cipher.init(Cipher.ENCRYPT_MODE, wrap)
            val iv = cipher.iv
            check(iv.size == GCM_IV_LEN) {
                "unexpected GCM IV length from Keystore: ${iv.size}"
            }
            val ciphertext = cipher.doFinal(plaintext)

            val out = ByteArray(GCM_IV_LEN + ciphertext.size)
            System.arraycopy(iv, 0, out, 0, GCM_IV_LEN)
            System.arraycopy(ciphertext, 0, out, GCM_IV_LEN, ciphertext.size)
            envelopeFile(context, accountIdB64).writeBytes(out)
            true
        } catch (_: Throwable) {
            false
        }
    }

    /**
     * Read + unwrap the log for [accountIdB64]. Returns `null`
     * when:
     *   * no envelope on disk for this account,
     *   * Keystore access denied / wrap key rotated since the
     *     envelope was written,
     *   * truncated / non-JSON blob (corrupt — caller starts
     *     with an empty timeline and re-bootstraps from the DS
     *     journal cursor).
     */
    public fun getLog(context: Context, accountIdB64: String): MessageLogPayload? {
        val file = envelopeFile(context, accountIdB64)
        if (!file.exists()) return null
        val blob = try {
            file.readBytes()
        } catch (_: Throwable) {
            return null
        }
        if (blob.size <= GCM_IV_LEN) return null
        val iv = blob.copyOfRange(0, GCM_IV_LEN)
        val ciphertext = blob.copyOfRange(GCM_IV_LEN, blob.size)

        val wrap = try {
            getOrCreateWrapKey()
        } catch (_: Throwable) {
            return null
        }
        val plaintext = try {
            val cipher = Cipher.getInstance(AES_GCM_NOPAD)
            cipher.init(Cipher.DECRYPT_MODE, wrap, GCMParameterSpec(GCM_TAG_BITS, iv))
            cipher.doFinal(ciphertext)
        } catch (_: Throwable) {
            return null
        }
        return try {
            json.decodeFromString(
                MessageLogPayload.serializer(),
                String(plaintext, Charsets.UTF_8),
            )
        } catch (_: Throwable) {
            null
        }
    }

    /**
     * Erase the log for [accountIdB64]. No-op when absent.
     * Called from `forgetDevice`. The Keystore wrap key is
     * left intact — shared across accounts on the same
     * device, rotating it would invalidate every surviving
     * envelope.
     */
    public fun deleteLog(context: Context, accountIdB64: String) {
        envelopeFile(context, accountIdB64).delete()
    }

    // -----------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------

    private fun getOrCreateWrapKey(): SecretKey {
        val ks = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        ks.getKey(WRAP_KEY_ALIAS, null)?.let { existing ->
            return existing as SecretKey
        }
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

    private fun envelopeFile(context: Context, accountIdB64: String): File {
        val dir = File(context.noBackupFilesDir, ENVELOPE_DIR)
        if (!dir.exists()) dir.mkdirs()
        val safe = accountIdB64.replace("/", "_")
        return File(dir, "$safe.bin")
    }
}

// =============================================================================
// JSON payload — mirror of the iOS PersistedMessage / PersistedAttachment shapes
// =============================================================================

@Serializable
public data class MessageLogPayload(
    val version: Int = 1,
    val groups: Map<String, PersistedGroup>,
)

@Serializable
public data class PersistedGroup(
    val cursor: Long,
    val messages: List<PersistedMessage>,
)

@Serializable
public data class PersistedMessage(
    val messageId: Long,
    val direction: String,
    val text: String,
    val attachments: List<PersistedAttachment>,
    val createdAt: String,
) {
    /** Explicit companion so the conversion helpers below can
     *  attach as `PersistedMessage.Companion.from(msg)`. */
    public companion object
}

@Serializable
public data class PersistedAttachment(
    val blobIdB64: String,
    val keyB64: String,
    val nonceB64: String,
    val mime: String?,
    val size: ULong,
    val name: String?,
) {
    public companion object
}

// =============================================================================
// ChatMessage ↔ PersistedMessage helpers
// =============================================================================

/** Build the JSON projection from a live in-memory [ChatMessage]. */
public fun PersistedMessage.Companion.from(msg: ChatMessage): PersistedMessage =
    PersistedMessage(
        messageId = msg.messageId,
        direction = when (msg.direction) {
            Direction.Outgoing -> "outgoing"
            Direction.Incoming -> "incoming"
            Direction.System -> "system"
        },
        text = msg.text,
        attachments = msg.attachments.map(PersistedAttachment.Companion::from),
        createdAt = msg.createdAt,
    )

/** Inverse — reconstruct a runtime [ChatMessage] from the JSON
 *  projection. Returns `null` on a malformed `direction`
 *  discriminator (older client parsing a future variant). */
public fun PersistedMessage.toChatMessage(): ChatMessage? {
    val dir = when (direction) {
        "outgoing" -> Direction.Outgoing
        "incoming" -> Direction.Incoming
        "system" -> Direction.System
        else -> return null
    }
    return ChatMessage(
        messageId = messageId,
        direction = dir,
        text = text,
        attachments = attachments.mapNotNull { it.toAttachmentRef() },
        createdAt = createdAt,
    )
}

public fun PersistedAttachment.Companion.from(
    ref: uniffi.lakko_ffi_uniffi.AttachmentRef,
): PersistedAttachment = PersistedAttachment(
    blobIdB64 = Base64.encodeToString(ref.blobId, Base64.NO_WRAP),
    keyB64 = Base64.encodeToString(ref.key, Base64.NO_WRAP),
    nonceB64 = Base64.encodeToString(ref.nonce, Base64.NO_WRAP),
    mime = ref.mime,
    size = ref.size,
    name = ref.name,
)

public fun PersistedAttachment.toAttachmentRef(): uniffi.lakko_ffi_uniffi.AttachmentRef? {
    return try {
        uniffi.lakko_ffi_uniffi.AttachmentRef(
            blobId = Base64.decode(blobIdB64, Base64.NO_WRAP),
            key = Base64.decode(keyB64, Base64.NO_WRAP),
            nonce = Base64.decode(nonceB64, Base64.NO_WRAP),
            mime = mime,
            size = size,
            name = name,
        )
    } catch (_: IllegalArgumentException) {
        null
    }
}
