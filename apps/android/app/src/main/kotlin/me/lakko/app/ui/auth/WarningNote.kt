// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize
import me.lakko.app.ui.theme.LakkoDark
import me.lakko.app.ui.theme.LakkoLight
import me.lakko.app.ui.theme.isLakkoDark

/** The warning colour token of the active scheme (not the danger colour). */
@Composable
public fun lakkoWarningColor(): Color = if (isLakkoDark()) LakkoDark.warning else LakkoLight.warning

/**
 * A note in the warning colour with the warning icon (UX 19.1a, 19.1c,
 * 19.1d, 15.1c): something the person should know, not a failure. Never
 * colour alone: the icon stands beside the text. Announced once by a screen
 * reader when it appears. [actions] are ordinary buttons in reading order.
 */
@Composable
public fun WarningNote(
    body: String,
    modifier: Modifier = Modifier,
    title: String? = null,
    extra: String? = null,
    actions: (@Composable () -> Unit)? = null,
) {
    val colour = lakkoWarningColor()
    Surface(
        color = MaterialTheme.colorScheme.surface,
        shape = MaterialTheme.shapes.medium,
        border = androidx.compose.foundation.BorderStroke(1.dp, colour),
        modifier = modifier
            .fillMaxWidth()
            .semantics { liveRegion = LiveRegionMode.Polite },
    ) {
        Row(
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            modifier = Modifier.padding(12.dp),
        ) {
            LkIcon(R.drawable.ic_lk_warning, contentDescription = null, tint = colour, size = LkIconSize.Row)
            Column(modifier = Modifier.weight(1f)) {
                if (title != null) {
                    Text(title, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold, color = colour)
                    Spacer(Modifier.height(4.dp))
                }
                Text(body, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurface)
                if (extra != null) {
                    Spacer(Modifier.height(8.dp))
                    Text(extra, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurface)
                }
                if (actions != null) {
                    Spacer(Modifier.height(8.dp))
                    actions()
                }
            }
        }
    }
}
