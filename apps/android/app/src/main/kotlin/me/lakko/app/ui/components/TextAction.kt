// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.PressInteraction
import androidx.compose.foundation.layout.Box
import androidx.compose.material3.ripple
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * The smallest touch target any Lakko action gets on Android: Material's
 * minimum interactive size (48dp), the same size `TextButton` and
 * `IconButton` already reserve.
 */
public val MinTouchTarget: Dp = 48.dp

/**
 * A text that is an action, drawn exactly as [content] draws it, whose touch
 * area is at least [MinTouchTarget] in each direction.
 *
 * Use it for an action that must keep the look of plain text inside a line
 * or a bubble (a `Retry` under a message, a word of the translation line, a
 * name that opens its editor). A stand-alone action that may take a
 * button's own padding stays a material3 `TextButton`, which already
 * reserves the 48dp target and carries the button role.
 *
 * Layout is unchanged: the node measures and reports the size of [content].
 * The extra area is a sibling placed behind it, extending past its bounds
 * by up to half of what [content] lacks on each side, so it may overlap a
 * neighbour's margin but never draws. A press anywhere in the area shows
 * the press state on [content] and runs [onClick].
 *
 * Accessibility: [content] is one merged node with [role] (a button by
 * default), [onClickLabel] and the disabled state; the extra area carries
 * no semantics, so TalkBack reads one control.
 */
@Composable
public fun TextAction(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    onClickLabel: String? = null,
    role: Role = Role.Button,
    content: @Composable () -> Unit,
) {
    val interaction = remember { MutableInteractionSource() }
    val currentOnClick = rememberUpdatedState(onClick)
    Layout(
        modifier = modifier,
        content = {
            // Child 0: the area outside [content], behind it.
            Box(
                Modifier.pointerInput(enabled) {
                    if (!enabled) return@pointerInput
                    detectTapGestures(
                        onPress = { offset ->
                            val press = PressInteraction.Press(offset)
                            interaction.emit(press)
                            val released = tryAwaitRelease()
                            interaction.emit(
                                if (released) PressInteraction.Release(press) else PressInteraction.Cancel(press),
                            )
                        },
                        onTap = { currentOnClick.value() },
                    )
                },
            )
            // Child 1: what the person sees, clickable on its own bounds.
            Box(
                Modifier.clickable(
                    interactionSource = interaction,
                    indication = ripple(),
                    enabled = enabled,
                    onClickLabel = onClickLabel,
                    role = role,
                    onClick = onClick,
                ),
            ) { content() }
        },
    ) { measurables, constraints ->
        val shown = measurables[1].measure(constraints)
        val min = MinTouchTarget.roundToPx()
        val extraX = ((min - shown.width).coerceAtLeast(0) + 1) / 2
        val extraY = ((min - shown.height).coerceAtLeast(0) + 1) / 2
        val area = measurables[0].measure(
            Constraints.fixed(shown.width + 2 * extraX, shown.height + 2 * extraY),
        )
        layout(shown.width, shown.height) {
            area.place(-extraX, -extraY)
            shown.place(0, 0)
        }
    }
}
