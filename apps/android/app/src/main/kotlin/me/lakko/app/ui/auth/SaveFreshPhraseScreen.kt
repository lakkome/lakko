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
import me.lakko.app.auth.PublishOwedReason
import me.lakko.app.auth.attemptsRetryAfterSecs
import me.lakko.app.auth.authLimitText
import me.lakko.app.auth.formatCountdown
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t

/**
 * A freshly created identity's 12-word phrase (the reset of the account
 * keys, and a guest account made from a call link), shown through the same
 * guided phrase step as sign-up ([RecoveryPhraseOnboarding]: grid, copy,
 * three-word check), then [onConfirm] lands the session.
 *
 * While [publishOwed] is non-null the words are NOT shown (UX 15.4): the
 * account keys were reset, but the new keys are not yet published to the
 * directory and the device list, so the contacts and the other devices
 * cannot find them. The screen explains that and offers "Try again", which
 * repeats only the publishing step ([onRetryPublish], never a second reset),
 * and "Log out". When the reset is older than 30 days it asks for a sign-in
 * first ([PublishOwedReason.SignInRequired]).
 */
@Composable
public fun SaveFreshPhraseScreen(
    phrase: String,
    onConfirm: () -> Unit,
    modifier: Modifier = Modifier,
    /** The account's username, or `null` / blank for an account that has
     *  none (the copy then does not name it). A guest account made from a
     *  call link no longer comes here: its phrase is shown only if the person
     *  keeps the account, through the sign-up phrase screen. */
    username: String? = null,
    publishOwed: PublishOwedReason? = null,
    onRetryPublish: suspend () -> Unit = {},
    onLogOut: () -> Unit = {},
    /** ADR-0075 - the account is NOT phrase-protected: the retry screen says
     *  nothing about words (none will be shown). */
    wordless: Boolean = false,
) {
    if (publishOwed != null) {
        PublishPending(
            reason = publishOwed,
            onRetry = onRetryPublish,
            onLogOut = onLogOut,
            modifier = modifier,
            wordless = wordless,
        )
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
                "\n\n" + t("recovery_phrase.fresh.safety_note") +
                "\n\n" + t("recovery_phrase.fresh.devices_signed_out")
        },
    )
}

/**
 * The catalog keys of the publish-owed screen's text, joined by a blank
 * line. A phrase-protected account keeps the existing text (it says the new
 * words are withheld); any other account is told the change is finishing,
 * with no mention of words ([wordless]).
 */
public fun publishOwedLeadKeys(reason: PublishOwedReason, wordless: Boolean): List<String> {
    val owed = if (wordless) "account.keys_changed.finishing" else "recovery_phrase.fresh.publish_owed"
    return when (reason) {
        is PublishOwedReason.SignInRequired -> listOf("recovery_phrase.fresh.sign_in_to_finish")
        is PublishOwedReason.Refused, is PublishOwedReason.Unverified ->
            listOf(owed, "recovery_phrase.fresh.publish_refused")
        is PublishOwedReason.Retryable -> listOf(owed)
    }
}

/** The screen's title: the phrase title only where words will follow. */
public fun publishOwedTitleKey(wordless: Boolean): String =
    if (wordless) "devices.rotate.step_keys" else "recovery_phrase.fresh.title"

/** The sentence for a failed retry of the publish of a reset. */
public fun freshPublishFailure(err: Throwable): UiText = when (err) {
    is AuthNetworkError -> UiText("common.error.offline")
    is me.lakko.app.discovery.DiscoveryNetworkError -> UiText("common.error.offline")
    is me.lakko.app.ds.DsNetworkError -> UiText("common.error.offline")
    is AuthHttpError -> authLimitText(err) ?: UiText("recovery_phrase.fresh.publish_error")
    else -> UiText("recovery_phrase.fresh.publish_error")
}

@Composable
private fun PublishPending(
    reason: PublishOwedReason,
    onRetry: suspend () -> Unit,
    onLogOut: () -> Unit,
    modifier: Modifier,
    wordless: Boolean = false,
) {
    val scope = rememberCoroutineScope()
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<UiText?>(null) }
    // A server limit says when to try again; the button waits with it.
    var waitUntilMs by remember {
        mutableStateOf((reason as? PublishOwedReason.Retryable)?.retryAfterSecs?.let { System.currentTimeMillis() + it * 1000L })
    }
    val waitLeft = rememberRemainingMs(waitUntilMs)
    val signInRequired = reason is PublishOwedReason.SignInRequired

    AuthScaffold(title = t(publishOwedTitleKey(wordless)), onBack = null, modifier = modifier) {
        Lead(publishOwedLeadKeys(reason, wordless).map { t(it) }.joinToString("\n\n"))
        if (waitLeft > 0L) {
            FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft))))
        } else {
            FormError(error)
        }
        Spacer(Modifier.height(24.dp))
        if (!signInRequired) {
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
                                error = freshPublishFailure(cause)
                            }
                        } finally {
                            busy = false
                        }
                    }
                },
            )
            Spacer(Modifier.height(8.dp))
        }
        OutlinedButton(onClick = onLogOut, enabled = !busy, modifier = Modifier.fillMaxWidth()) {
            Text(t("restore_identity.log_out"))
        }
        Spacer(Modifier.height(16.dp))
    }
}
