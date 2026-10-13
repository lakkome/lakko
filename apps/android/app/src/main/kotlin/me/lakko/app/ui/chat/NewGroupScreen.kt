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
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.chat.ChatRepository
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.contacts.contactRouteKey

/**
 * New group — docs/ux/new-group.md, the Android shape.
 *
 * An optional name — shared with every member (ADR-0051, sent into the group
 * right after the creating commit) — and the accepted contacts with a
 * checkbox each. Create starts the group with the first pick through the
 * repository's existing 1x1 create path, records the group as created-as-a-
 * group (so the Groups list owns it even while it has two people), adds the
 * remaining picks one after another through the repository's existing add
 * path, then opens the chat. A later add that fails leaves the group with the
 * members that did land and says so in the chat's members panel.
 *
 * A contact without a captured account id (a pre-ADR-0021 record) is listed
 * dimmed and cannot be picked: the create and add paths need the account id.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun NewGroupScreen(
    contactsRepository: ContactsRepository,
    chatRepository: ChatRepository,
    onBack: () -> Unit,
    onCreated: (groupIdHex: String) -> Unit,
    onOpenContacts: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by contactsRepository.state.collectAsStateWithLifecycle()
    val appContext = LocalContext.current.applicationContext
    val directory = remember(appContext) { ProfileDirectory.get(appContext) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()

    var name by remember { mutableStateOf("") }
    val picked = remember { mutableStateOf(setOf<Long>()) }
    var creating by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<UiText?>(null) }

    val selectable = state.accepted.filter { it.request.fromAccountId != null }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("chats.new_group.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack, enabled = !creating) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
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
            OutlinedTextField(
                value = name,
                onValueChange = { if (it.length <= GroupPresentationStore.MAX_NAME_CHARS) name = it },
                label = { Text(t("chats.new_group.name_label")) },
                singleLine = true,
                enabled = !creating,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            )
            if (state.accepted.isEmpty()) {
                Box(
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxWidth()
                        .padding(24.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Text(
                            t("chats.new_group.no_contacts"),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Spacer(Modifier.height(12.dp))
                        Button(onClick = onOpenContacts) { Text(t("chats.new_group.open_contacts")) }
                    }
                }
            } else {
                Text(
                    t("chat.members.title"),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(start = 16.dp, top = 8.dp, bottom = 4.dp),
                )
                LazyColumn(modifier = Modifier.weight(1f)) {
                    items(
                    state.accepted.distinctBy { contactRouteKey(it.request.fromPubkey) },
                    key = { contactRouteKey(it.request.fromPubkey) },
                ) { contact ->
                        val resolved = nameVersion.let { directory.resolve(contact.request.fromPubkey) }
                        val canPick = contact.request.fromAccountId != null
                        val checked = contact.request.messageId in picked.value
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier
                                .fillMaxWidth()
                                .alpha(if (canPick) 1f else 0.5f)
                                .clickable(enabled = canPick && !creating) {
                                    picked.value = if (checked) picked.value - contact.request.messageId
                                    else picked.value + contact.request.messageId
                                }
                                .padding(horizontal = 16.dp, vertical = 8.dp),
                        ) {
                            LakkoAvatar(
                                name = resolved.primary,
                                seed = java.util.Base64.getEncoder().encodeToString(contact.request.fromPubkey),
                            )
                            Spacer(Modifier.width(12.dp))
                            Column(modifier = Modifier.weight(1f)) {
                                Text(resolved.primary, style = MaterialTheme.typography.titleMedium)
                                val line = if (canPick) {
                                    resolved.verifiedUsername?.let { "@$it" }
                                } else {
                                    t("chats.new_group.accept_again")
                                }
                                line?.let {
                                    Text(
                                        it,
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                            }
                            Checkbox(
                                checked = checked,
                                onCheckedChange = null,
                                enabled = canPick && !creating,
                            )
                        }
                    }
                }
            }
            error?.let {
                Text(
                    it.text(),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }
            Button(
                enabled = !creating && picked.value.isNotEmpty(),
                onClick = {
                    val chosen: List<AcceptedContact> =
                        selectable.filter { it.request.messageId in picked.value }
                    if (chosen.isEmpty()) return@Button
                    creating = true
                    error = null
                    scope.launch {
                        val first = chosen.first()
                        val groupIdHex = try {
                            chatRepository.startGroupWith(
                                recipientAccountId = first.request.fromAccountId!!,
                                recipientSignaturePubkey = first.request.fromPubkey,
                                // ADR-0062 — a group from the first pick on,
                                // even while it holds two people.
                                kind = uniffi.lakko_ffi_uniffi.GroupKind.GROUP,
                            )
                        } catch (cause: Throwable) {
                            me.lakko.app.rethrowIfCancelled(cause) {
                                creating = false
                            }
                            android.util.Log.w("NewGroupScreen", "group creation failed", cause)
                            error = UiText("chats.new_group.error")
                            creating = false
                            return@launch
                        }
                        // ADR-0051 §3 — the name is written to the shared-name
                        // store and sent into the group right after the
                        // creating commit, BEFORE the follow-up adds, so the
                        // first Welcome recipients get it on their first poll.
                        // A failed send keeps the creator's own name (the
                        // growth re-send carries it to the members later).
                        if (name.isNotBlank()) {
                            try {
                                chatRepository.setGroupName(groupIdHex, name, storeFirst = true)
                            } catch (cause: Throwable) {
                                me.lakko.app.rethrowIfCancelled(cause)
                                android.util.Log.w("NewGroupScreen", "group name not sent", cause)
                            }
                        }
                        for (next in chosen.drop(1)) {
                            try {
                                chatRepository.addMemberToGroup(
                                    groupIdHex = groupIdHex,
                                    recipientAccountId = next.request.fromAccountId!!,
                                    recipientSignaturePubkey = next.request.fromPubkey,
                                )
                            } catch (cause: Throwable) {
                                me.lakko.app.rethrowIfCancelled(cause)
                                // The group exists with the members that landed;
                                // the members panel adds the rest.
                                android.util.Log.w("NewGroupScreen", "addMemberToGroup failed", cause)
                            }
                        }
                        creating = false
                        onCreated(groupIdHex)
                    }
                },
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(16.dp),
            ) {
                Text(t(if (creating) "chats.new_group.creating" else "chats.create.group"))
            }
        }
    }
}
