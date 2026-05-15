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
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * Compose recover-from-mnemonic screen — mirror of
 * `apps/web/src/routes/Recover.tsx`.
 *
 * Two render paths:
 *
 *   * **No cached account** ([RecoverViewModel.cachedUsername]
 *     is `null`) — the recovery flow can't run without an
 *     `accountId` on disk. We show a "no cached account" stub
 *     with deep links to Signup / Login instead of a form the
 *     user would fill and fail at submit. Cross-device recovery
 *     (mnemonic-only) is a 1.15e / discovery-service problem.
 *   * **Cached account present** — single-stage form: 24-word
 *     phrase + new password + confirm. On submit drives
 *     [me.lakko.app.auth.AuthRepository.recover].
 *
 * The mnemonic textfield is intentionally a multi-line
 * `OutlinedTextField` with `singleLine = false` and a generous
 * `heightIn(min = 120.dp)`. The BIP39 spec lets the user paste
 * a phrase with any whitespace — newlines, tabs, runs of spaces
 * — so we don't constrain layout-wise. `KeyboardCapitalization.None`
 * + `KeyboardOptions(autoCorrect = false)` (folded into the
 * IME action) keep autocorrect from helpfully rewriting `abandon`
 * into `Abandon`.
 */
@Composable
public fun RecoverScreen(
    viewModel: RecoverViewModel,
    onNavigateToLogin: () -> Unit,
    onNavigateToSignup: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val cachedUsername = viewModel.cachedUsername

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
    ) {
        Text(
            text = "Recover account",
            style = MaterialTheme.typography.headlineMedium,
        )
        Spacer(Modifier.height(8.dp))

        if (cachedUsername == null) {
            NoCachedAccountPanel(
                onNavigateToSignup = onNavigateToSignup,
                onNavigateToLogin = onNavigateToLogin,
            )
        } else {
            RecoverForm(
                state = state,
                cachedUsername = cachedUsername,
                onPhraseChange = viewModel::onPhraseChange,
                onSharesInputChange = viewModel::onSharesInputChange,
                onModeChange = viewModel::onModeChange,
                onPasswordChange = viewModel::onPasswordChange,
                onConfirmChange = viewModel::onConfirmChange,
                onSubmit = viewModel::onSubmit,
                onNavigateToLogin = onNavigateToLogin,
            )
        }
    }
}

// =============================================================================
// Form
// =============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RecoverForm(
    state: RecoverUiState,
    cachedUsername: String,
    onPhraseChange: (String) -> Unit,
    onSharesInputChange: (String) -> Unit,
    onModeChange: (RecoverMode) -> Unit,
    onPasswordChange: (String) -> Unit,
    onConfirmChange: (String) -> Unit,
    onSubmit: () -> Unit,
    onNavigateToLogin: () -> Unit,
) {
    Text(
        text = "Recover your account using either the 24-word phrase or 3+ " +
            "social-recovery shares. We'll prove ownership to the server " +
            "and rotate the password envelope.",
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Spacer(Modifier.height(4.dp))
    Text(
        text = "Account on this device: @$cachedUsername",
        style = MaterialTheme.typography.bodyMedium,
    )
    Spacer(Modifier.height(16.dp))

    // 1.21c3 — mode toggle (Phrase / Shares). SegmentedButtonRow
    // is the Material3-native pattern for a two-mode form switch.
    SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
        SegmentedButton(
            selected = state.mode == RecoverMode.Phrase,
            onClick = { onModeChange(RecoverMode.Phrase) },
            shape = SegmentedButtonDefaults.itemShape(index = 0, count = 2),
            enabled = !state.busy,
        ) { Text("24-word phrase") }
        SegmentedButton(
            selected = state.mode == RecoverMode.Shares,
            onClick = { onModeChange(RecoverMode.Shares) },
            shape = SegmentedButtonDefaults.itemShape(index = 1, count = 2),
            enabled = !state.busy,
        ) { Text("Recovery shares") }
    }
    Spacer(Modifier.height(16.dp))

    if (state.mode == RecoverMode.Phrase) {
        OutlinedTextField(
            value = state.phrase,
            onValueChange = onPhraseChange,
            label = { Text("Recovery phrase (24 words)") },
            placeholder = { Text("abandon abandon abandon … art") },
            singleLine = false,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(
                // Autocorrect on a wordlist phrase would helpfully
                // rewrite valid BIP39 words into adjacent dictionary
                // entries — disable it. We also force lowercase since
                // the BIP39 wordlist is exclusively lowercase.
                capitalization = KeyboardCapitalization.None,
                autoCorrect = false,
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Next,
            ),
            textStyle = MaterialTheme.typography.bodyMedium.copy(
                fontFamily = FontFamily.Monospace,
            ),
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = 120.dp),
        )
    } else {
        OutlinedTextField(
            value = state.sharesInput,
            onValueChange = onSharesInputChange,
            label = { Text("Recovery shares (one per line)") },
            placeholder = {
                Text(
                    text = "3:abcdEFGH...\n" +
                        "1:zzzzABCD...\n" +
                        "5:mmmm0123...",
                )
            },
            singleLine = false,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrect = false,
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Next,
            ),
            textStyle = MaterialTheme.typography.bodyMedium.copy(
                fontFamily = FontFamily.Monospace,
            ),
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = 140.dp),
            supportingText = {
                Text(
                    "Paste at least 3 shares (Lakko default is 3-of-5). " +
                        "Each line: <index>:<base64>.",
                    style = MaterialTheme.typography.bodySmall,
                )
            },
        )
    }
    Spacer(Modifier.height(12.dp))

    OutlinedTextField(
        value = state.password,
        onValueChange = onPasswordChange,
        label = { Text("New password") },
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
        label = { Text("Confirm new password") },
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
            Text("Verifying…")
        } else {
            Text("Recover account")
        }
    }

    Spacer(Modifier.height(16.dp))

    Row(
        horizontalArrangement = Arrangement.Center,
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(
            text = "Remembered your password? ",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
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

// =============================================================================
// No cached account stub
// =============================================================================

@Composable
private fun NoCachedAccountPanel(
    onNavigateToSignup: () -> Unit,
    onNavigateToLogin: () -> Unit,
) {
    Text(
        text = "This device has no cached account, so there's no account id " +
            "for the recovery flow to target. Cross-device recovery " +
            "(using just the mnemonic from a fresh install) arrives in " +
            "Phase 1.15e together with the username discovery service.",
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Spacer(Modifier.height(24.dp))

    Row(
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Button(
            onClick = onNavigateToSignup,
            modifier = Modifier.weight(1f),
        ) {
            Text("Create account")
        }
        Button(
            onClick = onNavigateToLogin,
            modifier = Modifier.weight(1f),
        ) {
            Text("Log in")
        }
    }
}
