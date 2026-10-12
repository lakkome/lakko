// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text

/**
 * Create account (docs/ux/client-redesign.md section 5.6): username with the
 * live availability hint, display name, password, confirm password, Create
 * account. No recovery phrase is shown (ADR-0067 D2); the "Protect your
 * account" step that follows ([ProtectAccountStep]) is rendered by the app
 * root from `AuthRepository.protectStepOwed` once the account exists.
 */
@Composable
public fun SignupScreen(
    viewModel: SignupViewModel,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()

    AuthScaffold(title = t("auth.signup.title"), onBack = onBack, modifier = modifier, showLanguageButton = true) {
        Spacer(Modifier.height(8.dp))
        OutlinedTextField(
            value = state.username,
            onValueChange = viewModel::onUsernameChange,
            label = { Text(t("auth.username.label")) },
            singleLine = true,
            enabled = !state.busy,
            isError = state.usernameError != null,
            supportingText = {
                val line = (state.usernameError ?: state.usernameHint)?.text()
                    ?: t("auth.signup.username.hint")
                Text(line)
            },
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrectEnabled = false,
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Next,
            ),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(8.dp))
        OutlinedTextField(
            value = state.displayName,
            onValueChange = viewModel::onDisplayNameChange,
            label = { Text(t("auth.signup.name.label")) },
            placeholder = {
                Text(state.username.trim().removePrefix("@").ifEmpty { t("common.optional") })
            },
            singleLine = true,
            enabled = !state.busy,
            isError = state.displayNameError != null,
            supportingText = {
                Text(state.displayNameError?.text() ?: t("auth.signup.name.hint"))
            },
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.Words,
                imeAction = ImeAction.Next,
            ),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(8.dp))
        PasswordField(
            value = state.password,
            onValueChange = viewModel::onPasswordChange,
            label = t("auth.password.label"),
            enabled = !state.busy,
            supportingText = t(
                "auth.password.hint",
                emptyMap(),
                mapOf("count" to SignupViewModel.MIN_PASSWORD_LEN.toULong()),
            ),
        )
        Spacer(Modifier.height(8.dp))
        PasswordField(
            value = state.confirm,
            onValueChange = viewModel::onConfirmChange,
            label = t("auth.password.confirm.label"),
            enabled = !state.busy,
            imeAction = ImeAction.Done,
            onDone = viewModel::onSubmit,
        )
        FormError(state.error)
        Spacer(Modifier.height(24.dp))
        // ADR-0054 UX section 1 - the working row: a disabled row with a
        // spinner, one line below it, and Cancel while the one heavy piece of
        // work runs. A network-wide signup limit turns the button into
        // `Try again`; the form keeps its contents either way.
        BusyButton(
            label = if (state.rateLimited) t("common.try_again") else t("auth.signup.submit"),
            busy = state.busy,
            busyLabel = t("auth.signup.working.title"),
            onClick = viewModel::onSubmit,
        )
        if (state.busy) {
            Spacer(Modifier.height(8.dp))
            Lead(t("auth.signup.working.line"))
            if (state.solving) {
                TextButton(onClick = viewModel::onCancel, modifier = Modifier.fillMaxWidth()) {
                    Text(t("common.cancel"))
                }
            }
        }
        Spacer(Modifier.height(12.dp))
        Lead(t("auth.signup.lede"))
        Spacer(Modifier.height(16.dp))
    }
}
