// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.storage.StorageClient
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.EncryptedAttachment
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.composeEnvelope as ffiComposeEnvelope
import uniffi.lakko_ffi_uniffi.decryptAttachment as ffiDecryptAttachment
import uniffi.lakko_ffi_uniffi.encryptAttachment as ffiEncryptAttachment
import uniffi.lakko_ffi_uniffi.parseEnvelope as ffiParseEnvelope

/**
 * Chat attachment bridge — picked-file bytes ↔ UniFFI encryption
 * ↔ storage HTTP. Mirror of:
 *
 *   * `apps/web/src/chat/attachments.ts` (1.19c2)
 *   * `apps/ios/Lakko/Chat/AttachmentBridge.swift` (1.19c3)
 *
 * Three Phase-1.19 layers join here on the Android client:
 *
 *   1. **Activity-result picker** (`ACTION_OPEN_DOCUMENT` or
 *      `PickVisualMedia`) — surfaces a content-URI; the screen
 *      layer resolves it to raw bytes + MIME + display name.
 *   2. **`uniffi.lakko_ffi_uniffi`** — `encryptAttachment` /
 *      `decryptAttachment` / `composeEnvelope` / `parseEnvelope`
 *      free functions, exposed in 1.19b.
 *   3. **`StorageClient`** — `lakko-svc-storage` HTTP wrapper
 *      added in 1.19c1 that PUTs / GETs ciphertext blobs by
 *      content hash.
 *
 * Two flows:
 *
 *   * [encryptAndUpload] — bytes → `encryptAttachment` →
 *     `StorageClient.putBlob` → return an [AttachmentRef] for
 *     embedding into the next MLS Application-message envelope.
 *
 *   * [downloadAndDecrypt] — `StorageClient.getBlob` →
 *     `decryptAttachment` → plaintext bytes.
 *
 * Plus [composeEnvelopeBytes] + [parseEnvelopeBytes] friendly
 * wrappers around the UniFFI free functions (rename-shadowed
 * at the top of this file so callers don't have to disambiguate
 * the bare names).
 *
 * ## Memory ceiling
 *
 * [MAX_ATTACHMENT_BYTES] mirrors the `MAX_ATTACHMENT_BYTES`
 * constant in `crates/lakko-attachments` and `services/storage`
 * (50 MB). Surface here so a misbehaving caller fails fast with
 * a friendly error before either the UniFFI AEAD seal or the
 * storage PUT spends cycles.
 */
public object AttachmentBridge {

    /** Per-attachment ceiling — must match `MAX_ATTACHMENT_BYTES`
     *  in `crates/lakko-attachments` and `services/storage`. */
    public const val MAX_ATTACHMENT_BYTES: Int = 50 * 1024 * 1024

    /**
     * Encrypt [bytes] under a fresh random CEK + nonce, PUT
     * the ciphertext to `lakko-svc-storage`, return the
     * [AttachmentRef] the caller embeds into the next outgoing
     * MLS Application-message envelope via
     * [composeEnvelopeBytes].
     *
     * The CEK + nonce are generated **inside Rust** via the
     * UniFFI `encryptAttachment` free function (which routes to
     * `lakko_crypto`'s CSPRNG); Kotlin never holds them as
     * persistent state, only as the in-flight ref returned
     * here. Once the ref ships inside an MLS Application
     * message, the Kotlin object goes out of scope and is GC'd.
     *
     * @param bytes   Raw plaintext bytes from the picker.
     * @param mime    Optional MIME (`null` / empty → wire-ref
     *                gets `null`).
     * @param name    Optional display name (`null` / empty →
     *                wire-ref gets `null`).
     * @param storage A [StorageClient] for the PUT — defaults
     *                to a fresh default-base-URL instance so
     *                most callers don't have to thread one
     *                through.
     */
    public suspend fun encryptAndUpload(
        bytes: ByteArray,
        mime: String?,
        name: String?,
        storage: StorageClient = StorageClient(),
        /** Polish 1.x — coarse-grained stage callback the
         *  composer surface drives off to render "Encrypting…"
         *  → "Uploading…" captions instead of a generic
         *  spinner. Invoked synchronously from this function's
         *  coroutine context; callers that mutate UI state from
         *  this callback should hand off to the right
         *  dispatcher (e.g. `Dispatchers.Main`). */
        onStage: ((UploadStage) -> Unit)? = null,
    ): AttachmentRef {
        if (bytes.size > MAX_ATTACHMENT_BYTES) {
            throw AttachmentBridgeException.TooLarge(
                got = bytes.size,
                max = MAX_ATTACHMENT_BYTES,
            )
        }

        onStage?.invoke(UploadStage.Encrypting)
        // 1) Encrypt via UniFFI — fresh CEK + nonce + SHA-256
        // blob_id all computed inside Rust.
        val enc: EncryptedAttachment = try {
            ffiEncryptAttachment(plaintext = bytes)
        } catch (e: FfiException) {
            throw AttachmentBridgeException.Encryption(e)
        }

        // 2) PUT the ciphertext under the content-addressed
        // blob_id.
        onStage?.invoke(UploadStage.Uploading)
        try {
            storage.putBlob(blobId = enc.blobId, ciphertext = enc.ciphertext)
        } catch (e: Throwable) {
            throw AttachmentBridgeException.Upload(e)
        }

        // 3) Build the wire-shape ref. `size` is the plaintext
        // size (not ciphertext) so the recipient UI can decide
        // whether to fetch on a metered connection without
        // first knowing the +16-byte AEAD overhead.
        return AttachmentRef(
            blobId = enc.blobId,
            key = enc.key,
            nonce = enc.nonce,
            mime = mime.takeUnless { it.isNullOrEmpty() },
            size = bytes.size.toULong(),
            name = name.takeUnless { it.isNullOrEmpty() },
        )
    }

    /**
     * GET the ciphertext for [ref] from storage, hand to
     * `decryptAttachment`, return the plaintext bytes.
     *
     * Throws [AttachmentBridgeException.Missing] on 404 from
     * storage — the blob has been GC'd or never made it onto
     * the server (a sender whose PUT failed but whose MLS
     * message still shipped). The caller decides whether to
     * surface this as "attachment unavailable" or to retry.
     */
    public suspend fun downloadAndDecrypt(
        ref: AttachmentRef,
        storage: StorageClient = StorageClient(),
    ): ByteArray {
        val ciphertext: ByteArray = try {
            storage.getBlob(ref.blobId)
                ?: throw AttachmentBridgeException.Missing(ref.blobId)
        } catch (e: AttachmentBridgeException) {
            throw e
        } catch (e: Throwable) {
            throw AttachmentBridgeException.Download(e)
        }
        return try {
            ffiDecryptAttachment(
                ciphertext = ciphertext,
                key = ref.key,
                nonce = ref.nonce,
            )
        } catch (e: FfiException) {
            throw AttachmentBridgeException.Decryption(e)
        }
    }

    /**
     * Build the MLS Application-message JSON envelope from
     * `(text, attachments)`. Empty [text] is valid
     * (attachment-only message); empty [attachments] is valid
     * (text-only message — same wire shape as the pre-1.19
     * plain-UTF-8 path).
     *
     * Hand the resulting bytes to
     * `group.createApplicationMessage(identity, bytes)`
     * exactly like the pre-1.19 plain-UTF-8 path did.
     */
    public fun composeEnvelopeBytes(
        text: String,
        attachments: List<AttachmentRef>,
    ): ByteArray = ffiComposeEnvelope(text = text, attachments = attachments)

    /**
     * Parse an incoming MLS Application-message body. Returns
     * the canonical `(text, attachments)` pair from the UniFFI
     * envelope decoder.
     *
     * **Never throws.** Legacy pre-1.19 plain-UTF-8 bytes
     * collapse to `(text = <utf-8 lossy>, attachments = [])`,
     * unknown-version envelopes collapse to
     * `(text = "[unsupported v\$N message]", attachments = [])`,
     * malformed individual refs are silently skipped. Same
     * posture as the UniFFI `parse_envelope` documented
     * fallback paths (1.19a + 1.19b).
     */
    public fun parseEnvelopeBytes(
        bytes: ByteArray,
    ): ParsedEnvelopeView {
        val parsed = ffiParseEnvelope(bytes = bytes)
        return ParsedEnvelopeView(
            text = parsed.text,
            attachments = parsed.attachments,
        )
    }

    /** Plain-data envelope-parse output, decoupled from the
     *  UniFFI `ParsedEnvelope` record so call sites can drop
     *  the FFI import on the read path. */
    public data class ParsedEnvelopeView(
        val text: String,
        val attachments: List<AttachmentRef>,
    )

    /**
     * Format an attachment size for UI display — `42 B`,
     * `1.2 kB`, `4.7 MB`. SI-decimal (matches Android's
     * standard `Formatter.formatShortFileSize` output);
     * base-2 conversion is too pedantic for chat UI.
     */
    public fun formatSize(bytes: ULong): String = when {
        bytes < 1_000UL -> "$bytes B"
        bytes < 1_000_000UL -> "%.1f kB".format(bytes.toDouble() / 1_000)
        bytes < 1_000_000_000UL -> "%.1f MB".format(bytes.toDouble() / 1_000_000)
        else -> "%.1f GB".format(bytes.toDouble() / 1_000_000_000)
    }

    /**
     * `true` when the ref's MIME type is one the chat UI knows
     * how to inline-preview. Currently: the four core web
     * image formats. SVG is deliberately excluded — an
     * attacker-supplied SVG can carry XML scripting in some
     * renderers; denying preview consistently is the safe path.
     */
    public fun isInlinePreviewable(ref: AttachmentRef): Boolean {
        val m = ref.mime ?: return false
        return m == "image/jpeg" || m == "image/png" ||
            m == "image/webp" || m == "image/gif"
    }

    /** Lowercase hex of an arbitrary byte buffer — used by the
     *  UI for the short-fallback filename when an attachment
     *  ref carries no `name`. Mirror of
     *  `StorageClient.blobIdHex`. */
    public fun hex(bytes: ByteArray): String {
        val sb = StringBuilder(bytes.size * 2)
        for (b in bytes) {
            val v = b.toInt() and 0xFF
            sb.append(HEX[v ushr 4])
            sb.append(HEX[v and 0x0F])
        }
        return sb.toString()
    }

    private const val HEX: String = "0123456789abcdef"
}

/**
 * Coarse-grained pipeline stage reported through
 * [AttachmentBridge.encryptAndUpload]'s `onStage` callback
 * (Polish 1.x). Mirror of the iOS `UploadStage` enum + the web
 * client's `progress.stage` discriminator. The callback fires
 * once per stage, before the corresponding work begins, so the
 * UI flips its caption to match the about-to-happen work.
 */
public enum class UploadStage {
    /** About to run `encryptAttachment` over the plaintext bytes. */
    Encrypting,
    /** About to PUT the ciphertext to `services/storage`. */
    Uploading,
}

// =============================================================================
// Errors
// =============================================================================

/**
 * Discriminated failure surfaced by [AttachmentBridge] flows.
 * Distinct subclasses so the screen layer can branch on a
 * "too large" message differently from a generic "couldn't
 * upload, try again."
 */
public sealed class AttachmentBridgeException(
    message: String,
    cause: Throwable? = null,
) : RuntimeException(message, cause) {

    /** Plaintext above [AttachmentBridge.MAX_ATTACHMENT_BYTES]
     *  — fail fast before any encryption / upload. */
    public class TooLarge(
        public val got: Int,
        public val max: Int,
    ) : AttachmentBridgeException(
        "attachment $got bytes exceeds the $max-byte ceiling",
    )

    /** UniFFI `encryptAttachment` failed. */
    public class Encryption(cause: Throwable) :
        AttachmentBridgeException("attachment encryption failed", cause)

    /** UniFFI `decryptAttachment` failed — wrong key, wrong
     *  nonce, AEAD tag mismatch. */
    public class Decryption(cause: Throwable) :
        AttachmentBridgeException("attachment decryption failed", cause)

    /** [StorageClient.putBlob] failed (4xx / 5xx / network). */
    public class Upload(cause: Throwable) :
        AttachmentBridgeException("attachment upload failed", cause)

    /** [StorageClient.getBlob] failed with a non-404 error. */
    public class Download(cause: Throwable) :
        AttachmentBridgeException("attachment download failed", cause)

    /** Storage returned 404 — the blob is no longer there. */
    public class Missing(public val blobId: ByteArray) :
        AttachmentBridgeException(
            "the attachment is no longer available on the server",
        )
}
