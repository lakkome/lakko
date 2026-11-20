// SPDX-License-Identifier: AGPL-3.0-or-later

package me.lakko.app.chat

/**
 * docs/ux/attachments.md section 2.5 (amended 2026-11-18): a photo or video
 * pasted into the composer, or inserted from the keyboard (an image or GIF
 * from Gboard), attaches through the same send preview as the attachment
 * menu's Photo or Video. Everything else a paste carries stays text.
 *
 * The decisions here are pure so the JVM unit tests can hold them; the
 * Compose receiver in the composer only feeds them what the clip says.
 */

/** One media item taken from a paste or a keyboard insertion. */
public data class PastedMedia(
    public val uri: android.net.Uri,
    /** The MIME type the item was taken under; never null once taken. */
    public val mime: String,
)

/**
 * Whether a paste or keyboard insertion of this MIME type attaches as a photo
 * or video. Any `image/` or `video/` type does, except SVG: it is a document a
 * photo decoder cannot read, and the send preview would only refuse it.
 * Parameters (`; charset=...`) and case do not matter. A missing or malformed
 * type never attaches, so the item stays with the text field.
 */
public fun pasteAttachesMime(mime: String?): Boolean {
    val essence = mimeEssence(mime) ?: return false
    if (essence == "image/svg+xml") return false
    val slash = essence.indexOf('/')
    if (slash <= 0 || slash == essence.length - 1) return false
    val top = essence.substring(0, slash)
    return top == "image" || top == "video"
}

/**
 * The MIME type to judge one pasted item by.
 *
 * [itemType] is what the content provider reports for the item's URI and wins
 * when present. Without it, the clip's own description decides, but only
 * when it names exactly one attachable type: a clip that lists several (or
 * none) cannot say which one this item is, and guessing would send a video as
 * a photo.
 */
public fun resolvePastedMime(itemType: String?, clipMimeTypes: List<String>): String? {
    mimeEssence(itemType)?.let { return it }
    val attachable = clipMimeTypes.mapNotNull { mimeEssence(it) }.filter { pasteAttachesMime(it) }.distinct()
    return attachable.singleOrNull()
}

/** Lower-cased `type/subtype` with parameters and spaces dropped; null when empty. */
internal fun mimeEssence(mime: String?): String? {
    val essence = mime?.substringBefore(';')?.trim()?.lowercase()
    return essence?.takeUnless { it.isEmpty() }
}
