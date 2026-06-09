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
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * ADR-0017 fresh-device fail-safe prompt — mirror of
 * `apps/web/src/auth/RestoreIdentityPrompt.tsx`.
 *
 * Rendered when the auth state is
 * [me.lakko.app.auth.AuthState.NeedsRecoveryPhrase]: the user
 * authenticated successfully (password / passkey) but this device has
 * no cached AccountIdentity (phrase) seed, so we cannot rebuild the
 * canonical identity. We MUST NOT derive it from the export key (wrong
 * identity now) nor silently mint a fresh one (breaks every contact's
 * pin) — so we ask for the recovery phrase. On submit
 * [RestoreIdentityPromptViewModel] drives
 * [me.lakko.app.auth.AuthRepository.provideRecoveryPhrase], which
 * HARD-BLOCK verifies the phrase against the published identity bundle
 * before finishing the session.
 *
 * [onSignOut] lets the user back out (e.g. they typed the wrong
 * account) — it routes to the host's logout, returning to the Anon
 * flow.
 */
@Composable
public fun RestoreIdentityPrompt(
    viewModel: RestoreIdentityPromptViewModel,
    username: String,
    onSignOut: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
    ) {
        Text(
            text = "Restore your identity",
            style = MaterialTheme.typography.headlineMedium,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            text = "New device — enter your recovery phrase to restore your " +
                "secure identity. We verify it against your published " +
                "identity before continuing, so your contacts keep " +
                "trusting you without re-verifying.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(4.dp))
        Text(
            text = "Signed in as @$username",
            style = MaterialTheme.typography.bodyMedium,
        )
        Spacer(Modifier.height(16.dp))

        OutlinedTextField(
            value = state.phrase,
            onValueChange = viewModel::onPhraseChange,
            label = { Text("Recovery phrase (24 words)") },
            placeholder = { Text("abandon abandon abandon … art") },
            singleLine = false,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(
                // The BIP39 wordlist is lowercase; autocorrect would
                // helpfully rewrite valid words — disable both.
                capitalization = KeyboardCapitalization.None,
                autoCorrect = false,
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Done,
            ),
            textStyle = MaterialTheme.typography.bodyMedium.copy(
                fontFamily = FontFamily.Monospace,
            ),
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = 120.dp),
        )

        if (state.error != null) {
            Spacer(Modifier.height(12.dp))
            Text(
                text = state.error!!,
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
                Text("Verifying…")
            } else {
                Text("Restore identity")
            }
        }

        Spacer(Modifier.height(16.dp))

        Row(
            horizontalArrangement = Arrangement.Center,
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(
                text = "Not your account? ",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                text = "Sign out",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier
                    .padding(start = 4.dp)
                    .clickable(enabled = !state.busy, onClick = onSignOut),
            )
        }
    }
}
