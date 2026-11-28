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
import androidx.compose.material3.HorizontalDivider
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
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.ChatsKeptBy
import me.lakko.app.auth.EMAIL_CODE_DIGITS
import me.lakko.app.auth.EmailRecoverFailure
import me.lakko.app.auth.FactorKind
import me.lakko.app.auth.FreshAuthRequired
import me.lakko.app.auth.NotMeDestination
import me.lakko.app.auth.PhraseSavedOutcome
import me.lakko.app.auth.RecoveryEmailEnrolStart
import me.lakko.app.auth.RecoveryEmailRow
import me.lakko.app.auth.RecoveryPhraseRow
import me.lakko.app.auth.accountNoticeBodyKey
import me.lakko.app.auth.accountNoticeShape
import me.lakko.app.auth.accountNoticeTitleKey
import me.lakko.app.auth.formatCountdown
import me.lakko.app.auth.normalizeEmailCode
import me.lakko.app.auth.notMeDestination
import me.lakko.app.auth.protectStepRows
import me.lakko.app.auth.protectionStatusKey
import me.lakko.app.auth.recoveryEmailEnrolFailureOf
import me.lakko.app.auth.recoveryEmailPlausible
import me.lakko.app.auth.recoveryEmailRow
import me.lakko.app.auth.recoveryPhraseRow
import me.lakko.app.auth.secureAccountRows
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
 * ADR-0075 (amended 2026-11-26) / `docs/ux/identity-recovery.md` section 19 -
 * the screens of an account's protection: the required "Protect your
 * account" step (19.1a, 19.1c), the recovery email form, "Save a recovery
 * phrase", the honest notice of 19.1d, the Settings > Account rows and the
 * notices the account's other devices show. Every decision is a pure
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
            // On every account: the mailbox reaches a new password, a
            // sign-in and the confirmation of a reset, never the keys.
            Spacer(Modifier.height(8.dp))
            Lead(t("recovery_email.mailbox_warning"))
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
 * Save a recovery phrase (UX 19.5, amended 2026-11-26). Saving a phrase
 * changes no key: after the 17.1 proof the screen shows the words this
 * device already holds, then the three-word check. When it passes the
 * account's protection is published again with the phrase in it (ADR-0101
 * section 10), which ends the notice of 19.1d. Nothing counts as saved
 * before that landed; a failure stays on the three-word step with `Try
 * again`, and a person who leaves has saved nothing.
 */
@Composable
public fun SaveRecoveryPhraseScreen(
    repository: AuthRepository,
    onDone: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val proof = rememberProofController(repository, ProofSurface.SavePhrase)
    var working by remember { mutableStateOf(false) }
    var failed by remember { mutableStateOf(false) }
    // Non-null once the words the device holds are on screen.
    var heldWords: String? by remember { mutableStateOf(null) }
    var finishing by remember { mutableStateOf(false) }
    var finishFailed by remember { mutableStateOf(false) }

    val run: () -> Unit = run@{
        if (working) return@run
        working = true
        failed = false
        scope.launch {
            try {
                if (!proof.ensure(context)) return@launch
                val held = withContext(Dispatchers.IO) { repository.devicePhrase() }
                if (held != null) {
                    proof.clear()
                    heldWords = held
                } else {
                    // A device with the account keys holds the words; one
                    // that does not cannot save them.
                    failed = true
                }
            } catch (cause: Throwable) {
                if (cause is CancellationException) throw cause
                Log.w(TAG, "WARN recovery phrase not shown: ${cause.javaClass.simpleName}")
                failed = true
            } finally {
                working = false
            }
        }
    }

    // After the three words: the phrase counts as saved once the account's
    // protection was published with it, not before.
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
                        onDone()
                    }
                    PhraseSavedOutcome.ProofRequired -> proof.onProofRefused()
                    PhraseSavedOutcome.Failed -> finishFailed = true
                }
            } catch (cause: Throwable) {
                if (cause is CancellationException) throw cause
                Log.w(TAG, "WARN recovery phrase not saved: ${cause.javaClass.simpleName}")
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

    BackHandler(enabled = !working) { onBack() }
    AuthScaffold(title = t("protect.phrase.title"), onBack = if (working) null else onBack, modifier = modifier) {
        Lead(t("protect.phrase.save.body"))
        Spacer(Modifier.height(16.dp))
        ProofBlock(
            controller = proof,
            onSubmit = run,
            onPasskey = { scope.launch { proof.provePasskey(context) } },
        )
        if (failed) FormError(t("protect.phrase.save.failed"))
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = if (failed) t("common.try_again") else t("protect.phrase.save.show"),
            busy = working || proof.view().working,
            busyLabel = t("common.checking"),
            enabled = proof.view().mainEnabled,
            onClick = run,
        )
        Spacer(Modifier.height(16.dp))
    }
}

/**
 * The honest notice of UX 19.1d, "Your chats depend on your password": one
 * copy, in the warning colour with the warning icon. `Add a passkey` only
 * where this device can create passkeys ([onAddPasskey] non-null);
 * [passkeyNoSeal] adds the line about a passkey that signs in but does not
 * keep the chats. It cannot be dismissed.
 */
@Composable
public fun PasswordOnlyNotice(
    onSavePhrase: () -> Unit,
    onAddPasskey: (() -> Unit)?,
    modifier: Modifier = Modifier,
    passkeyNoSeal: Boolean = false,
) {
    WarningNote(
        title = t("protect.password_only.title"),
        body = t("protect.password_only.body"),
        extra = if (passkeyNoSeal) t("protect.password_only.passkey_no_seal") else null,
        modifier = modifier,
        actions = {
            Column {
                OutlinedButton(onClick = onSavePhrase, modifier = Modifier.fillMaxWidth()) {
                    Text(t("protect.password_only.save_phrase"))
                }
                if (onAddPasskey != null) {
                    OutlinedButton(onClick = onAddPasskey, modifier = Modifier.fillMaxWidth()) {
                        Text(t("protect.password_only.add_passkey"))
                    }
                }
            }
        },
    )
}

/**
 * The once-per-device dialog of UX 19.1d (its third place): the first time
 * the app opens after a sign-in on a device, for an account kept by the
 * password only. The app's own dialog; `Not now` closes it for good here,
 * and `Save a recovery phrase` opens that screen above the app.
 */
@Composable
public fun PasswordOnlyDialogHost(repository: AuthRepository) {
    val due by repository.passwordOnlyDialog.collectAsStateWithLifecycle()
    var saving by remember { mutableStateOf(false) }
    if (saving) {
        Dialog(
            onDismissRequest = { saving = false },
            properties = DialogProperties(usePlatformDefaultWidth = false),
        ) {
            Surface(modifier = Modifier.fillMaxSize()) {
                SaveRecoveryPhraseScreen(
                    repository = repository,
                    onDone = { saving = false },
                    onBack = { saving = false },
                )
            }
        }
        return
    }
    if (!due) return
    LakkoDialog(
        // Answered by one of its two buttons, never by a tap outside.
        onDismissRequest = {},
        title = t("protect.password_only.title"),
        body = t("protect.password_only.body"),
        confirm = LakkoDialogAction(t("protect.password_only.save_phrase")) {
            repository.closePasswordOnlyDialog()
            saving = true
        },
        cancel = LakkoDialogAction(t("common.not_now"), LakkoDialogTone.Ghost) {
            repository.closePasswordOnlyDialog()
        },
    )
}

/**
 * "Protect your account" (UX 19.1a), required since 2026-11-26: a second way
 * to confirm it is you (a recovery email, an authenticator app or a passkey)
 * before the app. The recovery phrase is an optional extra and does not
 * count. There is no `Not now` and no way past the step but `Continue`,
 * which is enabled once one of the first three rows is done.
 *
 * The same step, with the heading "Add a second way to get back in", is
 * shown at a session landing to an account with a username and none of the
 * three (19.1c): [afterSignup] `false`.
 */
@Composable
public fun ProtectAccountStep(
    repository: AuthRepository,
    hasUsername: Boolean,
    modifier: Modifier = Modifier,
    afterSignup: Boolean = true,
    /** `Continue` went through: the step is over. */
    onFinished: () -> Unit = {},
) {
    val scope = rememberCoroutineScope()
    var sub: ProtectSub by remember { mutableStateOf(ProtectSub.Choose) }
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
    var passkeyCancelled by remember { mutableStateOf(false) }
    var finishing by remember { mutableStateOf(false) }
    val status by repository.recoveryEmail.collectAsStateWithLifecycle()
    val factors by repository.accountFactors.collectAsStateWithLifecycle()
    val seals by repository.sealView.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) { repository.refreshAccountFactors() }
    val done = me.lakko.app.auth.protectStepDone(
        here = doneHere,
        status = status,
        hasPasskey = factors?.passkey == true,
        phraseSaved = seals?.phrase != null || repository.phraseProtected(),
    )
    val passkeyKeepsChats = seals?.passkeys?.isNotEmpty() == true

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
        ProtectSub.Totp -> {
            AuthenticatorSetupScreen(
                repository = repository,
                onDone = {
                    doneHere = doneHere.copy(totp = true)
                    sub = ProtectSub.Choose
                },
                onBack = { sub = ProtectSub.Choose },
                modifier = modifier,
            )
            return
        }
        ProtectSub.Passkey -> {
            // The existing passkey creation flow, under sign-up's proof.
            BackHandler(enabled = true) { sub = ProtectSub.Choose }
            SignupPasskeyOffer(
                repository = repository,
                onDone = { sub = ProtectSub.Choose },
                modifier = modifier,
                onAdded = {
                    passkeyCancelled = false
                    doneHere = doneHere.copy(passkey = true)
                },
                onCancelled = {
                    passkeyCancelled = true
                    sub = ProtectSub.Choose
                },
            )
            return
        }
        ProtectSub.Phrase -> {
            SaveRecoveryPhraseScreen(
                repository = repository,
                // Back on the step with the row done; a saved phrase does
                // not end the step by itself.
                onDone = {
                    doneHere = doneHere.copy(phrase = true)
                    sub = ProtectSub.Choose
                },
                onBack = { sub = ProtectSub.Choose },
                modifier = modifier,
            )
            return
        }
        ProtectSub.Choose -> Unit
    }

    // The step has no way out but `Continue`.
    BackHandler(enabled = true) {}
    val doneMark = t("protect.done")
    val canContinue = me.lakko.app.auth.protectContinueEnabled(done)
    AuthScaffold(
        title = t(me.lakko.app.auth.protectStepTitleKey(afterSignup)),
        onBack = null,
        modifier = modifier,
    ) {
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
        if (rows.totp) {
            MethodRow(
                icon = R.drawable.ic_lk_clock,
                title = t("protect.totp.title") + if (done.totp) " · $doneMark" else "",
                subtitle = t("protect.totp.body"),
                onClick = { sub = ProtectSub.Totp },
                enabled = !done.totp,
            )
        }
        if (rows.passkey) {
            MethodRow(
                icon = R.drawable.ic_lk_passkey,
                title = t("protect.passkey.title") + if (done.passkey) " · $doneMark" else "",
                subtitle = t("protect.passkey.body"),
                onClick = {
                    passkeyCancelled = false
                    sub = ProtectSub.Passkey
                },
                enabled = !done.passkey,
            )
            if (passkeyCancelled && !done.passkey) {
                // No error styling: nothing went wrong.
                Lead(
                    t("protect.passkey.cancelled"),
                    modifier = Modifier
                        .padding(horizontal = 16.dp)
                        .semantics { liveRegion = LiveRegionMode.Polite },
                )
            }
        }
        if (rows.phrase) {
            Spacer(Modifier.height(12.dp))
            Text(
                t("protect.optional"),
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            MethodRow(
                icon = R.drawable.ic_lk_key,
                title = t("protect.phrase.title_optional") + if (done.phrase) " · $doneMark" else "",
                subtitle = t("protect.phrase.body"),
                onClick = { sub = ProtectSub.Phrase },
                enabled = !done.phrase,
            )
        }
        // The authenticator as the only one of the three: allowed, said.
        me.lakko.app.auth.totpOnlyWarningKey(done.factors, done.phrase)?.let { key ->
            Spacer(Modifier.height(12.dp))
            WarningNote(body = t(key))
        }
        // The honest notice of 19.1d, below the authenticator-only note. It
        // never blocks `Continue`.
        if (me.lakko.app.auth.protectStepNoticeShown(done, passkeyKeepsChats)) {
            Spacer(Modifier.height(12.dp))
            PasswordOnlyNotice(
                onSavePhrase = { sub = ProtectSub.Phrase },
                onAddPasskey = if (rows.passkey && !done.passkey) ({ sub = ProtectSub.Passkey }) else null,
                passkeyNoSeal = done.passkey && !passkeyKeepsChats,
            )
        }
        Spacer(Modifier.height(16.dp))
        // Disabled is drawn, never hidden, and read together with its line.
        Column(modifier = Modifier.semantics(mergeDescendants = true) {}) {
            BusyButton(
                label = t("protect.continue"),
                busy = finishing,
                enabled = canContinue,
                onClick = {
                    if (!finishing) {
                        finishing = true
                        scope.launch {
                            try {
                                if (repository.finishProtectStep()) onFinished()
                            } finally {
                                finishing = false
                            }
                        }
                    }
                },
            )
            if (!canContinue) {
                Spacer(Modifier.height(8.dp))
                Lead(t("protect.continue_needs"))
            }
        }
        Spacer(Modifier.height(16.dp))
    }
}

private enum class ProtectSub { Choose, Email, Totp, Passkey, Phrase }

/**
 * Settings > Account (UX 19.1c): the status line that says what keeps the
 * chats (in the warning colour, with the 19.1d actions, when it is the
 * password only), the `Recovery email` row (On / Off; `Replace` and
 * `Remove`; never the server's copy of the address) and the `Recovery
 * phrase` row. The last of the three second ways in cannot be removed.
 */
@Composable
public fun AccountProtectionSection(
    repository: AuthRepository,
    onOpenRecoveryEmail: () -> Unit,
    onOpenSavePhrase: () -> Unit,
    /** `Show phrase` lives on Settings > Security > Recovery. */
    onOpenShowPhrase: () -> Unit,
    /** `Add a passkey` of the 19.1d actions: Settings > Security > Passkeys. */
    onOpenPasskeys: () -> Unit = {},
) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val status by repository.recoveryEmail.collectAsStateWithLifecycle()
    // The address this device holds (the account's devices keep it).
    val heldAddress by repository.recoveryAddress.collectAsStateWithLifecycle()
    val factors by repository.accountFactors.collectAsStateWithLifecycle()
    val seals by repository.sealView.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) {
        repository.refreshRecoveryEmail()
        repository.refreshAccountFactors()
    }
    val known = status ?: return
    val phraseSaved = seals?.phrase != null || repository.phraseProtected()
    // What keeps the chats: from the account's published protection; a
    // device that could not read it says only what it knows for certain.
    val kept: ChatsKeptBy? = seals?.keptBy ?: if (phraseSaved) ChatsKeptBy.Phrase else null
    val phraseOnDevice by produceState(initialValue = false, repository, phraseSaved) {
        value = withContext(Dispatchers.IO) { repository.devicePhrase() != null }
    }
    var confirmingRemove by remember { mutableStateOf(false) }
    var lastFactor by remember { mutableStateOf(false) }
    var removing by remember { mutableStateOf(false) }
    var removeLine: UiText? by remember { mutableStateOf(null) }
    val proof = rememberProofController(repository, ProofSurface.RecoveryEmail, key = "remove")
    val background = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background)

    if (kept != null) {
        val warn = me.lakko.app.auth.passwordOnlyNoticeShown(factors?.any == true, kept)
        ListItem(
            headlineContent = {
                Text(
                    t(protectionStatusKey(kept)),
                    color = if (warn) lakkoWarningColor() else MaterialTheme.colorScheme.onSurface,
                )
            },
            leadingContent = {
                LkIcon(
                    if (warn) R.drawable.ic_lk_warning else R.drawable.ic_lk_lock_shield,
                    contentDescription = null,
                    tint = if (warn) lakkoWarningColor() else MaterialTheme.colorScheme.onSurfaceVariant,
                    size = LkIconSize.Row,
                )
            },
            colors = background,
        )
        if (warn) {
            // The 19.1d actions under the one-line status.
            Row(modifier = Modifier.padding(horizontal = 16.dp)) {
                TextButton(onClick = onOpenSavePhrase) { Text(t("protect.password_only.save_phrase")) }
                if (ProofController.PASSKEYS_SUPPORTED) {
                    TextButton(onClick = onOpenPasskeys) { Text(t("protect.password_only.add_passkey")) }
                }
            }
        }
    }
    val emailRow = recoveryEmailRow(known)
    if (emailRow != RecoveryEmailRow.Absent) {
        val on = emailRow == RecoveryEmailRow.On
        ListItem(
            headlineContent = { Text(t("recovery_email.title")) },
            supportingContent = {
                // A device that holds the address shows it, in plain text
                // under the row title; one that does not shows On.
                val shown = heldAddress
                Text(
                    if (on && shown != null) {
                        "⁦$shown⁩"
                    } else {
                        t(if (on) "recovery_email.status.on" else "recovery_email.status.off")
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
                TextButton(
                    onClick = {
                        // The last of the three cannot be removed: the app
                        // says so before anything is sent. The server
                        // refuses it too.
                        val now = factors
                        if (now != null && me.lakko.app.auth.removalLeavesNoFactor(now, FactorKind.Email, passkeyCount = 0)) {
                            lastFactor = true
                        } else {
                            confirmingRemove = true
                        }
                    },
                    enabled = !removing,
                ) {
                    Text(t("recovery_email.remove"), color = MaterialTheme.colorScheme.error)
                }
            }
            removeLine?.let { FormError(it, modifier = Modifier.padding(horizontal = 16.dp)) }
        }
    }
    when (recoveryPhraseRow(phraseSaved = phraseSaved, phraseOnDevice = phraseOnDevice)) {
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

    if (lastFactor) LastFactorDialog(onClose = { lastFactor = false })

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
                    confirmingRemove = false
                    if (me.lakko.app.auth.isLastFactor(cause)) {
                        lastFactor = true
                    } else {
                        val failure = recoveryEmailEnrolFailureOf(finishing = false, err = cause)
                        removeLine = (failure as? EmailRecoverFailure.Line)?.text ?: UiText("common.error.unavailable")
                    }
                } finally {
                    removing = false
                }
            }
        }
        // The line a removal adds when the authenticator would be the only
        // second way in afterwards (19.1c).
        val totpOnlyKey = factors?.let {
            me.lakko.app.auth.removalTotpOnlyNoticeKey(it, FactorKind.Email, passkeyCount = 0, phraseSaved = phraseSaved)
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
            if (totpOnlyKey != null) {
                WarningNote(body = t(totpOnlyKey))
                Spacer(Modifier.height(12.dp))
            }
            ProofBlock(
                controller = proof,
                onSubmit = remove,
                onPasskey = { scope.launch { proof.provePasskey(context) } },
            )
        }
    }
    HorizontalDivider()
}

/** "Add another way to get back in first. This is the last one this account
 *  has." (UX 19.1c), the app's own dialog with `OK`. */
@Composable
public fun LastFactorDialog(onClose: () -> Unit) {
    LakkoDialog(
        onDismissRequest = onClose,
        title = null,
        body = t("protect.last_factor"),
        confirm = LakkoDialogAction(t("common.ok"), onClick = onClose),
        cancel = null,
    )
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
public fun AccountNoticeHost(repository: AuthRepository) {
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
 * only while no phrase is saved - `Save a recovery phrase` (no key changes).
 */
@Composable
private fun SecureAccountScreen(
    repository: AuthRepository,
    onClose: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val status by repository.recoveryEmail.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) { repository.refreshRecoveryEmail() }
    val seals by repository.sealView.collectAsStateWithLifecycle()
    val rows = secureAccountRows(status, phraseSaved = seals?.phrase != null || repository.phraseProtected())
    var lastFactor by remember { mutableStateOf(false) }
    var passwordSaved by remember { mutableStateOf(false) }
    var savingPhrase by remember { mutableStateOf(false) }
    var removing by remember { mutableStateOf(false) }
    var removeLine: UiText? by remember { mutableStateOf(null) }
    val proof = rememberProofController(repository, ProofSurface.RecoveryEmail, key = "secure")

    if (savingPhrase) {
        SaveRecoveryPhraseScreen(
            repository = repository,
            onDone = { savingPhrase = false },
            onBack = { savingPhrase = false },
        )
        return
    }

    if (lastFactor) LastFactorDialog(onClose = { lastFactor = false })

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
                            if (me.lakko.app.auth.isLastFactor(cause)) {
                                // The account's last second way in (19.1c).
                                lastFactor = true
                            } else {
                                val failure = recoveryEmailEnrolFailureOf(finishing = false, err = cause)
                                removeLine = (failure as? EmailRecoverFailure.Line)?.text ?: UiText("common.error.unavailable")
                            }
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
            OutlinedButton(onClick = { savingPhrase = true }, modifier = Modifier.fillMaxWidth()) {
                Text(t("protect.phrase.title"))
            }
        }
        Spacer(Modifier.height(24.dp))
        TextButton(onClick = onClose, modifier = Modifier.fillMaxWidth()) { Text(t("common.done")) }
        Spacer(Modifier.height(16.dp))
    }
}
