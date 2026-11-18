// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Article
import androidx.compose.material.icons.automirrored.filled.InsertDriveFile
import androidx.compose.material.icons.filled.BarChart
import androidx.compose.material.icons.filled.Checklist
import androidx.compose.material.icons.filled.MusicNote
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.PhotoCamera
import androidx.compose.material.icons.filled.PhotoLibrary
import androidx.compose.material.icons.filled.Place
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.vector.ImageVector
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

private fun attachmentMenuIcon(item: AttachmentMenuItem): ImageVector = when (item) {
    AttachmentMenuItem.PHOTO_OR_VIDEO -> Icons.Filled.PhotoLibrary
    AttachmentMenuItem.CAMERA -> Icons.Filled.PhotoCamera
    AttachmentMenuItem.FILE -> Icons.AutoMirrored.Filled.InsertDriveFile
    AttachmentMenuItem.AUDIO -> Icons.Filled.MusicNote
    AttachmentMenuItem.POLL -> Icons.Filled.BarChart
    AttachmentMenuItem.CHECKLIST -> Icons.Filled.Checklist
    AttachmentMenuItem.LOCATION -> Icons.Filled.Place
    AttachmentMenuItem.CONTACT -> Icons.Filled.Person
    AttachmentMenuItem.ARTICLE -> Icons.AutoMirrored.Filled.Article
}

/**
 * UX 2 — the paperclip's one menu. The rows, their order, their sections and
 * which are disabled come from the core's `attachmentMenu` rule; this draws
 * them with the design system's `LakkoMenu`.
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
    )
}
