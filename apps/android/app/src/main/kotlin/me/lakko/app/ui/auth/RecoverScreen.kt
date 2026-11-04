// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Notes
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Email
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.filled.Password
import androidx.compose.material.icons.automirrored.filled.ShortText
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import me.lakko.app.auth.ThreeWordsAccount
import me.lakko.app.auth.budgetAt
import me.lakko.app.auth.formatCountdown
import me.lakko.app.auth.pauseRemainingMs
import me.lakko.app.auth.triesLeft
import me.lakko.app.l10n.listPositions
import me.lakko.app.l10n.t

/**
 * "Get back in": the ways back into an account as tappable rows, in two
 * groups, each row pushing its form.
 *
 *   - **Brings back your chats**: another device (first: "if you have
 *     one"), a passkey, the 12-word recovery phrase.
 *   - **Signs you in on this device**: three words from the phrase (only on
 *     a device that holds the phrase and the keys of its cached account),
 *     an authenticator code. These sign in only; afterwards the app offers
 *     "Choose a new password".
 *
 * Nothing in the list depends on server state: before a session the server
 * must not say whether an account has other devices, a passkey or an
 * authenticator. The one conditional row depends on a LOCAL fact
 * ([RecoverViewModel.threeWordsAccount]), and the note under the list names
 * three words only where the row is.
 */
@Composable
public fun RecoverScreen(
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val cachedUsername = viewModel.cachedUsername
    var method by rememberSaveable { mutableStateOf<RecoverMode?>(null) }

    // Whether this device offers three words: read off the main thread (it
    // opens Keystore-backed storage). `null` until read, and where the row
    // is not offered.
    val threeWords by produceState<ThreeWordsAccount?>(initialValue = null, viewModel, state.wordsRevision) {
        value = withContext(Dispatchers.IO) { viewModel.threeWordsAccount() }
    }

    LaunchedEffect(method) {
        method?.let { viewModel.onModeChange(it) }
    }
    BackHandler(enabled = method != null && !state.busy && !state.email.busy) { method = null }
    // The email flow ended without a session: back on the list, with its line.
    LaunchedEffect(state.email.step) {
        if (state.email.step is me.lakko.app.auth.EmailRecoverStep.Closed && method == RecoverMode.Email) method = null
    }

    val back: () -> Unit = { if (!state.busy && !state.email.busy) method = null }
    val chosen = method
    // The words form needs the account it was opened for (a restored
    // selection without it goes back to the list).
    if (chosen == null || (chosen == RecoverMode.Words && state.words == null)) {
        AuthScaffold(title = t("recovery.title"), onBack = onBack, modifier = modifier) {
            Lead(
                if (cachedUsername != null) {
                    t("recovery.lede_account", mapOf("username" to "@$cachedUsername"))
                } else {
                    t("recovery.lede")
                },
            )
            FormError(state.listLine)
            Spacer(Modifier.height(16.dp))
            // UX 19.3 - first, above both groups: what it brings depends on
            // the account, and no row depends on server state.
            MethodRow(
                icon = Icons.Filled.Email,
                title = t("recovery.email.row.title"),
                subtitle = t("recovery.email.row.body"),
                onClick = {
                    viewModel.openEmail()
                    method = RecoverMode.Email
                },
            )
            Spacer(Modifier.height(12.dp))
            GroupLabel(t("recovery.group.keys"))
            MethodRow(
                icon = Icons.Filled.Devices,
                title = t("recovery.method.device.title"),
                subtitle = t("recovery.method.device.sub"),
                onClick = { method = RecoverMode.AnotherDevice },
            )
            MethodRow(
                icon = Icons.Filled.Key,
                title = t("recovery.method.passkey.title"),
                subtitle = t("recovery.method.passkey.sub"),
                onClick = { method = RecoverMode.Passkey },
            )
            MethodRow(
                icon = Icons.AutoMirrored.Filled.Notes,
                title = t("recovery.method.phrase.title"),
                subtitle = t("recovery.method.phrase.sub"),
                onClick = { method = RecoverMode.Phrase },
            )
            Spacer(Modifier.height(12.dp))
            GroupLabel(t("recovery.group.signin"))
            val wordsAccount = threeWords
            if (wordsAccount != null) {
                MethodRow(
                    icon = Icons.AutoMirrored.Filled.ShortText,
                    title = t("recovery.method.words.title"),
                    subtitle = t("recovery.method.words.sub"),
                    onClick = {
                        viewModel.openWords(wordsAccount)
                        method = RecoverMode.Words
                    },
                )
            }
            MethodRow(
                icon = Icons.Filled.Password,
                title = t("recovery.method.totp.title"),
                subtitle = t("recovery.method.totp.sub"),
                onClick = { method = RecoverMode.Authenticator },
            )
            Spacer(Modifier.height(12.dp))
            Lead(t(accessOnlyNoteKey(threeWords = wordsAccount != null)))
            Spacer(Modifier.height(16.dp))
        }
        return
    }

    when (chosen) {
        RecoverMode.Phrase -> PhraseForm(state, cachedUsername, viewModel, back, modifier)
        RecoverMode.AnotherDevice -> AnotherDeviceForm(state, viewModel, back, modifier)
        RecoverMode.Passkey -> PasskeyForm(state, viewModel, back, modifier)
        RecoverMode.Words -> WordsForm(state, viewModel, back, modifier)
        RecoverMode.Authenticator -> AuthenticatorForm(state, cachedUsername, viewModel, back, modifier)
        RecoverMode.Email -> EmailCodeForm(state, cachedUsername, viewModel, back, modifier)
    }
}

/** The catalog key of the note under the list: it names three words only
 *  where the row is offered, so it never promises a route that is absent. */
public fun accessOnlyNoteKey(threeWords: Boolean): String =
    if (threeWords) "recovery.access_only_note_words" else "recovery.access_only_note"

/** The plain-text heading over a run of rows. */
@Composable
private fun GroupLabel(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(top = 4.dp, bottom = 4.dp),
    )
}

/**
 * Milliseconds left until [untilMs] (unix ms), ticking once a second; 0 when
 * there is no wait or it has run out.
 */
@Composable
internal fun rememberRemainingMs(untilMs: Long?): Long {
    var remaining by remember(untilMs) {
        mutableLongStateOf(if (untilMs == null) 0L else (untilMs - System.currentTimeMillis()).coerceAtLeast(0L))
    }
    LaunchedEffect(untilMs) {
        if (untilMs == null) return@LaunchedEffect
        while (true) {
            val left = untilMs - System.currentTimeMillis()
            remaining = left.coerceAtLeast(0L)
            if (left <= 0L) break
            delay(1000L)
        }
    }
    return remaining
}

/** "This method is not available right now" with the way back to the list. */
@Composable
private fun MethodUnavailable(onBack: () -> Unit) {
    FormError(t("recovery.error.unavailable"))
    Spacer(Modifier.height(16.dp))
    OutlinedButton(onClick = onBack, modifier = Modifier.fillMaxWidth()) {
        Text(t("recovery.methods.back_to_list"))
    }
}

/**
 * Passkey: one button. The system sheet lists the passkeys saved for Lakko,
 * so there is no username field and the sign-in works on a device that has
 * never held the account. What follows is decided by the repository: signed
 * in, keys restored, or the restore prompt saying the passkey brought none.
 */
@Composable
private fun PasskeyForm(
    state: RecoverUiState,
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    val context = LocalContext.current
    AuthScaffold(title = t("recovery.method.passkey.title"), onBack = onBack, modifier = modifier) {
        if (state.passkeyUnavailable) {
            MethodUnavailable(onBack)
        } else {
            Lead(t("recovery.passkey.intro"))
            FormError(state.error)
            Spacer(Modifier.height(24.dp))
            BusyButton(
                label = t("recovery.passkey.submit"),
                busy = state.busy,
                busyLabel = t("recovery.passkey.waiting"),
                onClick = { viewModel.onSubmitPasskey(context) },
            )
        }
        Spacer(Modifier.height(16.dp))
    }
}

/**
 * Three words from the phrase, checked on this device like a wallet does.
 * No username field: the account is the one cached here. The prompt lists
 * the three word numbers in the order they were drawn; a miss draws new
 * ones. Five misses pause the form for 15 minutes, with the time left
 * shown; that budget is this device's own gate, not a server limit.
 */
@Composable
private fun WordsForm(
    state: RecoverUiState,
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    val words = state.words ?: return
    AuthScaffold(title = t("recovery.method.words.title"), onBack = onBack, modifier = modifier) {
        if (words.unavailable) {
            MethodUnavailable(onBack)
            Spacer(Modifier.height(16.dp))
            return@AuthScaffold
        }
        if (words.suspended) {
            // The sign-in with the phrase stored here was refused; three
            // words are suspended on this device and the row is gone from
            // the list. The phrase itself is still stored.
            FormError(t("recovery.words.error.rejected"))
            Spacer(Modifier.height(16.dp))
            OutlinedButton(onClick = onBack, modifier = Modifier.fillMaxWidth()) {
                Text(t("recovery.methods.back_to_list"))
            }
            Spacer(Modifier.height(16.dp))
            return@AuthScaffold
        }
        Lead(t("recovery.cached_account", mapOf("username" to "@${words.username}")))
        Spacer(Modifier.height(12.dp))

        // The local pause: clamped, so a clock set back never shows more
        // than a full pause.
        val pauseUntil = remember(words.budget) {
            val now = System.currentTimeMillis()
            pauseRemainingMs(words.budget, now).takeIf { it > 0L }?.let { now + it }
        }
        val pauseLeft = rememberRemainingMs(pauseUntil)
        if (pauseLeft > 0L) {
            FormError(t("recovery.words.paused", mapOf("time" to formatCountdown(pauseLeft))))
            Spacer(Modifier.height(16.dp))
            return@AuthScaffold
        }

        Text(
            t("recovery.words.prompt", mapOf("positions" to listPositions(words.positions))),
            style = MaterialTheme.typography.bodyLarge,
        )
        Spacer(Modifier.height(12.dp))
        words.positions.forEachIndexed { i, position ->
            val last = i == words.positions.lastIndex
            OutlinedTextField(
                value = words.typed.getOrElse(i) { "" },
                onValueChange = { viewModel.onWordChange(i, it) },
                label = { Text(t("recovery_phrase.word_label", mapOf("position" to position.toString()))) },
                // Not a word of the list: a typo, marked as on web and iOS.
                isError = words.typed.getOrElse(i) { "" }.isNotBlank() &&
                    !me.lakko.app.auth.isPhraseWord(words.typed.getOrElse(i) { "" }),
                singleLine = true,
                enabled = !state.busy,
                textStyle = MaterialTheme.typography.bodyLarge.copy(fontFamily = FontFamily.Monospace),
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Ascii,
                    imeAction = if (last) ImeAction.Done else ImeAction.Next,
                ),
                keyboardActions = androidx.compose.foundation.text.KeyboardActions(
                    onDone = { viewModel.onSubmitWords() },
                ),
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(12.dp))
        }
        Lead(t("recovery.words.signin_only"))
        FormError(state.error)
        // The budget as it stands now: a pause that has run out counts as
        // no misses.
        val liveBudget = budgetAt(words.budget, System.currentTimeMillis())
        if (liveBudget.misses > 0) {
            Spacer(Modifier.height(8.dp))
            Lead(
                t(
                    "recovery.words.tries_left",
                    emptyMap(),
                    mapOf("count" to triesLeft(liveBudget, System.currentTimeMillis()).toULong()),
                ),
            )
        }
        // A server limit on the sign-in that follows a match: its own wait.
        val serverLeft = rememberRemainingMs(words.serverWaitUntilMs)
        if (serverLeft > 0L) {
            FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(serverLeft))))
        }
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = t("recovery.words.submit"),
            busy = state.busy,
            busyLabel = t("common.checking"),
            enabled = words.complete && serverLeft <= 0L,
            onClick = viewModel::onSubmitWords,
        )
        Spacer(Modifier.height(16.dp))
    }
}

/** The username field of the sign-in and recovery screens (same label). */
@Composable
internal fun UsernameField(
    value: String,
    onValueChange: (String) -> Unit,
    enabled: Boolean,
    imeAction: ImeAction = ImeAction.Next,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(t("auth.username.label")) },
        singleLine = true,
        enabled = enabled,
        keyboardOptions = KeyboardOptions(
            capitalization = KeyboardCapitalization.None,
            autoCorrectEnabled = false,
            keyboardType = KeyboardType.Ascii,
            imeAction = imeAction,
        ),
        modifier = Modifier.fillMaxWidth(),
    )
    Spacer(Modifier.height(12.dp))
}

@Composable
private fun NewPasswordFields(state: RecoverUiState, viewModel: RecoverViewModel) {
    PasswordField(
        value = state.password,
        onValueChange = viewModel::onPasswordChange,
        label = t("auth.password.new.label"),
        enabled = !state.busy,
        supportingText = t(
            "auth.password.hint",
            emptyMap(),
            mapOf("count" to RecoverViewModel.MIN_PASSWORD_LEN.toULong()),
        ),
    )
    Spacer(Modifier.height(8.dp))
    PasswordField(
        value = state.confirm,
        onValueChange = viewModel::onConfirmChange,
        label = t("auth.password.new_confirm.label"),
        enabled = !state.busy,
        imeAction = ImeAction.Done,
        onDone = viewModel::onSubmit,
    )
}

@Composable
private fun PhraseForm(
    state: RecoverUiState,
    cachedUsername: String?,
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    AuthScaffold(title = t("recovery.method.phrase.title"), onBack = onBack, modifier = modifier) {
        Lead(t("recovery.phrase.intro"))
        Spacer(Modifier.height(16.dp))
        if (cachedUsername == null) {
            UsernameField(state.username, viewModel::onUsernameChange, enabled = !state.busy)
        }
        RecoveryPhraseEntryGrid(
            phrase = state.phrase,
            onPhraseChange = viewModel::onPhraseChange,
            enabled = !state.busy,
        )
        Spacer(Modifier.height(16.dp))
        NewPasswordFields(state, viewModel)
        val recall = state.phraseRecall
        if (recall != null) {
            // UX 19.7 - the words were this account's phrase before its
            // keys were changed.
            Spacer(Modifier.height(16.dp))
            Text(
                t("recall.phrase.changed", mapOf("date" to recallDate(recall.status.changedAtMs))),
                style = MaterialTheme.typography.bodyLarge,
            )
            Spacer(Modifier.height(8.dp))
            for (key in PHRASE_RECALL_EFFECTS) Lead(t(key))
            FormError(state.error)
            Spacer(Modifier.height(16.dp))
            BusyButton(
                label = t("recall.phrase.take_back"),
                busy = state.busy,
                busyLabel = t("devices.recall.form.working"),
                onClick = viewModel::onPhraseRecallTakeBack,
            )
            OutlinedButton(
                onClick = viewModel::onPhraseRecallDeclined,
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            ) { Text(t("notice.was_me")) }
            Spacer(Modifier.height(16.dp))
            return@AuthScaffold
        }
        FormError(state.error)
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = t("recovery.submit"),
            busy = state.busy,
            busyLabel = t("common.checking"),
            onClick = viewModel::onSubmit,
        )
        Spacer(Modifier.height(16.dp))
    }
}

/** What taking the account back costs (the lines of 18.4's form). */
private val PHRASE_RECALL_EFFECTS: List<String> = listOf(
    "devices.recall.form.effect_keys",
    "devices.recall.form.effect_factors",
    "devices.recall.form.effect_messages",
)

@Composable
private fun recallDate(atMs: Long): String {
    val language = me.lakko.app.l10n.LocalStrings.current.language
    return remember(atMs, language) {
        me.lakko.app.l10n.DeviceFormats.mediumDate(atMs, language)
    }
}

/**
 * Email code (UX 19.3). Step 1: username and address. Step 2, always and
 * identical whatever was typed: "Check your email". Step 3, only when the
 * account has an authenticator: its code. Step 4: the new password; only
 * then is the server asked what the code brings, so nothing before it tells
 * which kind of account this is.
 */
@Composable
internal fun EmailCodeForm(
    state: RecoverUiState,
    cachedUsername: String?,
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    val email = state.email
    val waitLeft = rememberRemainingMs(email.waitUntilMs)
    val title = when (email.step) {
        is me.lakko.app.auth.EmailRecoverStep.Code -> t("recovery.email.check.title")
        is me.lakko.app.auth.EmailRecoverStep.Totp -> t("recovery.email.totp.title")
        is me.lakko.app.auth.EmailRecoverStep.NewPassword -> t("recovery.new_password.title")
        else -> t("recovery.email.row.title")
    }
    AuthScaffold(title = title, onBack = if (email.busy) null else onBack, modifier = modifier) {
        when (val step = email.step) {
            me.lakko.app.auth.EmailRecoverStep.Address, is me.lakko.app.auth.EmailRecoverStep.Closed -> {
                Lead(t("recovery.email.start.body"))
                Spacer(Modifier.height(16.dp))
                if (cachedUsername == null) {
                    UsernameField(state.username, viewModel::onUsernameChange, enabled = !email.busy)
                }
                EmailField(
                    value = state.emailAddress,
                    onValueChange = viewModel::onEmailAddressChange,
                    enabled = !email.busy,
                    onDone = viewModel::onSubmitEmailAddress,
                )
                FormError(email.error)
                if (waitLeft > 0L) {
                    FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft))))
                }
                Spacer(Modifier.height(24.dp))
                BusyButton(
                    label = t("recovery_email.send"),
                    busy = email.busy,
                    busyLabel = t("common.sending"),
                    enabled = waitLeft <= 0L && me.lakko.app.auth.recoveryEmailPlausible(state.emailAddress),
                    onClick = viewModel::onSubmitEmailAddress,
                )
            }
            is me.lakko.app.auth.EmailRecoverStep.Code -> {
                Lead(t("recovery.email.check.body"))
                Spacer(Modifier.height(16.dp))
                CodeField(
                    value = state.emailCode,
                    onValueChange = viewModel::onEmailCodeChange,
                    enabled = !email.busy,
                    label = t("recovery_email.code.title"),
                    onDone = viewModel::onSubmitEmailCode,
                )
                FormError(email.error)
                if (waitLeft > 0L) {
                    FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft))))
                }
                Spacer(Modifier.height(16.dp))
                BusyButton(
                    label = t("common.continue"),
                    busy = email.busy,
                    busyLabel = t("common.checking"),
                    enabled = me.lakko.app.auth.normalizeEmailCode(state.emailCode) != null && waitLeft <= 0L,
                    onClick = viewModel::onSubmitEmailCode,
                )
                ResendRow(
                    resendAtMs = step.resendAtMs,
                    enabled = !email.busy && waitLeft <= 0L,
                    onResend = viewModel::onSubmitEmailAddress,
                    secondLabel = t("common.back"),
                    onSecond = viewModel::onEmailBackToAddress,
                )
            }
            is me.lakko.app.auth.EmailRecoverStep.Totp -> {
                Lead(t("recovery.email.totp.body"))
                Spacer(Modifier.height(16.dp))
                CodeField(
                    value = state.emailCode,
                    onValueChange = viewModel::onEmailCodeChange,
                    enabled = !email.busy,
                    label = t("recovery.totp.code_label"),
                    onDone = viewModel::onSubmitEmailCode,
                )
                FormError(email.error)
                Spacer(Modifier.height(16.dp))
                BusyButton(
                    label = t("common.continue"),
                    busy = email.busy,
                    busyLabel = t("common.checking"),
                    enabled = me.lakko.app.auth.normalizeEmailCode(state.emailCode) != null,
                    onClick = viewModel::onSubmitEmailCode,
                )
                Spacer(Modifier.height(12.dp))
                Lead(t("recovery.email.totp.lost"))
            }
            is me.lakko.app.auth.EmailRecoverStep.NewPassword -> {
                PasswordField(
                    value = state.password,
                    onValueChange = viewModel::onPasswordChange,
                    label = t("auth.password.new.label"),
                    enabled = !email.busy,
                    supportingText = t(
                        "auth.password.hint",
                        emptyMap(),
                        mapOf("count" to RecoverViewModel.MIN_PASSWORD_LEN.toULong()),
                    ),
                )
                Spacer(Modifier.height(8.dp))
                PasswordField(
                    value = state.confirm,
                    onValueChange = viewModel::onConfirmChange,
                    label = t("auth.password.new_confirm.label"),
                    enabled = !email.busy,
                    imeAction = ImeAction.Done,
                    onDone = viewModel::onSubmitEmailPassword,
                )
                FormError(email.error)
                Spacer(Modifier.height(24.dp))
                BusyButton(
                    label = t("recovery.email.save"),
                    busy = email.busy,
                    busyLabel = t("common.saving"),
                    onClick = viewModel::onSubmitEmailPassword,
                )
            }
            me.lakko.app.auth.EmailRecoverStep.Restoring -> {
                androidx.compose.foundation.layout.Row(
                    verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
                    horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(12.dp),
                ) {
                    androidx.compose.material3.CircularProgressIndicator()
                    Text(t("recovery.email.restoring"))
                }
            }
        }
        Spacer(Modifier.height(16.dp))
    }
}

@Composable
private fun AnotherDeviceForm(
    state: RecoverUiState,
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    AuthScaffold(title = t("recovery.method.device.title"), onBack = onBack, modifier = modifier) {
        val target = state.deviceRestoreTarget
        if (target == null) {
            Lead(t("recovery.device.intro_qr"))
            Spacer(Modifier.height(16.dp))
            UsernameField(state.username, viewModel::onUsernameChange, enabled = !state.busy, imeAction = ImeAction.Done)
            FormError(state.error)
            Spacer(Modifier.height(12.dp))
            BusyButton(
                label = t("common.continue"),
                busy = state.busy,
                busyLabel = t("common.preparing"),
                onClick = viewModel::startDeviceRestore,
            )
        } else {
            Lead(t("recovery.device.restoring", mapOf("username" to "@${target.username}")))
            Spacer(Modifier.height(12.dp))
            me.lakko.app.ui.devices.RestoreViaDevicePanel(
                loginHandle = target.loginHandle,
                known = null,
                onRecovered = viewModel::completeFromDeviceRestore,
            )
        }
        Spacer(Modifier.height(16.dp))
    }
}

@Composable
private fun AuthenticatorForm(
    state: RecoverUiState,
    cachedUsername: String?,
    viewModel: RecoverViewModel,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    AuthScaffold(title = t("recovery.method.totp.title"), onBack = onBack, modifier = modifier) {
        Lead(t("recovery.totp.intro"))
        Spacer(Modifier.height(16.dp))
        if (cachedUsername == null) {
            UsernameField(state.username, viewModel::onUsernameChange, enabled = !state.busy)
        }
        OutlinedTextField(
            value = state.totpCode,
            onValueChange = viewModel::onTotpCodeChange,
            label = { Text(t("recovery.totp.code_label")) },
            singleLine = true,
            enabled = !state.busy,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
            keyboardActions = androidx.compose.foundation.text.KeyboardActions(onDone = { viewModel.onSubmitTotp() }),
            modifier = Modifier.fillMaxWidth(),
        )
        FormError(state.error)
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = t("common.continue"),
            busy = state.busy,
            busyLabel = t("common.checking"),
            onClick = viewModel::onSubmitTotp,
        )
        Spacer(Modifier.height(16.dp))
    }
}
