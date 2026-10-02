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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Group
import androidx.compose.material.icons.filled.PersonAdd
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import me.lakko.app.calls.ParticipantsMenuItem
import me.lakko.app.l10n.t
import me.lakko.app.ui.theme.LakkoDark

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
 */
@Composable
internal fun BoxScope.CallParticipantsMenu(
    items: List<ParticipantsMenuItem>,
    onItem: (ParticipantsMenuItem) -> Unit,
    onDismiss: () -> Unit,
    bottomInset: Dp,
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
            .align(Alignment.BottomCenter)
            .padding(bottom = bottomInset + 8.dp, start = 12.dp, end = 12.dp)
            .width(IntrinsicSize.Max)
            .clip(RoundedCornerShape(14.dp))
            .background(LakkoDark.surface)
            .border(1.dp, LakkoDark.border, RoundedCornerShape(14.dp))
            .padding(vertical = 6.dp)
            .semantics { paneTitle = menuLabel },
    ) {
        for (item in items) {
            ParticipantsMenuRow(
                icon = when (item) {
                    ParticipantsMenuItem.PARTICIPANTS -> Icons.Filled.Group
                    ParticipantsMenuItem.COPY_LINK -> Icons.Filled.ContentCopy
                    ParticipantsMenuItem.INVITE_CONTACTS -> Icons.Filled.PersonAdd
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

@Composable
private fun ParticipantsMenuRow(icon: ImageVector, label: String, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 16.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Icon(icon, contentDescription = null, tint = LakkoDark.text, modifier = Modifier.size(20.dp))
        Text(label, color = LakkoDark.text, fontSize = 15.sp, maxLines = 1)
    }
}
