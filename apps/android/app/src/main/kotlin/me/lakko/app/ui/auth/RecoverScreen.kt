// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Group
import androidx.compose.material.icons.filled.Mail
import androidx.compose.material.icons.automirrored.filled.Notes
import androidx.compose.material.icons.filled.Password
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.auth.ContactKind

/**
 * Recover (docs/ux/client-redesign.md section 5.6): a list of methods as
 * tappable rows, each pushing its form. The three-word method is not listed:
 * the Android client has no word-vault flow yet. Every form keeps the
 * behaviour of [RecoverViewModel]; only the chrome and the copy changed.
 */
@Composable
public fun RecoverScreen(
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val cachedUsername = viewModel.cachedUsername
    var method by rememberSaveable { mutableStateOf<RecoverMode?>(null) }

    LaunchedEffect(method) {
        method?.let { viewModel.onModeChange(it) }
    }
    BackHandler(enabled = method != null && !state.busy) { method = null }

    val chosen = method
    if (chosen == null) {
        AuthScaffold(title = "Recover account", onBack = onBack, modifier = modifier) {
            Lead(
                if (cachedUsername != null) {
                    "Choose how to get back into @$cachedUsername."
                } else {
                    "Choose how to prove the account is yours."
                },
            )
            Spacer(Modifier.height(16.dp))
            MethodRow(
                icon = Icons.AutoMirrored.Filled.Notes,
                title = "Recovery phrase",
                subtitle = "The 24 words you saved when you created the account",
                onClick = { method = RecoverMode.Phrase },
            )
            MethodRow(
                icon = Icons.Filled.Devices,
                title = "Another device",
                subtitle = "A phone or computer that is still logged in",
                onClick = { method = RecoverMode.AnotherDevice },
            )
            MethodRow(
                icon = Icons.Filled.Password,
                title = "Authenticator",
                subtitle = "A code from your authenticator app",
                onClick = { method = RecoverMode.Authenticator },
            )
            MethodRow(
                icon = Icons.Filled.Mail,
                title = "Email or phone",
                subtitle = "A code sent to the address or number you added",
                onClick = { method = RecoverMode.Contact },
            )
            MethodRow(
                icon = Icons.Filled.Group,
                title = "Recovery pieces",
                subtitle = "Three of the pieces you gave to people you trust",
                onClick = { method = RecoverMode.Shares },
            )
            Spacer(Modifier.height(16.dp))
        }
        return
    }

    val back: () -> Unit = { if (!state.busy) method = null }
    when (chosen) {
        RecoverMode.Phrase -> PhraseForm(state, cachedUsername, viewModel, back, modifier)
        RecoverMode.Shares -> SharesForm(state, cachedUsername, viewModel, back, modifier)
        RecoverMode.AnotherDevice -> AnotherDeviceForm(state, viewModel, back, modifier)
        RecoverMode.Authenticator -> AuthenticatorForm(state, cachedUsername, viewModel, back, modifier)
        RecoverMode.Contact -> ContactForm(state, cachedUsername, viewModel, back, modifier)
    }
}

@Composable
private fun UsernameField(
    value: String,
    onValueChange: (String) -> Unit,
    enabled: Boolean,
    imeAction: ImeAction = ImeAction.Next,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text("Username") },
        singleLine = true,
        enabled = enabled,
        keyboardOptions = KeyboardOptions(
            capitalization = KeyboardCapitalization.None,
            autoCorrectEnabled = false,
            keyboardType = KeyboardType.Ascii,
            imeAction = imeAction,
        ),
        modifier = Modifier.fillMaxWidth(),
    )
    Spacer(Modifier.height(12.dp))
}

@Composable
private fun NewPasswordFields(state: RecoverUiState, viewModel: RecoverViewModel) {
    PasswordField(
        value = state.password,
        onValueChange = viewModel::onPasswordChange,
        label = "New password",
        enabled = !state.busy,
        supportingText = "At least ${RecoverViewModel.MIN_PASSWORD_LEN} characters",
    )
    Spacer(Modifier.height(8.dp))
    PasswordField(
        value = state.confirm,
        onValueChange = viewModel::onConfirmChange,
        label = "Confirm new password",
        enabled = !state.busy,
        imeAction = ImeAction.Done,
        onDone = viewModel::onSubmit,
    )
}

@Composable
private fun PhraseForm(
    state: RecoverUiState,
    cachedUsername: String?,
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    AuthScaffold(title = "Recovery phrase", onBack = onBack, modifier = modifier) {
        Lead("Enter the 24 words in order, then choose a new password.")
        Spacer(Modifier.height(16.dp))
        if (cachedUsername == null) {
            UsernameField(state.username, viewModel::onUsernameChange, enabled = !state.busy)
        }
        RecoveryPhraseEntryGrid(
            phrase = state.phrase,
            onPhraseChange = viewModel::onPhraseChange,
            enabled = !state.busy,
        )
        Spacer(Modifier.height(16.dp))
        NewPasswordFields(state, viewModel)
        FormError(state.error)
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = "Recover account",
            busy = state.busy,
            busyLabel = "Checking...",
            onClick = viewModel::onSubmit,
        )
        Spacer(Modifier.height(16.dp))
    }
}

@Composable
private fun SharesForm(
    state: RecoverUiState,
    cachedUsername: String?,
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    AuthScaffold(title = "Recovery pieces", onBack = onBack, modifier = modifier) {
        Lead("Paste at least three pieces, one per line, then choose a new password.")
        Spacer(Modifier.height(16.dp))
        if (cachedUsername == null) {
            UsernameField(state.username, viewModel::onUsernameChange, enabled = !state.busy)
        }
        OutlinedTextField(
            value = state.sharesInput,
            onValueChange = viewModel::onSharesInputChange,
            label = { Text("Pieces, one per line") },
            singleLine = false,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrectEnabled = false,
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Next,
            ),
            textStyle = monoBody(),
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = 140.dp),
        )
        Spacer(Modifier.height(16.dp))
        NewPasswordFields(state, viewModel)
        FormError(state.error)
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = "Recover account",
            busy = state.busy,
            busyLabel = "Checking...",
            onClick = viewModel::onSubmit,
        )
        Spacer(Modifier.height(16.dp))
    }
}

@Composable
private fun monoBody() =
    androidx.compose.material3.MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace)

@Composable
private fun AnotherDeviceForm(
    state: RecoverUiState,
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    AuthScaffold(title = "Another device", onBack = onBack, modifier = modifier) {
        val target = state.deviceRestoreTarget
        if (target == null) {
            Lead("Enter your username. Then scan the code with a device that is still logged in.")
            Spacer(Modifier.height(16.dp))
            UsernameField(state.username, viewModel::onUsernameChange, enabled = !state.busy, imeAction = ImeAction.Done)
            FormError(state.error)
            Spacer(Modifier.height(12.dp))
            BusyButton(
                label = "Continue",
                busy = state.busy,
                busyLabel = "Looking up...",
                onClick = viewModel::lookupForDeviceRestore,
            )
        } else {
            Lead("Restoring @${target.username}")
            Spacer(Modifier.height(12.dp))
            me.lakko.app.ui.devices.RestoreViaDevicePanel(
                accountIdBytes = target.accountIdBytes,
                accountIdB64 = target.accountIdB64,
                onRecovered = viewModel::completeFromDeviceRestore,
            )
        }
        Spacer(Modifier.height(16.dp))
    }
}

@Composable
private fun AuthenticatorForm(
    state: RecoverUiState,
    cachedUsername: String?,
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    AuthScaffold(title = "Authenticator", onBack = onBack, modifier = modifier) {
        Lead("Enter the six-digit code from your authenticator app.")
        Spacer(Modifier.height(16.dp))
        if (cachedUsername == null) {
            UsernameField(state.username, viewModel::onUsernameChange, enabled = !state.busy)
        }
        OutlinedTextField(
            value = state.totpCode,
            onValueChange = viewModel::onTotpCodeChange,
            label = { Text("Code") },
            singleLine = true,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
            keyboardActions = androidx.compose.foundation.text.KeyboardActions(onDone = { viewModel.onSubmitTotp() }),
            modifier = Modifier.fillMaxWidth(),
        )
        FormError(state.error)
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = "Continue",
            busy = state.busy,
            busyLabel = "Checking...",
            onClick = viewModel::onSubmitTotp,
        )
        Spacer(Modifier.height(16.dp))
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ContactForm(
    state: RecoverUiState,
    cachedUsername: String?,
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    AuthScaffold(title = "Email or phone", onBack = onBack, modifier = modifier) {
        if (state.contactStep == ContactRecoverStep.Request) {
            Lead("Lakko sends a code to the email address or phone number you added to the account.")
            Spacer(Modifier.height(16.dp))
            if (cachedUsername == null) {
                UsernameField(state.username, viewModel::onUsernameChange, enabled = !state.busy)
            }
            SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
                SegmentedButton(
                    selected = state.contactKind == ContactKind.EMAIL,
                    onClick = { viewModel.onContactKindChange(ContactKind.EMAIL) },
                    shape = SegmentedButtonDefaults.itemShape(index = 0, count = 2),
                    enabled = !state.busy,
                ) { Text("Email") }
                SegmentedButton(
                    selected = state.contactKind == ContactKind.PHONE,
                    onClick = { viewModel.onContactKindChange(ContactKind.PHONE) },
                    shape = SegmentedButtonDefaults.itemShape(index = 1, count = 2),
                    enabled = !state.busy,
                ) { Text("Phone") }
            }
            Spacer(Modifier.height(12.dp))
            val email = state.contactKind == ContactKind.EMAIL
            OutlinedTextField(
                value = state.contact,
                onValueChange = viewModel::onContactChange,
                label = { Text(if (email) "Email address" else "Phone number") },
                placeholder = { Text(if (email) "name@example.com" else "+15551234567") },
                singleLine = true,
                enabled = !state.busy,
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrectEnabled = false,
                    keyboardType = if (email) KeyboardType.Email else KeyboardType.Phone,
                    imeAction = ImeAction.Done,
                ),
                keyboardActions = androidx.compose.foundation.text.KeyboardActions(onDone = { viewModel.onSendContactCode() }),
                modifier = Modifier.fillMaxWidth(),
            )
            FormError(state.error)
            Spacer(Modifier.height(24.dp))
            BusyButton(
                label = "Send code",
                busy = state.busy,
                busyLabel = "Sending...",
                onClick = viewModel::onSendContactCode,
            )
        } else {
            Lead(
                "If the ${if (state.contactKind == ContactKind.EMAIL) "address" else "number"} " +
                    "belongs to this account, a six-digit code is on its way. Enter it here.",
            )
            Spacer(Modifier.height(16.dp))
            OutlinedTextField(
                value = state.contactCode,
                onValueChange = viewModel::onContactCodeChange,
                label = { Text("Code") },
                singleLine = true,
                enabled = !state.busy,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
                keyboardActions = androidx.compose.foundation.text.KeyboardActions(onDone = { viewModel.onSubmitContactCode() }),
                modifier = Modifier.fillMaxWidth(),
            )
            FormError(state.error)
            Spacer(Modifier.height(24.dp))
            BusyButton(
                label = "Continue",
                busy = state.busy,
                busyLabel = "Checking...",
                onClick = viewModel::onSubmitContactCode,
            )
        }
        Spacer(Modifier.height(16.dp))
    }
}
