// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.runtime.Composable
import androidx.compose.ui.unit.dp
import me.lakko.app.l10n.t
import me.lakko.app.ui.components.LakkoMenu
import me.lakko.app.ui.components.LakkoMenuRow
import uniffi.lakko_ffi_uniffi.AttachmentMenuDisabled
import uniffi.lakko_ffi_uniffi.AttachmentMenuItem
import uniffi.lakko_ffi_uniffi.AttachmentMenuRow

/** UX 14 — the catalog key of an attachment-menu row's label. */
internal fun attachmentMenuLabelKey(item: AttachmentMenuItem): String = when (item) {
    AttachmentMenuItem.PHOTO_OR_VIDEO -> "attach.menu.photo_video"
    AttachmentMenuItem.CAMERA -> "attach.menu.camera"
    AttachmentMenuItem.FILE -> "attach.menu.file"
    AttachmentMenuItem.AUDIO -> "attach.menu.audio"
    AttachmentMenuItem.POLL -> "attach.menu.poll"
    AttachmentMenuItem.CHECKLIST -> "attach.menu.checklist"
    AttachmentMenuItem.LOCATION -> "attach.menu.location"
    AttachmentMenuItem.CONTACT -> "attach.menu.contact"
    AttachmentMenuItem.ARTICLE -> "attach.menu.article"
}

/** UX 2.4 / 5 — the catalog key of a disabled row's reason. */
internal fun attachmentMenuDisabledKey(reason: AttachmentMenuDisabled): String = when (reason) {
    AttachmentMenuDisabled.NO_CAMERA -> "attach.menu.no_camera"
    AttachmentMenuDisabled.CAMERA_IN_CALL -> "attach.menu.camera_in_call"
    AttachmentMenuDisabled.NO_LOCATION -> "attach.menu.no_location"
}

/** The row's icon of the Lakko set (design system section 8). */
@androidx.annotation.DrawableRes
private fun attachmentMenuIcon(item: AttachmentMenuItem): Int = when (item) {
    AttachmentMenuItem.PHOTO_OR_VIDEO -> me.lakko.app.R.drawable.ic_lk_photo_stack
    AttachmentMenuItem.CAMERA -> me.lakko.app.R.drawable.ic_lk_camera
    AttachmentMenuItem.FILE -> me.lakko.app.R.drawable.ic_lk_doc
    AttachmentMenuItem.AUDIO -> me.lakko.app.R.drawable.ic_lk_music_note
    AttachmentMenuItem.POLL -> me.lakko.app.R.drawable.ic_lk_chart_bar
    AttachmentMenuItem.CHECKLIST -> me.lakko.app.R.drawable.ic_lk_checklist
    AttachmentMenuItem.LOCATION -> me.lakko.app.R.drawable.ic_lk_location
    AttachmentMenuItem.CONTACT -> me.lakko.app.R.drawable.ic_lk_person
    AttachmentMenuItem.ARTICLE -> me.lakko.app.R.drawable.ic_lk_doc_text
}

/**
 * UX 2 — the paperclip's one menu. The rows, their order and which are
 * disabled come from the core's `attachmentMenu` rule; this draws them with
 * the design system's `LakkoMenu` as one list of 48dp rows with no
 * separators (UX 2.1 / 2.2).
 */
@Composable
internal fun AttachmentMenuSheet(
    rows: List<AttachmentMenuRow>,
    onPick: (AttachmentMenuItem) -> Unit,
    onDismiss: () -> Unit,
) {
    LakkoMenu(
        rows = rows.map { row ->
            LakkoMenuRow(
                label = t(attachmentMenuLabelKey(row.item)),
                icon = attachmentMenuIcon(row.item),
                section = row.section.toInt(),
                disabledReason = row.disabled?.let { t(attachmentMenuDisabledKey(it)) },
                testTag = "attach-menu-${row.item.name.lowercase()}",
                onClick = { onPick(row.item) },
            )
        },
        onDismissRequest = onDismiss,
        rowHeight = 48.dp,
        separators = false,
    )
}
