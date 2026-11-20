// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import androidx.annotation.DrawableRes
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Icon
import androidx.compose.material3.LocalContentColor
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * Box sizes of the Lakko icon set (`.claude/skills/ux-ui/design-system.md`,
 * section "Icons"). The drawing and its 1.75 stroke never change per call
 * site; only the box does.
 */
object LkIconSize {
    /** Marks inside a list row or bubble (muted, pinned, sent / read). */
    val Mark: Dp = 14.dp

    /** Settings and menu rows, composer buttons, inline buttons. */
    val Row: Dp = 20.dp

    /** Navigation and toolbar buttons, tab bar. */
    val Toolbar: Dp = 24.dp

    /** Call control buttons. */
    val Call: Dp = 28.dp

    /** Empty states, large prompts. */
    val Empty: Dp = 48.dp
}

/**
 * One icon of the Lakko set (`R.drawable.ic_lk_<name>`), drawn in a box of
 * [size] and tinted with [tint] (the current content colour by default, as a
 * Material `Icon` is). Mirroring in right-to-left comes from the drawable's
 * `android:autoMirrored`, never from the call site.
 */
@Composable
fun LkIcon(
    @DrawableRes icon: Int,
    contentDescription: String?,
    modifier: Modifier = Modifier,
    size: Dp = LkIconSize.Toolbar,
    tint: Color = LocalContentColor.current,
) {
    Icon(
        painter = painterResource(icon),
        contentDescription = contentDescription,
        modifier = modifier.size(size),
        tint = tint,
    )
}
