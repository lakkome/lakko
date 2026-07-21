// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.updates

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowCircleDown
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

/**
 * ADR-0038 S4 — the dismissible "update available" notice (Android). The Kotlin
 * twin of the iOS `UpdateAvailableBanner`.
 *
 * Clones [me.lakko.app.ui.auth.IdentitySecurityBanner]'s placement + structure (a
 * horizontal row: leading icon + message + trailing controls,
 * `RoundedCornerShape(8.dp)`, `.padding(12.dp)` inner / `.padding(8.dp)` outer) but
 * with an **informational** tone, NOT the security banner's danger palette
 * (UX §3 / §9): `secondaryContainer` / `onSecondaryContainer` (never
 * `errorContainer`, which is reserved for the security banner) so it reads
 * neutral/helpful and stacks legibly *below* the red security banner (severity
 * ordering — security first).
 *
 * The `Update` control is shown only when a store URL is configured; otherwise the
 * banner is purely informational text (no dead / greyed-out button, UX §5). The
 * whole banner is an instant conditional render — no entrance animation
 * (UX §10 reduced-motion).
 */
@Composable
public fun UpdateAvailableBanner(
    showUpdateButton: Boolean,
    onUpdate: () -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier
            .fillMaxWidth()
            .padding(8.dp),
        color = MaterialTheme.colorScheme.secondaryContainer,
        contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
        shape = RoundedCornerShape(8.dp),
    ) {
        Row(
            modifier = Modifier.padding(12.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // Leading glyph so the meaning survives color-blindness / greyscale
            // (UX §10 — icon + label, never color alone). Decorative; the text
            // carries the accessible content (contentDescription = null).
            Icon(
                imageVector = Icons.Filled.ArrowCircleDown,
                contentDescription = null,
                modifier = Modifier.size(24.dp),
            )
            Text(
                text = "A new version of Lakko is available.",
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.weight(1f),
            )
            if (showUpdateButton) {
                Button(
                    onClick = onUpdate,
                    modifier = Modifier
                        .defaultMinSize(minWidth = 48.dp, minHeight = 48.dp)
                        .semantics { contentDescription = "Update, opens the Play Store" },
                ) {
                    Text("Update")
                }
            }
            TextButton(
                onClick = onDismiss,
                modifier = Modifier
                    .defaultMinSize(minWidth = 48.dp, minHeight = 48.dp)
                    .semantics { contentDescription = "Dismiss" },
            ) {
                Text("Dismiss")
            }
        }
    }
}
