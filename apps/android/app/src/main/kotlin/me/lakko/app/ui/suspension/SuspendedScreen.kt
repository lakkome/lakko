// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.suspension

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import androidx.compose.foundation.BorderStroke
import androidx.compose.material3.ButtonDefaults
import me.lakko.app.auth.SuspendedSecondaryAction
import me.lakko.app.auth.appealCounterText
import me.lakko.app.auth.suspendedSecondaryAction
import me.lakko.app.auth.AppealAnswer
import me.lakko.app.auth.AppealPhase
import me.lakko.app.auth.AppealSheetState
import me.lakko.app.auth.AppealState
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.SuspensionDetailsPhase
import me.lakko.app.auth.SuspensionUi
import me.lakko.app.auth.appealPhaseAfter
import me.lakko.app.auth.suspendedReasonKey
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.ui.auth.AuthScaffold
import me.lakko.app.ui.auth.BusyButton
import me.lakko.app.ui.auth.Lead

/**
 * ADR-0098 section 4 (UX `docs/ux/reporting-and-moderation.md` section 4) -
 * the one screen that tells a suspended person why: the reason, the end
 * date or that there is none, what still works, and [Appeal] [Read my
 * chats] [Sign out]. Drawn by the app root in place of the app until "Read
 * my chats"; a device that holds no chats of the account shows it alone.
 */
@Composable
public fun SuspendedScreen(
    repository: AuthRepository,
    ui: SuspensionUi,
    onSignOut: () -> Unit,
    /** The usual Delete account confirmation, for a device without the
     *  account's chats ([SuspendedSecondaryAction.DELETE_ACCOUNT]). */
    deleteDialog: @Composable (onClose: () -> Unit) -> Unit,
    modifier: Modifier = Modifier,
) {
    val scope = rememberCoroutineScope()
    var appealOpen by remember { mutableStateOf(false) }
    var signingOut by remember { mutableStateOf(false) }
    var deleting by remember { mutableStateOf(false) }

    AuthScaffold(title = t("suspended.self.title"), onBack = null, modifier = modifier) {
        when (val details = ui.details) {
            SuspensionDetailsPhase.Loading -> Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            ) {
                CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp)
                Spacer(Modifier.width(12.dp))
                Text(t("suspended.self.loading"), style = MaterialTheme.typography.bodyMedium)
            }
            SuspensionDetailsPhase.Failed -> Column {
                Text(
                    t("suspended.self.load_failed"),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.error,
                )
                TextButton(onClick = {
                    scope.launch { withContext(Dispatchers.IO) { repository.reloadSuspensionDetails() } }
                }) { Text(t("common.retry")) }
            }
            is SuspensionDetailsPhase.Loaded -> {
                val reason = t(suspendedReasonKey(details.details.reasonName))
                Text(t("suspended.self.reason", mapOf("reason" to reason)), style = MaterialTheme.typography.bodyLarge)
                Spacer(Modifier.height(4.dp))
                val until = details.details.untilMs
                Text(
                    if (until != null) t("suspended.self.until", mapOf("date" to mediumDateTime(until))) else t("suspended.self.no_end"),
                    style = MaterialTheme.typography.bodyLarge,
                )
            }
        }
        Spacer(Modifier.height(12.dp))
        Lead(t("suspended.self.body"))
        Spacer(Modifier.height(24.dp))

        val loaded = (ui.details as? SuspensionDetailsPhase.Loaded)?.details
        when (loaded?.appeal) {
            AppealState.NONE -> BusyButton(
                label = t("suspended.appeal"),
                busy = false,
                onClick = { appealOpen = true },
            )
            AppealState.SENT -> Text(
                t("suspended.appeal.sent"),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            )
            AppealState.DECLINED -> Text(t("suspended.appeal.declined"), style = MaterialTheme.typography.bodyMedium)
            null -> Unit
        }
        Spacer(Modifier.height(12.dp))
        when (suspendedSecondaryAction(ui)) {
            SuspendedSecondaryAction.READ_CHATS -> OutlinedButton(
                onClick = { repository.suspensionReadChats() },
                modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp),
            ) { Text(t("suspended.self.read")) }
            // A device that holds no chats of the account: nothing to read
            // and Settings cannot be reached, so Delete account is here.
            SuspendedSecondaryAction.DELETE_ACCOUNT -> OutlinedButton(
                onClick = { deleting = true },
                colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                border = BorderStroke(1.dp, MaterialTheme.colorScheme.error),
                modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp),
            ) { Text(t("suspended.self.delete")) }
        }
        Spacer(Modifier.height(4.dp))
        TextButton(
            onClick = {
                if (signingOut) return@TextButton
                signingOut = true
                onSignOut()
            },
            enabled = !signingOut,
            modifier = Modifier.fillMaxWidth(),
        ) { Text(t("suspended.self.sign_out")) }
    }

    if (deleting) deleteDialog { deleting = false }

    if (appealOpen) {
        AppealSheet(
            send = { text -> withContext(Dispatchers.IO) { repository.sendAppeal(text) } },
            onClose = { appealOpen = false },
        )
    }
}

/** The appeal counter in the interface language's digits. */
@Composable
private fun appealCounter(length: Int): String {
    val language = LocalStrings.current.language
    return remember(length, language) {
        appealCounterText(length, java.util.Locale.forLanguageTag(language), me.lakko.app.l10n.DeviceFormats.region())
    }
}

/** The end of the suspension, date and time, in the interface language
 *  with the device's region and clock (a short suspension must be readable). */
@Composable
private fun mediumDateTime(atMs: Long): String {
    val language = LocalStrings.current.language
    return remember(atMs, language) { me.lakko.app.l10n.DeviceFormats.mediumDateTime(atMs, language) }
}

/**
 * The appeal sheet: one field (up to [APPEAL_MAX_CHARS] characters, with a
 * counter), the one-appeal hint, Send appeal. Closes on a send; the screen
 * then reads "Appeal sent".
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun AppealSheet(
    send: suspend (String) -> AppealAnswer,
    onClose: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    var state by remember { mutableStateOf(AppealSheetState()) }

    ModalBottomSheet(
        onDismissRequest = { if (state.phase != AppealPhase.SENDING) onClose() },
        sheetState = sheetState,
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .padding(bottom = 24.dp),
        ) {
            Text(t("suspended.appeal.title"), style = MaterialTheme.typography.titleLarge)
            Spacer(Modifier.height(16.dp))
            OutlinedTextField(
                value = state.text,
                onValueChange = { state = state.withText(it) },
                placeholder = { Text(t("suspended.appeal.field")) },
                enabled = state.phase != AppealPhase.SENDING,
                minLines = 4,
                supportingText = { Text(appealCounter(state.length)) },
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(4.dp))
            Text(
                t("suspended.appeal.once"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            val errorKey = when (state.phase) {
                AppealPhase.FAILED -> "suspended.appeal.failed"
                AppealPhase.UNAVAILABLE -> "suspended.appeal.unavailable"
                else -> null
            }
            if (errorKey != null) {
                Spacer(Modifier.height(8.dp))
                Text(t(errorKey), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.error)
            }
            Spacer(Modifier.height(16.dp))
            BusyButton(
                label = t("suspended.appeal.send"),
                busy = state.phase == AppealPhase.SENDING,
                enabled = state.canSend,
                onClick = {
                    if (!state.canSend) return@BusyButton
                    val text = state.text
                    state = state.copy(phase = AppealPhase.SENDING)
                    scope.launch {
                        val next = appealPhaseAfter(send(text))
                        if (next == null) onClose() else state = state.copy(phase = next)
                    }
                },
            )
        }
    }
}

/**
 * The row at the top of the chat list while the app is read-only: the
 * suspended screen is one tap away.
 */
@Composable
public fun SuspendedListRow(onOpen: () -> Unit, modifier: Modifier = Modifier) {
    Surface(
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier = modifier
            .fillMaxWidth()
            .clickable(role = Role.Button, onClick = onOpen),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
        ) {
            me.lakko.app.ui.components.LkIcon(me.lakko.app.R.drawable.ic_lk_nosign, contentDescription = null, size = me.lakko.app.ui.components.LkIconSize.Row, tint = MaterialTheme.colorScheme.error)
            Spacer(Modifier.width(12.dp))
            Text(
                t("suspended.self.title"),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.weight(1f),
            )
            me.lakko.app.ui.components.LkIcon(
                me.lakko.app.R.drawable.ic_lk_chevron_forward,
                contentDescription = null,
                size = me.lakko.app.ui.components.LkIconSize.Row,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    HorizontalDivider()
}

/** In place of a composer while the account is suspended: one muted line,
 *  no send control. */
@Composable
public fun SuspendedComposerLine(modifier: Modifier = Modifier) {
    Surface(color = MaterialTheme.colorScheme.surface, modifier = modifier.fillMaxWidth()) {
        Column {
            HorizontalDivider()
            Text(
                t("suspended.self.composer"),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 16.dp),
            )
        }
    }
}
