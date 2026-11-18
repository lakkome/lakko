// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

/** UX attachments sections 3-4 — one picked item in the send preview. */
public data class PreviewItem(
    val id: Long,
    val name: String?,
    val mime: String?,
    val bytes: ByteArray,
    /** A JPEG thumbnail for the strip, `null` for a file chip. */
    val thumb: ByteArray?,
    /** What the item would go as: a photo, a video, or a file. */
    val kind: PreviewKind,
    /** UX 3 — the catalog key of why this item cannot go as it is, or `null`. */
    val problemKey: String? = null,
    /** The video track's MIME (`video/avc` = H.264), `null` when unread or not a video. */
    val videoCodec: String? = null,
) {
    override fun equals(other: Any?): Boolean = other is PreviewItem && other.id == id && other.problemKey == problemKey
    override fun hashCode(): Int = id.hashCode()
}

public enum class PreviewKind { PHOTO, VIDEO, FILE }

/** The send preview: items, caption, and whether everything goes as files. */
public data class SendPreview(
    val items: List<PreviewItem>,
    val caption: String = "",
    val sendAsFile: Boolean = false,
    /** Opened from the File item: files only, no `Send as file` switch. */
    val filesOnly: Boolean = false,
)

/** The attachment cap the UX copy names (50 MB). */
public const val PREVIEW_MAX_BYTES: Long = 50L * 1024 * 1024

/**
 * UX 3 — the problem a video or file has as it would be sent, or `null`:
 * a video must be MP4 with H.264 and at most the cap (else send it as a file);
 * any file over the cap is refused. [videoCodec] is the video track's MIME
 * (`video/avc` for H.264), `null` when unread.
 */
public fun previewProblem(kind: PreviewKind, mime: String?, size: Long, videoCodec: String?, sendAsFile: Boolean): String? =
    when {
        size > PREVIEW_MAX_BYTES -> "attach.preview.file_too_big"
        sendAsFile || kind != PreviewKind.VIDEO -> null
        mime?.lowercase() != "video/mp4" || videoCodec != "video/avc" -> "attach.preview.video_refused"
        else -> null
    }

/**
 * UX 3 — several items go as several messages, in the order shown, the
 * caption on the first. Returns (item, caption) pairs.
 */
public fun previewSendOrder(items: List<PreviewItem>, caption: String): List<Pair<PreviewItem, String>> =
    items.mapIndexed { i, item -> item to if (i == 0) caption.trim() else "" }

/**
 * Read [uris] into preview items: kind, strip thumbnail, the video track's
 * codec and the problem each has as it would be sent (UX 3-4). Runs the
 * readers and decoders off the main thread.
 */
public suspend fun buildPreviewItems(
    context: android.content.Context,
    uris: List<android.net.Uri>,
    filesOnly: Boolean,
    nextId: () -> Long,
): List<PreviewItem> = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
    uris.map { uri ->
        val read = me.lakko.app.media.readPickedUri(context, uri, PREVIEW_MAX_BYTES.toInt())
        val ok = read as? me.lakko.app.media.PickedRead.Ok
        if (ok == null) {
            val failed = read as? me.lakko.app.media.PickedRead.Failed
            PreviewItem(
                id = nextId(), name = failed?.name, mime = null, bytes = ByteArray(0), thumb = null,
                kind = PreviewKind.FILE,
                problemKey = if (failed?.error is me.lakko.app.chat.AttachmentBridgeException.TooLarge) {
                    "attach.preview.file_too_big"
                } else {
                    "attach.preview.photo_unreadable"
                },
            )
        } else {
            val p = ok.payload
            val kind = when {
                filesOnly -> PreviewKind.FILE
                p.mime?.startsWith("image/") == true -> PreviewKind.PHOTO
                p.mime?.startsWith("video/") == true -> PreviewKind.VIDEO
                else -> PreviewKind.FILE
            }
            val thumb = if (kind == PreviewKind.FILE) null else runCatching {
                me.lakko.app.media.prepareVisualMedia(p.bytes, p.mime, context.cacheDir)?.thumb
            }.getOrNull()
            val codec = if (kind == PreviewKind.VIDEO) me.lakko.app.media.videoTrackMime(context, p.bytes) else null
            val problem = if (kind == PreviewKind.PHOTO && thumb == null) {
                "attach.preview.photo_unreadable"
            } else {
                previewProblem(kind, p.mime, p.bytes.size.toLong(), codec, sendAsFile = false)
            }
            PreviewItem(nextId(), p.name, p.mime, p.bytes, thumb, kind, problem, codec)
        }
    }
}

/**
 * Upload one preview item as it goes (UX 3): a photo re-encoded with its
 * metadata dropped, a video as picked with size and poster, or the original
 * bytes when it goes as a file. `null` when a photo cannot be re-encoded.
 * [idleDays] is a channel file's lifetime (ADR-0087 section 13), `null` in a chat.
 */
public suspend fun uploadPreviewItem(
    context: android.content.Context,
    item: PreviewItem,
    sendAsFile: Boolean,
    maxSide: Int,
    idleDays: Int? = null,
): uniffi.lakko_ffi_uniffi.AttachmentRef? {
    val asFile = sendAsFile || item.kind == PreviewKind.FILE
    val (bytes, mime, name) = if (!asFile && item.kind == PreviewKind.PHOTO && item.mime != "image/gif") {
        val jpeg = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.Default) {
            me.lakko.app.media.reencodePhoto(item.bytes, maxSide)
        } ?: return null
        Triple(jpeg, "image/jpeg", (item.name?.substringBeforeLast('.') ?: "photo") + ".jpg")
    } else {
        Triple(item.bytes, if (asFile) (item.mime ?: "application/octet-stream") else item.mime, item.name)
    }
    val meta = if (asFile) {
        null
    } else {
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.Default) {
            me.lakko.app.media.prepareVisualMedia(bytes, mime, context.cacheDir)
        }?.let { v ->
            val thumbRef = v.thumb?.let { t ->
                runCatching {
                    me.lakko.app.chat.AttachmentBridge.encryptAndUploadThumb(t, "image/jpeg", v.thumbSize?.width?.toUInt(), v.thumbSize?.height?.toUInt())
                }.getOrNull()
            }
            me.lakko.app.chat.AttachmentMediaMeta(width = v.size?.width?.toUInt(), height = v.size?.height?.toUInt(), thumb = thumbRef)
        }
    }
    return me.lakko.app.chat.AttachmentBridge.encryptAndUpload(bytes = bytes, mime = mime, name = name, media = meta, idleDays = idleDays)
}

/** The preview state changes shared by every conversation kind. */
public fun SendPreview.withSendAsFile(on: Boolean): SendPreview = copy(
    sendAsFile = on,
    items = items.map { item ->
        if (item.problemKey == "attach.preview.photo_unreadable") {
            item
        } else {
            item.copy(problemKey = previewProblem(item.kind, item.mime, item.bytes.size.toLong(), item.videoCodec, on))
        }
    },
)

public fun SendPreview.without(id: Long): SendPreview? = items.filterNot { it.id == id }.let { left ->
    if (left.isEmpty()) null else copy(items = left)
}
