// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.ThumbRef

/**
 * ADR-0052 decision 2 — sending a forward: the payload rule applied, every
 * attachment re-encrypted under a fresh key and uploaded again, then one
 * ordinary send into the target conversation.
 *
 * **Re-encrypted, not re-sent by reference** (ADR-0052 OQ1, closed by the
 * maintainer). An [AttachmentRef] carries its own decryption key, so
 * forwarding the reference would have handed the new conversation the same
 * stored blob — and left the two conversations sharing one ciphertext object
 * the storage tier could see fetched by members of both. So a forward
 * downloads, decrypts, re-encrypts under a fresh key and uploads its own
 * blob; nothing on the server ties the two conversations together. The
 * poster thumb goes the same way. What is kept is the metadata the bubble
 * draws from: kind, duration, waveform, dimensions.
 *
 * The effects are injected so the composition is host-testable:
 * production passes the attachment bridge and the repository's send, which
 * already serialises under the target group's MLS lock.
 */
public class ForwardSender(
    private val download: suspend (AttachmentRef) -> ByteArray = {
        AttachmentBridge.downloadAndDecrypt(it)
    },
    private val downloadThumb: suspend (ThumbRef) -> ByteArray = {
        AttachmentBridge.downloadAndDecryptThumb(it)
    },
    private val upload: suspend (
        bytes: ByteArray,
        mime: String?,
        name: String?,
        media: AttachmentMediaMeta?,
    ) -> AttachmentRef = { bytes, mime, name, media ->
        AttachmentBridge.encryptAndUpload(bytes = bytes, mime = mime, name = name, media = media)
    },
    private val uploadThumb: suspend (
        bytes: ByteArray,
        mime: String,
        width: UInt?,
        height: UInt?,
    ) -> ThumbRef = { bytes, mime, width, height ->
        AttachmentBridge.encryptAndUploadThumb(bytes = bytes, mime = mime, width = width, height = height)
    },
    private val send: suspend (
        targetGroupIdHex: String,
        text: String,
        attachments: List<AttachmentRef>,
        forward: ForwardRef,
        content: uniffi.lakko_ffi_uniffi.MessageContent?,
    ) -> Unit,
    private val onLog: (String) -> Unit = {},
) {
    /**
     * Forward [request]'s message into [targetGroupIdHex].
     *
     * Throws when an attachment cannot be fetched or re-uploaded: a forward
     * that silently dropped a photo would not be the message the person
     * chose. A poster thumb that fails is the one exception — the note is
     * sent without one and the bubble shows a neutral circle, the ADR-0047
     * answer for a failed poster upload.
     */
    public suspend fun forward(request: ForwardRequest, targetGroupIdHex: String) {
        val (text, ref) = forwardPayloadOf(
            message = request.message,
            authorLabel = request.authorLabel,
            originGroupIdHex = request.sourceGroupIdHex,
            sourceIsOneToOne = request.sourceIsOneToOne,
        )
        // ADR-0090 / UX 11a — a location, contact card or article goes as the
        // same content; an article's images are its attachments, re-encrypted
        // below in order so each image block's index still names its image. A
        // poll or a checklist is never forwarded (the menu does not offer it).
        val content = request.message.content
        check(isForwardableContent(content) { true }) { "a poll or checklist is not forwarded" }
        val attachments = request.message.attachments.map { reEncrypt(it) }
        send(targetGroupIdHex, text, attachments, ref, content)
    }

    /**
     * Section 8 — forward every request into [targetGroupIdHex], in the
     * order given (oldest first), each composed exactly as [forward] composes
     * one, stopping at the first failure. The result says how many landed and
     * which were not sent; it never throws for a failed send.
     */
    public suspend fun forwardAll(
        requests: List<ForwardRequest>,
        targetGroupIdHex: String,
    ): BulkResult<ForwardRequest> =
        performInOrder(requests) { forward(it, targetGroupIdHex) }

    /** One attachment under a fresh key, with its metadata carried over. */
    private suspend fun reEncrypt(source: AttachmentRef): AttachmentRef {
        val bytes = download(source)
        val thumb = source.thumb?.let { reEncryptThumb(it) }
        return upload(
            bytes,
            source.mime,
            source.name,
            AttachmentMediaMeta(
                kind = source.kind,
                durationMs = source.durationMs,
                waveform = source.waveform,
                width = source.width,
                height = source.height,
                thumb = thumb,
            ),
        )
    }

    private suspend fun reEncryptThumb(source: ThumbRef): ThumbRef? {
        val mime = source.mime ?: return null
        return try {
            uploadThumb(downloadThumb(source), mime, source.width, source.height)
        } catch (e: kotlin.coroutines.cancellation.CancellationException) {
            throw e
        } catch (e: Throwable) {
            onLog("forward: poster re-upload failed (${e.javaClass.simpleName}); sending without a poster")
            null
        }
    }
}
