// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.feed

import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Bolt
import androidx.compose.material.icons.filled.Book
import androidx.compose.material.icons.filled.ChatBubble
import androidx.compose.material.icons.filled.Code
import androidx.compose.material.icons.filled.DynamicFeed
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.Flag
import androidx.compose.material.icons.filled.MusicNote
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.PhotoCamera
import androidx.compose.material.icons.filled.Public
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import me.lakko.app.feed.FeedIcon
import me.lakko.app.feed.FeedPresetKey

/** The Material symbol for each of the twelve feed icons (UX spec section 4.1). */
internal fun FeedIcon.vector(): ImageVector = when (this) {
    FeedIcon.FEED -> Icons.Filled.DynamicFeed
    FeedIcon.STAR -> Icons.Filled.Star
    FeedIcon.HEART -> Icons.Filled.Favorite
    FeedIcon.LIGHTNING -> Icons.Filled.Bolt
    FeedIcon.BOOK -> Icons.Filled.Book
    FeedIcon.CODE -> Icons.Filled.Code
    FeedIcon.MUSIC -> Icons.Filled.MusicNote
    FeedIcon.CAMERA -> Icons.Filled.PhotoCamera
    FeedIcon.GLOBE -> Icons.Filled.Public
    FeedIcon.SPEECH_BUBBLE -> Icons.Filled.ChatBubble
    FeedIcon.BELL -> Icons.Filled.Notifications
    FeedIcon.FLAG -> Icons.Filled.Flag
}

/** The presets' fixed icons, not editable. */
internal fun FeedPresetKey.vector(): ImageVector = when (this) {
    FeedPresetKey.LATEST -> FeedIcon.FEED.vector()
    FeedPresetKey.POPULAR -> FeedIcon.LIGHTNING.vector()
    FeedPresetKey.FOR_ME -> Icons.Filled.Person
}

/** Discover's fixed icon: the globe. */
internal val DiscoverIcon: ImageVector = FeedIcon.GLOBE.vector()

/** The editor's icon grid: twelve choices under the name, two rows of six. */
@Composable
internal fun FeedIconPicker(selected: UByte, onSelect: (UByte) -> Unit, modifier: Modifier = Modifier) {
    val current = FeedIcon.of(selected)
    Column(modifier = modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        for (row in FeedIcon.entries.chunked(6)) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                for (icon in row) {
                    val isSelected = icon == current
                    Box(
                        contentAlignment = Alignment.Center,
                        modifier = Modifier
                            .size(44.dp)
                            .clip(RoundedCornerShape(10.dp))
                            .border(
                                width = if (isSelected) 2.dp else 1.dp,
                                color = if (isSelected) {
                                    MaterialTheme.colorScheme.primary
                                } else {
                                    MaterialTheme.colorScheme.outlineVariant
                                },
                                shape = RoundedCornerShape(10.dp),
                            )
                            .clickable(role = Role.RadioButton) { onSelect(icon.index) }
                            .semantics {
                                contentDescription = icon.label
                                this.selected = isSelected
                            },
                    ) {
                        Icon(
                            icon.vector(),
                            contentDescription = null,
                            tint = if (isSelected) {
                                MaterialTheme.colorScheme.primary
                            } else {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            },
                        )
                    }
                }
            }
        }
    }
}
