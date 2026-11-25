// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import androidx.compose.runtime.Composable
import me.lakko.app.R
import me.lakko.app.l10n.t

/**
 * The photo menu of a profile, a group or a channel: the app's own action
 * sheet (design system: Action sheet). Choose from library, Take photo when
 * the device has a camera, Remove (danger) when there is a photo.
 */
@Composable
internal fun PhotoMenu(
    hasCamera: Boolean,
    canRemove: Boolean,
    onDismiss: () -> Unit,
    onLibrary: () -> Unit,
    onCamera: () -> Unit,
    onRemove: () -> Unit,
) {
    val rows = buildList {
        add(LakkoMenuRow(t("profile.avatar.menu.library"), R.drawable.ic_lk_photo_stack) { onLibrary() })
        if (hasCamera) {
            add(LakkoMenuRow(t("profile.avatar.menu.camera"), R.drawable.ic_lk_camera) { onCamera() })
        }
        if (canRemove) {
            add(LakkoMenuRow(t("profile.avatar.menu.remove"), R.drawable.ic_lk_trash, danger = true) { onRemove() })
        }
    }
    LakkoMenu(rows = rows, onDismissRequest = onDismiss, separators = false)
}
