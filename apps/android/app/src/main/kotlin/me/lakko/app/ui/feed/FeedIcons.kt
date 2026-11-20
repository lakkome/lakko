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
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import me.lakko.app.feed.FeedIcon
import me.lakko.app.feed.FeedPresetKey
import me.lakko.app.l10n.t
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon

/** The Lakko icon for each of the twelve feed icons (UX spec section 4.1). */
@androidx.annotation.DrawableRes
internal fun FeedIcon.drawable(): Int = when (this) {
    FeedIcon.FEED -> R.drawable.ic_lk_feed
    FeedIcon.STAR -> R.drawable.ic_lk_star
    FeedIcon.HEART -> R.drawable.ic_lk_heart
    FeedIcon.LIGHTNING -> R.drawable.ic_lk_bolt
    FeedIcon.BOOK -> R.drawable.ic_lk_book
    FeedIcon.CODE -> R.drawable.ic_lk_code
    FeedIcon.MUSIC -> R.drawable.ic_lk_music_note
    FeedIcon.CAMERA -> R.drawable.ic_lk_camera
    FeedIcon.GLOBE -> R.drawable.ic_lk_globe
    FeedIcon.SPEECH_BUBBLE -> R.drawable.ic_lk_chat
    FeedIcon.BELL -> R.drawable.ic_lk_bell
    FeedIcon.FLAG -> R.drawable.ic_lk_flag
}

/** The presets' fixed icons, not editable. */
@androidx.annotation.DrawableRes
internal fun FeedPresetKey.drawable(): Int = when (this) {
    FeedPresetKey.LATEST -> FeedIcon.FEED.drawable()
    FeedPresetKey.POPULAR -> FeedIcon.LIGHTNING.drawable()
    FeedPresetKey.FOR_ME -> R.drawable.ic_lk_person
}

/** Discover's fixed icon: the globe. */
@androidx.annotation.DrawableRes
internal val DiscoverIcon: Int = FeedIcon.GLOBE.drawable()

/** The editor's icon grid: twelve choices under the name, two rows of six. */
@Composable
internal fun FeedIconPicker(selected: UByte, onSelect: (UByte) -> Unit, modifier: Modifier = Modifier) {
    val current = FeedIcon.of(selected)
    Column(modifier = modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        for (row in FeedIcon.entries.chunked(6)) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                for (icon in row) {
                    val isSelected = icon == current
                    val iconLabel = t(icon.labelKey)
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
                                contentDescription = iconLabel
                                this.selected = isSelected
                            },
                    ) {
                        LkIcon(
                            icon.drawable(),
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
