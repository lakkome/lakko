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
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Chat
import androidx.compose.material.icons.filled.CallEnd
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material.icons.filled.Group
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material.icons.filled.VideocamOff
import androidx.compose.material.icons.filled.Settings
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
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.coerceIn
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import me.lakko.app.l10n.text
import me.lakko.app.calls.CALL_CONTROL_BAR_PADDING_DP
import me.lakko.app.calls.CALL_CONTROL_GAP_DP
import me.lakko.app.calls.CallControl
import me.lakko.app.calls.CallControlEmphasis
import me.lakko.app.calls.CallControlKind
import me.lakko.app.calls.fitCallControlBar
import me.lakko.app.calls.fitGroupControlBar
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
 * side, floating over whatever the stage is showing. A group call's
 * Participants circle holds the participant list and, where the call can hand
 * out a link to itself, the invite items; `Invite` was a pill above the row
 * before.
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
 * A group call's row ([group]) is sized by `fitGroupControlBar` instead: one
 * line always, and on a phone no words under the circles (each keeps its
 * spoken name) with the circles sized from the width and the number of
 * controls. A row that wrapped into two lines of labelled circles took half a
 * phone screen (found live 2026-10-02).
 *
 * @param onControl invoked with the kind that was pressed. A disabled control
 *   never invokes it, and is drawn at 40 percent rather than hidden, so the
 *   user can still see where the button will be.
 * @param screenHeightDp the call screen's height, which with the width decides
 *   whether a group row is the compact one.
 * @param onButtonCenterX invoked with each circle's horizontal centre in the
 *   window root's coordinates whenever the circle is laid out, so a menu that
 *   opens from a control can sit above it (`anchoredMenuLeft`).
 */
@Composable
internal fun CallControlBar(
    controls: List<CallControl>,
    onControl: (CallControlKind) -> Unit,
    modifier: Modifier = Modifier,
    group: Boolean = false,
    screenHeightDp: Float = Float.MAX_VALUE,
    onButtonCenterX: (CallControlKind, Float) -> Unit = { _, _ -> },
) {
    BoxWithConstraints(modifier = modifier, contentAlignment = Alignment.Center) {
        if (group) {
            val fit = fitGroupControlBar(
                availableWidthDp = maxWidth.value,
                screenHeightDp = screenHeightDp,
                count = controls.size,
            )
            Row(
                modifier = Modifier
                    .clip(RoundedCornerShape(if (fit.showLabels) 44.dp else 28.dp))
                    .background(LakkoDark.background.copy(alpha = 0.62f))
                    .padding(horizontal = fit.paddingHorizontalDp.dp, vertical = fit.paddingVerticalDp.dp),
                verticalAlignment = if (fit.showLabels) Alignment.Bottom else Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(fit.gapDp.dp),
            ) {
                controls.forEachIndexed { index, control ->
                    val end = control.kind == CallControlKind.END
                    CallControlButton(
                        control = control,
                        diameter = (if (end) fit.endDiameterDp else fit.diameterDp).dp,
                        columnWidth = null,
                        showLabel = fit.showLabels,
                        extraStart = if (end && index > 0) fit.gapDp.dp else 0.dp,
                        onClick = { onControl(control.kind) },
                        onCenterX = { onButtonCenterX(control.kind, it) },
                    )
                }
            }
        } else {
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
                    val end = control.kind == CallControlKind.END
                    CallControlButton(
                        control = control,
                        diameter = (if (end) fit.endDiameterDp else fit.diameterDp).dp,
                        columnWidth = fit.columnDp.dp,
                        showLabel = true,
                        extraStart = if (end && index > 0) CONTROL_GAP else 0.dp,
                        onClick = { onControl(control.kind) },
                        onCenterX = { onButtonCenterX(control.kind, it) },
                    )
                }
            }
        }
    }
}

/** One circle, with its word underneath when the row shows words. */
@Composable
private fun CallControlButton(
    control: CallControl,
    diameter: Dp,
    /** The width of the column the circle sits in; `null` hugs the circle. */
    columnWidth: Dp?,
    showLabel: Boolean,
    /** The end button's extra distance from the control before it. */
    extraStart: Dp,
    onClick: () -> Unit,
    /** The circle's horizontal centre in the window root's coordinates. */
    onCenterX: (Float) -> Unit,
) {
    val spoken = control.spokenLabel.text()
    // The glyph keeps its share of the circle when the circle shrinks.
    val iconSize = (diameter * 0.42f).coerceIn(18.dp, 26.dp)
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        // One spoken label for the whole control, naming the action and the
        // state it will produce (section 4.1.1 + section 8). The label Text
        // below contributes nothing of its own, so TalkBack does not read
        // "Mute, Mute microphone".
        modifier = Modifier
            .padding(start = extraStart)
            .then(if (columnWidth != null) Modifier.width(columnWidth) else Modifier)
            .alpha(if (control.enabled || control.busy) 1f else DISABLED_ALPHA)
            .semantics(mergeDescendants = true) {
                contentDescription = spoken
                role = Role.Button
                if (!control.enabled) disabled()
            },
    ) {
        Box {
        Box(
            modifier = Modifier
                .size(diameter)
                .onGloballyPositioned { onCenterX(it.boundsInRoot().center.x) }
                .clip(CircleShape)
                .background(control.emphasis.fill())
                .clickable(enabled = control.enabled, onClick = onClick),
            contentAlignment = Alignment.Center,
        ) {
            if (control.busy) {
                CircularProgressIndicator(
                    modifier = Modifier.size(iconSize - 4.dp),
                    color = control.emphasis.foreground(),
                    strokeWidth = 2.dp,
                )
            } else {
                Icon(
                    imageVector = control.icon(),
                    contentDescription = null,
                    tint = control.emphasis.foreground(),
                    modifier = Modifier.size(iconSize),
                )
            }
        }
        // The design system's badge: the chat's unread count (section
        // 4.1.4). Spoken as part of the circle's name, so silent itself.
        control.badge?.let { badge ->
            Box(
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .offset(x = 4.dp, y = (-4).dp)
                    .heightIn(min = 18.dp)
                    .widthIn(min = 18.dp)
                    .clip(RoundedCornerShape(50))
                    .background(LakkoDark.accent)
                    .padding(horizontal = 5.dp)
                    .clearAndSetSemantics { },
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = badge,
                    color = LakkoDark.accentFg,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                    maxLines = 1,
                )
            }
        }
        }
        if (showLabel) {
            Spacer(Modifier.height(6.dp))
            Text(
                text = control.label.text(),
                color = LakkoDark.textMuted,
                fontSize = 12.sp,
                fontWeight = FontWeight.Medium,
                maxLines = 1,
                modifier = Modifier.clearAndSetSemantics { },
            )
        }
    }
}

/**
 * The slashed icon is the off state's, so the glyph and the fill say the same
 * thing (section 4.1.1: "solid white with a dark slashed icon").
 *
 * `internal` rather than file-private because the collapsed call bar
 * (`docs/ux/calls.md` section 4.5) draws the same mute button and must draw the
 * same glyph for it; a second copy of this mapping is a second thing that can
 * come to disagree about which state is the slashed one.
 */
internal fun CallControl.icon(): ImageVector = when (kind) {
    CallControlKind.MICROPHONE ->
        if (emphasis == CallControlEmphasis.SOLID) Icons.Filled.MicOff else Icons.Filled.Mic
    CallControlKind.CAMERA ->
        if (emphasis == CallControlEmphasis.SOLID) Icons.Filled.VideocamOff else Icons.Filled.Videocam
    CallControlKind.PARTICIPANTS -> Icons.Filled.Group
    CallControlKind.CHAT -> Icons.AutoMirrored.Filled.Chat
    CallControlKind.SETTINGS -> Icons.Filled.Settings
    CallControlKind.END -> Icons.Filled.CallEnd
}

internal fun CallControlEmphasis.fill(): Color = when (this) {
    // "a translucent white fill at about 12 percent with a white icon"
    CallControlEmphasis.OUTLINE -> LakkoDark.text.copy(alpha = 0.12f)
    CallControlEmphasis.SOLID -> LakkoDark.text
    CallControlEmphasis.DANGER -> LakkoDark.danger
}

internal fun CallControlEmphasis.foreground(): Color = when (this) {
    CallControlEmphasis.OUTLINE -> LakkoDark.text
    CallControlEmphasis.SOLID -> LakkoDark.background
    CallControlEmphasis.DANGER -> LakkoDark.dangerFg
}

/** Section 4.1.1: "Disabled is 40 percent opacity, never a hidden button." */
private const val DISABLED_ALPHA = 0.4f
