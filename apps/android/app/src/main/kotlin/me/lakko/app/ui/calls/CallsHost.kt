// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.calls

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
import me.lakko.app.calls.callInviteSlugFromInput
import me.lakko.app.calls.newCallStartMessage
import me.lakko.app.chat.ChatRepository
import me.lakko.app.ui.call.rememberCallPermissionGate

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
    /** Open a pasted `/c/` call link's landing. */
    onOpenCallLink: (slug: String) -> Unit,
    /** The signed-in session, read at the moment a call is started. */
    identitySecrets: () -> IdentitySecrets?,
    modifier: Modifier = Modifier,
) {
    val scope = rememberCoroutineScope()
    // The same just-in-time gate every call entry point uses. A video call
    // whose camera was refused proceeds with audio only; the gate reports
    // which, and the call is started either way.
    val permissions = rememberCallPermissionGate()
    var joinOpen by remember { mutableStateOf(false) }
    var starting by remember { mutableStateOf(false) }
    var startMessage by remember { mutableStateOf<String?>(null) }

    if (joinOpen) {
        JoinByLinkDialog(
            onDismiss = { joinOpen = false },
            onJoin = { slug ->
                joinOpen = false
                onOpenCallLink(slug)
            },
        )
    }

    startMessage?.let { message ->
        AlertDialog(
            onDismissRequest = { startMessage = null },
            text = { Text(message) },
            confirmButton = {
                TextButton(onClick = { startMessage = null }) { Text("OK") }
            },
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
                                accountIdentitySeed = secrets.accountIdentitySeed,
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
        onJoinByLink = { joinOpen = true },
        modifier = modifier,
    )
}

/**
 * Paste a call link and open it.
 *
 * The parsing is [callInviteSlugFromInput], which is pure and tested; this only
 * reports the one thing it can say no about. The link is not validated here —
 * that happens on the landing screen, which is where a bad link has somewhere
 * to be reported.
 */
@Composable
private fun JoinByLinkDialog(onDismiss: () -> Unit, onJoin: (slug: String) -> Unit) {
    var input by remember { mutableStateOf("") }
    val slug = remember(input) { callInviteSlugFromInput(input) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Join by link") },
        text = {
            Column(modifier = Modifier.fillMaxWidth()) {
                OutlinedTextField(
                    value = input,
                    onValueChange = { input = it },
                    label = { Text("Call link") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                if (input.isNotBlank() && slug == null) {
                    Text(
                        text = "That is not a call link. Paste the whole link and try again.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = { slug?.let(onJoin) },
                enabled = slug != null,
            ) { Text("Join") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}
