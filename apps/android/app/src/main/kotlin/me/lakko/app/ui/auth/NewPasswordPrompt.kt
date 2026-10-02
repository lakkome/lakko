// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.authLimitText
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t

/** The shortest password the forms accept (the sign-up floor). */
public const val NEW_PASSWORD_MIN_LENGTH: Int = 8

/** What the new-password form says before it sends anything, or `null`
 *  when the two fields can be submitted. */
public fun newPasswordProblem(password: String, confirm: String): UiText? = when {
    password != confirm -> UiText("auth.password.error.mismatch")
    password.length < NEW_PASSWORD_MIN_LENGTH -> UiText(
        "auth.password.error.too_short",
        counts = mapOf("count" to NEW_PASSWORD_MIN_LENGTH.toULong()),
    )
    else -> null
}

/** The sentence for a failed password change. */
public fun newPasswordFailure(err: Throwable): UiText = when (err) {
    is AuthNetworkError -> UiText("common.error.offline")
    is AuthHttpError -> authLimitText(err) ?: UiText("recovery.new_password.error.generic")
    is AuthClientArgError -> err.text ?: UiText("recovery.new_password.error.generic")
    else -> UiText("recovery.new_password.error.generic")
}

/**
 * The two fields and the Save button of a password change made from a
 * device that holds the account's keys: the person types only the new
 * password ([AuthRepository.replacePassword]). [onSkip] adds the "Not now"
 * button of the prompt shown after a forgot-password sign-in; Settings has
 * none.
 */
@Composable
public fun NewPasswordForm(
    repository: AuthRepository,
    onSaved: () -> Unit,
    modifier: Modifier = Modifier,
    onSkip: (() -> Unit)? = null,
) {
    val scope = rememberCoroutineScope()
    var password by rememberSaveable { mutableStateOf("") }
    var confirm by rememberSaveable { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<UiText?>(null) }

    fun submit() {
        if (busy) return
        newPasswordProblem(password, confirm)?.let {
            error = it
            return
        }
        busy = true
        error = null
        scope.launch {
            val bytes = password.toByteArray(Charsets.UTF_8)
            try {
                repository.replacePassword(bytes)
                password = ""
                confirm = ""
                onSaved()
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                error = newPasswordFailure(cause)
            } finally {
                bytes.fill(0)
                busy = false
            }
        }
    }

    PasswordField(
        value = password,
        onValueChange = { password = it },
        label = t("auth.password.new.label"),
        enabled = !busy,
        supportingText = t(
            "auth.password.hint",
            emptyMap(),
            mapOf("count" to NEW_PASSWORD_MIN_LENGTH.toULong()),
        ),
        modifier = modifier,
    )
    Spacer(Modifier.height(8.dp))
    PasswordField(
        value = confirm,
        onValueChange = { confirm = it },
        label = t("auth.password.new_confirm.label"),
        enabled = !busy,
        imeAction = ImeAction.Done,
        onDone = ::submit,
    )
    FormError(error)
    Spacer(Modifier.height(24.dp))
    BusyButton(
        label = t("recovery.new_password.save"),
        busy = busy,
        busyLabel = t("common.saving"),
        onClick = ::submit,
    )
    if (onSkip != null) {
        TextButton(onClick = onSkip, enabled = !busy, modifier = Modifier.fillMaxWidth()) {
            Text(t("recovery.new_password.not_now"))
        }
    }
}

/**
 * "Choose a new password": shown once by the app root, right after a
 * forgot-password route succeeded on a device that holds the account's keys
 * (three words, an authenticator code, a passkey, another device). The
 * person is already signed in.
 *
 * Saving replaces the password and opens the app. "Not now" opens the app
 * with the old (forgotten) password still valid; Settings > Security keeps
 * the same form.
 */
@Composable
public fun NewPasswordPrompt(
    repository: AuthRepository,
    modifier: Modifier = Modifier,
) {
    AuthScaffold(title = t("recovery.new_password.title"), onBack = null, modifier = modifier) {
        Lead(t("recovery.new_password.lede"))
        Spacer(Modifier.height(16.dp))
        NewPasswordForm(
            repository = repository,
            // `replacePassword` clears the offer itself; dismissing is harmless.
            onSaved = repository::dismissNewPasswordOffer,
            onSkip = repository::dismissNewPasswordOffer,
        )
        Spacer(Modifier.height(16.dp))
    }
}
