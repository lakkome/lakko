// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import me.lakko.app.l10n.t

/**
 * An informing banner (design-system Banner rule): the `info` icon, a short
 * title and a body, on the sunken surface. No action, no close button, not
 * focusable; it asks nothing.
 *
 * Accessibility (`docs/ux/beta-notice.md` section 5): one element read as the
 * title then the body (`mergeDescendants`), the icon decorative, nothing
 * clickable. The text wraps under font scaling; no line limit.
 */
@Composable
public fun InfoBanner(
    title: String,
    body: String,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier
            .fillMaxWidth()
            .semantics(mergeDescendants = true) {},
        color = MaterialTheme.colorScheme.surfaceVariant,
        contentColor = MaterialTheme.colorScheme.onSurface,
        shape = RoundedCornerShape(8.dp),
    ) {
        Row(
            modifier = Modifier.padding(12.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalAlignment = Alignment.Top,
        ) {
            LkIcon(
                icon = me.lakko.app.R.drawable.ic_lk_info_circle,
                contentDescription = null,
                size = 24.dp,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(text = title, style = MaterialTheme.typography.titleSmall)
                Text(
                    text = body,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/**
 * The beta notice (`docs/ux/beta-notice.md`): shown before an account is
 * made, on the welcome screen, the sign-up screen and the guest's
 * `Create account` form. Removed with the strings when the external review
 * is done (ADR-0046).
 */
@Composable
public fun BetaNoticeBanner(modifier: Modifier = Modifier) {
    InfoBanner(
        title = t("beta.notice.title"),
        body = t("beta.notice.body"),
        modifier = modifier,
    )
}
