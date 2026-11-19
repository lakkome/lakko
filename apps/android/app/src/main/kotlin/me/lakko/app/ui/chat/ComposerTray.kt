// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import uniffi.lakko_ffi_uniffi.AttachmentRef

/*
 * docs/ux/media-messages.md section 11 — the composer tray: every photo,
 * video and file a person attached to the message they are writing, shown
 * above the field and sent together with the text as ONE message. The rules
 * here are pure (no Android, no native core) so they run on the host JVM.
 */

/** What an item in the tray is: a square thumbnail (photo, video) or a chip. */
public enum class TrayKind { PHOTO, VIDEO, FILE }

/** Where an item's upload stands. */
public sealed interface TrayStatus {
    /** Being read and prepared (thumbnail, size); no upload yet. */
    public data object Preparing : TrayStatus

    /** Encrypting and uploading; [progress] 0..1 once bytes are moving. */
    public data class Uploading(val progress: Float?) : TrayStatus

    /** The upload failed: the item shows a retry glyph and Send stays off. */
    public data object Failed : TrayStatus

    /** Uploaded; the reference is ready to go with the message. */
    public data object Ready : TrayStatus
}

/** One item in the tray, in the order it was added. */
public class TrayItem(
    public val id: Long,
    public val kind: TrayKind,
    /** The file's name, `null` when the source gives none. */
    public val name: String?,
    /** Plaintext size in bytes, 0 until read. */
    public val size: Long = 0,
    /** A video's length, for the pill on its thumbnail; `null` when unknown. */
    public val durationMs: Long? = null,
    /** The JPEG thumbnail a photo or video shows, `null` for a file or until made. */
    public val thumb: ByteArray? = null,
    public val status: TrayStatus = TrayStatus.Preparing,
    /** The uploaded reference, set when [status] is [TrayStatus.Ready]. */
    public val ref: AttachmentRef? = null,
) {
    public fun copy(
        kind: TrayKind = this.kind,
        name: String? = this.name,
        size: Long = this.size,
        durationMs: Long? = this.durationMs,
        thumb: ByteArray? = this.thumb,
        status: TrayStatus = this.status,
        ref: AttachmentRef? = this.ref,
    ): TrayItem = TrayItem(id, kind, name, size, durationMs, thumb, status, ref)

    /** A photo or a video (a square thumbnail, counted against the media limit). */
    public val isMedia: Boolean get() = kind != TrayKind.FILE

    override fun equals(other: Any?): Boolean =
        other is TrayItem && other.id == id && other.kind == kind && other.name == name &&
            other.size == size && other.durationMs == durationMs && other.status == status &&
            (other.thumb === thumb || (other.thumb != null && thumb != null && other.thumb.contentEquals(thumb))) &&
            other.ref === ref

    override fun hashCode(): Int = id.hashCode() * 31 + status.hashCode()

    override fun toString(): String = "TrayItem($id, $kind, $status)"
}

/** A line shown under the strip for [TRAY_NOTICE_MS]. */
public data class TrayNotice(val text: me.lakko.app.l10n.UiText, val atMs: Long)

/** At most this many photos and videos in one message (chosen, not measured). */
public const val TRAY_MAX_MEDIA: Int = 10

/** At most this many files in one message (chosen, not measured). */
public const val TRAY_MAX_FILES: Int = 10

/** How long a limit line stays under the strip. */
public const val TRAY_NOTICE_MS: Long = 4_000L

public const val TRAY_LIMIT_MEDIA_KEY: String = "chat.tray.limit_media"
public const val TRAY_LIMIT_FILES_KEY: String = "chat.tray.limit_files"
public const val TRAY_FILE_TOO_BIG_KEY: String = "attach.preview.file_too_big"

/** The tray of one conversation's draft. */
public data class ComposerTray(
    val items: List<TrayItem> = emptyList(),
    val notice: TrayNotice? = null,
) {
    /** Photos and videos in the tray. */
    public val mediaCount: Int get() = items.count { it.isMedia }

    /** Files in the tray. */
    public val fileCount: Int get() = items.count { !it.isMedia }

    /** Some item is still being prepared, uploaded, or failed. */
    public val busy: Boolean get() = items.any { it.status != TrayStatus.Ready || it.ref == null }

    /** Section 11 Send — text or at least one item, and every item uploaded. */
    public fun canSend(text: String): Boolean = (text.isNotBlank() || items.isNotEmpty()) && !busy

    /** The references that go with the message, in the order added; the
     *  micro thumbnail (ADR-0047 section 6) only on the first photo or video. */
    public fun refs(allowlist: me.lakko.app.media.MediaAllowlist = me.lakko.app.media.coreAllowlist): List<AttachmentRef> =
        me.lakko.app.media.keepInlineThumbOnCarrier(items.mapNotNull { it.ref }, allowlist)

    /** The notice still on screen at [nowMs], or `null`. */
    public fun noticeAt(nowMs: Long): me.lakko.app.l10n.UiText? =
        notice?.takeIf { nowMs - it.atMs in 0 until TRAY_NOTICE_MS }?.text

    public fun item(id: Long): TrayItem? = items.firstOrNull { it.id == id }

    public fun update(id: Long, change: (TrayItem) -> TrayItem): ComposerTray =
        copy(items = items.map { if (it.id == id) change(it) else it })

    public fun without(id: Long): ComposerTray = copy(items = items.filterNot { it.id == id })

    /** The photos and videos, in order: what the viewer pages through. */
    public fun media(): List<TrayItem> = items.filter { it.isMedia }
}

/** Which of the incoming items fit, and the limit line to show, if any. */
public data class TrayAdmission(val admitted: List<Int>, val noticeKey: String?)

/**
 * Section 11 Limits — add what fits, in the order given: photos and videos up
 * to [TRAY_MAX_MEDIA], files up to [TRAY_MAX_FILES]. Returns the indices of
 * [incoming] that are admitted and the line that says what did not fit (the
 * media line when both overflowed).
 */
public fun ComposerTray.admit(incoming: List<TrayKind>): TrayAdmission {
    var media = mediaCount
    var files = fileCount
    var overMedia = false
    var overFiles = false
    val admitted = ArrayList<Int>(incoming.size)
    incoming.forEachIndexed { index, kind ->
        if (kind == TrayKind.FILE) {
            if (files < TRAY_MAX_FILES) {
                files += 1
                admitted += index
            } else {
                overFiles = true
            }
        } else {
            if (media < TRAY_MAX_MEDIA) {
                media += 1
                admitted += index
            } else {
                overMedia = true
            }
        }
    }
    val key = when {
        overMedia -> TRAY_LIMIT_MEDIA_KEY
        overFiles -> TRAY_LIMIT_FILES_KEY
        else -> null
    }
    return TrayAdmission(admitted, key)
}

/**
 * The tray kind an item is taken as before it is read, from its type: an
 * image or video picked as Photo or Video (or pasted) is a photo or video, and
 * anything else, or anything picked through File, is a file.
 */
public fun trayKindOf(mime: String?, filesOnly: Boolean): TrayKind {
    if (filesOnly) return TrayKind.FILE
    val essence = mime?.substringBefore(';')?.trim()?.lowercase() ?: return TrayKind.FILE
    return when {
        essence.startsWith("image/") -> TrayKind.PHOTO
        essence.startsWith("video/") -> TrayKind.VIDEO
        else -> TrayKind.FILE
    }
}

/**
 * docs/ux/attachments.md section 3 — the kind an item is SENT as once read: a
 * photo that cannot be decoded, and a video that is not MP4 with H.264, go as
 * files (never refused). [videoCodec] is the video track's MIME (`video/avc`
 * = H.264), `null` when unread.
 */
public fun trayKindAfterRead(guess: TrayKind, mime: String?, decodedPhoto: Boolean, videoCodec: String?): TrayKind =
    when (guess) {
        TrayKind.PHOTO -> if (decodedPhoto) TrayKind.PHOTO else TrayKind.FILE
        TrayKind.VIDEO -> {
            val essence = mime?.substringBefore(';')?.trim()?.lowercase()
            if (essence == "video/mp4" && videoCodec == "video/avc") TrayKind.VIDEO else TrayKind.FILE
        }
        TrayKind.FILE -> TrayKind.FILE
    }

/**
 * ADR-0047 section 6 — whether an item gets a micro thumbnail: its MIME is an
 * allowlisted image or video (the core's `inline_thumb_carrier` rule), however
 * it was added (Photo or Video, File, paste, drop, camera). [ComposerTray.refs]
 * then keeps it on the first such ref only.
 */
public fun wantsInlineThumb(mime: String?, allowlist: me.lakko.app.media.MediaAllowlist): Boolean =
    when (me.lakko.app.media.inlineKindOf(mime, allowlist)) {
        me.lakko.app.media.InlineKind.IMAGE, me.lakko.app.media.InlineKind.VIDEO -> true
        else -> false
    }
