// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.async
import kotlinx.coroutines.withTimeoutOrNull
import me.lakko.app.chat.AttachmentBridgeException

/**
 * UX spec §10 — the rules that keep a picked item from disappearing: every
 * pick gets a visible upload chip at once, a preview step that hangs is
 * abandoned instead of blocking the send, and a failure names the item.
 */

/** How long a photo / video preview may take before the item is sent
 *  without one. */
public const val VISUAL_PREP_TIMEOUT_MS: Long = 15_000L

/** The chip label before the picked item's name is known. */
public const val PICKED_ITEM_PLACEHOLDER: String = "Selected item"

/**
 * Run [block] in [scope] and wait at most [timeoutMs] for it. `null` on
 * timeout or when [block] throws. The block is NOT tied to the caller: a
 * native decoder that ignores cancellation keeps running in [scope] while the
 * caller moves on.
 */
public suspend fun <T : Any> awaitBounded(
    scope: CoroutineScope,
    timeoutMs: Long,
    block: suspend () -> T?,
): T? {
    val deferred = scope.async { runCatching { block() }.getOrNull() }
    return withTimeoutOrNull(timeoutMs) { deferred.await() }
}

/** The readable composer error for an attachment that could not be added. */
public fun attachFailureMessage(name: String?, error: Throwable?): String {
    val label = name?.takeUnless { it.isBlank() }?.let { "\"$it\"" } ?: "the selected item"
    val reason = when (error) {
        is AttachmentBridgeException.TooLarge -> "it is larger than 50 MB"
        is AttachmentBridgeException.Upload -> "the upload failed"
        is AttachmentBridgeException.Encryption -> "encryption failed"
        null -> "it could not be read"
        else -> "something went wrong"
    }
    return "Couldn't attach $label: $reason. Try again."
}

/** Bytes + metadata read from a picker URI. */
public class PickedPayload(
    public val bytes: ByteArray,
    public val mime: String?,
    public val name: String?,
)

/** What reading a picked URI produced. */
public sealed interface PickedRead {
    public class Ok(public val payload: PickedPayload) : PickedRead
    public class Failed(public val name: String?, public val error: Throwable?) : PickedRead
}

/**
 * Resolve a `content://` URI from a system picker to bytes, MIME type and
 * display name. The declared size is checked BEFORE reading so a huge video
 * fails with a readable error instead of exhausting memory. Must run off the
 * main thread; never throws.
 */
public fun readPickedUri(context: android.content.Context, uri: android.net.Uri, maxBytes: Int): PickedRead {
    val resolver = context.contentResolver
    val mime = runCatching { resolver.getType(uri) }.getOrNull()?.takeUnless { it.isEmpty() }
    var displayName: String? = null
    var declaredSize: Long? = null
    runCatching {
        resolver.query(
            uri,
            arrayOf(android.provider.OpenableColumns.DISPLAY_NAME, android.provider.OpenableColumns.SIZE),
            null,
            null,
            null,
        )?.use { cursor ->
            if (cursor.moveToFirst()) {
                val nameIdx = cursor.getColumnIndex(android.provider.OpenableColumns.DISPLAY_NAME)
                if (nameIdx >= 0) displayName = cursor.getString(nameIdx)
                val sizeIdx = cursor.getColumnIndex(android.provider.OpenableColumns.SIZE)
                if (sizeIdx >= 0 && !cursor.isNull(sizeIdx)) declaredSize = cursor.getLong(sizeIdx)
            }
        }
    }
    val name = displayName?.takeUnless { it.isEmpty() }
    val size = declaredSize
    if (size != null && size > maxBytes) {
        return PickedRead.Failed(name, AttachmentBridgeException.TooLarge(got = size.coerceAtMost(Int.MAX_VALUE.toLong()).toInt(), max = maxBytes))
    }
    return try {
        val bytes = resolver.openInputStream(uri)?.use { it.readBytes() }
        when {
            bytes == null -> PickedRead.Failed(name, null)
            bytes.size > maxBytes -> PickedRead.Failed(name, AttachmentBridgeException.TooLarge(bytes.size, maxBytes))
            else -> PickedRead.Ok(PickedPayload(bytes, mime, name))
        }
    } catch (e: Throwable) {
        // OutOfMemoryError included.
        android.util.Log.w("LakkoPickedUploads", "picked item unreadable: ${e.javaClass.simpleName}")
        PickedRead.Failed(name, null)
    }
}
