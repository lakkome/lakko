// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlin.math.roundToInt
import me.lakko.app.calls.CALL_MENU_EDGE_GAP_DP
import me.lakko.app.calls.ParticipantsMenuItem
import me.lakko.app.calls.anchoredMenuLeft
import me.lakko.app.l10n.t
import me.lakko.app.ui.theme.LakkoDark
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/**
 * What Participants opens on a call that can hand out a link to itself: a menu of
 * the app's own, above the control bar (`docs/ux/call-links.md` section 3).
 * Which items it holds is decided by `participantsMenuItems`; this draws them.
 *
 * Never the system's share sheet. The first version handed the link to an
 * `ACTION_SEND` chooser: a foreign surface in the middle of a call, different
 * on every platform, offering a dozen destinations where two are wanted
 * (found live 2026-10-02). So this is drawn by the call screen itself, in the
 * call's own colours.
 *
 * A tap anywhere else closes it.
 *
 * @param bottomInset the height the control bar takes at the bottom of the
 *   screen, so the menu sits above it.
 * @param anchorCenterX the Participants button's horizontal centre in the call
 *   screen's coordinates, in pixels; `null` before it has been measured.
 * @param areaWidthPx the call screen's width in pixels.
 */
@Composable
internal fun BoxScope.CallParticipantsMenu(
    items: List<ParticipantsMenuItem>,
    onItem: (ParticipantsMenuItem) -> Unit,
    onDismiss: () -> Unit,
    bottomInset: Dp,
    anchorCenterX: Float?,
    areaWidthPx: Int,
) {
    val menuLabel = t("call.invite.menu_a11y")
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
            .then(
                anchoredAboveControl(
                    anchorCenterX = anchorCenterX,
                    areaWidthPx = areaWidthPx,
                    bottomInset = bottomInset,
                    unmeasured = Alignment.BottomCenter,
                ),
            )
            .clip(RoundedCornerShape(14.dp))
            .background(LakkoDark.surface)
            .border(1.dp, LakkoDark.border, RoundedCornerShape(14.dp))
            .padding(vertical = 6.dp)
            .semantics { paneTitle = menuLabel },
    ) {
        for (item in items) {
            CallMenuRow(
                icon = when (item) {
                    ParticipantsMenuItem.PARTICIPANTS -> R.drawable.ic_lk_person_2
                    ParticipantsMenuItem.COPY_LINK -> R.drawable.ic_lk_copy
                    ParticipantsMenuItem.INVITE_CONTACTS -> R.drawable.ic_lk_person_plus
                },
                label = t(
                    when (item) {
                        ParticipantsMenuItem.PARTICIPANTS -> "call.control.participants"
                        ParticipantsMenuItem.COPY_LINK -> "call.invite.menu_copy"
                        ParticipantsMenuItem.INVITE_CONTACTS -> "call.invite.menu_contact"
                    },
                ),
                onClick = { onItem(item) },
            )
        }
    }
}

/**
 * Places a call-screen menu directly above the bar button that opened it,
 * centred on the button and slid inward to keep [CALL_MENU_EDGE_GAP_DP] from
 * the call screen's sides (`anchoredMenuLeft`, the web client's
 * `anchorToControl`). The menu's own width is measured here; until both the
 * button and the menu have been measured it sits as it did before, aligned to
 * [unmeasured], rather than at the left edge. Vertically it stays just above
 * the bar.
 */
@Composable
internal fun BoxScope.anchoredAboveControl(
    anchorCenterX: Float?,
    areaWidthPx: Int,
    bottomInset: Dp,
    unmeasured: Alignment,
): Modifier {
    var menuWidthPx by remember { mutableIntStateOf(0) }
    val density = LocalDensity.current
    val gap = CALL_MENU_EDGE_GAP_DP.dp
    val placed = if (anchorCenterX != null && menuWidthPx > 0 && areaWidthPx > 0) {
        val gapPx = with(density) { gap.toPx() }
        val areaWidth = with(density) { areaWidthPx.toDp() }
        Modifier
            .align(Alignment.BottomStart)
            .offset {
                IntOffset(
                    anchoredMenuLeft(
                        buttonCenterX = anchorCenterX,
                        menuWidth = menuWidthPx.toFloat(),
                        areaWidth = areaWidthPx.toFloat(),
                        edgeGap = gapPx,
                    ).roundToInt(),
                    0,
                )
            }
            .padding(bottom = bottomInset + 8.dp)
            .widthIn(max = (areaWidth - gap * 2).coerceAtLeast(0.dp))
    } else {
        Modifier
            .align(unmeasured)
            .padding(bottom = bottomInset + 8.dp, start = gap, end = gap)
    }
    return placed
        .width(IntrinsicSize.Max)
        .onSizeChanged { menuWidthPx = it.width }
}

/** One row of a call-screen menu. [color] is the icon's and the words'. */
@Composable
internal fun CallMenuRow(
    @androidx.annotation.DrawableRes icon: Int,
    label: String,
    onClick: () -> Unit,
    color: Color = LakkoDark.text,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 16.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        LkIcon(icon, contentDescription = null, tint = color, size = LkIconSize.Row)
        Text(label, color = color, fontSize = 15.sp, maxLines = 1)
    }
}
