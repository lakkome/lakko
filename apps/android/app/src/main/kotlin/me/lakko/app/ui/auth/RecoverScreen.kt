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
import me.lakko.app.auth.ContactKind

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

        // ADR-0017 — recover now works on a FRESH device too: when no
        // account is cached, render the same form but with a username
        // field so [AuthRepository.recover] can resolve the account_id
        // via the discovery service.
        RecoverForm(
            state = state,
            cachedUsername = cachedUsername,
            onUsernameChange = viewModel::onUsernameChange,
            onPhraseChange = viewModel::onPhraseChange,
            onSharesInputChange = viewModel::onSharesInputChange,
            onModeChange = viewModel::onModeChange,
            onPasswordChange = viewModel::onPasswordChange,
            onConfirmChange = viewModel::onConfirmChange,
            onSubmit = viewModel::onSubmit,
            onLookupForDeviceRestore = viewModel::lookupForDeviceRestore,
            onCompleteFromDeviceRestore = viewModel::completeFromDeviceRestore,
            onTotpCodeChange = viewModel::onTotpCodeChange,
            onSubmitTotp = viewModel::onSubmitTotp,
            onContactKindChange = viewModel::onContactKindChange,
            onContactChange = viewModel::onContactChange,
            onContactCodeChange = viewModel::onContactCodeChange,
            onSendContactCode = viewModel::onSendContactCode,
            onSubmitContactCode = viewModel::onSubmitContactCode,
            onNavigateToLogin = onNavigateToLogin,
            onNavigateToSignup = onNavigateToSignup,
        )
    }
}

// =============================================================================
// Form
// =============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RecoverForm(
    state: RecoverUiState,
    cachedUsername: String?,
    onUsernameChange: (String) -> Unit,
    onPhraseChange: (String) -> Unit,
    onSharesInputChange: (String) -> Unit,
    onModeChange: (RecoverMode) -> Unit,
    onPasswordChange: (String) -> Unit,
    onConfirmChange: (String) -> Unit,
    onSubmit: () -> Unit,
    onLookupForDeviceRestore: () -> Unit,
    onCompleteFromDeviceRestore:
        suspend (me.lakko.app.devices.PairingOrchestrator.JoinRecoveryOutcome) -> Unit,
    onTotpCodeChange: (String) -> Unit,
    onSubmitTotp: () -> Unit,
    onContactKindChange: (Int) -> Unit,
    onContactChange: (String) -> Unit,
    onContactCodeChange: (String) -> Unit,
    onSendContactCode: () -> Unit,
    onSubmitContactCode: () -> Unit,
    onNavigateToLogin: () -> Unit,
    onNavigateToSignup: () -> Unit,
) {
    Text(
        text = "Choose how to prove this account is yours. Your 24-word " +
            "phrase or another signed-in device restores your full identity " +
            "— your contacts keep trusting you. An authenticator or email/SMS " +
            "code restores access only; if this device doesn't already hold " +
            "your identity, you'll then save a new recovery phrase.",
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Spacer(Modifier.height(4.dp))
    if (cachedUsername != null) {
        Text(
            text = "Account on this device: @$cachedUsername",
            style = MaterialTheme.typography.bodyMedium,
        )
        Spacer(Modifier.height(16.dp))
    } else if (
        state.mode != RecoverMode.AnotherDevice &&
        state.mode != RecoverMode.Authenticator &&
        state.mode != RecoverMode.Contact
    ) {
        // ADR-0017 — fresh-device recover: no account is cached, so
        // the user types their username to target the recover. (The
        // AnotherDevice + Authenticator + Contact modes render their
        // own username fields below.)
        Spacer(Modifier.height(12.dp))
        OutlinedTextField(
            value = state.username,
            onValueChange = onUsernameChange,
            label = { Text("Username") },
            placeholder = { Text("alice") },
            singleLine = true,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrect = false,
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Next,
            ),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(16.dp))
    }

    // 1.21c3 — mode toggle (Phrase / Shares; ADR-0018 adds
    // AnotherDevice). SegmentedButtonRow is the Material3-native
    // pattern for a small-mode form switch.
    SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
        SegmentedButton(
            selected = state.mode == RecoverMode.Phrase,
            onClick = { onModeChange(RecoverMode.Phrase) },
            shape = SegmentedButtonDefaults.itemShape(index = 0, count = 5),
            enabled = !state.busy,
        ) { Text("Phrase") }
        SegmentedButton(
            selected = state.mode == RecoverMode.Shares,
            onClick = { onModeChange(RecoverMode.Shares) },
            shape = SegmentedButtonDefaults.itemShape(index = 1, count = 5),
            enabled = !state.busy,
        ) { Text("Shares") }
        SegmentedButton(
            selected = state.mode == RecoverMode.AnotherDevice,
            onClick = { onModeChange(RecoverMode.AnotherDevice) },
            shape = SegmentedButtonDefaults.itemShape(index = 2, count = 5),
            enabled = !state.busy,
        ) { Text("Device") }
        SegmentedButton(
            selected = state.mode == RecoverMode.Authenticator,
            onClick = { onModeChange(RecoverMode.Authenticator) },
            shape = SegmentedButtonDefaults.itemShape(index = 3, count = 5),
            enabled = !state.busy,
        ) { Text("Auth") }
        SegmentedButton(
            selected = state.mode == RecoverMode.Contact,
            onClick = { onModeChange(RecoverMode.Contact) },
            shape = SegmentedButtonDefaults.itemShape(index = 4, count = 5),
            enabled = !state.busy,
        ) { Text("Email/SMS") }
    }
    Spacer(Modifier.height(16.dp))

    if (state.mode == RecoverMode.Authenticator) {
        // ADR-0018 factor (c) — recover ACCESS with a 6-digit
        // authenticator code. Identity then restores via the
        // post-ACCESS state machine (cached seed -> straight to the
        // session; otherwise the restore-identity prompt).
        OutlinedTextField(
            value = state.username,
            onValueChange = onUsernameChange,
            label = { Text("Username") },
            placeholder = { Text("alice") },
            singleLine = true,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrect = false,
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Next,
            ),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(12.dp))
        OutlinedTextField(
            value = state.totpCode,
            onValueChange = onTotpCodeChange,
            label = { Text("6-digit code") },
            placeholder = { Text("123456") },
            singleLine = true,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(
                keyboardType = KeyboardType.Number,
                imeAction = ImeAction.Done,
            ),
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
        Spacer(Modifier.height(16.dp))
        Button(
            onClick = onSubmitTotp,
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
            } else {
                Text("Recover with code")
            }
        }
    }

    if (state.mode == RecoverMode.Contact) {
        // ADR-0018 factor (d) — recover ACCESS with a one-time code
        // sent to a previously-enrolled phone / email. Two steps:
        // request (username + kind + contact -> send) then code
        // (6-digit -> finish). Identity then restores via the
        // post-ACCESS state machine (cached seed -> straight to the
        // session; otherwise the restore-identity prompt).
        OutlinedTextField(
            value = state.username,
            onValueChange = onUsernameChange,
            label = { Text("Username") },
            placeholder = { Text("alice") },
            singleLine = true,
            enabled = !state.busy && state.contactStep == ContactRecoverStep.Request,
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrect = false,
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Next,
            ),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(12.dp))

        if (state.contactStep == ContactRecoverStep.Request) {
            // Kind toggle: Email / SMS.
            SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
                SegmentedButton(
                    selected = state.contactKind == ContactKind.EMAIL,
                    onClick = { onContactKindChange(ContactKind.EMAIL) },
                    shape = SegmentedButtonDefaults.itemShape(index = 0, count = 2),
                    enabled = !state.busy,
                ) { Text("Email") }
                SegmentedButton(
                    selected = state.contactKind == ContactKind.PHONE,
                    onClick = { onContactKindChange(ContactKind.PHONE) },
                    shape = SegmentedButtonDefaults.itemShape(index = 1, count = 2),
                    enabled = !state.busy,
                ) { Text("SMS") }
            }
            Spacer(Modifier.height(12.dp))
            OutlinedTextField(
                value = state.contact,
                onValueChange = onContactChange,
                label = {
                    Text(
                        if (state.contactKind == ContactKind.EMAIL) {
                            "Email address"
                        } else {
                            "Phone number"
                        },
                    )
                },
                placeholder = {
                    Text(
                        if (state.contactKind == ContactKind.EMAIL) {
                            "alice@example.com"
                        } else {
                            "+15551234567"
                        },
                    )
                },
                singleLine = true,
                enabled = !state.busy,
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrect = false,
                    keyboardType = if (state.contactKind == ContactKind.EMAIL) {
                        KeyboardType.Email
                    } else {
                        KeyboardType.Phone
                    },
                    imeAction = ImeAction.Done,
                ),
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
            Spacer(Modifier.height(16.dp))
            Button(
                onClick = onSendContactCode,
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
                } else {
                    Text("Send code")
                }
            }
        } else {
            // Code step.
            Text(
                text = "Enter the 6-digit code we sent to your " +
                    if (state.contactKind == ContactKind.EMAIL) "email." else "phone.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(12.dp))
            OutlinedTextField(
                value = state.contactCode,
                onValueChange = onContactCodeChange,
                label = { Text("6-digit code") },
                placeholder = { Text("123456") },
                singleLine = true,
                enabled = !state.busy,
                keyboardOptions = KeyboardOptions(
                    keyboardType = KeyboardType.Number,
                    imeAction = ImeAction.Done,
                ),
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
            Spacer(Modifier.height(16.dp))
            Button(
                onClick = onSubmitContactCode,
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
                } else {
                    Text("Recover with code")
                }
            }
        }
    }

    if (state.mode == RecoverMode.AnotherDevice) {
        // ADR-0018 factor (a), path (b) — no-password restore via an
        // already-linked device. Resolve the username first (the
        // pairing QR embeds the account id), then run the QR join
        // with the recovery payload opted in.
        val target = state.deviceRestoreTarget
        if (target == null) {
            OutlinedTextField(
                value = state.username,
                onValueChange = onUsernameChange,
                label = { Text("Username") },
                placeholder = { Text("alice") },
                singleLine = true,
                enabled = !state.busy,
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrect = false,
                    keyboardType = KeyboardType.Ascii,
                    imeAction = ImeAction.Done,
                ),
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
            Spacer(Modifier.height(16.dp))
            Button(
                onClick = onLookupForDeviceRestore,
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
                } else {
                    Text("Find account")
                }
            }
        } else {
            Text(
                text = "Restoring @${target.username}",
                style = MaterialTheme.typography.bodyMedium,
            )
            Spacer(Modifier.height(12.dp))
            me.lakko.app.ui.devices.RestoreViaDevicePanel(
                accountIdBytes = target.accountIdBytes,
                accountIdB64 = target.accountIdB64,
                onRecovered = onCompleteFromDeviceRestore,
            )
        }
    }

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
    } else if (state.mode == RecoverMode.Shares) {
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
    if (
        state.mode != RecoverMode.AnotherDevice &&
        state.mode != RecoverMode.Authenticator &&
        state.mode != RecoverMode.Contact
    ) {
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

    if (cachedUsername == null) {
        Spacer(Modifier.height(8.dp))
        Row(
            horizontalArrangement = Arrangement.Center,
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(
                text = "New here? ",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                text = "Create account",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier
                    .padding(start = 4.dp)
                    .clickable(onClick = onNavigateToSignup),
            )
        }
    }
}

