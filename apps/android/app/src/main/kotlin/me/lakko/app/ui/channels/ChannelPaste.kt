// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import me.lakko.app.chat.PastedMedia

/**
 * docs/ux/attachments.md 2.5 in a channel: the owner and publishers have the
 * composer and its attachments, so a pasted or keyboard-inserted photo or
 * video opens the same send preview as Photo or Video. A subscriber attaches
 * nothing (comments take no attachments, ADR-0087), so for a subscriber there
 * is no handler and a paste stays text.
 *
 * [openPreview] is the screen's send-preview entry: the URIs, whether they go
 * as files, and the type each item was taken under.
 */
internal fun channelPasteHandler(
    isPublisher: Boolean,
    openPreview: (uris: List<android.net.Uri>, filesOnly: Boolean, mimeHints: List<String?>) -> Unit,
): ((List<PastedMedia>) -> Unit)? {
    if (!isPublisher) return null
    return { items ->
        if (items.isNotEmpty()) openPreview(items.map { it.uri }, false, items.map { it.mime })
    }
}
