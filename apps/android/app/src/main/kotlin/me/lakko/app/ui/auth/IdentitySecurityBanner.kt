// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

/**
 * ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (i)) — a
 * dismissible security banner shown when the account's published identity
 * did not match this device's at login (someone replaced it; the
 * ACCESS-hijack signature). Mirror of the web / iOS `IdentitySecurityBanner`.
 */
@Composable
public fun IdentitySecurityBanner(
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier
            .fillMaxWidth()
            .padding(8.dp),
        color = MaterialTheme.colorScheme.errorContainer,
        contentColor = MaterialTheme.colorScheme.onErrorContainer,
        shape = RoundedCornerShape(8.dp),
    ) {
        Row(
            modifier = Modifier.padding(12.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    text = "Your account's secure identity was changed.",
                    style = MaterialTheme.typography.titleSmall,
                )
                Text(
                    text = "Someone published a different identity key for " +
                        "this account. If this was you (you reset your " +
                        "identity on another device), you can dismiss this. " +
                        "If it was NOT you, your account may be compromised " +
                        "— re-verify your safety number with your contacts " +
                        "out of band, and rotate your password / passkeys.",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            TextButton(onClick = onDismiss) {
                Text("Dismiss")
            }
        }
    }
}
