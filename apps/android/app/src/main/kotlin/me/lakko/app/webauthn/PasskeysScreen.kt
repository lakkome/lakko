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
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import kotlinx.coroutines.launch
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon

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
 *      ("Logs in and restores keys" / "Logs in only"), a state and not a
 *      setting. A passkey that holds no keys and is not known to lack PRF
 *      offers "Restore keys with this passkey" beside Remove (UX 10.2,
 *      amended 2026-11-11); with no phrase on this device the twelve-slot
 *      phrase input opens first.
 *   2. Nickname input + "Add passkey". The keys are stored for the new
 *      passkey with nothing to choose ([addPasskeyWithKeys]).
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
    /** Reached from Settings > Danger zone: the delete dialog is what was
     *  asked for, and closing it goes back there. */
    deleteAccountOnly: Boolean = false,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    // A3 — the data export leaves the app as a file the person saves.
    val fileSaver = me.lakko.app.ui.components.rememberFileSaver()
    // A3 — drives the delete-account confirmation dialog. Saved, so a
    // rotation brings the dialog back (its typed password is the ViewModel's).
    var confirmingDelete by rememberSaveable { mutableStateOf(deleteAccountOnly) }
    // A removal names its target on the button (docs/l10n/batch-2-settings.md).
    var removing by remember { mutableStateOf<CredentialSummary?>(null) }

    LaunchedEffect(Unit) { viewModel.refresh() }
    val scope = rememberCoroutineScope()
    // UX section 17 - a session alone may not add a passkey (a passkey is
    // itself a proof): the proof block sits above `Add passkey` while no
    // proof is live.
    val addProof = me.lakko.app.ui.auth.rememberProofController(
        viewModel.repository,
        me.lakko.app.ui.auth.ProofSurface.AddPasskey,
    )

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(t("settings.passkeys.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        LkIcon(R.drawable.ic_lk_chevron_back, contentDescription = t("common.back"))
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
                    // UX 15.3 (ADR-0073) - after a reset on this device the
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
                                offersRestore = passkeyOffersRestoreKeys(
                                    cred.credentialIdB64,
                                    state.withKeys,
                                    state.knownNoPrf,
                                ),
                                disabled = state.busy || state.repair != null,
                                onRestore = { viewModel.restoreKeys(context, cred) },
                                onDelete = { removing = cred },
                            )
                        }
                    }
                }
            }

            state.repair?.let { repair ->
                RestoreKeysPhrasePanel(
                    repair = repair,
                    busy = state.busy,
                    error = state.repairError,
                    onSubmit = { phrase ->
                        state.credentials.firstOrNull { it.credentialIdB64 == repair.credentialIdB64 }?.let { cred ->
                            viewModel.restoreKeys(context, cred, typedPhrase = phrase)
                        }
                    },
                    onCancel = viewModel::cancelRepair,
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
            val addView = addProof.view()
            val add: () -> Unit = add@{
                if (state.busy || !addProof.view().mainEnabled) return@add
                scope.launch {
                    // The proof first; the registration ceremony only after.
                    if (addProof.ensure(context)) {
                        viewModel.addPasskey(context, onProofRequired = addProof::onProofRefused)
                    }
                }
            }
            me.lakko.app.ui.auth.ProofBlock(
                controller = addProof,
                onSubmit = add,
                onPasskey = { scope.launch { addProof.provePasskey(context) } },
            )
            Button(
                onClick = add,
                enabled = !state.busy && addView.mainEnabled,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(if (state.busy || addView.working) t("common.adding") else t("settings.passkeys.add"))
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
                onClick = { viewModel.exportData(fileSaver::save) },
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
        DeleteAccountDialog(
            viewModel = viewModel,
            state = state,
            onClose = {
                viewModel.clearDeleteError()
                confirmingDelete = false
                if (deleteAccountOnly) onBack()
            },
        )
    }
    removing?.let { cred ->
        LakkoDialog(
            onDismissRequest = { removing = null },
            title = t("settings.passkeys.remove.title"),
            body = t("settings.passkeys.remove.body"),
            confirm = LakkoDialogAction(
                if (cred.nickname.isBlank()) {
                    t("settings.passkeys.remove.confirm")
                } else {
                    t("settings.passkeys.remove.confirm_named", mapOf("name" to cred.nickname))
                },
                LakkoDialogTone.Danger,
            ) {
                removing = null
                viewModel.deleteCredential(context, cred.credentialIdB64, cred.nickname)
            },
        )
    }
}

@Composable
private fun CredentialRow(
    summary: CredentialSummary,
    restoresKeys: Boolean,
    offersRestore: Boolean,
    disabled: Boolean,
    onRestore: () -> Unit,
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
            // A repair, not a setting: nothing to turn off (UX 10.2).
            if (offersRestore) {
                TextButton(onClick = onRestore, enabled = !disabled) {
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

/**
 * The phrase input of "Restore keys with this passkey" on a device that
 * does not hold the recovery phrase (UX 10.2): the twelve slots, checked
 * against this account before the one passkey prompt.
 */
@Composable
private fun RestoreKeysPhrasePanel(
    repair: PasskeyRepair,
    busy: Boolean,
    error: me.lakko.app.l10n.UiText?,
    onSubmit: (String) -> Unit,
    onCancel: () -> Unit,
) {
    var phrase by remember(repair.credentialIdB64) { mutableStateOf("") }
    androidx.compose.material3.Surface(
        shape = MaterialTheme.shapes.medium,
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (repair.nickname.isNotBlank()) {
                Text(
                    repair.nickname,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                )
            }
            Text(t("settings.passkeys.keys_choice.phrase_needed"), style = MaterialTheme.typography.bodyMedium)
            me.lakko.app.ui.auth.RecoveryPhraseEntryGrid(
                phrase = phrase,
                onPhraseChange = { phrase = it },
                enabled = !busy,
            )
            error?.let {
                Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
                Button(
                    onClick = { onSubmit(phrase) },
                    enabled = !busy && phrase.isNotBlank(),
                    modifier = Modifier.weight(1f),
                ) {
                    Text(if (busy) t("common.checking") else t("settings.passkeys.keys.turn_on"))
                }
                TextButton(onClick = onCancel, enabled = !busy) {
                    Text(t("common.cancel"))
                }
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
    // Words from the language, order and clock from the device (localization.md 5).
    return me.lakko.app.l10n.DeviceFormats.mediumDateTime(ms, language)
}

/**
 * `Delete your account?` (UX 17.3), drawn by the app ([me.lakko.app.ui.components.LakkoDialog]): the
 * body, then what the account needs to prove, then the actions.
 *
 *   - An account with a passkey: no field; `You will be asked for your
 *     passkey.`; `Delete account` opens ONE passkey sheet, the gesture bound
 *     to this deletion (ADR-0014), which is itself the proof. No passkey
 *     sign-in runs first, and a live proof does not skip the gesture.
 *   - No passkey, proof live: body and buttons.
 *   - No passkey, no proof: the proof block and `I do not know my password`.
 *   - `I do not know my password`: the field and `Delete account` give way
 *     to the "cannot" line and `I know my password`.
 *
 * A wrong password leaves the dialog open with its line; nothing is deleted
 * until the typed password (no passkey) or the gesture (a passkey account)
 * passed.
 */
/**
 * ADR-0098 section 4 - the usual Delete account confirmation, opened from
 * the suspended screen of a device that holds no chats of the account
 * (Settings cannot be reached there). [viewModel] deletes with the
 * restricted session ([AuthRepository.accountForDeletion]).
 */
@Composable
public fun DeleteAccountConfirmation(viewModel: PasskeysViewModel, onClose: () -> Unit) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    DeleteAccountDialog(
        viewModel = viewModel,
        state = state,
        onClose = {
            viewModel.clearDeleteError()
            onClose()
        },
    )
}

@Composable
private fun DeleteAccountDialog(
    viewModel: PasskeysViewModel,
    state: PasskeysUiState,
    onClose: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val proof = me.lakko.app.ui.auth.rememberProofController(
        viewModel.repository,
        me.lakko.app.ui.auth.ProofSurface.DeleteAccount,
    )
    var saidNoPassword by remember { mutableStateOf(false) }
    val hasPasskey = proof.hasPasskey ?: state.credentials.isNotEmpty()
    val view = proof.view()
    val panel = me.lakko.app.ui.auth.deletePanel(
        hasPasskey = hasPasskey,
        proofLive = proof.proofLive,
        saidNoPassword = saidNoPassword,
    )
    val working = state.busy || view.working
    val delete: () -> Unit = delete@{
        if (!me.lakko.app.ui.auth.deleteButtonEnabled(panel, proof.view(), state.busy)) return@delete
        scope.launch {
            // A passkey account proves with the one gesture bound to this
            // deletion, inside `deleteAccount`. Without a passkey the proof
            // comes first: the typed password.
            val proved = when (panel) {
                me.lakko.app.ui.auth.DeletePanel.PasskeyNotice -> true
                else -> proof.ensure(context)
            }
            if (proved) {
                viewModel.deleteAccount(context, hasPasskey = hasPasskey, onProofRequired = proof::onProofRefused)
            }
        }
    }
    me.lakko.app.ui.components.LakkoDialog(
        onDismissRequest = { if (!working) onClose() },
        title = t("settings.account.delete.title"),
        confirm = if (panel != me.lakko.app.ui.auth.DeletePanel.Cannot) {
            me.lakko.app.ui.components.LakkoDialogAction(
                label = t(me.lakko.app.ui.auth.deleteButtonLabel(deleting = state.busy, checking = view.working)),
                tone = me.lakko.app.ui.components.LakkoDialogTone.Danger,
                enabled = me.lakko.app.ui.auth.deleteButtonEnabled(panel, view, state.busy) && !view.working,
                onClick = delete,
            )
        } else {
            null
        },
        cancel = me.lakko.app.ui.components.LakkoDialogAction(
            label = t("common.cancel"),
            tone = me.lakko.app.ui.components.LakkoDialogTone.Ghost,
            enabled = !state.busy,
            onClick = onClose,
        ),
    ) {
        Text(t("settings.account.delete.body"))
        when (panel) {
            me.lakko.app.ui.auth.DeletePanel.PasskeyNotice -> {
                // The gesture's outcome is the dialog's own line below.
                Text(t("proof.passkey.notice"), style = MaterialTheme.typography.bodyMedium)
            }
            me.lakko.app.ui.auth.DeletePanel.NoBlock -> Unit
            me.lakko.app.ui.auth.DeletePanel.Block -> {
                me.lakko.app.ui.auth.ProofBlock(controller = proof, onSubmit = delete)
                TextButton(onClick = { saidNoPassword = true }, enabled = !working) {
                    Text(t("proof.no_password_link"))
                }
            }
            me.lakko.app.ui.auth.DeletePanel.Cannot -> {
                Text(t("proof.delete.cannot"), style = MaterialTheme.typography.bodyMedium)
                TextButton(onClick = { saidNoPassword = false }) { Text(t("proof.know_password_link")) }
            }
        }
        state.deleteError?.let {
            if (state.deleteCancelled) Text(it.text()) else Text(it.text(), color = MaterialTheme.colorScheme.error)
        }
    }
}
