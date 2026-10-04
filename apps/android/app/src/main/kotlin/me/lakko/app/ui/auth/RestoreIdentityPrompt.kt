// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.automirrored.filled.Notes
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.auth.RestoreNotice
import me.lakko.app.auth.base64Decode
import me.lakko.app.webauthn.restoreOffersPasskey
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text

/** The ways to bring the keys onto this phone, in the order the prompt
 *  lists them. Never a route that only signs in. */
private enum class RestoreWay { AnotherDevice, Passkey, Phrase, Neither }

/**
 * "Restore your keys": the person is signed in but this phone does not hold
 * the account's keys yet. The rows, in order: another device, a passkey
 * (hidden when the account is known to have no passkey that carries keys),
 * the 12-word phrase, and starting over with a new identity. After a passkey
 * sign-in that brought no keys, the first line says so.
 * [RestoreIdentityPromptViewModel] keeps every behaviour: whatever brings
 * the keys is checked against the published identity before the session
 * opens, and "start over" creates a new identity behind an explicit warning.
 */
@Composable
public fun RestoreIdentityPrompt(
    viewModel: RestoreIdentityPromptViewModel,
    username: String,
    accountIdB64: String,
    onSignOut: () -> Unit,
    modifier: Modifier = Modifier,
    /** Why the prompt is up after a passkey sign-in, or `null`. */
    restoreNotice: RestoreNotice? = null,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    var way by rememberSaveable { mutableStateOf<RestoreWay?>(null) }
    val context = LocalContext.current
    // The confirmation word of the reset warning: kept across the
    // "Resetting..." state so a wrong password does not ask for it again.
    var resetTyped by rememberSaveable { mutableStateOf("") }
    val closeWay: () -> Unit = {
        if (way == RestoreWay.Neither) {
            resetTyped = ""
            viewModel.onResetClosed()
        }
        way = null
    }

    BackHandler(enabled = way != null && !state.busy) { closeWay() }
    // UX 15.1a: opening the warning decides whether it asks for the password.
    // This runs again after a configuration change; the view model knows the
    // warning is already open and keeps what was typed.
    androidx.compose.runtime.LaunchedEffect(way) {
        if (way == RestoreWay.Neither) viewModel.onResetOpened()
    }
    // Leaving the prompt drops the typed password and username. A rotation
    // also disposes the composition, and must not.
    androidx.compose.runtime.DisposableEffect(Unit) {
        onDispose {
            if (context.findActivity()?.isChangingConfigurations != true) viewModel.onResetClosed()
        }
    }
    val usernameKnown = username.isNotBlank()

    when (way) {
        null -> AuthScaffold(title = t("restore_identity.title"), onBack = null, modifier = modifier) {
            when (restoreNotice) {
                RestoreNotice.PasskeyNoKeys -> {
                    Text(t("restore_identity.from_passkey_no_keys"), style = MaterialTheme.typography.bodyLarge)
                    Spacer(Modifier.height(8.dp))
                }
                RestoreNotice.PasskeyUnusable -> {
                    Text(t("restore_identity.from_passkey_unusable"), style = MaterialTheme.typography.bodyLarge)
                    Spacer(Modifier.height(8.dp))
                }
                null -> Unit
            }
            Lead(t("restore_identity.lede", mapOf("username" to "@$username")))
            Spacer(Modifier.height(16.dp))
            MethodRow(
                icon = Icons.Filled.Devices,
                title = t("recovery.method.device.title"),
                subtitle = t("recovery.method.device.sub"),
                onClick = { way = RestoreWay.AnotherDevice },
            )
            if (restoreOffersPasskey(state.passkeysWithKeys)) {
                MethodRow(
                    icon = Icons.Filled.Key,
                    title = t("recovery.method.passkey.title"),
                    subtitle = t("restore_identity.method.passkey.sub"),
                    onClick = { way = RestoreWay.Passkey },
                )
            }
            MethodRow(
                icon = Icons.AutoMirrored.Filled.Notes,
                title = t("recovery.method.phrase.title"),
                subtitle = t("recovery.method.phrase.restore_sub"),
                onClick = { way = RestoreWay.Phrase },
            )
            MethodRow(
                icon = Icons.Outlined.ErrorOutline,
                title = t("restore_identity.lost.summary"),
                subtitle = t("restore_identity.lost.sub"),
                onClick = { way = RestoreWay.Neither },
            )
            FormError(state.error)
            Spacer(Modifier.height(24.dp))
            FooterLink(
                prompt = t("restore_identity.not_you"),
                action = t("restore_identity.log_out"),
                onClick = onSignOut,
                enabled = !state.busy,
            )
        }

        RestoreWay.Passkey -> AuthScaffold(
            title = t("recovery.method.passkey.title"),
            onBack = { if (!state.busy) way = null },
            modifier = modifier,
        ) {
            Lead(t("recovery.passkey.intro"))
            FormError(state.error)
            Spacer(Modifier.height(24.dp))
            BusyButton(
                label = t("recovery.passkey.submit"),
                busy = state.busy,
                busyLabel = t("recovery.passkey.waiting"),
                // A passkey that brought no keys returns to the list, whose
                // first line then says so.
                onClick = { viewModel.restoreWithPasskey(context) { way = null } },
            )
            Spacer(Modifier.height(16.dp))
        }

        RestoreWay.Phrase -> AuthScaffold(
            title = t("recovery.method.phrase.title"),
            onBack = { if (!state.busy) way = null },
            modifier = modifier,
        ) {
            Lead(t("restore_identity.phrase_intro"))
            Spacer(Modifier.height(16.dp))
            RecoveryPhraseEntryGrid(
                phrase = state.phrase,
                onPhraseChange = viewModel::onPhraseChange,
                enabled = !state.busy,
                onDone = viewModel::onSubmit,
            )
            FormError(state.error)
            Spacer(Modifier.height(24.dp))
            BusyButton(
                label = t("restore_identity.submit"),
                busy = state.busy,
                busyLabel = t("restore_identity.submitting"),
                onClick = viewModel::onSubmit,
            )
            Spacer(Modifier.height(16.dp))
        }

        RestoreWay.AnotherDevice -> AuthScaffold(
            title = t("recovery.method.device.title"),
            onBack = { if (!state.busy) way = null },
            modifier = modifier,
        ) {
            // ADR-0056 — the code names the account by the login handle of
            // its username; an account with none (a guest) cannot pair.
            val handle = remember(username) {
                me.lakko.app.devices.ownPairingHandle(username, me.lakko.app.auth.UniffiLoginHandleSource)
            }
            if (handle == null) {
                Lead(me.lakko.app.devices.PAIRING_NO_USERNAME_COPY.text())
            } else {
                me.lakko.app.ui.devices.RestoreViaDevicePanel(
                    loginHandle = handle,
                    known = me.lakko.app.devices.PairingOrchestrator.KnownAccount(
                        accountIdBytes = base64Decode(accountIdB64),
                        accountIdB64 = accountIdB64,
                    ),
                    onRecovered = { outcome -> viewModel.completeFromPairing(outcome) },
                )
            }
            Spacer(Modifier.height(16.dp))
        }

        // ADR-0065 / docs/ux/identity-recovery.md 15.1 - the warning before a
        // reset of the account keys, with the typed confirmation. Nothing is
        // sent until the button is tapped; the bullets are unconditional (the
        // device does not know how many devices or passkeys the account has).
        RestoreWay.Neither -> if (state.busy) {
            // A full-screen "Resetting..." with no cancel: the server step is
            // one operation and cannot be half done.
            AuthScaffold(title = t("restore_identity.lost.sub"), onBack = null, modifier = modifier) {
                Lead(t("restore_identity.reset.resetting"))
            }
        } else {
            AuthScaffold(
                title = t("restore_identity.lost.sub"),
                onBack = closeWay,
                modifier = modifier,
            ) {
                Lead(t("restore_identity.reset.lede"))
                Spacer(Modifier.height(12.dp))
                Text(t("restore_identity.reset.what_it_does"), style = MaterialTheme.typography.bodyMedium)
                Spacer(Modifier.height(4.dp))
                for (key in RESET_CONSEQUENCE_KEYS) {
                    Text("\u2022 " + t(key), style = MaterialTheme.typography.bodyMedium)
                }
                Spacer(Modifier.height(16.dp))
                // UX 15.1a - the password, proven now: shown only when no
                // live reset grant is held. Never pre-filled.
                if (state.resetNeedsPassword) {
                    // The device does not know the account's username (a
                    // passkey sign-in that named none): the password sign-in
                    // needs it. The sign-in screen's field and label.
                    if (resetShowsUsername(state.resetNeedsPassword, usernameKnown)) {
                        UsernameField(
                            value = state.resetUsername,
                            onValueChange = viewModel::onResetUsernameChange,
                            enabled = true,
                        )
                    }
                    PasswordField(
                        value = state.resetPassword,
                        onValueChange = viewModel::onResetPasswordChange,
                        label = t("restore_identity.reset.password_label"),
                        enabled = true,
                        supportingText = t("restore_identity.reset.password_help"),
                    )
                    FormError(state.resetPasswordError)
                    Spacer(Modifier.height(16.dp))
                }
                Text(
                    t("restore_identity.reset.confirm_label", mapOf("word" to RESET_CONFIRMATION_WORD)),
                    style = MaterialTheme.typography.bodyMedium,
                )
                Spacer(Modifier.height(4.dp))
                androidx.compose.material3.OutlinedTextField(
                    value = resetTyped,
                    onValueChange = { resetTyped = it },
                    singleLine = true,
                    keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                        capitalization = androidx.compose.ui.text.input.KeyboardCapitalization.None,
                        autoCorrectEnabled = false,
                        keyboardType = androidx.compose.ui.text.input.KeyboardType.Ascii,
                        imeAction = androidx.compose.ui.text.input.ImeAction.Done,
                    ),
                    modifier = Modifier.fillMaxWidth(),
                )
                FormError(state.error)
                Spacer(Modifier.height(24.dp))
                androidx.compose.material3.Button(
                    onClick = viewModel::startFresh,
                    enabled = resetButtonEnabled(
                        typedWord = resetTyped,
                        needsPassword = state.resetNeedsPassword,
                        password = state.resetPassword,
                        usernameKnown = usernameKnown,
                        username = state.resetUsername,
                    ),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(t("restore_identity.lost.create"))
                }
                Spacer(Modifier.height(8.dp))
                OutlinedButton(
                    onClick = closeWay,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(t("common.cancel"))
                }
                Spacer(Modifier.height(16.dp))
            }
        }
    }
}

private tailrec fun android.content.Context.findActivity(): android.app.Activity? = when (this) {
    is android.app.Activity -> this
    is android.content.ContextWrapper -> baseContext.findActivity()
    else -> null
}

/** The word the person types to confirm a reset (UX 15.1): the same Latin
 *  letters in every language, like the slot numbers of the grid. */
public const val RESET_CONFIRMATION_WORD: String = "RESET"

/** Whether the typed confirmation enables the reset button: the exact word,
 *  surrounding whitespace ignored, case as typed (the line names it in
 *  capitals). */
public fun resetConfirmationMatches(typed: String): Boolean = typed.trim() == RESET_CONFIRMATION_WORD

/** The consequence bullets of the warning, in the order of UX 15.1. */
private val RESET_CONSEQUENCE_KEYS: List<String> = listOf(
    "restore_identity.reset.effect_devices",
    "restore_identity.reset.effect_passkeys",
    "restore_identity.reset.effect_contacts",
    "restore_identity.reset.effect_messages",
    "restore_identity.reset.effect_password",
)
