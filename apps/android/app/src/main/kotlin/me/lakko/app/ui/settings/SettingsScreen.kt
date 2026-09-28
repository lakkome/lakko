// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.Settings
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
import androidx.compose.material.icons.filled.Gavel
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.NotificationsOff
import androidx.compose.material.icons.filled.Palette
import androidx.compose.material.icons.filled.Restore
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.app.NotificationManagerCompat
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.push.NOTIFICATIONS_OFF_NOTICE
import me.lakko.app.push.NOTIFICATIONS_OFF_TITLE
import me.lakko.app.ui.chat.AutoplayVideoNotesSetting
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.theme.ThemeMode
import me.lakko.app.ui.theme.ThemePreference

/**
 * The Settings tab — docs/ux/client-redesign.md section 5.5.
 *
 * Header with the avatar, the name and the username; Account, Security
 * (Passkeys, Devices, Recovery), Appearance and Media inline, Network, About
 * (version, licence) and the Danger zone at the bottom: Log out and Forget
 * this device behind the confirmations the flows already had. There is no
 * Delete account row: the client has no account-deletion call yet.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun SettingsScreen(
    displayName: String?,
    username: String?,
    onOpenProfile: () -> Unit,
    onOpenPasskeys: () -> Unit,
    onOpenDevices: () -> Unit,
    onOpenRecovery: () -> Unit,
    onOpenAntiCensorship: () -> Unit,
    onLogout: () -> Unit,
    onForgetDevice: () -> Unit,
    modifier: Modifier = Modifier,
    /** docs/ux/chat-folders.md section 4 — between Notifications and Appearance. */
    onOpenChatFolders: () -> Unit = {},
) {
    val mode by ThemePreference.mode.collectAsStateWithLifecycle()
    val context = LocalContext.current
    // docs/ux/calls.md section 2.1 — whether a wake can reach this phone at
    // all. Read every time Settings is resumed, never cached at process start:
    // the row below sends the person to system settings, so the state they
    // come back to is precisely the one a cached read would still be wrong
    // about. Starts `true` so the notice does not flash before the first read.
    var notificationsEnabled by remember { mutableStateOf(true) }
    LifecycleResumeEffect(Unit) {
        notificationsEnabled = NotificationManagerCompat.from(context).areNotificationsEnabled()
        onPauseOrDispose { }
    }
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

            // First, not filed under device behaviour lower down: this only
            // appears when it is broken, and the person reading it is the one
            // asking why their phone did not ring. Nothing is shown when
            // notifications are on — a row confirming that things work is a row
            // nobody needs.
            if (!notificationsEnabled) {
                SectionLabel("Notifications")
                SettingsRow(
                    title = NOTIFICATIONS_OFF_TITLE,
                    icon = Icons.Filled.NotificationsOff,
                    onClick = { openAppNotificationSettings(context) },
                    supporting = NOTIFICATIONS_OFF_NOTICE,
                )
            }

            SectionLabel("Account")
            SettingsRow("Profile", Icons.Filled.Person, onOpenProfile)

            SectionLabel("Security")
            SettingsRow("Passkeys", Icons.Filled.Key, onOpenPasskeys)
            SettingsRow("Devices", Icons.Filled.Devices, onOpenDevices)
            SettingsRow(
                "Recovery",
                Icons.Filled.Restore,
                onOpenRecovery,
                supporting = "Recovery phrase, authenticator, email or phone",
            )

            SectionLabel("Chat folders")
            SettingsRow("Chat folders", Icons.Filled.Folder, onOpenChatFolders)

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

            SectionLabel("About")
            InfoRow(
                title = "Version",
                value = "${me.lakko.app.BuildConfig.VERSION_NAME} (${me.lakko.app.BuildConfig.VERSION_CODE})",
                icon = Icons.Filled.Info,
            )
            InfoRow(title = "Licence", value = "AGPL-3.0-or-later", icon = Icons.Filled.Gavel)

            Spacer(Modifier.height(16.dp))
            HorizontalDivider()
            SectionLabel("Danger zone", danger = true)
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
private fun SectionLabel(text: String, danger: Boolean = false) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelLarge,
        color = if (danger) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

/** A read-only row (About): title on the left, value as the supporting line. */
@Composable
private fun InfoRow(title: String, value: String, icon: ImageVector) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = { Text(value) },
        leadingContent = { Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant) },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
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

/**
 * Open this app's notification settings — the only place a refused
 * `POST_NOTIFICATIONS` can be changed, since Android stops showing the prompt
 * after two dismissals.
 *
 * Falls back to the app details page if an OEM build has no per-app
 * notification screen, so the row can never be a dead end.
 */
private fun openAppNotificationSettings(context: Context) {
    val perApp = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
        .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
    try {
        context.startActivity(perApp)
        return
    } catch (_: ActivityNotFoundException) {
        // Fall through to the app details page.
    }
    val details = Intent(
        Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
        Uri.fromParts("package", context.packageName, null),
    )
    try {
        context.startActivity(details)
    } catch (_: ActivityNotFoundException) {
        // Nothing further to offer; the notice still says what is wrong.
    }
}
