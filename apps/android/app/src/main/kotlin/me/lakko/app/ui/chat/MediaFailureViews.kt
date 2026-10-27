// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import me.lakko.app.l10n.t
import me.lakko.app.media.MediaFailure
import me.lakko.app.media.mediaFailureCopy

/**
 * docs/ux/saved-messages-and-file-lifetime.md 4.1 / 4.2 — the line a media
 * bubble shows in place of its media when it did not open: the two "no
 * longer available" lines (no Retry), the opening limit, or [generic] (the
 * bubble's own failure line). [onMedia] draws it over a dark picture.
 */
@Composable
internal fun MediaFailureLabel(
    failure: MediaFailure?,
    generic: String,
    modifier: Modifier = Modifier,
    onMedia: Boolean = true,
) {
    val copy = failure?.let(::mediaFailureCopy)
    val fg = if (onMedia) Color.White else MaterialTheme.colorScheme.error
    val secondary = if (onMedia) Color.White.copy(alpha = 0.8f) else MaterialTheme.colorScheme.onSurfaceVariant
    Column(
        modifier = modifier
            .widthIn(max = 260.dp)
            .then(
                if (onMedia) {
                    Modifier.clip(RoundedCornerShape(12.dp)).background(Color.Black.copy(alpha = 0.55f))
                } else {
                    Modifier
                },
            )
            .padding(horizontal = 10.dp, vertical = 6.dp),
    ) {
        Text(
            copy?.title?.let { t(it) } ?: generic,
            style = MaterialTheme.typography.labelMedium,
            color = if (failure == MediaFailure.GONE && !onMedia) MaterialTheme.colorScheme.onSurface else fg,
            textAlign = if (onMedia) TextAlign.Center else TextAlign.Start,
        )
        copy?.body?.let {
            Text(
                t(it),
                style = MaterialTheme.typography.labelSmall,
                color = secondary,
                textAlign = if (onMedia) TextAlign.Center else TextAlign.Start,
            )
        }
    }
}

/** Whether a failed bubble offers Try again: everything except gone. */
internal fun mediaRetryOffered(failure: MediaFailure?): Boolean = failure != MediaFailure.GONE
