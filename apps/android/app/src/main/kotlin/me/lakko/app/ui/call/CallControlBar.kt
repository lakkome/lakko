// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CallEnd
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material.icons.filled.MoreHoriz
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material.icons.filled.VideocamOff
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import me.lakko.app.calls.CallControl
import me.lakko.app.calls.CallControlEmphasis
import me.lakko.app.calls.CallControlKind
import me.lakko.app.ui.theme.LakkoDark

/**
 * The call's control row — `docs/ux/calls.md` sections 4.1 and 4.1.1. Which
 * buttons exist and how each one reads is decided in
 * [me.lakko.app.calls.callControlRow]; this file only draws the decision.
 *
 * ## What changed
 *
 * The old row was a bare `Row` with a `LakkoDark.sunken` background stretched
 * edge to edge along the bottom of the window — "glued to the bottom edge of a
 * mostly empty screen, unanchored to anything" (section 4.1). Section 4.2 found
 * the same thing by comparison: "WhatsApp, Telegram and FaceTime all float them
 * in one rounded container over the content, not loose on the background. That
 * container is what our screen was missing."
 *
 * So the row now lives in one rounded translucent bar with a margin on every
 * side, floating over whatever the stage is showing.
 */

/** Diameter of every control but End (section 4.1.1). */
private val CONTROL_DIAMETER = 64.dp

/** The End button is larger, and the only red thing on the screen. */
private val END_DIAMETER = 72.dp

/** Gap between controls; End is set apart by twice this. */
private val CONTROL_GAP = 12.dp

/**
 * The floating bar.
 *
 * @param onControl invoked with the kind that was pressed. A disabled control
 *   never invokes it, and is drawn at 40 percent rather than hidden, so the
 *   user can still see where the button will be.
 */
@Composable
internal fun CallControlBar(
    controls: List<CallControl>,
    onControl: (CallControlKind) -> Unit,
    modifier: Modifier = Modifier,
) {
    Row(
        modifier = modifier
            .clip(RoundedCornerShape(44.dp))
            // Translucent so the video underneath still reads through it, dark
            // so the white icons on top keep their contrast.
            .background(LakkoDark.background.copy(alpha = 0.62f))
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.Bottom,
        horizontalArrangement = Arrangement.spacedBy(CONTROL_GAP),
    ) {
        controls.forEachIndexed { index, control ->
            // "the end button is a 72 pt red circle set apart by twice the gap"
            if (control.kind == CallControlKind.END && index > 0) {
                Spacer(Modifier.width(CONTROL_GAP))
            }
            CallControlButton(control = control, onClick = { onControl(control.kind) })
        }
    }
}

/** One circle with its word underneath. */
@Composable
private fun CallControlButton(control: CallControl, onClick: () -> Unit) {
    val diameter = if (control.kind == CallControlKind.END) END_DIAMETER else CONTROL_DIAMETER
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        // One spoken label for the whole control, naming the action and the
        // state it will produce (section 4.1.1 + section 8). The label Text
        // below contributes nothing of its own, so TalkBack does not read
        // "Mute, Mute microphone".
        modifier = Modifier
            .width(END_DIAMETER)
            .alpha(if (control.enabled) 1f else DISABLED_ALPHA)
            .semantics(mergeDescendants = true) {
                contentDescription = control.spokenLabel
                role = Role.Button
                if (!control.enabled) disabled()
            },
    ) {
        Box(
            modifier = Modifier
                .size(diameter)
                .clip(CircleShape)
                .background(control.emphasis.fill())
                .clickable(enabled = control.enabled, onClick = onClick),
            contentAlignment = Alignment.Center,
        ) {
            if (control.busy) {
                CircularProgressIndicator(
                    modifier = Modifier.size(22.dp),
                    color = control.emphasis.foreground(),
                    strokeWidth = 2.dp,
                )
            } else {
                Icon(
                    imageVector = control.icon(),
                    contentDescription = null,
                    tint = control.emphasis.foreground(),
                    modifier = Modifier.size(26.dp),
                )
            }
        }
        Spacer(Modifier.height(6.dp))
        Text(
            text = control.label,
            color = LakkoDark.textMuted,
            fontSize = 12.sp,
            fontWeight = FontWeight.Medium,
            modifier = Modifier.clearAndSetSemantics { },
        )
    }
}

/**
 * The slashed icon is the off state's, so the glyph and the fill say the same
 * thing (section 4.1.1: "solid white with a dark slashed icon"). Speaker has no
 * slashed variant in the spec's icon list — its fill alone carries the state.
 */
private fun CallControl.icon(): ImageVector = when (kind) {
    CallControlKind.SPEAKER -> Icons.AutoMirrored.Filled.VolumeUp
    CallControlKind.MICROPHONE ->
        if (emphasis == CallControlEmphasis.SOLID) Icons.Filled.MicOff else Icons.Filled.Mic
    CallControlKind.CAMERA ->
        if (emphasis == CallControlEmphasis.SOLID) Icons.Filled.VideocamOff else Icons.Filled.Videocam
    CallControlKind.MORE -> Icons.Filled.MoreHoriz
    CallControlKind.END -> Icons.Filled.CallEnd
}

private fun CallControlEmphasis.fill(): Color = when (this) {
    // "a translucent white fill at about 12 percent with a white icon"
    CallControlEmphasis.OUTLINE -> LakkoDark.text.copy(alpha = 0.12f)
    CallControlEmphasis.SOLID -> LakkoDark.text
    CallControlEmphasis.DANGER -> LakkoDark.danger
}

private fun CallControlEmphasis.foreground(): Color = when (this) {
    CallControlEmphasis.OUTLINE -> LakkoDark.text
    CallControlEmphasis.SOLID -> LakkoDark.background
    CallControlEmphasis.DANGER -> LakkoDark.dangerFg
}

/** Section 4.1.1: "Disabled is 40 percent opacity, never a hidden button." */
private const val DISABLED_ALPHA = 0.4f
