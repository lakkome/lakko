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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * Compose signup screen — Mirror of `apps/web/src/routes/Signup.tsx`.
 *
 * Two stages, switched on [SignupUiState.stage]:
 *
 *   1. [Stage.Form]   — username + password + confirm-password.
 *   2. [Stage.Phrase] — 24-word BIP39 recovery phrase + the
 *                      "I have written it down" gate.
 *
 * The screen is purely a presentation layer over [SignupViewModel].
 * All state lives in the VM; the composable only renders + forwards
 * user events. This split:
 *
 *   * keeps the screen testable via Compose UI tests (1.15d7
 *     instrumented suite) without spinning up a Ktor mock;
 *   * lets the screen survive configuration changes (screen
 *     rotation, font-scale changes, dark-mode toggle) without
 *     re-running OPAQUE math — Compose recreates the composable
 *     tree, the VM persists.
 *
 * [onAuthenticated] is invoked exactly once after the user
 * acknowledges the recovery phrase. The MainActivity-level router
 * uses that callback to swap to the home destination.
 */
@Composable
public fun SignupScreen(
    viewModel: SignupViewModel,
    onAuthenticated: () -> Unit,
    onNavigateToLogin: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()

    when (val stage = state.stage) {
        Stage.Form -> SignupForm(
            state = state,
            onUsernameChange = viewModel::onUsernameChange,
            onPasswordChange = viewModel::onPasswordChange,
            onConfirmChange = viewModel::onConfirmChange,
            onSubmit = viewModel::onSubmit,
            onNavigateToLogin = onNavigateToLogin,
            modifier = modifier,
        )

        is Stage.Phrase -> RecoveryPhrasePanel(
            phrase = stage.phrase,
            acknowledged = state.acknowledged,
            onAcknowledgeChange = viewModel::onAcknowledgeChange,
            onAcknowledge = {
                viewModel.onAcknowledge()
                onAuthenticated()
            },
            modifier = modifier,
        )
    }
}

// =============================================================================
// Stage 1: signup form
// =============================================================================

@Composable
private fun SignupForm(
    state: SignupUiState,
    onUsernameChange: (String) -> Unit,
    onPasswordChange: (String) -> Unit,
    onConfirmChange: (String) -> Unit,
    onSubmit: () -> Unit,
    onNavigateToLogin: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            text = "Create a Lakko account",
            style = MaterialTheme.typography.headlineMedium,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            text = "OPAQUE-PAKE registration. The password never leaves your " +
                "device — the server only sees an OPRF-blinded envelope. After " +
                "signup you will be shown a 24-word recovery phrase once; write " +
                "it down before continuing.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))

        OutlinedTextField(
            value = state.username,
            onValueChange = onUsernameChange,
            label = { Text("Username") },
            singleLine = true,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Text),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(12.dp))

        OutlinedTextField(
            value = state.password,
            onValueChange = onPasswordChange,
            label = { Text("Password") },
            singleLine = true,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
            visualTransformation = PasswordVisualTransformation(),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(12.dp))

        OutlinedTextField(
            value = state.confirm,
            onValueChange = onConfirmChange,
            label = { Text("Confirm password") },
            singleLine = true,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
            visualTransformation = PasswordVisualTransformation(),
            modifier = Modifier.fillMaxWidth(),
        )

        if (state.error != null) {
            Spacer(Modifier.height(12.dp))
            Text(
                text = state.error,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
            )
        }

        Spacer(Modifier.height(24.dp))

        Button(
            onClick = onSubmit,
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
                Text("Creating account…")
            } else {
                Text("Sign up")
            }
        }

        Spacer(Modifier.height(16.dp))

        Row(
            horizontalArrangement = Arrangement.Center,
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(
                text = "Already have an account? ",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            // Click target wired but inactive in 1.15d4 — the
            // Activity supplies a no-op `onNavigateToLogin` until
            // 1.15d5 lands the [LoginScreen]. We keep the link
            // visible so the user can see the path-out exists;
            // tapping it is a no-op rather than a missing surface.
            Text(
                text = "Log in",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier
                    .padding(start = 4.dp)
                    .clickable(onClick = onNavigateToLogin),
            )
        }
    }
}

// =============================================================================
// Stage 2: recovery phrase display
// =============================================================================

@Composable
private fun RecoveryPhrasePanel(
    phrase: String,
    acknowledged: Boolean,
    onAcknowledgeChange: (Boolean) -> Unit,
    onAcknowledge: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val words = phrase.split(Regex("\\s+")).filter { it.isNotEmpty() }

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
    ) {
        Text(
            text = "Save your recovery phrase",
            style = MaterialTheme.typography.headlineMedium,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            text = "These 24 words are the ONLY way to recover your account if " +
                "you forget your password. Write them down on paper, in order. " +
                "Do not screenshot. Do not store in plain text. We cannot show " +
                "them to you again.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(16.dp))

        Card(
            colors = CardDefaults.cardColors(
                containerColor = MaterialTheme.colorScheme.surfaceVariant,
            ),
            shape = RoundedCornerShape(8.dp),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Column(
                modifier = Modifier.padding(16.dp),
            ) {
                // Two-column grid laid out manually — Compose has no
                // first-party CSS-grid analogue, but a pair of
                // Columns inside a Row is the standard idiom for a
                // small fixed grid (24 items, two columns of 12).
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        // 0..11 → column 1.
                        words.take(WORDS_PER_COLUMN).forEachIndexed { i, word ->
                            WordRow(index = i + 1, word = word)
                        }
                    }
                    Column(modifier = Modifier.weight(1f)) {
                        // 12..23 → column 2.
                        words.drop(WORDS_PER_COLUMN).forEachIndexed { i, word ->
                            WordRow(index = i + 1 + WORDS_PER_COLUMN, word = word)
                        }
                    }
                }
            }
        }

        Spacer(Modifier.height(24.dp))

        Row(
            verticalAlignment = Alignment.Top,
        ) {
            Checkbox(
                checked = acknowledged,
                onCheckedChange = onAcknowledgeChange,
            )
            Spacer(Modifier.width(8.dp))
            Text(
                text = "I have written down my 24-word recovery phrase. I " +
                    "understand that losing it means losing access to this " +
                    "account.",
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.padding(top = 12.dp),
            )
        }

        Spacer(Modifier.height(16.dp))

        Button(
            onClick = onAcknowledge,
            enabled = acknowledged,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Done — take me to Lakko")
        }
    }
}

@Composable
private fun WordRow(index: Int, word: String) {
    Row(
        modifier = Modifier.padding(vertical = 2.dp),
    ) {
        Text(
            text = "%2d.".format(index),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            fontFamily = FontFamily.Monospace,
            modifier = Modifier.width(28.dp),
        )
        Text(
            text = word,
            style = MaterialTheme.typography.bodyMedium,
            fontFamily = FontFamily.Monospace,
        )
    }
}

private const val WORDS_PER_COLUMN = 12
