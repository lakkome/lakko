// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.l10n.t
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.contacts.MessageAction
import me.lakko.app.ui.contacts.contactRouteKey
import me.lakko.app.ui.contacts.messageAction

/**
 * New chat — the contact picker behind the Chats tab's compose action.
 *
 * Lists the accepted contacts; tapping one opens the 1x1 chat that the accept
 * already created (the same path the Contacts screen's "Open chat" used). A
 * contact whose chat has not been set up on this device is listed dimmed.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun NewChatScreen(
    contactsRepository: ContactsRepository,
    onBack: () -> Unit,
    onOpenChat: (groupIdHex: String) -> Unit,
    onOpenContacts: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by contactsRepository.state.collectAsStateWithLifecycle()
    val appContext = LocalContext.current.applicationContext
    val directory = remember(appContext) { ProfileDirectory.get(appContext) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val startingChat by contactsRepository.startingChat.collectAsStateWithLifecycle()
    val messageNotices by contactsRepository.messageNotices.collectAsStateWithLifecycle()
    // A founded chat opens only while this list is still on screen.
    val onScreen = remember { java.util.concurrent.atomic.AtomicBoolean(true) }
    DisposableEffect(Unit) {
        onScreen.set(true)
        onDispose { onScreen.set(false) }
    }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("chats.create.chat")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        if (state.accepted.isEmpty()) {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding)
                    .padding(24.dp),
                contentAlignment = Alignment.Center,
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(t("chats.new_chat.no_contacts"), style = MaterialTheme.typography.titleMedium)
                    Spacer(Modifier.height(4.dp))
                    Text(
                        t("chats.new_chat.no_contacts_body"),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Spacer(Modifier.height(12.dp))
                    Button(onClick = onOpenContacts) { Text(t("chats.new_chat.add_contact")) }
                }
            }
        } else {
            LazyColumn(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
            ) {
                items(
                    state.accepted.distinctBy { contactRouteKey(it.request.fromPubkey) },
                    key = { contactRouteKey(it.request.fromPubkey) },
                ) { contact ->
                    val name = nameVersion.let { directory.resolve(contact.request.fromPubkey) }
                    // Their chat is their one-to-one here, never a recorded
                    // id that is a group (ContactsRepository.chatOf). With
                    // none, a pick starts one (maintainer, 2026-11-05); only a
                    // contact with no account id cannot be messaged.
                    val routeKey = contactRouteKey(contact.request.fromPubkey)
                    val action = messageAction(contactsRepository.chatOf(contact), contact.request.fromAccountId)
                    val waiting = contact.request.fromAccountId?.let { id ->
                        me.lakko.app.contacts.requestWaiting(state.sent, java.util.Base64.getEncoder().encodeToString(id))
                    } == true
                    val pick = newChatPickRow(
                        action = action,
                        starting = routeKey in startingChat,
                        line = me.lakko.app.ui.contacts.foundLine(
                            starting = routeKey in startingChat,
                            notice = messageNotices[routeKey],
                            waiting = waiting,
                        ),
                    )
                    ContactPickRow(
                        contact = contact,
                        pick = pick,
                        title = name.primary,
                        subtitle = name.verifiedUsername?.let { "@$it" },
                        onClick = {
                            when (action) {
                                is MessageAction.Open -> onOpenChat(action.groupIdHex)
                                MessageAction.Found -> contactsRepository.startChatWith(routeKey, contact) { founded ->
                                    if (onScreen.get()) onOpenChat(founded)
                                }
                                MessageAction.Unavailable -> Unit
                            }
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun ContactPickRow(
    contact: AcceptedContact,
    pick: NewChatPickRow,
    title: String,
    subtitle: String?,
    onClick: () -> Unit,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .alpha(if (pick.dimmed) 0.5f else 1f)
            .clickable(enabled = pick.enabled, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 10.dp),
    ) {
        LakkoAvatar(
            name = title,
            seed = java.util.Base64.getEncoder().encodeToString(contact.request.fromPubkey),
            personKey = contact.request.fromPubkey,
        )
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.titleMedium)
            val line = pick.lineKey?.let { key ->
                if (pick.lineNamesPerson) t(key, mapOf("name" to title)) else t(key)
            } ?: subtitle
            line?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = if (pick.lineIsError) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        if (pick.busy) {
            Spacer(Modifier.width(12.dp))
            androidx.compose.material3.CircularProgressIndicator(
                modifier = Modifier.size(20.dp),
                strokeWidth = 2.dp,
            )
        }
    }
}

/** How a contact's row in New chat behaves. */
internal data class NewChatPickRow(
    val enabled: Boolean,
    val dimmed: Boolean,
    val busy: Boolean,
    /** A catalog key shown in place of the username, or `null`. */
    val lineKey: String?,
    val lineIsError: Boolean,
    /** [lineKey] takes the person's name as `{name}`. */
    val lineNamesPerson: Boolean = false,
)

/**
 * A person in contacts can always be messaged (maintainer, 2026-11-05): a
 * contact with a chat opens it; one with an account id and no chat runs what
 * Message on the card runs (a spinner while it runs; then the line it left,
 * the row usable again). Only a contact with no account id stays unavailable.
 */
internal fun newChatPickRow(
    action: MessageAction,
    starting: Boolean,
    line: me.lakko.app.ui.contacts.FoundLine?,
): NewChatPickRow = when (action) {
    is MessageAction.Open -> NewChatPickRow(enabled = true, dimmed = false, busy = false, lineKey = null, lineIsError = false)
    // `docs/ux/who-can-find-me.md` 6.4 — the same order as Message on the card:
    // a grant founds the one-to-one, else a request goes inside a shared group,
    // else nothing; the row says which ([me.lakko.app.ui.contacts.foundLine]).
    MessageAction.Found -> NewChatPickRow(
        enabled = !starting,
        dimmed = false,
        busy = starting,
        lineKey = line?.key,
        lineIsError = line?.isError == true,
        lineNamesPerson = line?.namesPerson == true,
    )
    MessageAction.Unavailable -> NewChatPickRow(
        enabled = false,
        dimmed = true,
        busy = false,
        lineKey = "chats.new_chat.not_available",
        lineIsError = false,
    )
}
