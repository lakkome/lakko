// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.compose.runtime.CompositionLocalProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import java.text.DateFormat
import java.util.Date
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.RecallAvailability
import me.lakko.app.auth.RecallLine
import me.lakko.app.auth.RecallStage
import me.lakko.app.auth.RecallUi
import me.lakko.app.auth.formatCountdown
import me.lakko.app.auth.recallFormProblem
import me.lakko.app.auth.recallNoEnabled
import me.lakko.app.auth.recallSubmitEnabled
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.auth.AuthScaffold
import me.lakko.app.ui.auth.BusyButton
import me.lakko.app.ui.auth.FormError
import me.lakko.app.ui.auth.Lead
import me.lakko.app.ui.auth.PasswordField
import me.lakko.app.ui.auth.rememberRemainingMs

/**
 * "Was it you?" and "Take the account back"
 * (`docs/ux/identity-recovery.md` section 18.4; ADR-0066 part 1).
 *
 * Shown by the app root instead of the whole app while this device holds a
 * recall record: the account's keys were reset on another device and what
 * the account kept here is already deleted. Driven by [AuthRepository.recall]
 * (which is rebuilt from the record at launch), so it survives a restart; no
 * dismiss, no chat list behind it, and system back only steps inside it.
 *
 * Every surface is the app's own: the "Yes" confirmation and the form are
 * drawn inside this screen, never a dialog.
 */
@Composable
internal fun RecallRoot(repository: AuthRepository, ui: RecallUi, modifier: Modifier = Modifier) {
    val scope = rememberCoroutineScope()

    // Ask the server whether the account can be taken back, and keep asking:
    // quickly while offline, slowly once it said yes (the window can close
    // while the screen is open). The loop ends with the recall.
    LaunchedEffect(ui.accountIdB64) {
        while (true) {
            val next = withContext(Dispatchers.IO) { repository.recallCheck() } ?: break
            delay(next)
        }
    }

    // Back steps inside the screen; on the question it does nothing (the
    // screen has no way out but its two answers).
    BackHandler(enabled = true) {
        if (ui.stage != RecallStage.Question) repository.recallShow(RecallStage.Question)
    }

    when (ui.stage) {
        RecallStage.Question, RecallStage.ConfirmYes -> RecallQuestion(
            ui = ui,
            onYes = { repository.recallShow(RecallStage.ConfirmYes) },
            onNo = { repository.recallShow(RecallStage.Form) },
            onConfirmYes = { repository.recallConfirmOwn() },
            onBack = { repository.recallShow(RecallStage.Question) },
            modifier = modifier,
        )
        RecallStage.Form -> RecallForm(
            ui = ui,
            onBack = { repository.recallShow(RecallStage.Question) },
            onSubmit = { old, new ->
                scope.launch {
                    val oldBytes = old.toByteArray(Charsets.UTF_8)
                    val newBytes = new.toByteArray(Charsets.UTF_8)
                    try {
                        withContext(Dispatchers.IO) { repository.recallSubmit(oldBytes, newBytes) }
                    } finally {
                        oldBytes.fill(0)
                        newBytes.fill(0)
                    }
                }
            },
            modifier = modifier,
        )
    }
}

/** The platform's medium date in the interface language. */
@Composable
private fun mediumDate(atMs: Long): String {
    val language = LocalStrings.current.language
    return remember(atMs, language) {
        DateFormat.getDateInstance(DateFormat.MEDIUM, Locale.forLanguageTag(language)).format(Date(atMs))
    }
}

@Composable
private fun accountName(username: String): String =
    if (username.isBlank()) t("devices.signed_out.this_account") else "@$username"

@Composable
private fun RecallQuestion(
    ui: RecallUi,
    onYes: () -> Unit,
    onNo: () -> Unit,
    onConfirmYes: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier,
) {
    val body = t(
        "devices.recall.body",
        mapOf("name" to accountName(ui.username), "date" to mediumDate(ui.resetAtMs)),
    )
    val question = t("devices.recall.question")
    val title = t("devices.signed_out.title")
    val noFocus = remember { FocusRequester() }
    AuthScaffold(title = title, onBack = null, modifier = modifier) {
        // Title, body and question are one announcement.
        Column(
            modifier = Modifier.semantics(mergeDescendants = true) {
                contentDescription = "$title. $body $question"
            },
        ) {
            Lead(body)
            Spacer(Modifier.height(16.dp))
            Text(question, style = MaterialTheme.typography.titleMedium)
        }
        Spacer(Modifier.height(16.dp))
        if (ui.stage == RecallStage.ConfirmYes) {
            // The confirmation of "Yes, it was me": inside the screen.
            Text(
                t("devices.recall.yes.confirm"),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            )
            Spacer(Modifier.height(16.dp))
            Button(onClick = onConfirmYes, modifier = Modifier.fillMaxWidth()) {
                Text(t("devices.recall.yes.delete"))
            }
            Spacer(Modifier.height(8.dp))
            OutlinedButton(onClick = onBack, modifier = Modifier.fillMaxWidth()) {
                Text(t("devices.recall.yes.back"))
            }
            return@AuthScaffold
        }
        // Both full width, stacked, neither destructive; "No" is the primary.
        OutlinedButton(onClick = onYes, enabled = !ui.working, modifier = Modifier.fillMaxWidth()) {
            Text(t("devices.recall.yes"))
        }
        Spacer(Modifier.height(8.dp))
        Button(
            onClick = onNo,
            enabled = recallNoEnabled(ui),
            modifier = Modifier
                .fillMaxWidth()
                .focusRequester(noFocus),
        ) {
            Text(t("devices.recall.no"))
        }
        Spacer(Modifier.height(16.dp))
        when (val availability = ui.availability) {
            RecallAvailability.Checking -> Text(
                t("devices.recall.checking"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            RecallAvailability.Offline -> Text(
                t("common.error.offline"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            )
            is RecallAvailability.Open -> Text(
                t("devices.recall.until", mapOf("date" to mediumDate(availability.status.expiresAtMs))),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Spacer(Modifier.height(16.dp))
    }
    // Focus lands on "No, take the account back" once it can be pressed.
    val canTakeBack = recallNoEnabled(ui)
    LaunchedEffect(canTakeBack) {
        if (canTakeBack) {
            try {
                noFocus.requestFocus()
            } catch (_: IllegalStateException) {
                // Not laid out yet; focus stays where the platform put it.
            }
        }
    }
}

@Composable
private fun RecallForm(
    ui: RecallUi,
    onBack: () -> Unit,
    onSubmit: (oldPassword: String, newPassword: String) -> Unit,
    modifier: Modifier,
) {
    // Memory only: never saved across a process death, never pre-filled.
    var oldPassword by remember { mutableStateOf("") }
    var newPassword by remember { mutableStateOf("") }
    var repeat by remember { mutableStateOf("") }
    val oldFocus = remember { FocusRequester() }

    val wait = ui.line as? RecallLine.RateLimited
    val waitLeft = rememberRemainingMs(wait?.untilMs)
    val problem = if (newPassword.isEmpty() || repeat.isEmpty()) null else recallFormProblem(newPassword, repeat)
    val enabled = recallSubmitEnabled(ui, oldPassword, newPassword, repeat, System.currentTimeMillis()) &&
        waitLeft <= 0L
    val submit = { if (enabled) onSubmit(oldPassword, newPassword) }
    val fieldsEnabled = !ui.working

    // A wrong password: the first field keeps focus.
    val wrong = ui.line as? RecallLine.WrongPassword
    LaunchedEffect(wrong) {
        if (wrong != null) {
            try {
                oldFocus.requestFocus()
            } catch (_: IllegalStateException) {
                // Not laid out yet.
            }
        }
    }

    AuthScaffold(
        title = t("devices.recall.form.title"),
        onBack = if (ui.working) null else onBack,
        modifier = modifier,
    ) {
        Lead(t("devices.recall.form.lede"))
        Spacer(Modifier.height(16.dp))
        // Password fields stay left-to-right in a right-to-left interface.
        CompositionLocalProvider(LocalLayoutDirection provides LayoutDirection.Ltr) {
            PasswordField(
                value = oldPassword,
                onValueChange = { oldPassword = it },
                label = t("devices.recall.form.old_password"),
                enabled = fieldsEnabled,
                modifier = Modifier.focusRequester(oldFocus),
            )
        }
        if (wrong != null) {
            Column(modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite }) {
                FormError(t("devices.recall.error.wrong_password"))
                wrong.triesLeft?.let { left ->
                    FormError(
                        me.lakko.app.l10n.UiText(
                            "devices.recall.error.tries_left",
                            counts = mapOf("count" to left.toULong()),
                        ),
                    )
                }
            }
        }
        Spacer(Modifier.height(12.dp))
        CompositionLocalProvider(LocalLayoutDirection provides LayoutDirection.Ltr) {
            PasswordField(
                value = newPassword,
                onValueChange = { newPassword = it },
                label = t("devices.recall.form.new_password"),
                enabled = fieldsEnabled,
                supportingText = t(
                    "auth.password.hint",
                    emptyMap(),
                    mapOf("count" to me.lakko.app.auth.RECALL_MIN_PASSWORD_LEN.toULong()),
                ),
            )
            Spacer(Modifier.height(12.dp))
            PasswordField(
                value = repeat,
                onValueChange = { repeat = it },
                label = t("devices.recall.form.repeat_password"),
                enabled = fieldsEnabled,
                imeAction = ImeAction.Done,
                onDone = submit,
            )
        }
        FormError(problem)
        Spacer(Modifier.height(16.dp))
        for (key in EFFECT_LINES) {
            Row(modifier = Modifier.padding(vertical = 2.dp)) {
                Text("-", style = MaterialTheme.typography.bodyMedium)
                Spacer(Modifier.padding(horizontal = 4.dp))
                Text(t(key), style = MaterialTheme.typography.bodyMedium)
            }
        }
        when {
            waitLeft > 0L -> FormError(t("recovery.error.rate_limited", mapOf("time" to formatCountdown(waitLeft))))
            ui.line is RecallLine.Offline -> FormError(t("common.error.offline"))
            // No catalog key names this failure; the reset's closest
            // sentence stands in.
            ui.line is RecallLine.Failed -> FormError(t("restore_identity.reset.error.generic"))
        }
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = t("devices.recall.form.submit"),
            busy = ui.working,
            busyLabel = t("devices.recall.form.working"),
            enabled = enabled,
            onClick = submit,
        )
        Spacer(Modifier.height(8.dp))
        OutlinedButton(onClick = onBack, enabled = !ui.working, modifier = Modifier.fillMaxWidth()) {
            Text(t("common.back"))
        }
        Spacer(Modifier.height(16.dp))
    }
}

private val EFFECT_LINES: List<String> = listOf(
    "devices.recall.form.effect_keys",
    "devices.recall.form.effect_factors",
    "devices.recall.form.effect_messages",
)

/**
 * `You took your account back. Add your passkeys and your other devices
 * again.` - one line at the top of the chat list for the session that took
 * the account back (UX 18.4 "Done"). Informational, announced once, never
 * takes focus; `Dismiss` removes it.
 */
@Composable
public fun RecallDoneLine(repository: AuthRepository, modifier: Modifier = Modifier) {
    val done by repository.recallDone.collectAsStateWithLifecycle()
    if (!done) return
    Surface(
        modifier = modifier
            .fillMaxWidth()
            .padding(8.dp),
        color = MaterialTheme.colorScheme.secondaryContainer,
        contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
        shape = RoundedCornerShape(8.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.padding(start = 12.dp, top = 4.dp, bottom = 4.dp, end = 4.dp),
        ) {
            Text(
                t("devices.recall.done"),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier
                    .weight(1f)
                    .semantics { liveRegion = LiveRegionMode.Polite },
            )
            TextButton(
                onClick = { repository.dismissRecallDone() },
                modifier = Modifier.defaultMinSize(minWidth = 48.dp, minHeight = 48.dp),
            ) { Text(t("common.dismiss")) }
        }
    }
}
