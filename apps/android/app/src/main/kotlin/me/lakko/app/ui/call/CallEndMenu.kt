// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Logout
import androidx.compose.material.icons.filled.CallEnd
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import me.lakko.app.l10n.t
import me.lakko.app.ui.theme.LakkoDark

/**
 * What `End` opens for the person who started a group or link call while
 * someone else is in it (`docs/ux/calls.md` section 4.1.2): leave, or end the
 * call for every person in it. Whether it opens at all is `endOpensMenu`.
 *
 * A menu of the app's own above the control bar, in the pattern of
 * [CallParticipantsMenu]; never a system dialog. A tap anywhere else, the
 * `End` button under the scrim included, closes it and does nothing: the first
 * press never ends the call, and the menu is the only confirmation.
 *
 * @param bottomInset the height the control bar takes at the bottom of the
 *   screen, so the menu sits above it.
 * @param anchorCenterX the End button's horizontal centre in the call screen's
 *   coordinates, in pixels; `null` before it has been measured.
 * @param areaWidthPx the call screen's width in pixels.
 */
@Composable
internal fun BoxScope.CallEndMenu(
    onLeave: () -> Unit,
    onEndForEveryone: () -> Unit,
    onDismiss: () -> Unit,
    bottomInset: Dp,
    anchorCenterX: Float?,
    areaWidthPx: Int,
) {
    val menuLabel = t("call.end_menu.a11y")
    Box(
        modifier = Modifier
            .fillMaxSize()
            .clickable(
                indication = null,
                interactionSource = remember { MutableInteractionSource() },
                onClick = onDismiss,
            )
            .clearAndSetSemantics { },
    )
    Column(
        modifier = Modifier
            // Over the button that opened it; at the end of the row until
            // that button has been measured.
            .then(
                anchoredAboveControl(
                    anchorCenterX = anchorCenterX,
                    areaWidthPx = areaWidthPx,
                    bottomInset = bottomInset,
                    unmeasured = Alignment.BottomEnd,
                ),
            )
            .clip(RoundedCornerShape(14.dp))
            .background(LakkoDark.surface)
            .border(1.dp, LakkoDark.border, RoundedCornerShape(14.dp))
            .padding(vertical = 6.dp)
            .semantics { paneTitle = menuLabel },
    ) {
        CallMenuRow(
            icon = Icons.AutoMirrored.Filled.Logout,
            label = t("call.end_menu.leave"),
            onClick = onLeave,
        )
        CallMenuRow(
            icon = Icons.Filled.CallEnd,
            label = t("call.end_menu.end_for_all"),
            onClick = onEndForEveryone,
            color = LakkoDark.danger,
        )
    }
}
