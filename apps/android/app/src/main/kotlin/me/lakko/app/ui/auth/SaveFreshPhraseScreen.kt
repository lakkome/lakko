// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.attemptsRetryAfterSecs
import me.lakko.app.auth.authLimitText
import me.lakko.app.auth.formatCountdown
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t

/**
 * A freshly created identity's 12-word phrase (the lost-everything route,
 * and a guest account made from a call link), shown through the same guided
 * phrase step as sign-up ([RecoveryPhraseOnboarding]: grid, copy, three-word
 * check), then [onConfirm] lands the session. The new identity is already
 * published; this screen only makes sure the phrase is saved.
 *
 * While [credentialPending] the words are NOT shown: the server's recovery
 * credential still belongs to the old phrase, so the new words would not
 * sign in yet. The screen explains that and offers "Try again", which
 * repeats only that last step ([onRetryCredential], never a second
 * identity), and "Log out".
 */
@Composable
public fun SaveFreshPhraseScreen(
    phrase: String,
    onConfirm: () -> Unit,
    modifier: Modifier = Modifier,
    /** The account's username, or `null` / blank for a guest account made
     *  from a call link (it has no password, so the copy differs). */
    username: String? = null,
    credentialPending: Boolean = false,
    onRetryCredential: suspend () -> Unit = {},
    onLogOut: () -> Unit = {},
) {
    if (credentialPending) {
        CredentialPending(onRetry = onRetryCredential, onLogOut = onLogOut, modifier = modifier)
        return
    }
    val guest = username.isNullOrBlank()
    RecoveryPhraseOnboarding(
        phrase = phrase,
        onDone = onConfirm,
        modifier = modifier,
        title = if (guest) t("recovery_phrase.save.title") else t("recovery_phrase.fresh.title"),
        lead = if (guest) {
            t("recovery_phrase.guest.lede")
        } else {
            t("recovery_phrase.fresh.lede", mapOf("username" to "@$username")) +
                "\n\n" + t("recovery_phrase.fresh.safety_note")
        },
    )
}

/** The sentence for a failed retry of the recovery-credential replace. */
public fun freshCredentialFailure(err: Throwable): UiText = when (err) {
    is AuthNetworkError -> UiText("common.error.offline")
    is AuthHttpError -> authLimitText(err) ?: UiText("recovery_phrase.fresh.credential_error")
    else -> UiText("recovery_phrase.fresh.credential_error")
}

@Composable
private fun CredentialPending(
    onRetry: suspend () -> Unit,
    onLogOut: () -> Unit,
    modifier: Modifier,
) {
    val scope = rememberCoroutineScope()
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<UiText?>(null) }
    // A server limit says when to try again; the button waits with it.
    var waitUntilMs by remember { mutableStateOf<Long?>(null) }
    val waitLeft = rememberRemainingMs(waitUntilMs)

    AuthScaffold(title = t("recovery_phrase.fresh.title"), onBack = null, modifier = modifier) {
        Lead(t("recovery_phrase.fresh.credential_pending"))
        if (waitLeft > 0L) {
            FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft))))
        } else {
            FormError(error)
        }
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = t("common.try_again"),
            busy = busy,
            busyLabel = t("common.checking"),
            enabled = waitLeft <= 0L,
            onClick = {
                busy = true
                error = null
                scope.launch {
                    try {
                        onRetry()
                    } catch (cause: Throwable) {
                        if (cause is kotlinx.coroutines.CancellationException) throw cause
                        val secs = attemptsRetryAfterSecs(cause)
                        if (secs != null) {
                            waitUntilMs = System.currentTimeMillis() + secs * 1000L
                        } else {
                            error = freshCredentialFailure(cause)
                        }
                    } finally {
                        busy = false
                    }
                }
            },
        )
        Spacer(Modifier.height(8.dp))
        OutlinedButton(onClick = onLogOut, enabled = !busy, modifier = Modifier.fillMaxWidth()) {
            Text(t("restore_identity.log_out"))
        }
        Spacer(Modifier.height(16.dp))
    }
}
