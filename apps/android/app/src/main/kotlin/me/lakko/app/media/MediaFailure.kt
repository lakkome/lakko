// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import me.lakko.app.chat.AttachmentBridgeException
import me.lakko.app.storage.BlobPassKind
import me.lakko.app.storage.StorageFullError
import me.lakko.app.storage.StorageLimitError

/**
 * docs/ux/saved-messages-and-file-lifetime.md section 4 — why a file did not
 * open, the same three answers for a photo, a video, a voice message, a video
 * note, a GIF and a file.
 */
public enum class MediaFailure {
    /** The storage service no longer has it (4.1): two lines, no Retry. */
    GONE,

    /** The opening limit is reached (4.2): `Too many files opened` with Try
     *  again. */
    LIMIT,

    /** Anything else: the existing failure with Try again. */
    FAILED,
}

/** The failure a download or open threw, read through its causes. */
public fun mediaFailureOf(error: Throwable): MediaFailure {
    var e: Throwable? = error
    var depth = 0
    while (e != null && depth < 8) {
        if (e is AttachmentBridgeException.Missing) return MediaFailure.GONE
        if (e is StorageLimitError) return MediaFailure.LIMIT
        e = e.cause
        depth += 1
    }
    return MediaFailure.FAILED
}

/** Whether an upload failed for the sending limit (4.2): the composer then
 *  says so and keeps the text. */
public fun isUploadLimit(error: Throwable): Boolean {
    var e: Throwable? = error
    var depth = 0
    while (e != null && depth < 8) {
        if (e is StorageLimitError && e.kind == BlobPassKind.UPLOAD) return true
        e = e.cause
        depth += 1
    }
    return false
}

/** docs/ux/storage-full.md — whether an upload failed because the server
 *  has no room for new files (`507 storage-full`). Said where the sending
 *  limit is said; the text and the chosen file stay. */
public fun isStorageFull(error: Throwable): Boolean {
    var e: Throwable? = error
    var depth = 0
    while (e != null && depth < 8) {
        if (e is StorageFullError) return true
        e = e.cause
        depth += 1
    }
    return false
}

/** Why an upload was refused, when it is one the person is told about. */
public enum class UploadRefusal { LIMIT, FULL }

/** The told refusal behind an upload failure, or `null` for any other. */
public fun uploadRefusalOf(error: Throwable): UploadRefusal? = when {
    isStorageFull(error) -> UploadRefusal.FULL
    isUploadLimit(error) -> UploadRefusal.LIMIT
    else -> null
}

/** The composer / bubble line for a told upload refusal. */
public fun uploadRefusalSendKey(refusal: UploadRefusal): String = when (refusal) {
    UploadRefusal.LIMIT -> "chat.attachment.limit.send"
    UploadRefusal.FULL -> "chat.attachment.full.send"
}

/** The line under a group or channel photo for a failed upload, or `null`
 *  when the refusal is not one of the told ones. */
public fun groupPhotoRefusalKey(error: Throwable): String? = when (uploadRefusalOf(error)) {
    UploadRefusal.FULL -> "group_photo.error.full"
    UploadRefusal.LIMIT -> "group_photo.error.limit"
    null -> null
}

/** The profile Save line for a failed photo upload. */
public fun profilePhotoUploadFailureKey(error: Throwable): String =
    if (isStorageFull(error)) "profile.save.error.full" else "profile.save.error.upload"

/** The catalog keys of a failure's lines; `null` for [MediaFailure.FAILED]
 *  (each bubble keeps its own generic line). */
public data class MediaFailureCopy(val title: String, val body: String?, val retry: Boolean)

public fun mediaFailureCopy(failure: MediaFailure): MediaFailureCopy? = when (failure) {
    MediaFailure.GONE -> MediaFailureCopy("chat.attachment.gone.title", "chat.attachment.gone.body", retry = false)
    MediaFailure.LIMIT -> MediaFailureCopy("chat.attachment.limit.open", null, retry = true)
    MediaFailure.FAILED -> null
}
