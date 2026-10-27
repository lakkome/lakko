// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import me.lakko.app.chat.AttachmentBridgeException
import me.lakko.app.storage.BlobPassKind
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

/** The catalog keys of a failure's lines; `null` for [MediaFailure.FAILED]
 *  (each bubble keeps its own generic line). */
public data class MediaFailureCopy(val title: String, val body: String?, val retry: Boolean)

public fun mediaFailureCopy(failure: MediaFailure): MediaFailureCopy? = when (failure) {
    MediaFailure.GONE -> MediaFailureCopy("chat.attachment.gone.title", "chat.attachment.gone.body", retry = false)
    MediaFailure.LIMIT -> MediaFailureCopy("chat.attachment.limit.open", null, retry = true)
    MediaFailure.FAILED -> null
}
