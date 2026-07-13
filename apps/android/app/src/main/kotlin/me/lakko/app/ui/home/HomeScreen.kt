// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.home

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Button
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
    onOpenContacts: () -> Unit,
    onOpenChats: () -> Unit,
    onOpenChannels: () -> Unit,
    onOpenPasskeys: () -> Unit,
    onOpenDevices: () -> Unit,
    onOpenAntiCensorship: () -> Unit,
    onLogout: () -> Unit,
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
        Button(
            onClick = onOpenContacts,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Open contacts")
        }
        Spacer(Modifier.height(8.dp))
        Button(
            onClick = onOpenChats,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Open chats")
        }
        Spacer(Modifier.height(8.dp))
        // Phase 4 (ADR-0010) — broadcast channels. Tapping pushes
        // `ChannelsRootScreen` via the SessionRoot destination
        // toggle, threading the AccountIdentity (phrase) seed so a
        // publisher can drive the MLS PublisherGroup.
        OutlinedButton(
            onClick = onOpenChannels,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Channels")
        }
        Spacer(Modifier.height(8.dp))
        // 2a2e — passkey management lives behind its own
        // surface. Tapping pushes `PasskeysScreen` via the
        // SessionRoot destination toggle.
        OutlinedButton(
            onClick = onOpenPasskeys,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Manage passkeys")
        }
        Spacer(Modifier.height(8.dp))
        // 2a3b4 — DeviceGroup pairing surface. Tapping pushes
        // `DevicesScreen` via the SessionRoot destination
        // toggle.
        OutlinedButton(
            onClick = onOpenDevices,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Devices")
        }
        Spacer(Modifier.height(8.dp))
        // ADR-0026 — anti-censorship arming surface. Tapping
        // pushes `AntiCensorshipSettingsScreen` via the
        // SessionRoot destination toggle.
        OutlinedButton(
            onClick = onOpenAntiCensorship,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Anti-censorship")
        }
        Spacer(Modifier.height(16.dp))
        Text(
            text = "Contacts (1.15e) and 1×1 chat (1.15f) are live. Accept on a " +
                "pending request drives the MLS handshake — switch to the " +
                "Chats list to find the new thread.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))
        Row(
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            modifier = Modifier.fillMaxWidth(),
        ) {
            // `Logout` keeps the on-device material; the user can
            // log back in quickly with their password. `Forget
            // device` wipes the SessionStore marker + the
            // Keystore-wrapped device root key — the only path
            // back is the 24-word recovery phrase (1.15d6).
            Button(
                onClick = onLogout,
                modifier = Modifier.weight(1f),
            ) {
                Text("Log out")
            }
            OutlinedButton(
                onClick = onForgetDevice,
                modifier = Modifier.weight(1f),
            ) {
                Text("Forget device")
            }
        }
    }
}
