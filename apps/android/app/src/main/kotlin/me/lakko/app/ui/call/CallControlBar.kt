// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
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
import androidx.compose.material.icons.filled.PersonAdd
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
import me.lakko.app.calls.CALL_CONTROL_BAR_PADDING_DP
import me.lakko.app.calls.CALL_CONTROL_GAP_DP
import me.lakko.app.calls.CallControl
import me.lakko.app.calls.CallControlBarFit
import me.lakko.app.calls.CallControlEmphasis
import me.lakko.app.calls.CallControlKind
import me.lakko.app.calls.fitCallControlBar
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

/** Sizes come from `fitCallControlBar` (`CallControls.kt`), which is what keeps
 *  the end button on the screen; these are the row's fixed metrics. */
private val CONTROL_GAP = CALL_CONTROL_GAP_DP.dp
private val BAR_PADDING = CALL_CONTROL_BAR_PADDING_DP.dp

/**
 * The floating bar.
 *
 * Measured against the width it is given, so that every control is on the
 * screen whatever the count: five controls at the spec sizes do not fit a
 * 411 dp phone, and before this the End button of a group call was clipped
 * past the right edge (see `fitCallControlBar`).
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
    BoxWithConstraints(modifier = modifier, contentAlignment = Alignment.Center) {
        val fit = fitCallControlBar(availableWidthDp = maxWidth.value, count = controls.size)
        Row(
            modifier = Modifier
                .clip(RoundedCornerShape(44.dp))
                // Translucent so the video underneath still reads through it, dark
                // so the white icons on top keep their contrast.
                .background(LakkoDark.background.copy(alpha = 0.62f))
                .padding(horizontal = BAR_PADDING, vertical = 12.dp),
            verticalAlignment = Alignment.Bottom,
            horizontalArrangement = Arrangement.spacedBy(CONTROL_GAP),
        ) {
            controls.forEachIndexed { index, control ->
                // "the end button is a 72 pt red circle set apart by twice the gap":
                // the extra gap is padding on the end column, not a child of the
                // row, so `spacedBy` does not add a third gap around it (which
                // is what squeezed the end button once the row was sized to fit).
                CallControlButton(
                    control = control,
                    fit = fit,
                    setApart = control.kind == CallControlKind.END && index > 0,
                    onClick = { onControl(control.kind) },
                )
            }
        }
    }
}

/** One circle with its word underneath. */
@Composable
private fun CallControlButton(
    control: CallControl,
    fit: CallControlBarFit,
    setApart: Boolean,
    onClick: () -> Unit,
) {
    val diameter = (if (control.kind == CallControlKind.END) fit.endDiameterDp else fit.diameterDp).dp
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        // One spoken label for the whole control, naming the action and the
        // state it will produce (section 4.1.1 + section 8). The label Text
        // below contributes nothing of its own, so TalkBack does not read
        // "Mute, Mute microphone".
        modifier = Modifier
            .padding(start = if (setApart) CONTROL_GAP else 0.dp)
            .width(fit.columnDp.dp)
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

/**
 * `Invite` — `docs/ux/call-links.md` section 3.
 *
 * One control, and it sits ABOVE the circle row rather than in it. The row's
 * width is already the thing `fitCallControlBar` exists to rescue (five
 * controls at the spec sizes do not fit a 411 dp phone), so a sixth circle
 * would shrink every control towards the minimum touch target on a narrow
 * phone and push the End button — the one control that must never be missing —
 * back over the edge. Putting it in the overflow menu instead was the other
 * option, and `CallControls.kt` names that exact move as one of the four faults
 * the control row was rebuilt to fix: this is a real action, reached in one
 * tap, most wanted in the first seconds of a call the person is alone in.
 *
 * What it reads is decided in [me.lakko.app.calls.callInviteButton]; this draws
 * the decision.
 */
@Composable
internal fun CallInviteControl(
    button: me.lakko.app.calls.CallInviteButton,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        button.note?.let { note ->
            Text(
                text = note,
                color = LakkoDark.textMuted,
                fontSize = 12.sp,
                modifier = Modifier
                    .clip(RoundedCornerShape(12.dp))
                    .background(LakkoDark.background.copy(alpha = 0.62f))
                    .padding(horizontal = 12.dp, vertical = 6.dp),
            )
            Spacer(Modifier.height(8.dp))
        }
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .clip(RoundedCornerShape(24.dp))
                .background(LakkoDark.background.copy(alpha = 0.62f))
                .alpha(if (button.enabled) 1f else 0.4f)
                .clickable(enabled = button.enabled, onClick = onClick)
                .padding(horizontal = 18.dp, vertical = 10.dp)
                .semantics(mergeDescendants = true) {
                    contentDescription = button.spokenLabel
                    role = Role.Button
                    if (!button.enabled) disabled()
                },
        ) {
            if (button.busy) {
                CircularProgressIndicator(
                    modifier = Modifier.size(16.dp),
                    color = Color.White,
                    strokeWidth = 2.dp,
                )
            } else {
                Icon(
                    imageVector = Icons.Filled.PersonAdd,
                    contentDescription = null,
                    tint = Color.White,
                    modifier = Modifier.size(18.dp),
                )
            }
            Spacer(Modifier.width(8.dp))
            Text(
                text = button.label,
                color = Color.White,
                fontSize = 14.sp,
                fontWeight = FontWeight.Medium,
            )
        }
    }
}
