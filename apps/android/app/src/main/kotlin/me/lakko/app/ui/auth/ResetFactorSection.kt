// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import android.content.Context
import android.util.Log
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
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
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.R
import me.lakko.app.auth.AccountFactors
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.FactorKind
import me.lakko.app.auth.ResetFactorCall
import me.lakko.app.auth.ResetFactorConfirmed
import me.lakko.app.auth.ResetFactorFailure
import me.lakko.app.auth.ResetPanel
import me.lakko.app.auth.SealPrediction
import me.lakko.app.auth.afterWrongResetEmailCode
import me.lakko.app.auth.formatCountdown
import me.lakko.app.auth.normalizeEmailCode
import me.lakko.app.auth.recoveryEmailPlausible
import me.lakko.app.auth.resetCannotBodyKey
import me.lakko.app.auth.resetFactorDoneKey
import me.lakko.app.auth.resetFactorFailureOf
import me.lakko.app.auth.resetFactorLive
import me.lakko.app.auth.resetFactorRows
import me.lakko.app.auth.resetFactorTitleKey
import me.lakko.app.auth.resetPanel
import me.lakko.app.auth.resetPasswordReady
import me.lakko.app.auth.sealPredictionKey
import me.lakko.app.auth.unsealedOffersPasskey
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/*
 * ADR-0073 (amended 2026-11-26) / UX `identity-recovery.md` 15.1b and 15.1c:
 * the reset warning's "Confirm it is you" section. The password, then one
 * second factor the account has (an authenticator code, a code by email, a
 * passkey); once confirmed, the line that says whether this reset brings the
 * chats back. Every decision is a pure function of
 * `me.lakko.app.auth.ResetFactor.kt`; this file holds what was typed (memory
 * only, never saved state) and draws.
 */

private const val TAG = "lakko-reset"

/** The state of one reset warning. Memory only. */
@Stable
public class ResetFactorController(private val repo: AuthRepository) {
    /** The account's three second ways in; `null` until the server answered. */
    public var factors: AccountFactors? by mutableStateOf(null)
        private set

    /** The factors could not be read: one line and `Try again`. */
    public var loadFailed: Boolean by mutableStateOf(false)
        private set

    public var saidCannot: Boolean by mutableStateOf(false)

    public var passwordLive: Boolean by mutableStateOf(repo.resetPasswordProofLive())
        private set

    public val usernameKnown: Boolean = repo.proofUsernameKnown()

    public var password: String by mutableStateOf("")
    public var username: String by mutableStateOf("")
    public var passwordLine: UiText? by mutableStateOf(null)
        private set

    /** The row expanded in place; the others stay visible. */
    public var open: FactorKind? by mutableStateOf(null)
        private set

    /** The row whose request is running. */
    public var working: FactorKind? by mutableStateOf(null)
        private set

    public var confirmed: ResetFactorConfirmed? by mutableStateOf(repo.resetFactorConfirmed())
        private set

    public var totpCode: String by mutableStateOf("")
    public var address: String by mutableStateOf(repo.recoveryAddress.value.orEmpty())
    public var emailCode: String by mutableStateOf("")
    public var emailChallenge: String? by mutableStateOf(null)
        private set
    public var resendAtMs: Long? by mutableStateOf(null)
        private set
    private var wrongEmailTries: Int = 0

    /** One line per row, and whether it is drawn as an error. */
    public var lines: Map<FactorKind, Pair<UiText, Boolean>> by mutableStateOf(emptyMap())
        private set

    /** A limit per row: the other rows stay usable. */
    public var waits: Map<FactorKind, Long> by mutableStateOf(emptyMap())
        private set

    public val passkeysUsable: Boolean = ProofController.PASSKEYS_SUPPORTED

    public val passwordReady: Boolean
        get() = resetPasswordReady(passwordLive, password, usernameKnown, username)

    public fun panel(): ResetPanel = resetPanel(factors, saidCannot)

    public suspend fun load() {
        loadFailed = false
        val read = repo.resetAccountFactors()
        factors = read
        loadFailed = read == null
    }

    /** Re-read what time changes: a password proof or a confirmation that
     *  ran out opens the section again as it was, with no error styling. */
    public fun refresh() {
        passwordLive = repo.resetPasswordProofLive()
        if (confirmed != null && !resetFactorLive(confirmed, System.currentTimeMillis())) {
            repo.dropResetFactor()
            confirmed = null
        }
    }

    public fun choose(kind: FactorKind) {
        if (working != null) return
        open = if (open == kind) null else kind
    }

    public fun onPasswordChange(value: String) {
        password = value
        passwordLine = null
    }

    private fun typedPassword(): ByteArray? =
        if (passwordLive) null else password.toByteArray(Charsets.UTF_8)

    private fun line(kind: FactorKind, text: UiText?, error: Boolean = true) {
        lines = if (text == null) lines - kind else lines + (kind to (text to error))
    }

    private fun apply(kind: FactorKind, failure: ResetFactorFailure) {
        when (failure) {
            ResetFactorFailure.WrongPassword -> {
                passwordLine = UiText("restore_identity.reset.error.wrong_password")
                passwordLive = false
            }
            ResetFactorFailure.PasswordNeeded -> {
                // The proof ran out: the block appears, nothing went wrong.
                passwordLive = false
                passwordLine = null
            }
            is ResetFactorFailure.WrongCode -> {
                if (kind == FactorKind.Totp) totpCode = "" else emailCode = ""
                line(kind, failure.line)
            }
            is ResetFactorFailure.EmailRestart -> {
                emailChallenge = null
                emailCode = ""
                line(kind, failure.line)
            }
            is ResetFactorFailure.Wait -> {
                val secs = failure.retryAfterSecs
                if (secs != null && secs > 0L) {
                    waits = waits + (kind to System.currentTimeMillis() + secs * 1000L)
                    line(kind, null)
                } else {
                    line(kind, UiText("common.error.too_many_attempts"))
                }
                if (kind == FactorKind.Totp) totpCode = ""
            }
            is ResetFactorFailure.Line ->
                // A cancelled passkey sheet carries no error styling.
                line(kind, failure.line, error = failure.line.key != "proof.reset.cancelled")
        }
    }

    private suspend fun run(kind: FactorKind, call: ResetFactorCall, block: suspend (ByteArray?) -> Unit) {
        if (working != null) return
        working = kind
        line(kind, null)
        passwordLine = null
        val typed = typedPassword()
        try {
            block(typed)
            // The password is not kept a moment longer than its check.
            password = ""
            passwordLive = repo.resetPasswordProofLive()
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            Log.w(TAG, "WARN the reset was not confirmed: ${cause.javaClass.simpleName}")
            passwordLive = repo.resetPasswordProofLive()
            val failure = if (call == ResetFactorCall.EmailFinish &&
                resetFactorFailureOf(call, cause) is ResetFactorFailure.WrongCode
            ) {
                afterWrongResetEmailCode(wrongEmailTries).also { wrongEmailTries += 1 }
            } else {
                resetFactorFailureOf(call, cause)
            }
            apply(kind, failure)
        } finally {
            typed?.fill(0)
            working = null
        }
    }

    private fun typedUsername(): String? = username.takeIf { it.isNotBlank() }

    /** The authenticator's six digits: checked on the sixth, no button. */
    public suspend fun confirmTotp() {
        val code = me.lakko.app.auth.normalizeTotpCode(totpCode) ?: return
        if (!passwordReady) return
        run(FactorKind.Totp, ResetFactorCall.Totp) { typed ->
            confirmed = repo.resetConfirmTotp(code, typed, typedUsername())
            totpCode = ""
            open = null
        }
    }

    public suspend fun sendEmail() {
        if (!passwordReady || !recoveryEmailPlausible(address)) return
        run(FactorKind.Email, ResetFactorCall.EmailStart) { typed ->
            val start = repo.resetEmailStart(address.trim(), typed, typedUsername())
            emailChallenge = start.challenge_id
            emailCode = ""
            wrongEmailTries = 0
            resendAtMs = System.currentTimeMillis() + start.resend_after_secs * 1000L
        }
    }

    public suspend fun confirmEmail() {
        val challenge = emailChallenge ?: return
        val code = normalizeEmailCode(emailCode) ?: return
        run(FactorKind.Email, ResetFactorCall.EmailFinish) {
            confirmed = repo.resetEmailFinish(challenge, code)
            emailChallenge = null
            emailCode = ""
            open = null
        }
    }

    public fun otherAddress() {
        emailChallenge = null
        emailCode = ""
        address = ""
        line(FactorKind.Email, null)
    }

    public fun resendEmail() {
        emailChallenge = null
    }

    /** `Use passkey`: the platform's own sheet, then the factor. Also the
     *  second line of an unsealed prediction (15.1c). */
    public suspend fun usePasskey(activityContext: Context) {
        if (!passwordReady) return
        run(FactorKind.Passkey, ResetFactorCall.Passkey) { typed ->
            confirmed = repo.resetConfirmPasskey(activityContext, typed, typedUsername())
            open = null
        }
    }

    /** The reset found the confirmation gone, or the server refused it: the
     *  section opens again, nothing else changes, no error. */
    public fun reopen() {
        repo.dropResetFactor()
        confirmed = null
        passwordLive = repo.resetPasswordProofLive()
    }

    /** The warning closed: nothing typed or confirmed is kept. */
    public fun clear() {
        repo.dropResetFactor()
        confirmed = null
        password = ""
        username = ""
        passwordLine = null
        totpCode = ""
        emailCode = ""
        emailChallenge = null
        open = null
        saidCannot = false
        lines = emptyMap()
    }

    public fun waitUntil(kind: FactorKind): Long? = waits[kind]

    /** The name the email row's "a code is on its way" line shows. */
    public fun shownUsername(): String = repo.proofUsername() ?: username.trim()
}

/** A [ResetFactorController], loaded once and kept fresh while composed. */
@Composable
public fun rememberResetFactorController(repo: AuthRepository): ResetFactorController {
    val controller = remember(repo) { ResetFactorController(repo) }
    LaunchedEffect(controller) { controller.load() }
    LaunchedEffect(controller) {
        while (true) {
            controller.refresh()
            delay(RESET_REFRESH_MS)
        }
    }
    return controller
}

private const val RESET_REFRESH_MS: Long = 5_000L

/**
 * The "cannot reset" panel (UX 15.1b): in place of the factor section, the
 * typed word and the button. No disabled reset button, no control to add a
 * factor, no sentence suggesting support.
 */
@Composable
public fun ResetCannotPanel(panel: ResetPanel, onBack: () -> Unit, onCancel: () -> Unit) {
    Text(
        t("proof.reset.no_factor.title"),
        style = MaterialTheme.typography.titleMedium,
        fontWeight = FontWeight.SemiBold,
    )
    Spacer(Modifier.height(8.dp))
    Text(t(resetCannotBodyKey(panel)), style = MaterialTheme.typography.bodyMedium)
    Spacer(Modifier.height(12.dp))
    Text(t("proof.reset.cannot.what_you_can_do"), style = MaterialTheme.typography.bodyMedium)
    Spacer(Modifier.height(4.dp))
    for (key in RESET_CANNOT_OPTION_KEYS) {
        Text("• " + t(key), style = MaterialTheme.typography.bodyMedium)
    }
    Spacer(Modifier.height(12.dp))
    Text(t("proof.reset.cannot.otherwise"), style = MaterialTheme.typography.bodyMedium)
    Spacer(Modifier.height(24.dp))
    // `Back` only when the panel came from `I cannot use any of these`.
    if (panel == ResetPanel.CannotUse) {
        TextButton(onClick = onBack, modifier = Modifier.fillMaxWidth()) { Text(t("common.back")) }
    }
    OutlinedButton(onClick = onCancel, modifier = Modifier.fillMaxWidth()) { Text(t("common.cancel")) }
}

/**
 * The factor section (UX 15.1b), or its collapsed "Confirmed with ..." line
 * followed by the sealed / unsealed line of 15.1c.
 */
@Composable
public fun ResetFactorSection(controller: ResetFactorController, modifier: Modifier = Modifier) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val confirmed = controller.confirmed
    Column(modifier = modifier.fillMaxWidth()) {
        if (confirmed != null) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            ) {
                LkIcon(
                    R.drawable.ic_lk_checkmark_circle,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.primary,
                    size = LkIconSize.Row,
                )
                Text(t(resetFactorDoneKey(confirmed.factor)), style = MaterialTheme.typography.bodyMedium)
            }
            Spacer(Modifier.height(12.dp))
            when (confirmed.prediction) {
                is SealPrediction.Sealed ->
                    Text(t(sealPredictionKey(confirmed.prediction)), style = MaterialTheme.typography.bodyMedium)
                is SealPrediction.Unsealed -> {
                    val offer = unsealedOffersPasskey(confirmed.prediction, controller.passkeysUsable) &&
                        confirmed.factor != FactorKind.Passkey
                    WarningNote(
                        body = t(sealPredictionKey(confirmed.prediction)),
                        extra = if (offer) t("restore_identity.reset.unsealed_use_passkey") else null,
                        actions = if (offer) {
                            {
                                OutlinedButton(
                                    onClick = { scope.launch { controller.usePasskey(context) } },
                                    enabled = controller.working == null,
                                ) { Text(t("proof.reset.factor.passkey.use")) }
                            }
                        } else {
                            null
                        },
                    )
                    controller.lines[FactorKind.Passkey]?.let { (text, error) -> RowLine(text, error) }
                }
            }
            return@Column
        }

        Text(
            t("proof.reset.factor.heading"),
            style = MaterialTheme.typography.titleMedium,
            fontWeight = FontWeight.SemiBold,
        )
        Spacer(Modifier.height(8.dp))
        val busy = controller.working != null
        if (!controller.passwordLive) {
            if (!controller.usernameKnown) {
                UsernameField(
                    value = controller.username,
                    onValueChange = { controller.username = it },
                    enabled = !busy,
                )
            }
            // The 17.1 field without `Use a passkey instead`: a passkey is
            // one of the second factors here, not a stand-in for the password.
            PasswordField(
                value = controller.password,
                onValueChange = controller::onPasswordChange,
                label = t("auth.password.label"),
                enabled = !busy,
                imeAction = ImeAction.Done,
                supportingText = t("restore_identity.reset.password_help"),
            )
            Column(modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite }) {
                FormError(controller.passwordLine)
            }
            Spacer(Modifier.height(12.dp))
        }
        Text(t(resetFactorTitleKey(controller.passwordLive)), style = MaterialTheme.typography.bodyMedium)
        Spacer(Modifier.height(4.dp))
        val factors = controller.factors ?: return@Column
        for (kind in resetFactorRows(factors, controller.passkeysUsable)) {
            FactorRow(kind = kind, expanded = controller.open == kind, onClick = { controller.choose(kind) })
            if (controller.open == kind) {
                Column(modifier = Modifier.padding(start = 16.dp, end = 16.dp, bottom = 12.dp)) {
                    when (kind) {
                        FactorKind.Totp -> TotpRow(controller)
                        FactorKind.Email -> EmailRow(controller)
                        FactorKind.Passkey -> PasskeyRow(controller)
                    }
                }
            }
        }
        Spacer(Modifier.height(4.dp))
        TextButton(onClick = { controller.saidCannot = true }, enabled = !busy) {
            Text(t("proof.reset.factor.none"))
        }
    }
}

@Composable
private fun FactorRow(kind: FactorKind, expanded: Boolean, onClick: () -> Unit) {
    val (icon, title) = when (kind) {
        FactorKind.Totp -> R.drawable.ic_lk_clock to "proof.reset.factor.totp"
        FactorKind.Email -> R.drawable.ic_lk_envelope to "proof.reset.factor.email"
        FactorKind.Passkey -> R.drawable.ic_lk_passkey to "proof.reset.factor.passkey"
    }
    ListItem(
        headlineContent = { Text(t(title)) },
        leadingContent = {
            LkIcon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.primary, size = LkIconSize.Row)
        },
        trailingContent = {
            LkIcon(
                if (expanded) R.drawable.ic_lk_chevron_down else R.drawable.ic_lk_chevron_forward,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                size = LkIconSize.Mark,
            )
        },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surface),
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick),
    )
}

@Composable
private fun RowLine(text: UiText, error: Boolean) {
    if (error) {
        Column(modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite }) { FormError(text) }
    } else {
        Text(
            text.text(),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(top = 8.dp).semantics { liveRegion = LiveRegionMode.Polite },
        )
    }
}

@Composable
private fun WaitLine(untilMs: Long?): Long {
    val left = rememberRemainingMs(untilMs)
    if (left > 0L) FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(left))))
    return left
}

@Composable
private fun TotpRow(controller: ResetFactorController) {
    val scope = rememberCoroutineScope()
    val working = controller.working == FactorKind.Totp
    Lead(t("proof.reset.factor.totp.prompt"))
    Spacer(Modifier.height(8.dp))
    val left = rememberRemainingMs(controller.waitUntil(FactorKind.Totp))
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Column(modifier = Modifier.weight(1f)) {
            CodeField(
                value = controller.totpCode,
                onValueChange = { typed ->
                    controller.totpCode = typed
                    // Checked on the sixth digit, with no separate button.
                    if (me.lakko.app.auth.normalizeTotpCode(typed) != null) scope.launch { controller.confirmTotp() }
                },
                enabled = controller.working == null && left <= 0L && controller.passwordReady,
                label = t("recovery.totp.code_label"),
                onDone = { scope.launch { controller.confirmTotp() } },
            )
        }
        if (working) CircularProgressIndicator(modifier = Modifier.height(20.dp))
    }
    if (left > 0L) FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(left))))
    controller.lines[FactorKind.Totp]?.let { (text, error) -> RowLine(text, error) }
}

@Composable
private fun EmailRow(controller: ResetFactorController) {
    val scope = rememberCoroutineScope()
    val working = controller.working == FactorKind.Email
    val idle = controller.working == null
    if (controller.emailChallenge == null) {
        EmailField(
            value = controller.address,
            onValueChange = { controller.address = it },
            enabled = idle,
            onDone = { scope.launch { controller.sendEmail() } },
        )
        if (controller.address.isNotBlank() && !recoveryEmailPlausible(controller.address)) {
            FormError(t("recovery_email.invalid"))
        }
        controller.lines[FactorKind.Email]?.let { (text, error) -> RowLine(text, error) }
        val left = WaitLine(controller.waitUntil(FactorKind.Email))
        Spacer(Modifier.height(8.dp))
        BusyButton(
            label = t("recovery_email.send"),
            busy = working,
            busyLabel = t("common.sending"),
            enabled = idle && left <= 0L && controller.passwordReady && recoveryEmailPlausible(controller.address),
            onClick = { scope.launch { controller.sendEmail() } },
        )
    } else {
        // The same answer whether or not the address is the account's.
        Lead(t("proof.reset.factor.email.sent", mapOf("username" to "@${controller.shownUsername()}")))
        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Column(modifier = Modifier.weight(1f)) {
                CodeField(
                    value = controller.emailCode,
                    onValueChange = { typed ->
                        controller.emailCode = typed
                        if (normalizeEmailCode(typed) != null) scope.launch { controller.confirmEmail() }
                    },
                    enabled = idle,
                    label = t("recovery_email.code.title"),
                    onDone = { scope.launch { controller.confirmEmail() } },
                )
            }
            if (working) CircularProgressIndicator(modifier = Modifier.height(20.dp))
        }
        controller.lines[FactorKind.Email]?.let { (text, error) -> RowLine(text, error) }
        val left = WaitLine(controller.waitUntil(FactorKind.Email))
        ResendRow(
            resendAtMs = controller.resendAtMs,
            enabled = idle && left <= 0L,
            onResend = {
                controller.resendEmail()
                scope.launch { controller.sendEmail() }
            },
            secondLabel = t("recovery_email.code.other_address"),
            onSecond = controller::otherAddress,
        )
    }
}

@Composable
private fun PasskeyRow(controller: ResetFactorController) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val working = controller.working == FactorKind.Passkey
    BusyButton(
        label = t("proof.reset.factor.passkey.use"),
        busy = working,
        busyLabel = t("recovery.passkey.waiting"),
        enabled = controller.working == null && controller.passwordReady,
        onClick = { scope.launch { controller.usePasskey(context) } },
    )
    controller.lines[FactorKind.Passkey]?.let { (text, error) -> RowLine(text, error) }
}
