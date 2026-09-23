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
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.calls.callInviteSlugFromInput
import me.lakko.app.chat.ChatRepository
import me.lakko.app.ui.contacts.CallLinksSheet
import me.lakko.app.ui.contacts.ContactsViewModel
import me.lakko.app.ui.contacts.CreateCallLinkDialog
import me.lakko.app.ui.contacts.copyToClipboard
import me.lakko.app.ui.contacts.launchShareIntent

/**
 * The Calls destination: the history, plus the two actions section 5.2 puts
 * beside it.
 *
 * `docs/ux/navigation.md` sections 3 and 5. "Create call link" was in the
 * Contacts "+" menu only because Calls had been removed; with Calls back it
 * comes home, which is why this reuses [ContactsViewModel]'s call-link surface
 * rather than growing a second copy of the mint-and-share flow. The view model
 * is the same singleton instance Contacts holds, so a link created here is the
 * same link listed there.
 *
 * [CallsScreen] itself knows nothing about links; it draws the history and
 * reports the two taps. That keeps the screen testable against a store and
 * leaves the link machinery in one place.
 */
@Composable
public fun CallsHost(
    repository: ChatRepository,
    viewModel: ContactsViewModel,
    onOpenConversation: (groupIdHex: String) -> Unit,
    onCallBack: (groupIdHex: String, video: Boolean) -> Unit,
    /** Open a `/c/` call link's landing — a created one, or a pasted one. */
    onOpenCallLink: (slug: String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val callLinks by viewModel.callLinks.collectAsStateWithLifecycle()
    var linksSheetOpen by remember { mutableStateOf(false) }
    var joinOpen by remember { mutableStateOf(false) }

    // A created link opens the sheet so the result is shown in the link-row
    // style right away, the way it already did from Contacts.
    var seenCallLinks by remember { mutableIntStateOf(callLinks.created.size) }
    LaunchedEffect(callLinks.created.size) {
        if (callLinks.created.size > seenCallLinks) linksSheetOpen = true
        seenCallLinks = callLinks.created.size
    }

    if (callLinks.formOpen) {
        CreateCallLinkDialog(
            state = callLinks,
            onMediaChange = viewModel::onCallLinkMediaChange,
            onExpiryChange = viewModel::onCallLinkExpiryChange,
            onMaxUsesChange = viewModel::onCallLinkMaxUsesChange,
            onLabelChange = viewModel::onCallLinkLabelChange,
            onCreate = viewModel::onCreateCallLink,
            onDismiss = viewModel::onDismissCallLinkForm,
        )
    }

    if (linksSheetOpen) {
        CallLinksSheet(
            links = callLinks.created,
            onCreate = {
                linksSheetOpen = false
                viewModel.onOpenCallLinkForm()
            },
            onCopy = { url -> copyToClipboard(context, url) },
            onShare = { url -> launchShareIntent(context, url) },
            onStartCall = { slug ->
                linksSheetOpen = false
                onOpenCallLink(slug)
            },
            onDismiss = { linksSheetOpen = false },
        )
    }

    if (joinOpen) {
        JoinByLinkDialog(
            onDismiss = { joinOpen = false },
            onJoin = { slug ->
                joinOpen = false
                onOpenCallLink(slug)
            },
        )
    }

    CallsScreen(
        repository = repository,
        onOpenConversation = onOpenConversation,
        onCallBack = onCallBack,
        onCreateCallLink = {
            // The sheet when there is something to list, the form when there
            // is not: a person with no links wants to make one, not to read an
            // empty list first.
            if (callLinks.created.isEmpty()) {
                viewModel.onOpenCallLinkForm()
            } else {
                linksSheetOpen = true
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
