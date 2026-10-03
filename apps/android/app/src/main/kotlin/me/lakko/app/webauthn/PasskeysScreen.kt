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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import java.util.Locale
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text

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
 *   1. The registered passkeys. Each row says what the passkey does
 *      ("Signs in and restores keys" / "Signs in only") and offers the
 *      matching action ("Stop restoring keys" / "Turn on restoring keys")
 *      beside Remove.
 *   2. Nickname input + "Add passkey". Adding stores nothing for the
 *      passkey beyond the credential; when the provider can protect keys
 *      with it, the opt-in choice ([PasskeyKeysChoice]) opens.
 *   3. A single-line status banner that flips between success
 *      and error.
 *
 * Reached from Settings > Security > Passkeys.
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
    // A removal names its target on the button (docs/l10n/batch-2-settings.md).
    var removing by remember { mutableStateOf<CredentialSummary?>(null) }
    // "Stop restoring keys" asks first.
    var stopping by remember { mutableStateOf<CredentialSummary?>(null) }

    LaunchedEffect(Unit) { viewModel.refresh() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(t("settings.passkeys.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                t("settings.passkeys.lede_device"),
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
                        Text(t("common.loading"))
                    }
                }
                state.credentials.isEmpty() -> {
                    Text(
                        t("settings.passkeys.empty"),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    // UX 15.3 (ADR-0065) - after a reset on this device the
                    // empty state says why the passkeys are gone.
                    val resetThisSession by viewModel.repository.resetThisSession.collectAsStateWithLifecycle()
                    if (resetThisSession) {
                        Text(
                            t("settings.passkeys.reset_note"),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                else -> {
                    Column(
                        modifier = Modifier.fillMaxWidth(),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        state.credentials.forEach { cred ->
                            CredentialRow(
                                summary = cred,
                                restoresKeys = passkeyKeysLabel(cred.credentialIdB64, state.withKeys) ==
                                    PasskeyKeysLabel.RestoresKeys,
                                disabled = state.busy || state.choice != null,
                                onTurnOn = { viewModel.openChoice(cred) },
                                onStop = { stopping = cred },
                                onDelete = { removing = cred },
                            )
                        }
                    }
                }
            }

            state.choice?.let { choice ->
                PasskeyKeysChoice(
                    repo = viewModel.repository,
                    credentialIdB64 = choice.credentialIdB64,
                    nickname = choice.nickname,
                    prfOutput = choice.prfOutput,
                    onDone = viewModel::onChoiceDone,
                )
            }

            HorizontalDivider()

            Text(
                t("settings.passkeys.add"),
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            OutlinedTextField(
                value = state.nickname,
                onValueChange = viewModel::onNicknameChange,
                label = { Text(t("settings.passkeys.name.label")) },
                singleLine = true,
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            )
            Button(
                onClick = { viewModel.addPasskey(context) },
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(if (state.busy) t("common.adding") else t("settings.passkeys.add"))
            }

            when (val m = state.message) {
                is Message.Ok -> StatusBanner(text = m.text.text(), isError = false)
                is Message.Error -> StatusBanner(text = m.text.text(), isError = true)
                null -> Spacer(Modifier.height(0.dp))
            }

            // A3 (GDPR) — data portability.
            HorizontalDivider(modifier = Modifier.padding(vertical = 16.dp))
            Text(text = t("settings.data.title"))
            Text(
                text = t("settings.data.lede"),
                style = MaterialTheme.typography.bodySmall,
            )
            Button(
                onClick = { viewModel.exportData(context) },
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(t("settings.data.export"))
            }

            // A3 (GDPR) — permanent account deletion (danger zone).
            HorizontalDivider(modifier = Modifier.padding(vertical = 16.dp))
            Text(
                text = t("settings.account.delete.confirm"),
                color = MaterialTheme.colorScheme.error,
            )
            Text(
                text = t("settings.account.delete.sub"),
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
                Text(t("settings.account.delete.confirm"))
            }
        }
    }

    if (confirmingDelete) {
        AlertDialog(
            onDismissRequest = { confirmingDelete = false },
            title = { Text(t("settings.account.delete.title")) },
            text = {
                Text(t("settings.account.delete.body"))
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmingDelete = false
                    viewModel.deleteAccount(context)
                }) {
                    Text(t("settings.account.delete.confirm"), color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmingDelete = false }) { Text(t("common.cancel")) }
            },
        )
    }
    stopping?.let { cred ->
        AlertDialog(
            onDismissRequest = { stopping = null },
            title = { Text(t("settings.passkeys.stop_restoring.title")) },
            text = { Text(t("settings.passkeys.stop_restoring.body")) },
            confirmButton = {
                TextButton(onClick = {
                    stopping = null
                    viewModel.stopRestoring(cred.credentialIdB64)
                }) { Text(t("settings.passkeys.stop_restoring.action")) }
            },
            dismissButton = {
                TextButton(onClick = { stopping = null }) { Text(t("common.cancel")) }
            },
        )
    }
    removing?.let { cred ->
        AlertDialog(
            onDismissRequest = { removing = null },
            title = { Text(t("settings.passkeys.remove.title")) },
            text = { Text(t("settings.passkeys.remove.body")) },
            confirmButton = {
                TextButton(onClick = {
                    removing = null
                    viewModel.deleteCredential(context, cred.credentialIdB64, cred.nickname)
                }) {
                    Text(
                        if (cred.nickname.isBlank()) {
                            t("settings.passkeys.remove.confirm")
                        } else {
                            t("settings.passkeys.remove.confirm_named", mapOf("name" to cred.nickname))
                        },
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            },
            dismissButton = {
                TextButton(onClick = { removing = null }) { Text(t("common.cancel")) }
            },
        )
    }
}

@Composable
private fun CredentialRow(
    summary: CredentialSummary,
    restoresKeys: Boolean,
    disabled: Boolean,
    onTurnOn: () -> Unit,
    onStop: () -> Unit,
    onDelete: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
    ) {
        Text(
            if (summary.nickname.isBlank()) t("settings.passkeys.unnamed") else summary.nickname,
            style = MaterialTheme.typography.bodyLarge,
        )
        // What this passkey does, in words (never colour alone).
        Text(
            if (restoresKeys) {
                t("settings.passkeys.label.restores_keys")
            } else {
                t("settings.passkeys.label.signin_only")
            },
            style = MaterialTheme.typography.bodyMedium,
            fontWeight = FontWeight.SemiBold,
        )
        Text(
            metadataLine(summary, LocalStrings.current.language),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            if (restoresKeys) {
                TextButton(onClick = onStop, enabled = !disabled) {
                    Text(t("settings.passkeys.stop_restoring.action"))
                }
            } else {
                TextButton(onClick = onTurnOn, enabled = !disabled) {
                    Text(t("settings.passkeys.keys.turn_on"))
                }
            }
            Spacer(Modifier.weight(1f))
            TextButton(onClick = onDelete, enabled = !disabled) {
                Text(t("common.remove"), color = MaterialTheme.colorScheme.error)
            }
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

@Composable
private fun metadataLine(c: CredentialSummary, language: String): String {
    val created = format(c.createdAtMs, language) ?: t("common.unknown")
    val lastUsed = format(c.lastUsedAtMs, language)
    return if (lastUsed != null) {
        t("settings.passkeys.added_last_used", mapOf("added" to created, "used" to lastUsed))
    } else {
        t("settings.passkeys.added_never_used", mapOf("added" to created))
    }
}

/** A date and time in the interface language (docs/l10n/batch-2-settings.md). */
private fun format(ms: Long?, language: String): String? {
    if (ms == null) return null
    val date = Date(ms)
    val formatter = DateFormat.getDateTimeInstance(
        DateFormat.MEDIUM,
        DateFormat.SHORT,
        Locale.forLanguageTag(language),
    )
    return formatter.format(date)
}
