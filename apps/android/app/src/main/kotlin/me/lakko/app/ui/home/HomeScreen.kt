// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.home

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import me.lakko.app.auth.SessionAccount

/**
 * Placeholder home screen — Phase 1.15d4.
 *
 * Once 1.15e (contacts) and 1.15f (chat) land, this becomes the
 * Lakko app surface proper. For 1.15d4 the only "you're logged
 * in" surface we need is a confirmation that the OPAQUE round-
 * trip + device-key persistence worked end-to-end, plus a button
 * to wipe the device and start over (parallel to the web's
 * `forgetDevice` action).
 */
@Composable
public fun HomeScreen(
    account: SessionAccount,
    onForgetDevice: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            text = "Lakko",
            style = MaterialTheme.typography.displaySmall,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            text = "Signed in as @${account.username}",
            style = MaterialTheme.typography.titleMedium,
        )
        Spacer(Modifier.height(16.dp))
        Text(
            text = "Account id (base64):",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            text = account.accountIdB64,
            style = MaterialTheme.typography.bodySmall,
            fontFamily = FontFamily.Monospace,
        )
        Spacer(Modifier.height(32.dp))
        Text(
            text = "Phase 1.15d4 placeholder. Contacts (1.15e) and chat (1.15f) " +
                "arrive in the next sub-sprints. The OPAQUE-PAKE round trip + " +
                "Keystore-wrapped device root key are live behind this screen.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))
        OutlinedButton(
            onClick = onForgetDevice,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Forget device")
        }
    }
}
