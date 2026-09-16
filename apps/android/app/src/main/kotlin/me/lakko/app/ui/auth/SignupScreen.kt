// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * Create account (docs/ux/client-redesign.md section 5.6): username with the
 * live availability hint, display name, password, confirm password, Create
 * account. The recovery-phrase step that follows is [RecoveryPhraseOnboarding],
 * rendered by the app root from [SignupPhraseGate] once the account exists.
 */
@Composable
public fun SignupScreen(
    viewModel: SignupViewModel,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()

    AuthScaffold(title = "Create account", onBack = onBack, modifier = modifier) {
        Spacer(Modifier.height(8.dp))
        OutlinedTextField(
            value = state.username,
            onValueChange = viewModel::onUsernameChange,
            label = { Text("Username") },
            singleLine = true,
            enabled = !state.busy,
            isError = state.usernameError != null,
            supportingText = {
                val line = state.usernameError ?: state.usernameHint
                    ?: "Latin letters, digits and _, 3-32 characters"
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
            label = { Text("Your name") },
            placeholder = {
                Text(state.username.trim().removePrefix("@").ifEmpty { "Optional" })
            },
            singleLine = true,
            enabled = !state.busy,
            isError = state.displayNameError != null,
            supportingText = {
                Text(state.displayNameError ?: "Optional. Shown to people you talk to.")
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
            label = "Password",
            enabled = !state.busy,
            supportingText = "At least ${SignupViewModel.MIN_PASSWORD_LEN} characters",
        )
        Spacer(Modifier.height(8.dp))
        PasswordField(
            value = state.confirm,
            onValueChange = viewModel::onConfirmChange,
            label = "Confirm password",
            enabled = !state.busy,
            imeAction = ImeAction.Done,
            onDone = viewModel::onSubmit,
        )
        FormError(state.error)
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = "Create account",
            busy = state.busy,
            busyLabel = "Creating account...",
            onClick = viewModel::onSubmit,
        )
        Spacer(Modifier.height(12.dp))
        Lead("You will get a 24-word recovery phrase next. It is the only way back in if you lose this phone and your password.")
        Spacer(Modifier.height(16.dp))
    }
}
