// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import java.text.DateFormat
import java.util.Date

/**
 * `PasskeysScreen` — manage the user's stored passkeys
 * (Phase 2a2e).
 *
 * Mirror of:
 *
 *   * `apps/web/src/auth/PasskeysSection.tsx`
 *   * `apps/ios/Lakko/Webauthn/PasskeysView.swift`
 *
 * Surfaces:
 *
 *   1. List of registered passkeys (nickname + timestamps +
 *      per-row Delete icon).
 *   2. Nickname input + "Add passkey" button driving
 *      [WebauthnRegistration].
 *   3. A single-line status banner that flips between success
 *      and error toasts.
 *
 * Reachable from `HomeScreen` via the "Manage passkeys" row.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun PasskeysScreen(
    viewModel: PasskeysViewModel,
    onBack: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    // A3 — drives the delete-account confirmation dialog.
    var confirmingDelete by remember { mutableStateOf(false) }

    LaunchedEffect(Unit) { viewModel.refresh() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Passkeys") },
                navigationIcon = {
                    // No `material-icons-*` dependency on this
                    // module, so a plain text-button labeled
                    // "Back" carries the navigation affordance.
                    TextButton(onClick = onBack) { Text("Back") }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                "Sign in with the device biometric, a hardware key, or " +
                    "your password manager. The 24-word recovery phrase " +
                    "always works as the last-resort path.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            when {
                state.loading -> {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        CircularProgressIndicator()
                        Text("Loading passkeys…")
                    }
                }
                state.credentials.isEmpty() -> {
                    Text(
                        "No passkeys yet. Add one below.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                else -> {
                    LazyColumn(
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(min = 80.dp, max = 320.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        items(state.credentials, key = { it.credentialIdB64 }) { cred ->
                            CredentialRow(
                                summary = cred,
                                disabled = state.busy,
                                onDelete = {
                                    viewModel.deleteCredential(
                                        context,
                                        cred.credentialIdB64,
                                        cred.nickname,
                                    )
                                },
                            )
                        }
                    }
                }
            }

            HorizontalDivider()

            Text(
                "Add a passkey",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            OutlinedTextField(
                value = state.nickname,
                onValueChange = viewModel::onNicknameChange,
                label = { Text("Nickname (e.g. Pixel fingerprint)") },
                singleLine = true,
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            )
            Button(
                onClick = { viewModel.addPasskey(context) },
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(if (state.busy) "Adding…" else "Add passkey")
            }

            when (val m = state.message) {
                is Message.Ok -> StatusBanner(text = m.text, isError = false)
                is Message.Error -> StatusBanner(text = m.text, isError = true)
                null -> Spacer(Modifier.height(0.dp))
            }

            // A3 (GDPR) — permanent account deletion (danger zone).
            HorizontalDivider(modifier = Modifier.padding(vertical = 16.dp))
            Text(
                text = "Delete account",
                color = MaterialTheme.colorScheme.error,
            )
            Text(
                text = "Permanently erase your account from the server (username, " +
                    "keys, recovery factors, backup) and wipe this device. This " +
                    "cannot be undone.",
                style = MaterialTheme.typography.bodySmall,
            )
            Button(
                onClick = { confirmingDelete = true },
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
                colors = ButtonDefaults.buttonColors(
                    containerColor = MaterialTheme.colorScheme.error,
                ),
            ) {
                Text("Delete my account")
            }
        }
    }

    if (confirmingDelete) {
        AlertDialog(
            onDismissRequest = { confirmingDelete = false },
            title = { Text("Delete your account permanently?") },
            text = {
                Text("This erases your server-side identity and wipes this device. It cannot be undone.")
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmingDelete = false
                    viewModel.deleteAccount(context)
                }) {
                    Text("Delete my account", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmingDelete = false }) { Text("Cancel") }
            },
        )
    }
}

@Composable
private fun CredentialRow(
    summary: CredentialSummary,
    disabled: Boolean,
    onDelete: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Column(modifier = Modifier.fillMaxWidth(0.85f)) {
            Text(
                if (summary.nickname.isBlank()) "(unnamed passkey)" else summary.nickname,
                style = MaterialTheme.typography.bodyLarge,
            )
            Text(
                metadataLine(summary),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        TextButton(onClick = onDelete, enabled = !disabled) {
            Text("Delete")
        }
    }
}

@Composable
private fun StatusBanner(text: String, isError: Boolean) {
    val color = if (isError) {
        MaterialTheme.colorScheme.error
    } else {
        Color(0xFF2E7D32)  // dark green; theme.tertiary varies by spec
    }
    Text(
        text,
        style = MaterialTheme.typography.bodyMedium,
        color = color,
    )
}

private fun metadataLine(c: CredentialSummary): String {
    val created = format(c.createdAtMs) ?: "unknown"
    val lastUsed = format(c.lastUsedAtMs)
    return if (lastUsed != null) {
        "Added $created · last used $lastUsed"
    } else {
        "Added $created · never used"
    }
}

private fun format(ms: Long?): String? {
    if (ms == null) return null
    val date = Date(ms)
    val formatter = DateFormat.getDateTimeInstance(
        DateFormat.MEDIUM,
        DateFormat.SHORT,
    )
    return formatter.format(date)
}
