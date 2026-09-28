// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.automirrored.filled.Notes
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.auth.base64Decode

/** The ways to bring the identity onto this phone, as the prompt lists them. */
private enum class RestoreWay { Phrase, AnotherDevice, Neither }

/**
 * ADR-0017 fresh-device prompt: the user is logged in but this phone holds
 * no identity yet. A list of ways (recovery phrase, another device, neither)
 * pushes the matching form; the phrase form is the entry grid with paste.
 * [RestoreIdentityPromptViewModel] keeps every behaviour: the phrase is
 * checked against the published identity before the session opens, and
 * "neither" creates a new identity behind an explicit warning.
 */
@Composable
public fun RestoreIdentityPrompt(
    viewModel: RestoreIdentityPromptViewModel,
    username: String,
    accountIdB64: String,
    onSignOut: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    var way by rememberSaveable { mutableStateOf<RestoreWay?>(null) }

    BackHandler(enabled = way != null && !state.busy) { way = null }

    when (way) {
        null -> AuthScaffold(title = "Restore your identity", onBack = null, modifier = modifier) {
            Lead(
                "You are logged in as @$username, but this phone does not have your " +
                    "identity yet. Choose how to bring it over.",
            )
            Spacer(Modifier.height(16.dp))
            MethodRow(
                icon = Icons.AutoMirrored.Filled.Notes,
                title = "Recovery phrase",
                subtitle = "Type or paste the 24 words you saved",
                onClick = { way = RestoreWay.Phrase },
            )
            MethodRow(
                icon = Icons.Filled.Devices,
                title = "Another device",
                subtitle = "Scan a code with a phone or computer that is still signed in",
                onClick = { way = RestoreWay.AnotherDevice },
            )
            MethodRow(
                icon = Icons.Outlined.ErrorOutline,
                title = "I don't have either",
                subtitle = "Start with a new identity",
                onClick = { way = RestoreWay.Neither },
            )
            FormError(state.error)
            Spacer(Modifier.height(24.dp))
            FooterLink(prompt = "Not your account?", action = "Log out", onClick = onSignOut, enabled = !state.busy)
        }

        RestoreWay.Phrase -> AuthScaffold(
            title = "Recovery phrase",
            onBack = { if (!state.busy) way = null },
            modifier = modifier,
        ) {
            Lead("Enter the 24 words in order. Lakko checks them against your account before continuing.")
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
                label = "Restore",
                busy = state.busy,
                busyLabel = "Checking...",
                onClick = viewModel::onSubmit,
            )
            Spacer(Modifier.height(16.dp))
        }

        RestoreWay.AnotherDevice -> AuthScaffold(
            title = "Another device",
            onBack = { if (!state.busy) way = null },
            modifier = modifier,
        ) {
            // ADR-0056 — the code names the account by the login handle of
            // its username; an account with none (a guest) cannot pair.
            val handle = remember(username) {
                me.lakko.app.devices.ownPairingHandle(username, me.lakko.app.auth.UniffiLoginHandleSource)
            }
            if (handle == null) {
                Lead(me.lakko.app.devices.PAIRING_NO_USERNAME_COPY)
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

        RestoreWay.Neither -> AuthScaffold(
            title = "Start with a new identity",
            onBack = { if (!state.busy) way = null },
            modifier = modifier,
        ) {
            Lead(
                "Without the phrase or another device, your old identity cannot be " +
                    "brought back. Lakko can create a new one for this account.",
            )
            Spacer(Modifier.height(12.dp))
            Lead(
                "People you already talk to will be asked to verify you again, " +
                    "messages on your lost devices stay out of reach, and you will get " +
                    "a new recovery phrase to save.",
            )
            FormError(state.error)
            Spacer(Modifier.height(24.dp))
            OutlinedButton(
                onClick = viewModel::startFresh,
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(if (state.busy) "Creating..." else "Create a new identity")
            }
            Spacer(Modifier.height(16.dp))
        }
    }
}
