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
import me.lakko.app.l10n.t

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
        AuthScaffold(title = t("recovery.title"), onBack = onBack, modifier = modifier) {
            Lead(
                if (cachedUsername != null) {
                    t("recovery.lede_account", mapOf("username" to "@$cachedUsername"))
                } else {
                    t("recovery.lede")
                },
            )
            Spacer(Modifier.height(16.dp))
            MethodRow(
                icon = Icons.AutoMirrored.Filled.Notes,
                title = t("recovery.method.phrase.title"),
                subtitle = t("recovery.method.phrase.sub"),
                onClick = { method = RecoverMode.Phrase },
            )
            MethodRow(
                icon = Icons.Filled.Devices,
                title = t("recovery.method.device.title"),
                subtitle = t("recovery.method.device.sub"),
                onClick = { method = RecoverMode.AnotherDevice },
            )
            MethodRow(
                icon = Icons.Filled.Password,
                title = t("recovery.method.totp.title"),
                subtitle = t("recovery.method.totp.sub"),
                onClick = { method = RecoverMode.Authenticator },
            )
            MethodRow(
                icon = Icons.Filled.Mail,
                title = t("recovery.method.contact.title"),
                subtitle = t("recovery.method.contact.sub"),
                onClick = { method = RecoverMode.Contact },
            )
            MethodRow(
                icon = Icons.Filled.Group,
                title = t("recovery.method.shares.title"),
                subtitle = t("recovery.method.shares.sub"),
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
        label = { Text(t("auth.username.label")) },
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
        label = t("auth.password.new.label"),
        enabled = !state.busy,
        supportingText = t(
            "auth.password.hint",
            emptyMap(),
            mapOf("count" to RecoverViewModel.MIN_PASSWORD_LEN.toULong()),
        ),
    )
    Spacer(Modifier.height(8.dp))
    PasswordField(
        value = state.confirm,
        onValueChange = viewModel::onConfirmChange,
        label = t("auth.password.new_confirm.label"),
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
    AuthScaffold(title = t("recovery.method.phrase.title"), onBack = onBack, modifier = modifier) {
        Lead(t("recovery.phrase.intro"))
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
            label = t("recovery.submit"),
            busy = state.busy,
            busyLabel = t("common.checking"),
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
    AuthScaffold(title = t("recovery.method.shares.title"), onBack = onBack, modifier = modifier) {
        Lead(t("recovery.shares.intro"))
        Spacer(Modifier.height(16.dp))
        if (cachedUsername == null) {
            UsernameField(state.username, viewModel::onUsernameChange, enabled = !state.busy)
        }
        OutlinedTextField(
            value = state.sharesInput,
            onValueChange = viewModel::onSharesInputChange,
            label = { Text(t("recovery.shares.label")) },
            supportingText = { Text(t("recovery.shares.help")) },
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
            label = t("recovery.submit"),
            busy = state.busy,
            busyLabel = t("common.checking"),
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
    AuthScaffold(title = t("recovery.method.device.title"), onBack = onBack, modifier = modifier) {
        val target = state.deviceRestoreTarget
        if (target == null) {
            Lead(t("recovery.device.intro_qr"))
            Spacer(Modifier.height(16.dp))
            UsernameField(state.username, viewModel::onUsernameChange, enabled = !state.busy, imeAction = ImeAction.Done)
            FormError(state.error)
            Spacer(Modifier.height(12.dp))
            BusyButton(
                label = t("common.continue"),
                busy = state.busy,
                busyLabel = t("common.preparing"),
                onClick = viewModel::startDeviceRestore,
            )
        } else {
            Lead(t("recovery.device.restoring", mapOf("username" to "@${target.username}")))
            Spacer(Modifier.height(12.dp))
            me.lakko.app.ui.devices.RestoreViaDevicePanel(
                loginHandle = target.loginHandle,
                known = null,
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
    AuthScaffold(title = t("recovery.method.totp.title"), onBack = onBack, modifier = modifier) {
        Lead(t("recovery.totp.intro"))
        Spacer(Modifier.height(16.dp))
        if (cachedUsername == null) {
            UsernameField(state.username, viewModel::onUsernameChange, enabled = !state.busy)
        }
        OutlinedTextField(
            value = state.totpCode,
            onValueChange = viewModel::onTotpCodeChange,
            label = { Text(t("recovery.totp.code_label")) },
            singleLine = true,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
            keyboardActions = androidx.compose.foundation.text.KeyboardActions(onDone = { viewModel.onSubmitTotp() }),
            modifier = Modifier.fillMaxWidth(),
        )
        FormError(state.error)
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = t("common.continue"),
            busy = state.busy,
            busyLabel = t("common.checking"),
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
    AuthScaffold(title = t("recovery.method.contact.title"), onBack = onBack, modifier = modifier) {
        if (state.contactStep == ContactRecoverStep.Request) {
            Lead(t("recovery.contact.intro"))
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
                ) { Text(t("common.email")) }
                SegmentedButton(
                    selected = state.contactKind == ContactKind.PHONE,
                    onClick = { viewModel.onContactKindChange(ContactKind.PHONE) },
                    shape = SegmentedButtonDefaults.itemShape(index = 1, count = 2),
                    enabled = !state.busy,
                ) { Text(t("common.phone")) }
            }
            Spacer(Modifier.height(12.dp))
            val email = state.contactKind == ContactKind.EMAIL
            OutlinedTextField(
                value = state.contact,
                onValueChange = viewModel::onContactChange,
                label = { Text(if (email) t("recovery.contact.email_label") else t("recovery.contact.phone_label")) },
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
                label = t("recovery.contact.send"),
                busy = state.busy,
                busyLabel = t("common.sending"),
                onClick = viewModel::onSendContactCode,
            )
        } else {
            Lead(
                if (state.contactKind == ContactKind.EMAIL) {
                    t("recovery.contact.sent_email")
                } else {
                    t("recovery.contact.sent_phone")
                },
            )
            Spacer(Modifier.height(16.dp))
            OutlinedTextField(
                value = state.contactCode,
                onValueChange = viewModel::onContactCodeChange,
                label = { Text(t("recovery.contact.code_label")) },
                singleLine = true,
                enabled = !state.busy,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
                keyboardActions = androidx.compose.foundation.text.KeyboardActions(onDone = { viewModel.onSubmitContactCode() }),
                modifier = Modifier.fillMaxWidth(),
            )
            FormError(state.error)
            Spacer(Modifier.height(24.dp))
            BusyButton(
                label = t("common.continue"),
                busy = state.busy,
                busyLabel = t("common.checking"),
                onClick = viewModel::onSubmitContactCode,
            )
        }
        Spacer(Modifier.height(16.dp))
    }
}
