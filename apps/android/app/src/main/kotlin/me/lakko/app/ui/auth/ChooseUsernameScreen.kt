// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import android.util.Log
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.UsernameClaimState
import me.lakko.app.auth.UsernameClaimStep
import me.lakko.app.auth.afterFirstPasswordSaved
import me.lakko.app.auth.afterUsernameClaimFailure
import me.lakko.app.auth.afterUsernameSaved
import me.lakko.app.auth.authLimitText
import me.lakko.app.auth.usernameClaimStep
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.profile.UsernameCheck
import me.lakko.app.profile.checkUsername
import me.lakko.app.profile.usernameCanonicalHintText
import me.lakko.app.profile.usernameFieldErrorText

/** The line for a failed name step: the claim's own sentence (a taken or
 *  refused name), a limit, offline, or the generic one. */
public fun usernameClaimFailure(err: Throwable): UiText = when (err) {
    is AuthClientArgError -> err.text ?: UiText("common.error.unavailable")
    is AuthNetworkError, is me.lakko.app.discovery.DiscoveryNetworkError -> UiText("common.error.offline")
    is AuthHttpError -> authLimitText(err) ?: UiText("common.error.unavailable")
    else -> UiText("common.error.unavailable")
}

/**
 * "Choose a username" - a guest account (made by a call-link join) has no
 * username and a random password nobody knows. One screen: the username with
 * the sign-up's live validation, a new password and its confirmation, Save.
 *
 * The name is claimed first ([AuthRepository.claimGuestUsername]), then the
 * password is set with the device's seed-based change and no proof block
 * ([AuthRepository.setFirstPassword]): the account has no password to prove.
 * If the password step fails after the name was saved, the name is kept, the
 * password step's line is shown and only the password is retried
 * ([UsernameClaimState]). A restart in between reopens on the password.
 */
@Composable
public fun ChooseUsernameScreen(
    repository: AuthRepository,
    /** The name and the password are saved and the "done" line was read;
     *  [username] is the canonical name. */
    onDone: (username: String) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
    validateUsername: (String) -> String = { uniffi.lakko_ffi_uniffi.validateUsername(it) },
) {
    val scope = rememberCoroutineScope()
    val authState by repository.state.collectAsStateWithLifecycle()
    val current = (authState as? AuthState.Session)?.account?.username.orEmpty()
    var state by remember {
        mutableStateOf(
            UsernameClaimState(
                step = usernameClaimStep(
                    hasUsername = current.isNotBlank(),
                    passwordOwed = repository.firstPasswordOwed(),
                ),
            ),
        )
    }
    var username by remember { mutableStateOf("") }
    var password by remember { mutableStateOf("") }
    var confirm by remember { mutableStateOf("") }
    val check = checkUsername(username, validateUsername)

    BackHandler(enabled = !state.busy) { onBack() }

    val save: () -> Unit = save@{
        if (state.busy || state.step == UsernameClaimStep.Done) return@save
        newPasswordProblem(password, confirm)?.let {
            state = afterUsernameClaimFailure(state, it)
            return@save
        }
        val typed = (check as? UsernameCheck.Valid)?.normalized
        if (state.step == UsernameClaimStep.Name && typed == null) {
            state = afterUsernameClaimFailure(state, usernameFieldErrorText(check, username) ?: UiText("username.error.empty"))
            return@save
        }
        state = state.copy(busy = true, error = null)
        scope.launch {
            if (state.step == UsernameClaimStep.Name) {
                try {
                    repository.claimGuestUsername(typed.orEmpty())
                    state = afterUsernameSaved(state)
                } catch (cause: Throwable) {
                    if (cause is CancellationException) throw cause
                    Log.w("lakko-auth", "username not claimed: ${cause.javaClass.simpleName}")
                    state = afterUsernameClaimFailure(state, usernameClaimFailure(cause))
                    return@launch
                }
            }
            val bytes = password.toByteArray(Charsets.UTF_8)
            try {
                repository.setFirstPassword(bytes)
                password = ""
                confirm = ""
                state = afterFirstPasswordSaved(state)
            } catch (cause: Throwable) {
                if (cause is CancellationException) throw cause
                Log.w("lakko-auth", "first password not set: ${cause.javaClass.simpleName}")
                // The name stays; only the password is tried again.
                state = afterUsernameClaimFailure(state, newPasswordFailure(cause))
            } finally {
                bytes.fill(0)
            }
        }
    }

    AuthScaffold(title = t("username.claim.title"), onBack = if (state.busy) null else onBack, modifier = modifier) {
        if (state.step == UsernameClaimStep.Done) {
            Lead(t("username.claim.done", mapOf("username" to "⁦@$current⁩")))
            Spacer(Modifier.height(24.dp))
            BusyButton(label = t("common.done"), busy = false, onClick = { onDone(current) })
            Spacer(Modifier.height(16.dp))
            return@AuthScaffold
        }
        Lead(t("username.claim.body"))
        Spacer(Modifier.height(16.dp))
        if (state.step == UsernameClaimStep.Name) {
            val fieldError = usernameFieldErrorText(check, username)
            OutlinedTextField(
                value = username,
                onValueChange = {
                    username = it
                    state = state.copy(error = null)
                },
                label = { Text(t("auth.username.label")) },
                singleLine = true,
                enabled = state.nameEditable,
                isError = fieldError != null,
                supportingText = {
                    Text((fieldError ?: usernameCanonicalHintText(check, username))?.text() ?: t("auth.signup.username.hint"))
                },
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Ascii,
                    imeAction = ImeAction.Next,
                ),
                modifier = Modifier.fillMaxWidth(),
            )
        } else {
            // The name is saved; only the password is owed.
            Lead("⁦@$current⁩")
        }
        Spacer(Modifier.height(8.dp))
        PasswordField(
            value = password,
            onValueChange = {
                password = it
                state = state.copy(error = null)
            },
            label = t("auth.password.new.label"),
            enabled = !state.busy,
            supportingText = t("username.claim.password_help"),
        )
        Spacer(Modifier.height(8.dp))
        PasswordField(
            value = confirm,
            onValueChange = {
                confirm = it
                state = state.copy(error = null)
            },
            label = t("auth.password.new_confirm.label"),
            enabled = !state.busy,
            imeAction = ImeAction.Done,
            onDone = save,
        )
        FormError(state.error)
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = t("username.claim.save"),
            busy = state.busy,
            busyLabel = t("common.saving"),
            onClick = save,
        )
        Spacer(Modifier.height(16.dp))
    }
}
