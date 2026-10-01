// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Groups
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.ui.chat.initialsOf
import me.lakko.app.ui.theme.avatarTintFor
import me.lakko.app.ui.theme.isLakkoDark

/**
 * Initials on one of four tints derived from [seed] (design system section 2).
 * A group without a name gets the group glyph instead of letters.
 */
@Composable
public fun LakkoAvatar(
    name: String,
    seed: String = name,
    size: Dp = 40.dp,
    group: Boolean = false,
    modifier: Modifier = Modifier,
) {
    val tint = avatarTintFor(seed, isLakkoDark())
    Box(
        contentAlignment = Alignment.Center,
        modifier = modifier
            .size(size)
            .clip(CircleShape)
            .background(tint.background),
    ) {
        // The unnamed-group stand-in is rendered in the interface language
        // (`chat.group.fallback_name`), so the glyph keys on that, not on
        // the English word.
        if (group && name == LocalStrings.current["chat.group.fallback_name"]) {
            Icon(
                Icons.Filled.Groups,
                contentDescription = null,
                tint = tint.foreground,
                modifier = Modifier.size(size * 0.55f),
            )
        } else {
            Text(
                text = initialsOf(name),
                color = tint.foreground,
                fontWeight = FontWeight.SemiBold,
                fontSize = (size.value * 0.4f).sp,
                style = MaterialTheme.typography.labelLarge,
            )
        }
    }
}
