// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.PersistableBundle
import android.util.Log
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.size
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.TotpEnrollStartResult
import me.lakko.app.auth.TotpSetupFailure
import me.lakko.app.auth.formatCountdown
import me.lakko.app.auth.groupedTotpKey
import me.lakko.app.auth.totpCodeToCheck
import me.lakko.app.auth.totpSetupFailureOf
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t

private const val TAG = "lakko-totp"

/**
 * "Use an authenticator app" (UX 19.1b): the QR code, the key with `Copy`,
 * `Open in authenticator app` (a phone cannot scan its own screen), and the
 * six-digit code, checked on the sixth digit with no separate button. The
 * same screen from the sign-up step and from Settings > Security > Recovery.
 *
 * Setting one up needs the 17.1 proof (it is one of the reset's factors):
 * the sign-up counts for ten minutes; otherwise the block is drawn first.
 * `Back` before the code was accepted sets nothing up; the key shown is then
 * never used, and the next setup makes a new one. The key lives in this
 * screen's memory only.
 */
@Composable
public fun AuthenticatorSetupScreen(
    repository: AuthRepository,
    /** The code was accepted (or the account turned out to have an
     *  authenticator already): the row is done. */
    onDone: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
    /** Settings shows the "done" line before it closes; the sign-up step
     *  returns at once with its row marked. */
    showDoneLine: Boolean = false,
) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val proof = rememberProofController(repository, ProofSurface.AddTotp)
    var setup: TotpEnrollStartResult? by remember { mutableStateOf(null) }
    var starting by remember { mutableStateOf(false) }
    var checking by remember { mutableStateOf(false) }
    var code by remember { mutableStateOf("") }
    var line: UiText? by remember { mutableStateOf(null) }
    var waitUntilMs: Long? by remember { mutableStateOf(null) }
    // The setup is not on screen after a failure: `Try again` starts over
    // with a new key.
    var failed by remember { mutableStateOf(false) }
    var done by remember { mutableStateOf(false) }
    var alreadyEnabled by remember { mutableStateOf(false) }

    fun apply(failure: TotpSetupFailure) {
        when (failure) {
            TotpSetupFailure.ProofRequired -> proof.onProofRefused()
            TotpSetupFailure.WrongCode -> {
                code = ""
                line = UiText("settings.recovery.totp.error.wrong")
            }
            is TotpSetupFailure.Wait -> {
                code = ""
                val secs = failure.retryAfterSecs
                if (secs != null && secs > 0L) {
                    waitUntilMs = System.currentTimeMillis() + secs * 1000L
                } else {
                    line = UiText("common.error.too_many_attempts")
                }
                if (setup == null) failed = true
            }
            TotpSetupFailure.AlreadyEnabled -> {
                repository.noteTotpEnabled()
                alreadyEnabled = true
            }
            TotpSetupFailure.Offline -> {
                // The QR and the key stay when they are on screen.
                line = UiText("common.error.offline")
                if (setup == null) failed = true
            }
            TotpSetupFailure.Generic -> {
                failed = true
                setup = null
                code = ""
                line = UiText("settings.recovery.totp.error.generic")
            }
        }
    }

    val start: () -> Unit = start@{
        if (starting || setup != null) return@start
        starting = true
        failed = false
        line = null
        scope.launch {
            try {
                if (!proof.ensure(context)) return@launch
                setup = repository.totpEnrolStart()
                proof.clear()
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (cause: Throwable) {
                Log.w(TAG, "WARN the authenticator setup did not start: ${cause.javaClass.simpleName}")
                apply(totpSetupFailureOf(finishing = false, err = cause))
            } finally {
                starting = false
            }
        }
    }

    val check: (String) -> Unit = check@{ digits ->
        if (checking) return@check
        checking = true
        line = null
        scope.launch {
            try {
                repository.totpEnrolFinish(digits)
                code = ""
                if (showDoneLine) done = true else onDone()
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (cause: Throwable) {
                Log.w(TAG, "WARN the authenticator code was not accepted: ${cause.javaClass.simpleName}")
                apply(totpSetupFailureOf(finishing = true, err = cause))
            } finally {
                checking = false
            }
        }
    }

    // With a live proof (the sign-up, or one given a moment ago) the setup
    // starts by itself; otherwise the block's button starts it.
    LaunchedEffect(proof.proofLive) {
        if (proof.proofLive && setup == null && !failed && !done && !alreadyEnabled) start()
    }

    BackHandler(enabled = !checking) { if (done || alreadyEnabled) onDone() else onBack() }
    val waitLeft = rememberRemainingMs(waitUntilMs)

    AuthScaffold(
        title = t("protect.totp.title"),
        onBack = if (checking) null else ({ if (done || alreadyEnabled) onDone() else onBack() }),
        modifier = modifier,
    ) {
        if (done || alreadyEnabled) {
            Column(modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite }) {
                Lead(t(if (done) "settings.recovery.totp.done" else "settings.recovery.totp.already_enabled"))
            }
            Spacer(Modifier.height(24.dp))
            BusyButton(label = t("common.done"), busy = false, onClick = onDone)
            Spacer(Modifier.height(16.dp))
            return@AuthScaffold
        }
        Lead(t("settings.recovery.totp.scan"))
        Spacer(Modifier.height(16.dp))
        val ready = setup
        if (ready == null) {
            val view = proof.view()
            if (view.visible) {
                ProofBlock(controller = proof, onSubmit = start)
                Spacer(Modifier.height(16.dp))
            }
            FormError(line)
            if (waitLeft > 0L) {
                FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft))))
            }
            if (starting || (proof.proofLive && !failed)) {
                // Starting: the QR area shows a spinner, nothing else usable.
                Row(horizontalArrangement = Arrangement.Center, modifier = Modifier.fillMaxWidth()) {
                    CircularProgressIndicator()
                }
            } else {
                Spacer(Modifier.height(8.dp))
                BusyButton(
                    label = if (failed) t("common.try_again") else t("common.continue"),
                    busy = view.working,
                    busyLabel = t("common.checking"),
                    enabled = view.mainEnabled && waitLeft <= 0L,
                    onClick = start,
                )
            }
            Spacer(Modifier.height(16.dp))
            return@AuthScaffold
        }

        Row(horizontalArrangement = Arrangement.Center, modifier = Modifier.fillMaxWidth()) {
            me.lakko.app.ui.devices.BrandedQrImage(
                text = ready.otpauthUri,
                size = 220.dp,
                contentDescription = t("settings.recovery.totp.scan"),
            )
        }
        Spacer(Modifier.height(16.dp))
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                t("settings.recovery.totp.key", mapOf("key" to groupedTotpKey(ready.secretB32))),
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = { copyTotpKey(context, ready.secretB32) }) { Text(t("common.copy")) }
        }
        Spacer(Modifier.height(8.dp))
        // A link to another app, not a share sheet: whichever authenticator
        // app the phone has takes the setup link.
        OutlinedButton(
            onClick = { openInAuthenticator(context, ready.otpauthUri) },
            modifier = Modifier.fillMaxWidth(),
        ) { Text(t("settings.recovery.totp.open_app")) }
        Spacer(Modifier.height(16.dp))
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Column(modifier = Modifier.weight(1f)) {
                CodeField(
                    value = code,
                    onValueChange = { typed ->
                        code = typed
                        line = null
                        totpCodeToCheck(typed)?.let(check)
                    },
                    enabled = !checking && waitLeft <= 0L,
                    label = t("recovery.totp.code_label"),
                    onDone = { totpCodeToCheck(code)?.let(check) },
                )
            }
            if (checking) CircularProgressIndicator(modifier = Modifier.size(20.dp))
        }
        Column(modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite }) { FormError(line) }
        if (waitLeft > 0L) {
            FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft))))
        }
        Spacer(Modifier.height(16.dp))
    }
}

/** The key itself (no spaces), marked sensitive so the system does not show
 *  it in the clipboard preview. */
private fun copyTotpKey(context: Context, secretB32: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return
    val clip = ClipData.newPlainText(me.lakko.app.l10n.Localization.strings.value["settings.recovery.totp.title"], secretB32)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        clip.description.extras = PersistableBundle().apply {
            putBoolean(android.content.ClipDescription.EXTRA_IS_SENSITIVE, true)
        }
    }
    clipboard.setPrimaryClip(clip)
}

/** Hand the `otpauth://` setup link to the phone's authenticator app. */
private fun openInAuthenticator(context: Context, otpauthUri: String) {
    if (!otpauthUri.startsWith("otpauth://")) return
    try {
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(otpauthUri)))
    } catch (_: ActivityNotFoundException) {
        // No authenticator app on the phone; the button does nothing. The QR
        // code and the key above remain.
    } catch (_: SecurityException) {
        // The same.
    }
}
