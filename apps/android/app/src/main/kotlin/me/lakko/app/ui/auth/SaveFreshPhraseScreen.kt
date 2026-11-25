// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.PublishOwedReason
import me.lakko.app.auth.attemptsRetryAfterSecs
import me.lakko.app.auth.authLimitText
import me.lakko.app.auth.formatCountdown
import me.lakko.app.auth.publishOwedScreenModel
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t

/**
 * A freshly created identity's 12-word phrase (the reset of the account
 * keys, and a guest account made from a call link): the grid and the
 * acknowledgement of UX 9.1, no three-word check (the web
 * `SaveFreshPhrasePrompt`), then [onConfirm] lands the session.
 *
 * While [publishOwed] is non-null the words are NOT shown (UX 15.4): the
 * account keys were reset, but the new keys are not yet published to the
 * directory and the device list, so the contacts and the other devices
 * cannot find them. The screen explains that and offers "Try again", which
 * repeats only the publishing step ([onRetryPublish], never a second reset),
 * and "Log out". When the reset is older than 30 days it asks for a sign-in
 * first ([PublishOwedReason.SignInRequired]). When the directory refused the
 * keys because they changed less than an hour ago (its cool-down), the screen
 * says how many minutes are left, shows no "Try again", and retries by
 * itself when the time is up ([publishOwedScreenModel]).
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
    FreshPhraseWords(phrase = phrase, username = username, onConfirm = onConfirm, modifier = modifier)
}

/**
 * The words of UX 9.1 "Ready": the grid and the acknowledgement that gates
 * Done, with no three-word check (the web `SaveFreshPhrasePrompt`).
 */
@Composable
private fun FreshPhraseWords(
    phrase: String,
    username: String?,
    onConfirm: () -> Unit,
    modifier: Modifier,
) {
    val guest = username.isNullOrBlank()
    val words = remember(phrase) { phraseWords(phrase) }
    var acknowledged by remember { mutableStateOf(false) }
    AuthScaffold(
        title = if (guest) t("recovery_phrase.save.title") else t("recovery_phrase.fresh.title"),
        onBack = null,
        modifier = modifier,
    ) {
        if (guest) {
            Lead(t("recovery_phrase.guest.lede"))
        } else {
            Lead(t("recovery_phrase.fresh.lede", mapOf("username" to "@$username")))
            Spacer(Modifier.height(8.dp))
            Lead(t("recovery_phrase.fresh.safety_note"))
            Spacer(Modifier.height(8.dp))
            Lead(t("recovery_phrase.fresh.devices_signed_out"))
        }
        Spacer(Modifier.height(16.dp))
        RecoveryPhraseGrid(words)
        Spacer(Modifier.height(8.dp))
        Lead(t("recovery_phrase.save.no_screenshot"))
        Spacer(Modifier.height(16.dp))
        AuthCheckRow(
            text = t("recovery_phrase.fresh.acknowledge"),
            checked = acknowledged,
            onCheckedChange = { acknowledged = it },
        )
        Spacer(Modifier.height(16.dp))
        BusyButton(
            label = t("common.done"),
            busy = false,
            enabled = freshPhraseDoneEnabled(words, acknowledged),
            onClick = { if (freshPhraseDoneEnabled(words, acknowledged)) onConfirm() },
        )
        Spacer(Modifier.height(16.dp))
    }
}

/** Done on the new-words screen of UX 9.1: there are words and the
 *  acknowledgement is ticked. */
public fun freshPhraseDoneEnabled(words: List<String>, acknowledged: Boolean): Boolean =
    acknowledged && words.isNotEmpty()

/**
 * The catalog keys of the publish-owed screen's text, joined by a blank
 * line. A phrase-protected account keeps the existing text (it says the new
 * words are withheld); any other account is told the change is finishing,
 * with no mention of words ([wordless]).
 */
public fun publishOwedLeadKeys(reason: PublishOwedReason, wordless: Boolean): List<String> =
    publishOwedScreenModel(reason, wordless, System.currentTimeMillis()).leadKeys

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
    // The directory's cool-down (UX 15.4): when the wait ends, retried here
    // by itself; the minutes below count down while the screen is open.
    val autoRetryAtMs = remember(reason, wordless) {
        publishOwedScreenModel(reason, wordless, System.currentTimeMillis()).autoRetryAtMs
    }
    // The remaining time ticks every second, and the minutes follow it.
    val autoRetryLeft = rememberRemainingMs(autoRetryAtMs)
    val nowMs = autoRetryAtMs?.let { it - autoRetryLeft } ?: System.currentTimeMillis()
    val model = publishOwedScreenModel(reason, wordless, nowMs)
    LaunchedEffect(reason, autoRetryAtMs) {
        val at = autoRetryAtMs ?: return@LaunchedEffect
        delay((at - System.currentTimeMillis()).coerceAtLeast(0L))
        busy = true
        try {
            onRetry()
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            // The repository records the new reason; the screen follows it.
            android.util.Log.w("lakko-auth", "automatic publish retry did not finish: ${cause.javaClass.simpleName}")
        } finally {
            busy = false
        }
    }
    // Another server limit (not the cool-down) says when to try again; the
    // button waits with it.
    var waitUntilMs by remember(reason) {
        mutableStateOf(
            if (autoRetryAtMs != null) {
                null
            } else {
                (reason as? PublishOwedReason.Retryable)?.retryAfterSecs?.let { reason.observedAtMs + it * 1000L }
            },
        )
    }
    val waitLeft = rememberRemainingMs(waitUntilMs)

    AuthScaffold(title = t(publishOwedTitleKey(wordless)), onBack = null, modifier = modifier) {
        val lead = buildList {
            addAll(model.leadKeys.map { t(it) })
            val waitingKey = model.waitingKey
            val minutes = model.waitingMinutes
            if (waitingKey != null && minutes != null) {
                add(t(waitingKey, emptyMap(), mapOf("minutes" to minutes.toULong())))
            }
        }
        Lead(lead.joinToString("\n\n"))
        if (model.showTryAgain) {
            if (waitLeft > 0L) {
                FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft))))
            } else {
                FormError(error)
            }
        }
        Spacer(Modifier.height(24.dp))
        if (model.showTryAgain) {
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
