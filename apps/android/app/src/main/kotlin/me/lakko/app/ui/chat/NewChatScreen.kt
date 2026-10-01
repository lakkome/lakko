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
import me.lakko.app.ui.contacts.contactRouteKey

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
                    ContactPickRow(
                        contact = contact,
                        title = name.primary,
                        subtitle = name.verifiedUsername?.let { "@$it" },
                        onClick = { contact.groupIdHex?.let(onOpenChat) },
                    )
                }
            }
        }
    }
}

@Composable
private fun ContactPickRow(
    contact: AcceptedContact,
    title: String,
    subtitle: String?,
    onClick: () -> Unit,
) {
    val available = contact.groupIdHex != null
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .alpha(if (available) 1f else 0.5f)
            .clickable(enabled = available, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 10.dp),
    ) {
        LakkoAvatar(name = title, seed = java.util.Base64.getEncoder().encodeToString(contact.request.fromPubkey))
        Spacer(Modifier.width(12.dp))
        Column {
            Text(title, style = MaterialTheme.typography.titleMedium)
            val line = if (available) subtitle else t("chats.new_chat.not_available")
            line?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}
