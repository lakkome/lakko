// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.BetaNoticeBanner

/**
 * Create account (docs/ux/client-redesign.md section 5.6): username with the
 * live availability hint, display name, password, confirm password, Create
 * account. No recovery phrase is shown (ADR-0075 D2); the "Protect your
 * account" step that follows ([ProtectAccountStep]) is rendered by the app
 * root from `AuthRepository.protectStepOwed` once the account exists.
 */
@Composable
public fun SignupScreen(
    viewModel: SignupViewModel,
    onBack: () -> Unit,
    /** `Log in` on the full state: the sign-in screen. */
    onLogIn: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    // `docs/ux/beta-account-cap.md` section 2 item 1: asked on open.
    LaunchedEffect(viewModel) { viewModel.onShown() }

    if (state.closed && state.codeEntry) {
        SupporterCodeEntry(
            code = state.code,
            checking = state.codeChecking,
            error = state.codeError,
            onCodeChange = viewModel::onSupporterCodeChange,
            onContinue = viewModel::onSupporterCodeContinue,
            onBack = viewModel::onSupporterCodeBack,
            onLeave = onBack,
            modifier = modifier,
        )
        return
    }
    if (state.closed) {
        SignupClosedState(
            checking = state.checking,
            codeUsed = state.codeUsed,
            onRetry = viewModel::onRetryOpen,
            onLogIn = onLogIn,
            onSupporterCode = viewModel::onOpenSupporterCode,
            onBack = onBack,
            modifier = modifier,
        )
        return
    }

    AuthScaffold(title = t("auth.signup.title"), onBack = onBack, modifier = modifier, showLanguageButton = true) {
        Spacer(Modifier.height(8.dp))
        // docs/ux/beta-notice.md section 3 item 2: above the form and above
        // the full state alike.
        BetaNoticeBanner()
        if (state.placeHeld) {
            // `docs/ux/supporters.md` section 1.2: one line above the form.
            Spacer(Modifier.height(16.dp))
            Lead(t("auth.signup.code.held"))
        }
        Spacer(Modifier.height(16.dp))
        OutlinedTextField(
            value = state.username,
            onValueChange = viewModel::onUsernameChange,
            label = { Text(t("auth.username.label")) },
            singleLine = true,
            enabled = !state.busy,
            isError = state.usernameError != null,
            supportingText = {
                val line = (state.usernameError ?: state.usernameHint)?.text()
                    ?: t("auth.signup.username.hint")
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
            label = { Text(t("auth.signup.name.label")) },
            placeholder = {
                Text(state.username.trim().removePrefix("@").ifEmpty { t("common.optional") })
            },
            singleLine = true,
            enabled = !state.busy,
            isError = state.displayNameError != null,
            supportingText = {
                Text(state.displayNameError?.text() ?: t("auth.signup.name.hint"))
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
            label = t("auth.password.label"),
            enabled = !state.busy,
            supportingText = t(
                "auth.password.hint",
                emptyMap(),
                mapOf("count" to SignupViewModel.MIN_PASSWORD_LEN.toULong()),
            ),
        )
        Spacer(Modifier.height(8.dp))
        PasswordField(
            value = state.confirm,
            onValueChange = viewModel::onConfirmChange,
            label = t("auth.password.confirm.label"),
            enabled = !state.busy,
            imeAction = ImeAction.Done,
            onDone = viewModel::onSubmit,
        )
        FormError(state.error)
        Spacer(Modifier.height(24.dp))
        // ADR-0054 UX section 1 - the working row: a disabled row with a
        // spinner, one line below it, and Cancel while the one heavy piece of
        // work runs. A network-wide signup limit turns the button into
        // `Try again`; the form keeps its contents either way.
        BusyButton(
            label = if (state.rateLimited) t("common.try_again") else t("auth.signup.submit"),
            busy = state.busy,
            busyLabel = t("auth.signup.working.title"),
            onClick = viewModel::onSubmit,
        )
        // docs/ux/legal-pages.md section 2 item 1.
        Spacer(Modifier.height(8.dp))
        LegalSignupNotice()
        if (state.busy) {
            Spacer(Modifier.height(8.dp))
            Lead(t("auth.signup.working.line"))
            if (state.solving) {
                TextButton(onClick = viewModel::onCancel, modifier = Modifier.fillMaxWidth()) {
                    Text(t("common.cancel"))
                }
            }
        }
        Spacer(Modifier.height(12.dp))
        Lead(t("auth.signup.lede"))
        Spacer(Modifier.height(16.dp))
    }
}

/**
 * "Lakko is full for now" (`docs/ux/beta-account-cap.md` section 2 item 3):
 * in place of the form while the beta has no room. `Try again` asks again
 * (`Checking...`, disabled, while it runs); `Log in` goes to the sign-in
 * screen. Nothing about how many places there are.
 */
@Composable
private fun SignupClosedState(
    checking: Boolean,
    /** `docs/ux/supporters.md` section 1.2: the held code was already used. */
    codeUsed: Boolean,
    onRetry: () -> Unit,
    onLogIn: () -> Unit,
    onSupporterCode: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    AuthScaffold(title = t("auth.signup.title"), onBack = onBack, modifier = modifier, showLanguageButton = true) {
        Spacer(Modifier.height(8.dp))
        // docs/ux/beta-notice.md section 3 item 2: above the form and above
        // the full state alike.
        BetaNoticeBanner()
        Spacer(Modifier.height(16.dp))
        if (codeUsed) {
            FormError(t("auth.signup.code.used"))
            Spacer(Modifier.height(8.dp))
        }
        Text(t("auth.signup.closed.title"), style = MaterialTheme.typography.titleLarge)
        Spacer(Modifier.height(8.dp))
        Lead(t("auth.signup.closed.body"))
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = t("auth.signup.closed.retry"),
            busy = checking,
            busyLabel = t("auth.signup.closed.checking"),
            onClick = onRetry,
        )
        Spacer(Modifier.height(8.dp))
        OutlinedButton(onClick = onLogIn, enabled = !checking, modifier = Modifier.fillMaxWidth()) {
            Text(t("auth.signup.closed.log_in"))
        }
        // `docs/ux/supporters.md` section 1.1: a link below the two buttons.
        Spacer(Modifier.height(8.dp))
        TextButton(onClick = onSupporterCode, enabled = !checking, modifier = Modifier.fillMaxWidth()) {
            Text(t("auth.signup.code.link"))
        }
        Spacer(Modifier.height(16.dp))
    }
}

/**
 * "Supporter code" (`docs/ux/supporters.md` section 1.1): in place of the
 * full state. `Continue` is enabled once the field holds 16 letters and
 * digits and reads `Checking...` while the code is checked; `Back` returns
 * to the full state. A failed check leaves what was typed in the field.
 */
@Composable
private fun SupporterCodeEntry(
    code: String,
    checking: Boolean,
    error: me.lakko.app.l10n.UiText?,
    onCodeChange: (String) -> Unit,
    onContinue: () -> Unit,
    onBack: () -> Unit,
    onLeave: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val complete = me.lakko.app.auth.supporterCodeComplete(code)
    AuthScaffold(title = t("auth.signup.title"), onBack = onLeave, modifier = modifier, showLanguageButton = true) {
        Spacer(Modifier.height(8.dp))
        BetaNoticeBanner()
        Spacer(Modifier.height(16.dp))
        Text(t("auth.signup.code.title"), style = MaterialTheme.typography.titleLarge)
        Spacer(Modifier.height(16.dp))
        // UX 1.1's body line is the field's label, as on the other auth
        // fields, so TalkBack announces it with the field.
        OutlinedTextField(
            value = code,
            onValueChange = onCodeChange,
            label = { Text(t("auth.signup.code.body")) },
            singleLine = true,
            enabled = !checking,
            isError = error != null,
            supportingText = error?.let { { Text(it.text()) } },
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.Characters,
                autoCorrectEnabled = false,
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Done,
            ),
            keyboardActions = androidx.compose.foundation.text.KeyboardActions(
                onDone = { if (complete && !checking) onContinue() },
            ),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = t("auth.signup.code.continue"),
            busy = checking,
            busyLabel = t("auth.signup.code.checking"),
            enabled = complete,
            onClick = onContinue,
        )
        Spacer(Modifier.height(8.dp))
        OutlinedButton(onClick = onBack, enabled = !checking, modifier = Modifier.fillMaxWidth()) {
            Text(t("auth.signup.code.back"))
        }
        Spacer(Modifier.height(16.dp))
    }
}
