// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.RenameResult
import me.lakko.app.auth.renameWarnings
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.profile.UsernameCheck
import me.lakko.app.profile.checkUsername
import me.lakko.app.profile.usernameCanonicalHintText
import me.lakko.app.profile.usernameFieldErrorText

@Composable
private fun renameDate(atMs: Long): String {
    val language = LocalStrings.current.language
    return remember(atMs, language) {
        java.text.DateFormat.getDateInstance(java.text.DateFormat.MEDIUM, java.util.Locale.forLanguageTag(language))
            .format(java.util.Date(atMs))
    }
}

/**
 * Change username (ADR-0068). The warning lines, the proof block of 17.1
 * when no fresh grant is live, the username field with the sign-up's live
 * validation, and `Change`. The rename itself is one resumable operation in
 * the repository ([AuthRepository.changeUsername]); this screen shows its
 * outcome: done (with the line about the other devices), a refusal (taken,
 * too soon with its date), or a step still owed.
 */
@Composable
public fun ChangeUsernameScreen(
    repository: AuthRepository,
    onDone: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
    validateUsername: (String) -> String = { uniffi.lakko_ffi_uniffi.validateUsername(it) },
) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val authState by repository.state.collectAsStateWithLifecycle()
    val current = (authState as? AuthState.Session)?.account?.username.orEmpty()
    val status by repository.recoveryEmail.collectAsStateWithLifecycle()
    val heldAddress by repository.recoveryAddress.collectAsStateWithLifecycle()
    val proof = rememberProofController(repository, ProofSurface.ChangeUsername)
    LaunchedEffect(Unit) { if (status == null) repository.refreshRecoveryEmail() }

    var username by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error: UiText? by remember { mutableStateOf(null) }
    // Too soon: by this device's own stamp when the screen opens, or by the
    // server's answer (the authority).
    var allowedAtMs: Long? by remember { mutableStateOf(repository.renameAllowedAtMs()) }
    var doneAs: String? by remember { mutableStateOf(null) }
    val check = checkUsername(username, validateUsername)

    BackHandler(enabled = !busy) { if (doneAs != null) onDone() else onBack() }

    val change: () -> Unit = change@{
        val typed = (check as? UsernameCheck.Valid)?.normalized ?: return@change
        if (busy || typed == current) return@change
        busy = true
        error = null
        scope.launch {
            try {
                if (!proof.ensure(context)) return@launch
                when (val result = repository.changeUsername(typed)) {
                    is RenameResult.Done -> {
                        proof.clear()
                        doneAs = result.username
                    }
                    is RenameResult.Refused -> when {
                        result.needsProof -> proof.onProofRefused()
                        result.allowedAtMs != null -> allowedAtMs = result.allowedAtMs
                        else -> error = result.line
                    }
                    is RenameResult.Pending -> error = result.line
                }
            } finally {
                busy = false
            }
        }
    }

    AuthScaffold(
        title = t("username.change.title"),
        onBack = if (busy) null else ({ if (doneAs != null) onDone() else onBack() }),
        modifier = modifier,
    ) {
        val done = doneAs
        if (done != null) {
            Lead(t("username.claim.done", mapOf("username" to "⁦@$done⁩")))
            Spacer(Modifier.height(8.dp))
            Lead(t("username.change.other_devices"))
            Spacer(Modifier.height(24.dp))
            BusyButton(label = t("common.done"), busy = false, onClick = onDone)
            Spacer(Modifier.height(16.dp))
            return@AuthScaffold
        }
        val wait = allowedAtMs
        if (wait != null) {
            Lead(t("username.change.too_soon", mapOf("date" to renameDate(wait))))
            Spacer(Modifier.height(16.dp))
            return@AuthScaffold
        }
        val warnings = renameWarnings(hasRecoveryEmail = status?.enabled == true, addressHeld = heldAddress != null)
        Lead(t("username.change.body", mapOf("old" to "⁦@$current⁩")))
        Spacer(Modifier.height(8.dp))
        Lead(t("username.change.next_change"))
        if (warnings.recoveryEmailTurnsOff) {
            Spacer(Modifier.height(8.dp))
            Lead(t("recovery_email.username_change"))
        }
        Spacer(Modifier.height(16.dp))
        ProofBlock(
            controller = proof,
            onSubmit = change,
            onPasskey = { scope.launch { proof.provePasskey(context) } },
        )
        if (proof.view().visible) Spacer(Modifier.height(12.dp))
        val fieldError = usernameFieldErrorText(check, username)
        OutlinedTextField(
            value = username,
            onValueChange = {
                username = it
                error = null
            },
            label = { Text(t("auth.username.label")) },
            singleLine = true,
            enabled = !busy,
            isError = fieldError != null,
            supportingText = {
                Text((fieldError ?: usernameCanonicalHintText(check, username))?.let { it.renderWithCurrent() } ?: t("auth.signup.username.hint"))
            },
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrectEnabled = false,
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Done,
            ),
            keyboardActions = KeyboardActions(onDone = { change() }),
            modifier = Modifier.fillMaxWidth(),
        )
        FormError(error)
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = t("username.change.save"),
            busy = busy || proof.view().working,
            busyLabel = t("common.saving"),
            enabled = check is UsernameCheck.Valid && check.normalized != current && proof.view().mainEnabled,
            onClick = change,
        )
        Spacer(Modifier.height(16.dp))
    }
}

@Composable
private fun UiText.renderWithCurrent(): String = renderWith(LocalStrings.current)
