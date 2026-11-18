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
import me.lakko.app.l10n.t
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/**
 * ADR-0038 S4 — the dismissible "update available" notice (Android). The Kotlin
 * twin of the iOS `UpdateAvailableBanner`.
 *
 * The banner shape of the earlier identity-security banner (deleted with
 * ADR-0073: a changed published identity is now the signed-out screen or
 * nothing): a horizontal row, leading icon + message + trailing controls,
 * `RoundedCornerShape(8.dp)`, `.padding(12.dp)` inner / `.padding(8.dp)` outer,
 * with an **informational** tone (UX section 3 / 9): `secondaryContainer` /
 * `onSecondaryContainer`, never `errorContainer`, which stays reserved for
 * a security alarm.
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
            LkIcon(
                icon = R.drawable.ic_lk_arrow_down_circle_fill,
                contentDescription = null,
                size = LkIconSize.Toolbar,
            )
            Text(
                text = t("update.available.message"),
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.weight(1f),
            )
            val updateA11y = t("update.available.update_play_a11y")
            val dismissLabel = t("common.dismiss")
            if (showUpdateButton) {
                Button(
                    onClick = onUpdate,
                    modifier = Modifier
                        .defaultMinSize(minWidth = 48.dp, minHeight = 48.dp)
                        .semantics { contentDescription = updateA11y },
                ) {
                    Text(t("update.available.update"))
                }
            }
            TextButton(
                onClick = onDismiss,
                modifier = Modifier
                    .defaultMinSize(minWidth = 48.dp, minHeight = 48.dp)
                    .semantics { contentDescription = dismissLabel },
            ) {
                Text(dismissLabel)
            }
        }
    }
}
