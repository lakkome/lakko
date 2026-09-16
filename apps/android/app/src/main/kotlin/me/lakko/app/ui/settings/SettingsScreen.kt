// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Logout
import androidx.compose.material.icons.filled.DeleteForever
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.filled.Palette
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.PlayCircle
import androidx.compose.material.icons.filled.Shield
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.ui.chat.AutoplayVideoNotesSetting
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.theme.ThemeMode
import me.lakko.app.ui.theme.ThemePreference

/**
 * The Settings tab — docs/ux/client-redesign.md section 5.5 (Phase A shape).
 *
 * Header with the avatar, the name and the username; rows for the existing
 * screens (Profile, Passkeys, Devices, Anti-censorship); Appearance and Media
 * inline; Log out and Forget this device at the bottom, behind the
 * confirmations the flows already had.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun SettingsScreen(
    displayName: String?,
    username: String?,
    onOpenProfile: () -> Unit,
    onOpenPasskeys: () -> Unit,
    onOpenDevices: () -> Unit,
    onOpenAntiCensorship: () -> Unit,
    onLogout: () -> Unit,
    onForgetDevice: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val mode by ThemePreference.mode.collectAsStateWithLifecycle()
    var confirmingLogout by remember { mutableStateOf(false) }
    var confirmingForget by remember { mutableStateOf(false) }
    val headerName = displayName?.takeIf { it.isNotBlank() }
        ?: username?.let { "@$it" }
        ?: "Guest"

    Scaffold(
        modifier = modifier,
        topBar = { TopAppBar(title = { Text("Settings") }) },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState()),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable(onClick = onOpenProfile)
                    .padding(horizontal = 16.dp, vertical = 16.dp),
            ) {
                LakkoAvatar(name = headerName, seed = username ?: headerName, size = 56.dp)
                Spacer(Modifier.width(16.dp))
                Column {
                    Text(headerName, style = MaterialTheme.typography.titleLarge)
                    username?.let {
                        Text(
                            "@$it",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
            HorizontalDivider()

            SectionLabel("Account")
            SettingsRow("Profile", Icons.Filled.Person, onOpenProfile)

            SectionLabel("Security")
            SettingsRow("Passkeys", Icons.Filled.Key, onOpenPasskeys)
            SettingsRow("Devices", Icons.Filled.Devices, onOpenDevices)

            SectionLabel("Appearance")
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            ) {
                Icon(
                    Icons.Filled.Palette,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.width(16.dp))
                Text("Theme", modifier = Modifier.weight(1f))
            }
            SingleChoiceSegmentedButtonRow(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp),
            ) {
                val options = ThemeMode.entries
                options.forEachIndexed { index, option ->
                    SegmentedButton(
                        selected = mode == option,
                        onClick = { ThemePreference.set(option) },
                        shape = SegmentedButtonDefaults.itemShape(index = index, count = options.size),
                    ) {
                        Text(option.name)
                    }
                }
            }
            Spacer(Modifier.height(8.dp))

            SectionLabel("Media")
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 4.dp),
            ) {
                Icon(
                    Icons.Filled.PlayCircle,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.width(16.dp))
                // ADR-0047 / UX spec §3 — autoplay downloads media, so it is
                // off until the user turns it on.
                AutoplayVideoNotesSetting(modifier = Modifier.weight(1f))
            }

            SectionLabel("Network")
            SettingsRow("Anti-censorship", Icons.Filled.Shield, onOpenAntiCensorship)

            Spacer(Modifier.height(16.dp))
            HorizontalDivider()
            SettingsRow(
                title = "Log out",
                icon = Icons.AutoMirrored.Filled.Logout,
                onClick = { confirmingLogout = true },
                danger = true,
            )
            SettingsRow(
                title = "Forget this device",
                icon = Icons.Filled.DeleteForever,
                onClick = { confirmingForget = true },
                danger = true,
                supporting = "Removes your account data from this phone. You will need your recovery phrase to come back.",
            )
            Spacer(Modifier.height(24.dp))
        }
    }

    if (confirmingLogout) {
        AlertDialog(
            onDismissRequest = { confirmingLogout = false },
            title = { Text("Log out?") },
            text = { Text("You can log back in with your password.") },
            confirmButton = {
                TextButton(onClick = { confirmingLogout = false; onLogout() }) { Text("Log out") }
            },
            dismissButton = {
                TextButton(onClick = { confirmingLogout = false }) { Text("Cancel") }
            },
        )
    }
    if (confirmingForget) {
        AlertDialog(
            onDismissRequest = { confirmingForget = false },
            title = { Text("Forget this device?") },
            text = {
                Text(
                    "Your messages and keys are removed from this phone. " +
                        "The only way back is your recovery phrase or another device.",
                )
            },
            confirmButton = {
                TextButton(onClick = { confirmingForget = false; onForgetDevice() }) {
                    Text("Forget device", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmingForget = false }) { Text("Cancel") }
            },
        )
    }
}

@Composable
private fun SectionLabel(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

@Composable
private fun SettingsRow(
    title: String,
    icon: ImageVector,
    onClick: () -> Unit,
    danger: Boolean = false,
    supporting: String? = null,
) {
    val tint = if (danger) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant
    ListItem(
        headlineContent = {
            Text(title, color = if (danger) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
        },
        supportingContent = supporting?.let { { Text(it) } },
        leadingContent = { Icon(icon, contentDescription = null, tint = tint) },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
        modifier = Modifier.clickable(onClick = onClick),
    )
}
