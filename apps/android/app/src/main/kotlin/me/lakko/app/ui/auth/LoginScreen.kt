// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * Compose login screen — mirror of `apps/web/src/routes/Login.tsx`.
 *
 * Single-stage form: username + password. Pre-fills the username
 * field from the cached value passed into [LoginViewModel]. On
 * submit, drives [me.lakko.app.auth.AuthRepository.login] which
 * flips the auth state — the top-level `AppRoot` then snaps
 * to [me.lakko.app.ui.home.HomeScreen].
 *
 * [onNavigateToSignup] and [onNavigateToRecover] are wired via
 * `Modifier.clickable` on the footer links. The Activity-side
 * router supplies handlers that flip the local "which anon screen"
 * toggle; Compose Navigation lands in 1.15d7 with a NavController.
 */
@Composable
public fun LoginScreen(
    viewModel: LoginViewModel,
    onNavigateToSignup: () -> Unit,
    onNavigateToRecover: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            text = "Log in to Lakko",
            style = MaterialTheme.typography.headlineMedium,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            text = "OPAQUE-PAKE login. Wrong username and wrong password produce " +
                "the same error — by design (the server cannot tell us which one " +
                "you got wrong without leaking whether the account exists).",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))

        OutlinedTextField(
            value = state.username,
            onValueChange = viewModel::onUsernameChange,
            label = { Text("Username") },
            singleLine = true,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Text),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(12.dp))

        OutlinedTextField(
            value = state.password,
            onValueChange = viewModel::onPasswordChange,
            label = { Text("Password") },
            singleLine = true,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
            visualTransformation = PasswordVisualTransformation(),
            modifier = Modifier.fillMaxWidth(),
        )

        // `state.error` reads from a delegated property
        // (`state` is `by collectAsStateWithLifecycle`), so the
        // smart-cast from `!= null` doesn't carry into the body.
        // Capture into a local first.
        val errorText = state.error
        if (errorText != null) {
            Spacer(Modifier.height(12.dp))
            Text(
                text = errorText,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
            )
        }

        Spacer(Modifier.height(24.dp))

        Button(
            onClick = viewModel::onSubmit,
            enabled = !state.busy,
            modifier = Modifier.fillMaxWidth(),
        ) {
            if (state.busy) {
                CircularProgressIndicator(
                    modifier = Modifier
                        .height(20.dp)
                        .width(20.dp),
                    strokeWidth = 2.dp,
                    color = MaterialTheme.colorScheme.onPrimary,
                )
                Spacer(Modifier.width(8.dp))
                Text("Signing in…")
            } else {
                Text("Log in")
            }
        }

        // Phase 2a2c2-android — passkey shortcut. Surfaced only
        // on a device this account has signed into before
        // (cached username pre-fill is the marker). Cache-miss
        // paths surface as inline error messages from
        // `loginWithPasskey`.
        if (state.username.isNotEmpty()) {
            Spacer(Modifier.height(8.dp))
            OutlinedButton(
                onClick = viewModel::onPasskeyLogin,
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Sign in with passkey")
            }
            Spacer(Modifier.height(4.dp))
            Text(
                text = "Uses the passkey you registered earlier on this device.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.fillMaxWidth(),
            )
        }

        Spacer(Modifier.height(16.dp))

        Row(
            horizontalArrangement = Arrangement.Center,
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(
                text = "New to Lakko? ",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                text = "Create an account",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier
                    .padding(start = 4.dp)
                    .clickable(onClick = onNavigateToSignup),
            )
        }

        Spacer(Modifier.height(8.dp))

        Row(
            horizontalArrangement = Arrangement.Center,
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(
                text = "Lost your password? ",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            // Click target wired but inactive in 1.15d5 — the
            // Activity supplies a no-op `onNavigateToRecover`
            // until 1.15d6 lands the [RecoverScreen]. Same
            // posture as the "Log in" link on [SignupScreen]
            // during 1.15d4.
            Text(
                text = "Recover with your 24-word phrase",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier
                    .padding(start = 4.dp)
                    .clickable(onClick = onNavigateToRecover),
            )
        }
    }
}
