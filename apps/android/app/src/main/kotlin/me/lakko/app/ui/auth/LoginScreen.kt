// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * Log in (docs/ux/client-redesign.md section 5.6): username, password with
 * a show / hide toggle, Log in, Use passkey, Forgot password. One small link
 * opens the plain-language sheet on how the password is protected; nothing
 * on the screen explains a protocol. [LoginViewModel] keeps every behaviour,
 * including the deliberately identical message for a wrong username and a
 * wrong password.
 */
@Composable
public fun LoginScreen(
    viewModel: LoginViewModel,
    onBack: () -> Unit,
    onNavigateToRecover: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    var explainerOpen by rememberSaveable { mutableStateOf(false) }

    AuthScaffold(title = "Log in", onBack = onBack, modifier = modifier) {
        Spacer(Modifier.height(8.dp))
        OutlinedTextField(
            value = state.username,
            onValueChange = viewModel::onUsernameChange,
            label = { Text("Username") },
            singleLine = true,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrectEnabled = false,
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Next,
            ),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(12.dp))
        PasswordField(
            value = state.password,
            onValueChange = viewModel::onPasswordChange,
            label = "Password",
            enabled = !state.busy,
            imeAction = ImeAction.Done,
            onDone = viewModel::onSubmit,
        )
        FormError(state.error)
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = "Log in",
            busy = state.busy,
            busyLabel = "Logging in...",
            onClick = viewModel::onSubmit,
        )
        Spacer(Modifier.height(8.dp))
        FilledTonalButton(
            onClick = viewModel::onPasskeyLogin,
            enabled = !state.busy,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Use passkey")
        }
        Spacer(Modifier.height(8.dp))
        TextButton(onClick = onNavigateToRecover, enabled = !state.busy) {
            Text("Forgot password?")
        }
        Spacer(Modifier.height(24.dp))
        TextButton(onClick = { explainerOpen = true }) {
            Text("How Lakko protects your password")
        }
    }

    if (explainerOpen) {
        PasswordExplainerSheet(onDismiss = { explainerOpen = false })
    }
}
