// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.calls

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import kotlinx.coroutines.launch
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.calls.AppCallHost
import me.lakko.app.calls.NEW_CALL_SIGNED_OUT_MESSAGE
import me.lakko.app.calls.newCallStartMessage
import me.lakko.app.chat.ChatRepository
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.call.rememberCallPermissionGate
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction

/**
 * The Calls destination: the history, plus the one action beside it.
 *
 * `docs/ux/call-links.md` sections 1 and 2. The destination used to hold a
 * create-a-link form, reached through [me.lakko.app.ui.contacts.ContactsViewModel]'s
 * call-link surface; the form asked four questions and produced a string of
 * text the person then carried somewhere themselves. All of that is gone.
 * `New call` founds a group video call through the process-wide
 * [AppCallHost.startNewCall] and the person lands in it; a link to that call is
 * offered from inside it, by the `Invite` control, and only if they want one.
 *
 * [CallsScreen] itself knows nothing about calls or links; it draws the history
 * and reports the taps. That keeps the screen testable against a store and
 * leaves the call machinery in one place.
 */
@Composable
public fun CallsHost(
    repository: ChatRepository,
    onOpenConversation: (groupIdHex: String) -> Unit,
    onCallBack: (groupIdHex: String, video: Boolean) -> Unit,
    /** The signed-in session, read at the moment a call is started. */
    identitySecrets: () -> IdentitySecrets?,
    modifier: Modifier = Modifier,
) {
    val scope = rememberCoroutineScope()
    // The same just-in-time gate every call entry point uses. A video call
    // whose camera was refused proceeds with audio only; the gate reports
    // which, and the call is started either way.
    val permissions = rememberCallPermissionGate()
    var starting by remember { mutableStateOf(false) }
    var startMessage by remember { mutableStateOf<me.lakko.app.l10n.UiText?>(null) }

    startMessage?.let { message ->
        LakkoDialog(
            onDismissRequest = { startMessage = null },
            title = null,
            body = message.text(),
            confirm = LakkoDialogAction(t("common.ok")) { startMessage = null },
            cancel = null,
        )
    }

    CallsScreen(
        repository = repository,
        onOpenConversation = onOpenConversation,
        onCallBack = onCallBack,
        onNewCall = {
            val secrets = identitySecrets()
            when {
                // Section 5's "signed out mid-flow": one sentence, no form.
                secrets == null -> startMessage = NEW_CALL_SIGNED_OUT_MESSAGE
                // A second tap while the first start is in flight would race the
                // host's own slot check for the same call; drop it instead.
                starting -> Unit
                else -> permissions.request(video = true) { video ->
                    starting = true
                    scope.launch {
                        try {
                            val outcome = AppCallHost.startNewCall(
                                // ADR-0074 Stage A: the conversation identity.
                                accountIdentitySeed = me.lakko.app.auth.conversationAccountSeedOf(
                                    repository.context,
                                    me.lakko.app.auth.base64Encode(secrets.accountId),
                                    secrets.accountIdentitySeed,
                                ),
                                accountId = secrets.accountId,
                                sessionToken = secrets.sessionToken,
                                video = video,
                            )
                            startMessage = newCallStartMessage(outcome)
                        } finally {
                            starting = false
                        }
                    }
                }
            }
        },
        modifier = modifier,
    )
}
