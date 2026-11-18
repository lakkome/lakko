// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import android.util.Log
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.auth.AccountNotice
import me.lakko.app.auth.AccountNoticeShape
import me.lakko.app.auth.AccountProtection
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.EMAIL_CODE_DIGITS
import me.lakko.app.auth.EmailRecoverFailure
import me.lakko.app.auth.FreshAuthRequired
import me.lakko.app.auth.NotMeDestination
import me.lakko.app.auth.PhraseSavePath
import me.lakko.app.auth.PhraseSavedOutcome
import me.lakko.app.auth.RecoveryEmailEnrolStart
import me.lakko.app.auth.RecoveryEmailRow
import me.lakko.app.auth.RecoveryPhraseRow
import me.lakko.app.auth.accountNoticeBodyKey
import me.lakko.app.auth.accountNoticeShape
import me.lakko.app.auth.accountNoticeTitleKey
import me.lakko.app.auth.accountProtection
import me.lakko.app.auth.formatCountdown
import me.lakko.app.auth.normalizeEmailCode
import me.lakko.app.auth.notMeDestination
import me.lakko.app.auth.protectBannerVisible
import me.lakko.app.auth.protectStepRows
import me.lakko.app.auth.protectionStatusKey
import me.lakko.app.auth.recoveryEmailEnrolFailureOf
import me.lakko.app.auth.recoveryEmailPlausible
import me.lakko.app.auth.recoveryEmailRow
import me.lakko.app.auth.recoveryPhraseRow
import me.lakko.app.auth.secureAccountRows
import me.lakko.app.chat.ChatRepository
import me.lakko.app.devices.KeyRotationDevice
import me.lakko.app.devices.SavePhraseOutcome
import me.lakko.app.devices.savePhraseOutcome
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/*
 * ADR-0075 / `docs/ux/identity-recovery.md` section 19 - the screens of an
 * account's protection: the "Protect your account" step of sign-up, the
 * recovery email form, "Save a recovery phrase", the Settings > Account rows
 * and the notices the account's other devices show. Every decision is a pure
 * function in `me.lakko.app.auth` (`AccountProtection.kt`,
 * `EmailRecoverFlow.kt`, `AccountNotices.kt`); this file draws them.
 *
 * The address lives in a `remember` of the form that asked for it and
 * nowhere else: never saved state, never a store, never a log line.
 */

private const val TAG = "lakko-protect"

/** A six-digit code field. */
@Composable
internal fun CodeField(
    value: String,
    onValueChange: (String) -> Unit,
    enabled: Boolean,
    label: String,
    onDone: () -> Unit,
) {
    OutlinedTextField(
        value = value,
        onValueChange = { typed -> onValueChange(typed.filter { it.isDigit() }.take(EMAIL_CODE_DIGITS)) },
        label = { Text(label) },
        singleLine = true,
        enabled = enabled,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword, imeAction = ImeAction.Done),
        keyboardActions = KeyboardActions(onDone = { onDone() }),
        visualTransformation = androidx.compose.ui.text.input.VisualTransformation.None,
        modifier = Modifier.fillMaxWidth(),
    )
}

/** The email address field: never capitalised, never corrected, never saved. */
@Composable
internal fun EmailField(value: String, onValueChange: (String) -> Unit, enabled: Boolean, onDone: () -> Unit) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(t("recovery_email.field")) },
        singleLine = true,
        enabled = enabled,
        keyboardOptions = KeyboardOptions(
            capitalization = KeyboardCapitalization.None,
            autoCorrectEnabled = false,
            keyboardType = KeyboardType.Email,
            imeAction = ImeAction.Done,
        ),
        keyboardActions = KeyboardActions(onDone = { onDone() }),
        modifier = Modifier.fillMaxWidth(),
    )
}

/**
 * Recovery email (UX 19.2): the address, then the mailed code, on one
 * surface. Used to add and to replace (the same two steps). The proof block
 * of 17.1 is drawn above the address while no proof is live.
 */
@Composable
public fun RecoveryEmailScreen(
    repository: AuthRepository,
    onDone: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val authState by repository.state.collectAsStateWithLifecycle()
    val status by repository.recoveryEmail.collectAsStateWithLifecycle()
    val heldAddress by repository.recoveryAddress.collectAsStateWithLifecycle()
    val username = (authState as? AuthState.Session)?.account?.username.orEmpty()
    val proof = rememberProofController(repository, ProofSurface.RecoveryEmail)

    // Memory only, and only while this surface is open.
    var address by remember { mutableStateOf("") }
    var code by remember { mutableStateOf("") }
    var started: RecoveryEmailEnrolStart? by remember { mutableStateOf(null) }
    var resendAtMs: Long? by remember { mutableStateOf(null) }
    var waitUntilMs: Long? by remember { mutableStateOf(null) }
    var busy by remember { mutableStateOf(false) }
    var error: UiText? by remember { mutableStateOf(null) }
    var unavailable by remember { mutableStateOf(false) }
    // Wrong tries on the code that was sent (five per code).
    var wrongTries by remember { mutableStateOf(0) }

    LaunchedEffect(Unit) { if (status == null) repository.refreshRecoveryEmail() }
    // After a recall the server dropped the stored value: the form opens
    // with the address this device holds. A replacement starts empty.
    var prefilled by remember { mutableStateOf(false) }
    LaunchedEffect(status, heldAddress) {
        val known = status ?: return@LaunchedEffect
        if (!prefilled && address.isEmpty() && started == null) {
            val start = me.lakko.app.auth.enrolPrefill(known.enabled, heldAddress)
            if (start.isNotEmpty()) address = start
            prefilled = true
        }
    }

    fun apply(failure: EmailRecoverFailure) {
        when (failure) {
            is EmailRecoverFailure.Line -> error = failure.text
            is EmailRecoverFailure.WrongCode -> {
                code = ""
                wrongTries += 1
                if (failure.text.key == "recovery_email.code.wrong" && me.lakko.app.auth.emailCodeUsedUp(wrongTries)) {
                    // Too many wrong codes: back to the address step.
                    error = UiText("recovery_email.code.used_up")
                    started = null
                } else {
                    error = failure.text
                }
            }
            is EmailRecoverFailure.Restart -> {
                // The code is used up: back to the address step.
                error = failure.text
                code = ""
                started = null
            }
            is EmailRecoverFailure.Wait -> {
                error = if (failure.retryAfterSecs == null) UiText("common.error.too_many_attempts") else null
                waitUntilMs = failure.retryAfterSecs?.let { System.currentTimeMillis() + it * 1000L }
            }
            EmailRecoverFailure.Unavailable -> unavailable = true
        }
    }

    val send: () -> Unit = send@{
        if (busy || !recoveryEmailPlausible(address)) return@send
        busy = true
        error = null
        scope.launch {
            try {
                if (!proof.ensure(context)) return@launch
                val start = repository.recoveryEmailEnrolStart(address.trim())
                started = start
                code = ""
                wrongTries = 0
                resendAtMs = System.currentTimeMillis() + start.resendAfterSecs * 1000L
                proof.clear()
            } catch (cause: FreshAuthRequired) {
                proof.onProofRefused()
            } catch (cause: Throwable) {
                if (cause is CancellationException) throw cause
                Log.w(TAG, "WARN recovery email not started: ${cause.javaClass.simpleName}")
                apply(recoveryEmailEnrolFailureOf(finishing = false, err = cause))
            } finally {
                busy = false
            }
        }
    }

    val finish: () -> Unit = finish@{
        val start = started ?: return@finish
        val digits = normalizeEmailCode(code) ?: return@finish
        if (busy) return@finish
        busy = true
        error = null
        scope.launch {
            try {
                // Seals the vault off the main thread (64 MiB Argon2id) when
                // no phrase is saved, then sends the code.
                repository.recoveryEmailEnrolFinish(start, address.trim(), digits)
                address = ""
                code = ""
                onDone()
            } catch (cause: Throwable) {
                if (cause is CancellationException) throw cause
                Log.w(TAG, "WARN recovery email not confirmed: ${cause.javaClass.simpleName}")
                apply(recoveryEmailEnrolFailureOf(finishing = true, err = cause))
            } finally {
                busy = false
            }
        }
    }

    BackHandler(enabled = !busy) { if (started != null) started = null else onBack() }
    val waitLeft = rememberRemainingMs(waitUntilMs)

    AuthScaffold(
        title = t("recovery_email.title"),
        onBack = if (busy) null else ({ if (started != null) started = null else onBack() }),
        modifier = modifier,
    ) {
        if (username.isBlank()) {
            // An account with no username: the stored value is made from
            // it. (A guest never reaches this screen: it is asked to create
            // an account after its call, docs/ux/call-links.md section 9.)
            Lead(t("recovery_email.needs_username"))
            return@AuthScaffold
        }
        if (unavailable || status?.available == false) {
            FormError(t("recovery_email.unavailable"))
            return@AuthScaffold
        }
        val current = started
        if (current == null) {
            Lead(t("recovery_email.body"))
            Spacer(Modifier.height(16.dp))
            ProofBlock(
                controller = proof,
                onSubmit = send,
                onPasskey = { scope.launch { proof.provePasskey(context) } },
            )
            if (proof.view().visible) Spacer(Modifier.height(12.dp))
            EmailField(value = address, onValueChange = { address = it; error = null }, enabled = !busy, onDone = send)
            if (address.isNotBlank() && !recoveryEmailPlausible(address)) {
                FormError(t("recovery_email.invalid"))
            }
            Spacer(Modifier.height(12.dp))
            Lead(t("recovery_email.not_kept"))
            // The mailbox warning only while no phrase is saved.
            if (status?.phraseSaved != true) {
                Spacer(Modifier.height(8.dp))
                Lead(t("recovery_email.mailbox_warning"))
            }
            // The account has an authenticator: its code is asked for too.
            if (status?.totp == true) {
                Spacer(Modifier.height(8.dp))
                Lead(t("recovery_email.totp_warning"))
            }
            FormError(error)
            if (waitLeft > 0L) {
                FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft))))
            }
            Spacer(Modifier.height(24.dp))
            BusyButton(
                label = t("recovery_email.send"),
                busy = busy || proof.view().working,
                busyLabel = t("common.sending"),
                enabled = recoveryEmailPlausible(address) && waitLeft <= 0L && proof.view().mainEnabled,
                onClick = send,
            )
        } else {
            Text(t("recovery_email.code.title"), style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(8.dp))
            // The address is shown back here and only until this closes.
            Lead(t("recovery_email.code.sent_to", mapOf("address" to "⁦${address.trim()}⁩")))
            Spacer(Modifier.height(16.dp))
            CodeField(code, { code = it; error = null }, enabled = !busy, label = t("recovery_email.code.title"), onDone = finish)
            Column(modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite }) { FormError(error) }
            if (waitLeft > 0L) {
                FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft))))
            }
            Spacer(Modifier.height(16.dp))
            BusyButton(
                label = t("common.continue"),
                busy = busy,
                busyLabel = t("common.checking"),
                enabled = normalizeEmailCode(code) != null,
                onClick = finish,
            )
            ResendRow(
                resendAtMs = resendAtMs,
                enabled = !busy && waitLeft <= 0L,
                onResend = {
                    started = null
                    send()
                },
                secondLabel = t("recovery_email.code.other_address"),
                onSecond = {
                    started = null
                    address = ""
                    code = ""
                    error = null
                },
            )
        }
        Spacer(Modifier.height(16.dp))
    }
}

/** "Send again (in 0:45)" and a second plain action, side by side. */
@Composable
internal fun ResendRow(
    resendAtMs: Long?,
    enabled: Boolean,
    onResend: () -> Unit,
    secondLabel: String,
    onSecond: () -> Unit,
) {
    val left = rememberRemainingMs(resendAtMs)
    Row(horizontalArrangement = Arrangement.SpaceBetween, modifier = Modifier.fillMaxWidth()) {
        TextButton(onClick = onResend, enabled = enabled && left <= 0L) {
            Text(
                if (left > 0L) {
                    t("recovery_email.code.resend_in", mapOf("time" to formatCountdown(left)))
                } else {
                    t("recovery_email.code.resend")
                },
            )
        }
        TextButton(onClick = onSecond, enabled = enabled) { Text(secondLabel) }
    }
}

/**
 * Save a recovery phrase (UX 19.5, ADR-0075 section 3 as amended 2026-11-01).
 * `GET /account/phrase` first, and no path is guessed when it fails.
 *
 * A phrase that was never in an email vault keeps the keys: the words the
 * device holds, the three-word check, then `POST /account/phrase/saved`.
 * Nothing counts as saved before that answer; a failure stays on the
 * three-word step with `Try again`, and a person who leaves has saved nothing.
 *
 * Otherwise (and on a `409` from that POST) the ADR-0074 key rotation with
 * `phrase_saved` on its server step, removing no device: the NEW words are
 * shown only once that step succeeded; until then a progress line, and on a
 * failure "We could not finish. Nothing changed." with `Try again`.
 */
@Composable
public fun SaveRecoveryPhraseScreen(
    repository: AuthRepository,
    chatRepository: ChatRepository?,
    onDone: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val proof = rememberProofController(repository, ProofSurface.SavePhrase)
    var working by remember { mutableStateOf(false) }
    var failed by remember { mutableStateOf(false) }
    var waitUntilMs: Long? by remember { mutableStateOf(null) }
    // Non-null once the key change went through: the words (or 15.4's
    // withheld state) replace this screen.
    var words: SavePhraseOutcome.ShowWords? by remember { mutableStateOf(null) }
    // `null` until the server answered; nothing is decided without it.
    var path: PhraseSavePath? by remember { mutableStateOf(null) }
    // Non-null once the words the device already holds are on screen.
    var heldWords: String? by remember { mutableStateOf(null) }
    var finishing by remember { mutableStateOf(false) }
    var finishFailed by remember { mutableStateOf(false) }

    suspend fun readPath() {
        try {
            path = repository.phraseSavePath()
        } catch (cause: Throwable) {
            if (cause is CancellationException) throw cause
            Log.w(TAG, "WARN recovery phrase path not read: ${cause.javaClass.simpleName}")
            failed = true
        }
    }

    LaunchedEffect(Unit) { readPath() }

    val run: () -> Unit = run@{
        if (working) return@run
        working = true
        failed = false
        scope.launch {
            try {
                if (path == null) {
                    // `Try again` after a failed read: read, then the person
                    // sees what this path says before going on.
                    readPath()
                    return@launch
                }
                if (!proof.ensure(context)) return@launch
                if (path == PhraseSavePath.KeepKeys) {
                    val held = withContext(Dispatchers.IO) { repository.devicePhrase() }
                    if (held != null) {
                        proof.clear()
                        heldWords = held
                    } else {
                        path = PhraseSavePath.Rotate
                    }
                    return@launch
                }
                val outcome = withContext(Dispatchers.IO) {
                    repository.ensureIdentityKeyRecorded()
                    val runner = KeyRotationDevice.runner(
                        context = context,
                        authRepository = repository,
                        chatRepository = chatRepository,
                        savingPhrase = true,
                    ) ?: throw IllegalStateException("no session")
                    val result = runner.savePhrase()
                    savePhraseOutcome(result, runner.owed())
                }
                when (outcome) {
                    is SavePhraseOutcome.ShowWords -> {
                        proof.clear()
                        words = outcome
                    }
                    SavePhraseOutcome.ShowProofBlock -> proof.onProofRefused()
                    is SavePhraseOutcome.Failed -> {
                        failed = true
                        waitUntilMs = outcome.retryAfterSecs?.let { System.currentTimeMillis() + it * 1000L }
                    }
                    SavePhraseOutcome.Unsettled -> failed = true
                }
            } catch (cause: Throwable) {
                if (cause is CancellationException) throw cause
                Log.w(TAG, "WARN recovery phrase not saved: ${cause.javaClass.simpleName}")
                failed = true
            } finally {
                working = false
            }
        }
    }

    // After the three words: the account counts as protected from the
    // server's answer on, not before.
    val finishSave: () -> Unit = finishSave@{
        if (finishing) return@finishSave
        finishing = true
        finishFailed = false
        scope.launch {
            try {
                if (!proof.ensure(context)) return@launch
                when (withContext(Dispatchers.IO) { repository.savePhraseKeepingKeys() }) {
                    PhraseSavedOutcome.Saved -> {
                        proof.clear()
                        scope.launch { repository.refreshRecoveryEmail() }
                        onDone()
                    }
                    // This phrase was in an email vault after all: the words
                    // on screen are not the ones to keep.
                    PhraseSavedOutcome.NeedsRotation -> {
                        path = PhraseSavePath.Rotate
                        heldWords = null
                    }
                    PhraseSavedOutcome.ProofRequired -> proof.onProofRefused()
                    PhraseSavedOutcome.Failed -> finishFailed = true
                }
            } catch (cause: Throwable) {
                if (cause is CancellationException) throw cause
                Log.w(TAG, "WARN recovery phrase not marked saved: ${cause.javaClass.simpleName}")
                finishFailed = true
            } finally {
                finishing = false
            }
        }
    }

    val heldPhrase = heldWords
    if (heldPhrase != null) {
        // Leaving saves nothing: the row in Settings stays as it was.
        BackHandler(enabled = !finishing) { heldWords = null }
        RecoveryPhraseOnboarding(
            phrase = heldPhrase,
            onDone = finishSave,
            title = t("protect.phrase.title"),
            lead = t("protect.phrase.save.body"),
            onLeave = { heldWords = null },
            finish = PhraseFinish(
                busy = finishing || proof.view().working,
                failure = if (finishFailed) t("protect.phrase.save.failed") else null,
                enabled = proof.view().mainEnabled,
                above = {
                    ProofBlock(
                        controller = proof,
                        onSubmit = finishSave,
                        onPasskey = { scope.launch { proof.provePasskey(context) } },
                    )
                },
            ),
            modifier = modifier,
        )
        return
    }

    val shown = words
    if (shown != null) {
        me.lakko.app.ui.devices.RotatedPhraseScreen(
            authRepository = repository,
            chatRepository = chatRepository,
            initiallyWithheld = shown.withheld,
            onClosed = {
                scope.launch { repository.refreshRecoveryEmail() }
                onDone()
            },
            title = t("protect.phrase.title"),
            lead = t("protect.phrase.save.body"),
            withheldLine = t("protect.phrase.save.finishing"),
        )
        return
    }

    BackHandler(enabled = !working) { onBack() }
    val waitLeft = rememberRemainingMs(waitUntilMs)
    AuthScaffold(title = t("protect.phrase.title"), onBack = if (working) null else onBack, modifier = modifier) {
        Lead(t("protect.phrase.save.body"))
        // Only where the keys change.
        if (path == PhraseSavePath.Rotate) {
            Spacer(Modifier.height(12.dp))
            Lead(t("protect.phrase.save.effect"))
        }
        Spacer(Modifier.height(16.dp))
        ProofBlock(
            controller = proof,
            onSubmit = run,
            onPasskey = { scope.launch { proof.provePasskey(context) } },
        )
        if (working && path == PhraseSavePath.Rotate) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier
                    .padding(vertical = 8.dp)
                    .semantics { liveRegion = LiveRegionMode.Polite },
            ) {
                CircularProgressIndicator()
                Text(t("protect.phrase.save.progress"))
            }
        }
        if (failed) FormError(t("protect.phrase.save.failed"))
        if (waitLeft > 0L) {
            FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft))))
        }
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = if (failed) t("common.try_again") else t("protect.phrase.save.show"),
            busy = working || proof.view().working || (path == null && !failed),
            busyLabel = t("common.checking"),
            enabled = waitLeft <= 0L && (path == null || proof.view().mainEnabled),
            onClick = run,
        )
        Spacer(Modifier.height(16.dp))
    }
}

/**
 * "Protect your account" (UX 19.1a): the one step after sign-up. `Add an
 * email` (absent when the server cannot send mail or the account has no
 * username), `Save a recovery phrase`, and `Not now` behind the app's own
 * confirmation. Any of the three ends the step; the other stays in Settings.
 */
@Composable
public fun ProtectAccountStep(
    repository: AuthRepository,
    chatRepository: ChatRepository?,
    hasUsername: Boolean,
    modifier: Modifier = Modifier,
) {
    var sub: ProtectSub by remember { mutableStateOf(ProtectSub.Choose) }
    var confirmingSkip by remember { mutableStateOf(false) }
    val mailAvailable by produceState<Boolean?>(initialValue = null, repository) {
        value = repository.mailAvailable()
    }
    val rows = protectStepRows(
        mailAvailable = mailAvailable == true,
        hasUsername = hasUsername,
        passkeysSupported = ProofController.PASSKEYS_SUPPORTED,
    )
    // What was finished on this step; a row already true of the account is
    // marked the same way.
    var doneHere by remember { mutableStateOf(me.lakko.app.auth.ProtectStepDone()) }
    val status by repository.recoveryEmail.collectAsStateWithLifecycle()
    // The account may already have a passkey (a step shown again after a
    // restart): asked once, `false` until it answered.
    val hasPasskey by produceState(initialValue = false, repository) {
        value = repository.accountHasPasskey() == true
    }
    val done = me.lakko.app.auth.protectStepDone(doneHere, status, hasPasskey)
    val exit = me.lakko.app.auth.protectStepExit(done)

    when (sub) {
        ProtectSub.Email -> {
            RecoveryEmailScreen(
                repository = repository,
                // Back on the step; the row is marked done from the
                // account's status, which the confirmation just updated.
                onDone = { sub = ProtectSub.Choose },
                onBack = { sub = ProtectSub.Choose },
                modifier = modifier,
            )
            return
        }
        ProtectSub.Passkey -> {
            // The existing passkey creation flow (its own "also restore my
            // keys" choice included), under sign-up's fresh grant.
            BackHandler(enabled = true) { sub = ProtectSub.Choose }
            SignupPasskeyOffer(
                repository = repository,
                onDone = { sub = ProtectSub.Choose },
                modifier = modifier,
                onAdded = { doneHere = doneHere.copy(passkey = true) },
            )
            return
        }
        ProtectSub.Phrase -> {
            SaveRecoveryPhraseScreen(
                repository = repository,
                chatRepository = chatRepository,
                onDone = { repository.finishProtectStep() },
                onBack = { sub = ProtectSub.Choose },
                modifier = modifier,
            )
            return
        }
        ProtectSub.Choose -> Unit
    }

    // The step has no way out but its three answers.
    val leave: () -> Unit = {
        if (exit == me.lakko.app.auth.ProtectStepExit.Continue) repository.finishProtectStep() else confirmingSkip = true
    }
    BackHandler(enabled = true) { leave() }
    val doneMark = t("protect.done")
    AuthScaffold(title = t("protect.title"), onBack = null, modifier = modifier) {
        Lead(t("protect.body"))
        Spacer(Modifier.height(16.dp))
        if (rows.email) {
            MethodRow(
                icon = R.drawable.ic_lk_envelope,
                title = t("protect.email.title") + " · " +
                    if (done.email) doneMark else t("protect.email.badge"),
                subtitle = t("protect.email.body"),
                onClick = { sub = ProtectSub.Email },
                enabled = !done.email,
            )
        }
        if (rows.passkey) {
            MethodRow(
                icon = R.drawable.ic_lk_passkey,
                title = t("protect.passkey.title") + if (done.passkey) " · $doneMark" else "",
                subtitle = t("protect.passkey.body"),
                onClick = { sub = ProtectSub.Passkey },
                enabled = !done.passkey,
            )
        }
        if (rows.phrase) {
            MethodRow(
                icon = R.drawable.ic_lk_key,
                title = t("protect.phrase.title") + if (done.phrase) " · $doneMark" else "",
                subtitle = t("protect.phrase.body"),
                onClick = { sub = ProtectSub.Phrase },
                enabled = !done.phrase,
            )
        }
        Spacer(Modifier.height(16.dp))
        if (exit == me.lakko.app.auth.ProtectStepExit.Continue) {
            BusyButton(label = t("protect.continue"), busy = false, onClick = leave)
        } else {
            TextButton(onClick = leave, modifier = Modifier.fillMaxWidth()) { Text(t("protect.skip")) }
        }
        Spacer(Modifier.height(16.dp))
    }

    if (confirmingSkip) {
        LakkoDialog(
            onDismissRequest = { confirmingSkip = false },
            title = t("protect.skip.confirm.title"),
            body = t("protect.skip.confirm.body"),
            confirm = if (rows.email) {
                LakkoDialogAction(t("protect.email.title")) { confirmingSkip = false; sub = ProtectSub.Email }
            } else {
                LakkoDialogAction(t("protect.phrase.title")) { confirmingSkip = false; sub = ProtectSub.Phrase }
            },
            // Not Cancel: the person goes on without protection.
            cancel = LakkoDialogAction(t("protect.skip.confirm.continue"), LakkoDialogTone.Ghost) {
                confirmingSkip = false
                repository.finishProtectStep()
            },
        )
    }
}

private enum class ProtectSub { Choose, Email, Passkey, Phrase }

/**
 * Settings > Account (UX 19.1): the status line, the no-protection banner,
 * the `Recovery email` row (On / Off; `Replace` and `Remove`; never the
 * address) and the `Recovery phrase` row.
 */
@Composable
public fun AccountProtectionSection(
    repository: AuthRepository,
    onOpenRecoveryEmail: () -> Unit,
    onOpenSavePhrase: () -> Unit,
    /** `Show phrase` lives on Settings > Security > Recovery. */
    onOpenShowPhrase: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val status by repository.recoveryEmail.collectAsStateWithLifecycle()
    // The address this device holds (the account's devices keep it).
    val heldAddress by repository.recoveryAddress.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) { repository.refreshRecoveryEmail() }
    val known = status ?: return
    val addressHeld = heldAddress != null
    // A device that holds the address re-seals by itself: nothing is asked.
    val resealing = me.lakko.app.auth.resealNeeded(known.enabled, known.vault, known.phraseSaved, addressHeld)
    val protection = if (resealing) AccountProtection.Email else accountProtection(known)
    var bannerRevision by remember { mutableStateOf(0) }
    // An account with a passkey has a way back in: no banner. Unknown until
    // the passkey list answered, and until then the banner waits.
    val hasPasskey by produceState<Boolean?>(initialValue = null, repository) {
        value = repository.accountHasPasskey() ?: false
    }
    val bannerShown = remember(protection, bannerRevision, hasPasskey) {
        hasPasskey != null && protectBannerVisible(
            protection,
            repository.protectBannerDismissedAtMs(),
            System.currentTimeMillis(),
            hasPasskey = hasPasskey == true,
        )
    }
    val phraseOnDevice by produceState(initialValue = false, repository, known.phraseSaved) {
        value = withContext(Dispatchers.IO) { repository.devicePhrase() != null }
    }
    var confirmingRemove by remember { mutableStateOf(false) }
    var removing by remember { mutableStateOf(false) }
    var removeLine: UiText? by remember { mutableStateOf(null) }
    val proof = rememberProofController(repository, ProofSurface.RecoveryEmail, key = "remove")
    val background = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background)

    ListItem(
        headlineContent = { Text(t(protectionStatusKey(protection))) },
        leadingContent = {
            LkIcon(
                R.drawable.ic_lk_lock_shield,
                contentDescription = null,
                tint = if (protection == AccountProtection.None) {
                    MaterialTheme.colorScheme.error
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
                size = LkIconSize.Row,
            )
        },
        colors = background,
    )
    if (bannerShown) {
        Surface(
            color = MaterialTheme.colorScheme.errorContainer,
            shape = MaterialTheme.shapes.medium,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp).fillMaxWidth(),
        ) {
            Column(modifier = Modifier.padding(12.dp)) {
                val prompt = me.lakko.app.auth.protectPrompt(known, addressHeld)
                Text(t(prompt.title), color = MaterialTheme.colorScheme.onErrorContainer)
                prompt.body?.let {
                    Text(
                        t(it),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onErrorContainer,
                    )
                }
                Row {
                    TextButton(
                        onClick = {
                            if (recoveryEmailRow(known, addressHeld) == RecoveryEmailRow.Absent) onOpenSavePhrase() else onOpenRecoveryEmail()
                        },
                    ) { Text(t("protect.banner.action")) }
                    TextButton(
                        onClick = {
                            repository.dismissProtectBanner()
                            bannerRevision += 1
                        },
                    ) { Text(t("common.dismiss")) }
                }
            }
        }
    }
    val emailRow = recoveryEmailRow(known, addressHeld)
    if (emailRow != RecoveryEmailRow.Absent) {
        val on = emailRow == RecoveryEmailRow.On
        ListItem(
            headlineContent = { Text(t("recovery_email.title")) },
            supportingContent = {
                Text(
                    t(
                        when (emailRow) {
                            RecoveryEmailRow.On -> "recovery_email.status.on"
                            RecoveryEmailRow.AddAgain -> "recovery_email.add_again.title"
                            else -> "recovery_email.status.off"
                        },
                    ).let { line ->
                        // A device that holds the address shows it, in plain
                        // text under the row title; one that does not shows On.
                        val shown = heldAddress
                        if (emailRow == RecoveryEmailRow.On && shown != null) "\u2066$shown\u2069" else line
                    },
                )
            },
            leadingContent = {
                LkIcon(R.drawable.ic_lk_envelope, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, size = LkIconSize.Row)
            },
            colors = background,
            modifier = if (on) Modifier else Modifier.clickable(onClick = onOpenRecoveryEmail),
        )
        if (on) {
            Row(modifier = Modifier.padding(horizontal = 16.dp)) {
                TextButton(onClick = onOpenRecoveryEmail, enabled = !removing) { Text(t("recovery_email.replace")) }
                TextButton(onClick = { confirmingRemove = true }, enabled = !removing) {
                    Text(t("recovery_email.remove"), color = MaterialTheme.colorScheme.error)
                }
            }
            removeLine?.let { FormError(it, modifier = Modifier.padding(horizontal = 16.dp)) }
        }
    }
    when (recoveryPhraseRow(phraseSaved = known.phraseSaved, phraseOnDevice = phraseOnDevice)) {
        RecoveryPhraseRow.Save -> ListItem(
            headlineContent = { Text(t("protect.phrase.title")) },
            supportingContent = { Text(t("protect.phrase.body")) },
            leadingContent = {
                LkIcon(R.drawable.ic_lk_key, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, size = LkIconSize.Row)
            },
            colors = background,
            modifier = Modifier.clickable(onClick = onOpenSavePhrase),
        )
        RecoveryPhraseRow.Show -> ListItem(
            headlineContent = { Text(t("recovery.method.phrase.title")) },
            supportingContent = { Text(t("settings.recovery.phrase.show")) },
            leadingContent = {
                LkIcon(R.drawable.ic_lk_key, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, size = LkIconSize.Row)
            },
            colors = background,
            modifier = Modifier.clickable(onClick = onOpenShowPhrase),
        )
        RecoveryPhraseRow.SavedElsewhere -> ListItem(
            headlineContent = { Text(t("recovery.method.phrase.title")) },
            supportingContent = { Text(t("settings.recovery.phrase.not_saved_here")) },
            leadingContent = {
                LkIcon(R.drawable.ic_lk_key, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, size = LkIconSize.Row)
            },
            colors = background,
        )
    }

    if (confirmingRemove) {
        val remove: () -> Unit = remove@{
            if (removing) return@remove
            removing = true
            removeLine = null
            scope.launch {
                try {
                    if (!proof.ensure(context)) return@launch
                    repository.recoveryEmailRemove()
                    proof.clear()
                    confirmingRemove = false
                } catch (cause: FreshAuthRequired) {
                    proof.onProofRefused()
                } catch (cause: Throwable) {
                    if (cause is CancellationException) throw cause
                    Log.w(TAG, "WARN recovery email not removed: ${cause.javaClass.simpleName}")
                    val failure = recoveryEmailEnrolFailureOf(finishing = false, err = cause)
                    removeLine = (failure as? EmailRecoverFailure.Line)?.text ?: UiText("common.error.unavailable")
                    confirmingRemove = false
                } finally {
                    removing = false
                }
            }
        }
        LakkoDialog(
            onDismissRequest = { if (!removing) confirmingRemove = false },
            title = t("recovery_email.remove.confirm.title"),
            body = t("recovery_email.remove.confirm.body"),
            confirm = LakkoDialogAction(
                if (removing) t("common.removing") else t("recovery_email.remove"),
                LakkoDialogTone.Danger,
                enabled = !removing && proof.view().mainEnabled,
                onClick = remove,
            ),
            cancel = LakkoDialogAction(t("common.cancel"), LakkoDialogTone.Ghost, enabled = !removing) {
                confirmingRemove = false
            },
        ) {
            ProofBlock(
                controller = proof,
                onSubmit = remove,
                onPasskey = { scope.launch { proof.provePasskey(context) } },
            )
        }
    }
    HorizontalDivider()
}

/** The platform's medium date in the interface language. */
@Composable
private fun noticeDate(atMs: Long): String {
    val language = LocalStrings.current.language
    return remember(atMs, language) {
        me.lakko.app.l10n.DeviceFormats.mediumDate(atMs, language)
    }
}

/**
 * The notices of UX 19.2 / 19.5 / 19.6, one at a time, the next time the
 * app is opened on a signed-in device: what happened, `It was me` (or `OK`)
 * and `It was not me`. The latter opens the one screen of 19.6 or the
 * recovery email form, above the app.
 */
@Composable
public fun AccountNoticeHost(repository: AuthRepository, chatRepository: ChatRepository?) {
    val notices by repository.accountNotices.collectAsStateWithLifecycle()
    var destination: NotMeDestination? by remember { mutableStateOf(null) }

    val open = destination
    if (open != null) {
        Dialog(
            onDismissRequest = { destination = null },
            properties = DialogProperties(usePlatformDefaultWidth = false),
        ) {
            Surface(modifier = Modifier.fillMaxSize()) {
                SecureAccountScreen(
                    repository = repository,
                    chatRepository = chatRepository,
                    onClose = { destination = null },
                )
            }
        }
        return
    }

    val notice: AccountNotice = notices.firstOrNull() ?: return
    val notMe: () -> Unit = {
        // Reached from a notice: the password change on the secure screen is
        // open for ten minutes without another proof. Only here.
        repository.unlockPasswordChangeAfterNotice()
        repository.dismissAccountNotice(notice.id)
        destination = notMeDestination(notice.kind)
    }
    val alarm = accountNoticeShape(notice.kind) == AccountNoticeShape.Alarm
    val bodyKey = accountNoticeBodyKey(notice.kind)
    LakkoDialog(
        // Answered by one of its two buttons, never by a tap outside.
        onDismissRequest = {},
        title = if (alarm) t(accountNoticeTitleKey(notice.kind)) else null,
        body = if (bodyKey != null) {
            t(bodyKey, mapOf("date" to noticeDate(notice.atMs)))
        } else {
            t(accountNoticeTitleKey(notice.kind))
        },
        confirm = LakkoDialogAction(t("notice.was_me")) { repository.dismissAccountNotice(notice.id) },
        cancel = LakkoDialogAction(t("notice.was_not_me"), LakkoDialogTone.Ghost, onClick = notMe),
    )
}

/**
 * `It was not me` (UX 19.6): in order, choose a new password (this device
 * holds the keys, so nothing else is asked), `Remove recovery email`, and -
 * only while no phrase is saved - `Save a recovery phrase`.
 */
@Composable
private fun SecureAccountScreen(
    repository: AuthRepository,
    chatRepository: ChatRepository?,
    onClose: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val status by repository.recoveryEmail.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) { repository.refreshRecoveryEmail() }
    val rows = secureAccountRows(status)
    var passwordSaved by remember { mutableStateOf(false) }
    var savingPhrase by remember { mutableStateOf(false) }
    var removing by remember { mutableStateOf(false) }
    var removeLine: UiText? by remember { mutableStateOf(null) }
    val proof = rememberProofController(repository, ProofSurface.RecoveryEmail, key = "secure")

    if (savingPhrase) {
        SaveRecoveryPhraseScreen(
            repository = repository,
            chatRepository = chatRepository,
            onDone = { savingPhrase = false },
            onBack = { savingPhrase = false },
        )
        return
    }

    AuthScaffold(title = t("notice.secure.title"), onBack = onClose) {
        if (rows.newPassword) {
            if (passwordSaved) {
                Lead(t("notice.secure.password_saved"))
            } else if (repository.canReplacePassword()) {
                NewPasswordForm(repository = repository, onSaved = { passwordSaved = true })
            }
            Spacer(Modifier.height(16.dp))
            HorizontalDivider()
        }
        if (rows.removeEmail) {
            Spacer(Modifier.height(16.dp))
            ProofBlock(
                controller = proof,
                onPasskey = { scope.launch { proof.provePasskey(context) } },
            )
            OutlinedButton(
                enabled = !removing && proof.view().mainEnabled,
                modifier = Modifier.fillMaxWidth(),
                onClick = {
                    removing = true
                    removeLine = null
                    scope.launch {
                        try {
                            if (!proof.ensure(context)) return@launch
                            repository.recoveryEmailRemove()
                            proof.clear()
                        } catch (cause: FreshAuthRequired) {
                            proof.onProofRefused()
                        } catch (cause: Throwable) {
                            if (cause is CancellationException) throw cause
                            Log.w(TAG, "WARN recovery email not removed: ${cause.javaClass.simpleName}")
                            val failure = recoveryEmailEnrolFailureOf(finishing = false, err = cause)
                            removeLine = (failure as? EmailRecoverFailure.Line)?.text ?: UiText("common.error.unavailable")
                        } finally {
                            removing = false
                        }
                    }
                },
            ) { Text(if (removing) t("common.removing") else t("recovery_email.remove")) }
            FormError(removeLine)
        }
        if (rows.savePhrase) {
            Spacer(Modifier.height(16.dp))
            Lead(t("notice.secure.new_keys"))
            Spacer(Modifier.height(8.dp))
            OutlinedButton(onClick = { savingPhrase = true }, modifier = Modifier.fillMaxWidth()) {
                Text(t("protect.phrase.title"))
            }
        }
        Spacer(Modifier.height(24.dp))
        TextButton(onClick = onClose, modifier = Modifier.fillMaxWidth()) { Text(t("common.done")) }
        Spacer(Modifier.height(16.dp))
    }
}
