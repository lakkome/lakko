// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
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
 * The password change made from a device that holds the account's keys
 * ([AuthRepository.replacePassword]). It does not run on a bare session (UX
 * 17.8): the form opens only behind a live fresh-auth proof (the current
 * password or a passkey, via the proof block) or the local three-word check
 * of the recovery phrase, for the person who forgot the password
 * ([passwordChangePanel]). Then the two new-password fields and Save.
 * [onSkip] adds the "Not now" button of the prompt shown after a
 * forgot-password sign-in; Settings has none.
 */
@Composable
public fun NewPasswordForm(
    repository: AuthRepository,
    onSaved: () -> Unit,
    modifier: Modifier = Modifier,
    onSkip: (() -> Unit)? = null,
) {
    val scope = rememberCoroutineScope()
    val context = androidx.compose.ui.platform.LocalContext.current
    val proof = rememberProofController(repository, ProofSurface.ChangePassword)
    var choseWords by remember { mutableStateOf(false) }
    // Bumped when the three-word check passes, so the gate is read again.
    var wordsPassed by remember { mutableStateOf(0) }
    val holdsPhrase by androidx.compose.runtime.produceState(initialValue = false, repository) {
        value = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            repository.holdsPhraseForPasswordChange()
        }
    }
    val unlocked = remember(proof.proofLive, wordsPassed) { repository.passwordChangeUnlocked() }
    when (passwordChangePanel(unlocked = unlocked, choseWords = choseWords, holdsPhrase = holdsPhrase)) {
        PasswordChangePanel.Form -> Unit
        PasswordChangePanel.Proof -> {
            val view = proof.view()
            val go: () -> Unit = { scope.launch { proof.ensure(context) } }
            ProofBlock(
                controller = proof,
                modifier = modifier,
                onSubmit = go,
                onPasskey = { scope.launch { proof.provePasskey(context) } },
            )
            Spacer(Modifier.height(24.dp))
            BusyButton(
                label = t("common.continue"),
                busy = view.working,
                busyLabel = t("common.checking"),
                enabled = view.mainEnabled,
                onClick = go,
            )
            if (holdsPhrase) {
                // The person who forgot the password: three words of the
                // phrase, checked on this device.
                TextButton(onClick = { choseWords = true }, enabled = !view.working, modifier = Modifier.fillMaxWidth()) {
                    Text(t(PASSWORD_CHANGE_USE_WORDS))
                }
            }
            if (onSkip != null) {
                TextButton(onClick = onSkip, enabled = !view.working, modifier = Modifier.fillMaxWidth()) {
                    Text(t("recovery.new_password.not_now"))
                }
            }
            return
        }
        PasswordChangePanel.Words -> {
            ThreeWordsGate(
                repository = repository,
                onPassed = { wordsPassed += 1 },
                onBack = { choseWords = false },
                modifier = modifier,
            )
            return
        }
    }
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
            } catch (_: me.lakko.app.auth.PasswordChangeNeedsProof) {
                // The ten minutes ran out while the form was open: the gate
                // comes back, nothing went wrong.
                proof.refresh()
                wordsPassed += 1
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
 * The local three-word check as the gate of a password change: the same
 * prompt, fields, miss budget and 15-minute pause as the three-word sign-in
 * (`docs/ux/identity-recovery.md` section 6), but nothing is sent anywhere
 * and nobody is signed in: a match only opens the new-password form.
 */
@Composable
private fun ThreeWordsGate(
    repository: AuthRepository,
    onPassed: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scope = rememberCoroutineScope()
    val accountIdB64 = (repository.state.collectAsState().value as? me.lakko.app.auth.AuthState.Session)
        ?.account?.accountIdB64 ?: return
    var positions by remember { mutableStateOf(drawWordPositions()) }
    var typed by remember { mutableStateOf(List(me.lakko.app.auth.THREE_WORDS) { "" }) }
    var budget by remember {
        mutableStateOf(
            me.lakko.app.auth.budgetAt(repository.threeWordsBudget(accountIdB64), System.currentTimeMillis()),
        )
    }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<UiText?>(null) }

    val pauseUntil = remember(budget) {
        val now = System.currentTimeMillis()
        me.lakko.app.auth.pauseRemainingMs(budget, now).takeIf { it > 0L }?.let { now + it }
    }
    val pauseLeft = rememberRemainingMs(pauseUntil)

    fun submit() {
        if (busy || pauseLeft > 0L || !me.lakko.app.auth.threeWordsSubmittable(typed)) return
        busy = true
        error = null
        scope.launch {
            try {
                val matched = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                    repository.checkThreeWordsForPasswordChange(
                        positions = ByteArray(positions.size) { positions[it].toByte() },
                        words = typed,
                    )
                }
                when (matched) {
                    true -> onPassed()
                    false -> {
                        budget = repository.recordThreeWordsMiss(accountIdB64, System.currentTimeMillis())
                        error = UiText("recovery.words.error.no_match")
                        // New positions for every prompt.
                        positions = drawWordPositions()
                        typed = List(me.lakko.app.auth.THREE_WORDS) { "" }
                    }
                    null -> error = UiText("recovery.words.error.unavailable")
                }
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                error = UiText("recovery.words.error.unavailable")
            } finally {
                busy = false
            }
        }
    }

    androidx.compose.foundation.layout.Column(modifier = modifier.fillMaxWidth()) {
        if (pauseLeft > 0L) {
            FormError(t("recovery.words.paused", mapOf("time" to me.lakko.app.auth.formatCountdown(pauseLeft))))
        } else {
            Text(
                t("recovery.words.prompt", mapOf("positions" to me.lakko.app.l10n.listPositions(positions))),
                style = androidx.compose.material3.MaterialTheme.typography.bodyLarge,
            )
            Spacer(Modifier.height(12.dp))
            positions.forEachIndexed { i, position ->
                androidx.compose.material3.OutlinedTextField(
                    value = typed[i],
                    onValueChange = { value -> typed = typed.toMutableList().also { it[i] = value } },
                    label = { Text(t("recovery_phrase.word_label", mapOf("position" to position.toString()))) },
                    isError = typed[i].isNotBlank() && !me.lakko.app.auth.isPhraseWord(typed[i]),
                    singleLine = true,
                    enabled = !busy,
                    keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                        capitalization = androidx.compose.ui.text.input.KeyboardCapitalization.None,
                        autoCorrectEnabled = false,
                        keyboardType = androidx.compose.ui.text.input.KeyboardType.Ascii,
                        imeAction = if (i == positions.lastIndex) ImeAction.Done else ImeAction.Next,
                    ),
                    keyboardActions = androidx.compose.foundation.text.KeyboardActions(onDone = { submit() }),
                    modifier = Modifier.fillMaxWidth(),
                )
                Spacer(Modifier.height(12.dp))
            }
            FormError(error)
            val live = me.lakko.app.auth.budgetAt(budget, System.currentTimeMillis())
            if (live.misses > 0) {
                Spacer(Modifier.height(8.dp))
                Lead(
                    t(
                        "recovery.words.tries_left",
                        emptyMap(),
                        mapOf("count" to me.lakko.app.auth.triesLeft(live, System.currentTimeMillis()).toULong()),
                    ),
                )
            }
            Spacer(Modifier.height(24.dp))
            BusyButton(
                label = t("common.continue"),
                busy = busy,
                busyLabel = t("common.checking"),
                enabled = me.lakko.app.auth.threeWordsSubmittable(typed),
                onClick = ::submit,
            )
        }
        TextButton(onClick = onBack, enabled = !busy, modifier = Modifier.fillMaxWidth()) {
            Text(t("common.back"))
        }
    }
}

/** Three distinct word numbers in drawn order (the core's rule, as the
 *  three-word sign-in draws them). */
private fun drawWordPositions(): List<Int> =
    uniffi.lakko_ffi_uniffi.phraseCheckRandomPositions().map { it.toInt() and 0xFF }

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
