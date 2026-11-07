// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.storage.StorageClient
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.EncryptedAttachment
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.ThumbRef
import uniffi.lakko_ffi_uniffi.composeEnvelope as ffiComposeEnvelope
import uniffi.lakko_ffi_uniffi.decryptAttachment as ffiDecryptAttachment
import uniffi.lakko_ffi_uniffi.encryptAttachment as ffiEncryptAttachment

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
 *      `decryptAttachment` / `composeEnvelope` free functions,
 *      exposed in 1.19b.
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
 * Plus the [composeEnvelopeBytes] friendly wrapper around the
 * UniFFI free function (rename-shadowed
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
        /** ADR-0047 — the optional media metadata (`kind`, duration,
         *  waveform, dimensions, poster thumb) carried on the ref. `null` is a
         *  plain file, byte-identical to the pre-ADR-0047 ref. The core drops
         *  any field outside its bound on the receiving side; this side does
         *  not rely on that and never sets an out-of-bound value. */
        media: AttachmentMediaMeta? = null,
        /** ADR-0047 — upload progress, 0..1, for the determinate ring. */
        onProgress: ((Float) -> Unit)? = null,
        /** ADR-0087 section 13 — a channel file lives 45 idle days. */
        idleDays: Int? = null,
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
            storage.putBlob(
                blobId = enc.blobId,
                ciphertext = enc.ciphertext,
                onProgress = onProgress?.let { report ->
                    { sent: Long, total: Long ->
                        report(if (total <= 0L) 0f else (sent.toFloat() / total).coerceIn(0f, 1f))
                    }
                },
                idleDays = idleDays,
            )
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
            kind = media?.kind,
            durationMs = media?.durationMs,
            waveform = media?.waveform?.takeIf { it.isNotEmpty() && it.size <= MAX_WAVEFORM_BYTES },
            width = media?.width?.takeIf { it in 1u..MAX_MEDIA_DIMENSION },
            height = media?.height?.takeIf { it in 1u..MAX_MEDIA_DIMENSION },
            thumb = media?.thumb,
        )
    }

    /**
     * ADR-0047 — encrypt and upload a video-note poster frame, returning the
     * [ThumbRef] the note's ref carries. Refuses a poster over
     * [MAX_THUMB_BYTES] (the core would drop it on receipt anyway).
     */
    public suspend fun encryptAndUploadThumb(
        bytes: ByteArray,
        mime: String,
        width: UInt?,
        height: UInt?,
        storage: StorageClient = StorageClient(),
    ): ThumbRef {
        if (bytes.isEmpty() || bytes.size > MAX_THUMB_BYTES) {
            throw AttachmentBridgeException.TooLarge(got = bytes.size, max = MAX_THUMB_BYTES)
        }
        val enc: EncryptedAttachment = try {
            ffiEncryptAttachment(plaintext = bytes)
        } catch (e: FfiException) {
            throw AttachmentBridgeException.Encryption(e)
        }
        try {
            storage.putBlob(blobId = enc.blobId, ciphertext = enc.ciphertext)
        } catch (e: Throwable) {
            throw AttachmentBridgeException.Upload(e)
        }
        return ThumbRef(
            blobId = enc.blobId,
            key = enc.key,
            nonce = enc.nonce,
            mime = mime,
            size = bytes.size.toULong(),
            width = width?.takeIf { it in 1u..MAX_MEDIA_DIMENSION },
            height = height?.takeIf { it in 1u..MAX_MEDIA_DIMENSION },
        )
    }

    /** ADR-0047 — download and AEAD-open a video-note poster. */
    public suspend fun downloadAndDecryptThumb(
        thumb: ThumbRef,
        storage: StorageClient = StorageClient(),
    ): ByteArray = downloadAndDecrypt(
        ref = AttachmentRef(
            blobId = thumb.blobId,
            key = thumb.key,
            nonce = thumb.nonce,
            mime = thumb.mime,
            size = thumb.size,
            name = null,
        ),
        storage = storage,
    )

    /** ADR-0047 §1 — mirror of the core's waveform bound. */
    public const val MAX_WAVEFORM_BYTES: Int = 128

    /** ADR-0047 §1 — mirror of the core's per-side dimension bound. */
    public const val MAX_MEDIA_DIMENSION: UInt = 4096u

    /** ADR-0047 §1 — mirror of the core's poster size bound (256 KiB). */
    public const val MAX_THUMB_BYTES: Int = 256 * 1024

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

    // There is deliberately no `parseEnvelope` wrapper here. Its fallback
    // turned a newer-wire-version envelope into the core's English text
    // "[unsupported vN message]", which would have reached the screen
    // (ADR-0061 section 6). The receive path parses through
    // `parseBodyEnvelope`, whose `UNSUPPORTED` kind carries no text and is
    // rendered from the catalog (`chat.message.unsupported`).

    /**
     * Format an attachment size for UI display — `42 B`,
     * `1.2 kB`, `4.7 MB`. SI-decimal (matches Android's
     * standard `Formatter.formatShortFileSize` output);
     * base-2 conversion is too pedantic for chat UI.
     */
    public fun formatSize(
        bytes: ULong,
        strings: me.lakko.app.l10n.Strings = me.lakko.app.l10n.Localization.strings.value,
    ): String {
        // ADR-0061: the number in the interface language's digits, the unit
        // from the catalog (`common.size.*`); the decimal and grouping
        // separators from the device's region (localization.md section 5).
        val locale = java.util.Locale.forLanguageTag(strings.language)
        val region = me.lakko.app.l10n.DeviceFormats.region()
        fun dec(v: Double) = me.lakko.app.l10n.DeviceFormats.decimal(v, 1, locale, region)
        val (key, value) = when {
            bytes < 1_000UL -> "common.size.bytes" to me.lakko.app.l10n.DeviceFormats.integer(bytes.toLong(), locale, region)
            bytes < 1_000_000UL -> "common.size.kb" to dec(bytes.toDouble() / 1_000)
            bytes < 1_000_000_000UL -> "common.size.mb" to dec(bytes.toDouble() / 1_000_000)
            else -> "common.size.gb" to dec(bytes.toDouble() / 1_000_000_000)
        }
        return strings.format(key, mapOf("size" to value))
    }

    /**
     * `true` when the ref's MIME type is one the chat UI knows
     * how to inline-preview as an image, per the core's allowlist
     * (`inlineMediaType`). SVG is never on it — an attacker-supplied
     * SVG can carry XML scripting in some renderers.
     */
    public fun isInlinePreviewable(
        ref: AttachmentRef,
        allowlist: me.lakko.app.media.MediaAllowlist = me.lakko.app.media.coreAllowlist,
    ): Boolean =
        // ADR-0047 §1 — the core's MIME allowlist decides, never a list kept
        // here (the core refuses SVG, and ignores MIME parameters).
        me.lakko.app.media.inlineKindOf(ref.mime, allowlist) == me.lakko.app.media.InlineKind.IMAGE

    /**
     * A filename that is safe to join onto a local directory, derived from
     * the ref's `name`.
     *
     * `name` is chosen by the SENDER and arrives inside an MLS application
     * message, so every member of a group controls it. Passed to
     * `File(dir, name)` verbatim, `../../files/x` walks out of the share
     * cache and overwrites app-private files when the recipient taps Save.
     * This keeps only the last path component (splitting on both `/` and
     * `\\`), drops control and format characters (the latter include the
     * right-to-left override that disguises an extension), trims, caps the
     * length, and falls back when nothing usable is left — including `.`
     * and `..`, which are path components rather than names.
     */
    public fun safeFileName(raw: String?, fallback: String): String {
        if (raw == null) return fallback
        val lastComponent = raw.split('/', '\\').last()
        val cleaned = buildString {
            var i = 0
            while (i < lastComponent.length) {
                val cp = lastComponent.codePointAt(i)
                val type = Character.getType(cp)
                if (type != Character.CONTROL.toInt() && type != Character.FORMAT.toInt()) {
                    appendCodePoint(cp)
                }
                i += Character.charCount(cp)
            }
        }.trim()
        val capped = if (cleaned.codePointCount(0, cleaned.length) > MAX_SAFE_FILE_NAME_LENGTH) {
            cleaned.substring(0, cleaned.offsetByCodePoints(0, MAX_SAFE_FILE_NAME_LENGTH))
        } else {
            cleaned
        }
        return if (capped.isEmpty() || capped == "." || capped == "..") fallback else capped
    }

    /** Upper bound on [safeFileName]'s output, in code points. */
    public const val MAX_SAFE_FILE_NAME_LENGTH: Int = 128

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
 * ADR-0047 — the optional media metadata an [AttachmentRef] carries. Every
 * field is optional on the wire; `kind` picks the bubble on the receiving side
 * and nothing else (inline playback is decided by the MIME allowlist).
 */
public data class AttachmentMediaMeta(
    val kind: String? = null,
    val durationMs: UInt? = null,
    val waveform: ByteArray? = null,
    val width: UInt? = null,
    val height: UInt? = null,
    val thumb: ThumbRef? = null,
) {
    override fun equals(other: Any?): Boolean =
        other is AttachmentMediaMeta && kind == other.kind && durationMs == other.durationMs &&
            (waveform?.contentEquals(other.waveform) ?: (other.waveform == null)) &&
            width == other.width && height == other.height && thumb == other.thumb

    override fun hashCode(): Int {
        var h = kind.hashCode()
        h = 31 * h + durationMs.hashCode()
        h = 31 * h + (waveform?.contentHashCode() ?: 0)
        h = 31 * h + width.hashCode()
        h = 31 * h + height.hashCode()
        return 31 * h + thumb.hashCode()
    }
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
