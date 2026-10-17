// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.Schedule
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.contacts.PendingContacts
import me.lakko.app.l10n.t

/**
 * The chat with a person added through their invite link whose app has not
 * answered yet (ADR-0079 section 5, `docs/ux/contact-invite-link.md` section
 * 4). Mirror of the web `routes/PendingChat.tsx` and the iOS
 * `PendingChatScreen`.
 *
 * One line at the top says the person gets the messages when their app is
 * next online (after 7 days: that they have not answered yet). What is
 * written is kept on this device in the one held-send queue under the
 * contact's queue key (reworked 2026-10-17, as on web and iOS), shown with
 * the clock mark of any unsent message, and sent in order once the
 * inviter's Welcome lands; the screen then hands over to the real chat. The
 * call buttons are disabled.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun PendingChatScreen(
    accountIdHex: String,
    contacts: ContactsRepository,
    onBack: () -> Unit,
    /** The one-to-one exists now: replace this screen with it. */
    onConnected: (groupIdHex: String) -> Unit,
) {
    val invited by contacts.invited.collectAsStateWithLifecycle()
    val handedOver by contacts.handedOver.collectAsStateWithLifecycle()
    val contact = invited.firstOrNull { it.accountIdHex == accountIdHex }
    val name = contact?.let(PendingContacts::name) ?: t("invite.contact.page.unnamed")
    var draft by remember { mutableStateOf("") }
    var callHintShown by remember { mutableStateOf(false) }
    /** Bumped after every message this screen keeps (the queue is not a
     *  flow). */
    var queueVersion by remember { mutableIntStateOf(0) }
    val messages = remember(queueVersion, contact) { contacts.pendingInvitedMessages(accountIdHex) }

    LaunchedEffect(handedOver) { handedOver[accountIdHex]?.let(onConnected) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(name) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
                actions = {
                    // Disabled until the inviter's app answers; a tap shows
                    // the hint (a disabled IconButton takes no tap).
                    IconButton(onClick = { callHintShown = true }) {
                        Icon(
                            Icons.Filled.Call,
                            contentDescription = t("chat.pending.call_disabled", mapOf("name" to name)),
                            tint = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.38f),
                        )
                    }
                    IconButton(onClick = { callHintShown = true }) {
                        Icon(
                            Icons.Filled.Videocam,
                            contentDescription = t("chat.pending.call_disabled", mapOf("name" to name)),
                            tint = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.38f),
                        )
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .imePadding(),
        ) {
            LazyColumn(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
                horizontalAlignment = Alignment.End,
            ) {
                if (contact != null) {
                    item {
                        val line = when (PendingContacts.line(contact, System.currentTimeMillis())) {
                            PendingContacts.Line.WAITING -> t("chat.pending.waiting", mapOf("name" to name))
                            PendingContacts.Line.STALE -> t("chat.pending.stale", mapOf("name" to name))
                        }
                        Text(
                            text = line,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            textAlign = TextAlign.Center,
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(vertical = 12.dp),
                        )
                    }
                    itemsIndexed(messages, key = { i, m -> "${m.heldAtMs}-$i" }) { _, message ->
                        Row(verticalAlignment = Alignment.Bottom) {
                            Surface(
                                shape = RoundedCornerShape(16.dp),
                                color = MaterialTheme.colorScheme.primaryContainer,
                            ) {
                                Text(
                                    text = message.text,
                                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                                )
                            }
                            Icon(
                                Icons.Filled.Schedule,
                                contentDescription = null,
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier
                                    .padding(start = 4.dp)
                                    .size(12.dp),
                            )
                        }
                    }
                }
            }
            if (callHintShown) {
                Text(
                    text = t("chat.pending.call_disabled", mapOf("name" to name)),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(12.dp),
            ) {
                OutlinedTextField(
                    value = draft,
                    onValueChange = { draft = it },
                    placeholder = { Text(t("chat.composer.placeholder")) },
                    maxLines = 5,
                    modifier = Modifier.weight(1f),
                )
                IconButton(
                    enabled = draft.isNotBlank() && contact != null,
                    onClick = {
                        val text = draft.trim()
                        if (contacts.queuePendingInvitedMessage(accountIdHex, text)) {
                            draft = ""
                            queueVersion += 1
                        }
                    },
                ) {
                    Icon(Icons.AutoMirrored.Filled.Send, contentDescription = t("chat.composer.send"))
                }
            }
        }
    }
}
