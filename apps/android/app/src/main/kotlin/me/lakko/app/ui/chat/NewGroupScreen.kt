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
/** Create works with a name, a picked person, or both; a group of nobody with
 *  no name is the one thing it does not start. */
internal fun canCreateGroup(creating: Boolean, name: String, pickedCount: Int): Boolean =
    !creating && (pickedCount > 0 || name.isNotBlank())

/** ADR-0087 / group-and-channel-privacy.md section 3.1 — Create is enabled
 *  when the @name is well formed and, if Public, there is a name; nobody has
 *  to be picked. [wellFormed] is the core's username rule. */
internal fun canCreateGroupWithHandle(
    creating: Boolean,
    name: String,
    handleWellFormed: Boolean,
    public: Boolean,
): Boolean = !creating && handleWellFormed && (!public || name.isNotBlank())

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
    // Section 3.1 — the proposed @name follows the name field until the person
    // edits it; Private by default.
    var public by remember { mutableStateOf(false) }
    val random = remember { me.lakko.app.chat.handleRandom() }
    var handle by remember { mutableStateOf(me.lakko.app.chat.suggestedGroupHandle("", true, random)) }
    var handleEdited by remember { mutableStateOf(false) }
    var handleError by remember { mutableStateOf<UiText?>(null) }
    // ADR-0087 4.5.3 — the optional photo, uploaded at Create.
    var photo by remember { mutableStateOf<me.lakko.app.avatar.AvatarMedia.Encoded?>(null) }
    var photoError by remember { mutableStateOf<UiText?>(null) }
    val appCtx = LocalContext.current.applicationContext
    val refreshSuggestion: () -> Unit = {
        if (!handleEdited) handle = me.lakko.app.chat.suggestedGroupHandle(name, !public, random)
    }
    val wellFormed = me.lakko.app.chat.handleWellFormed(handle)
    // Section 3.3 — Public: 500 ms after typing stops, from 3 characters, a
    // lookup says whether the name is visibly taken. Private asks nothing.
    var visiblyTaken by remember { mutableStateOf(false) }
    androidx.compose.runtime.LaunchedEffect(handle, public) {
        visiblyTaken = false
        if (!public || handle.length < 3 || !wellFormed) return@LaunchedEffect
        kotlinx.coroutines.delay(500)
        visiblyTaken = chatRepository.handleVisiblyTaken(handle) == true
    }

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
            NewGroupPhotoSlot(
                name = name,
                chosen = photo,
                enabled = !creating,
                onChosen = {
                    photo = it
                    photoError = null
                },
                error = photoError,
            )
            OutlinedTextField(
                value = name,
                onValueChange = {
                    if (it.length <= GroupPresentationStore.MAX_NAME_CHARS) {
                        name = it
                        refreshSuggestion()
                    }
                },
                label = { Text(t("chats.new_group.name_label")) },
                singleLine = true,
                enabled = !creating,
                supportingText = if (public && name.isBlank()) {
                    { Text(t("group_type.needs_name")) }
                } else {
                    null
                },
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            )
            // Section 3.1 — the @name, always LTR and Latin.
            OutlinedTextField(
                value = handle,
                onValueChange = {
                    handle = me.lakko.app.chat.canonicalHandleInput(it).take(32)
                    handleEdited = true
                    handleError = null
                },
                label = { Text(t("group_type.handle")) },
                prefix = { Text("@") },
                singleLine = true,
                enabled = !creating,
                isError = handleError != null || visiblyTaken || (handle.isNotEmpty() && !wellFormed),
                supportingText = {
                    val line = when {
                        handleError != null -> handleError!!.text()
                        visiblyTaken -> t("group_handle.error.taken")
                        handle.isNotEmpty() && !wellFormed -> t("group_handle.error.bad_form")
                        public -> t("group_type.handle_public")
                        else -> t("group_type.handle_private")
                    }
                    Text(line)
                },
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp),
            )
            // Section 3.1 — Who can join: Private (default) or Public.
            Text(
                t("group_type.who_can_join"),
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(start = 16.dp, top = 8.dp),
            )
            for (option in listOf(false, true)) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable(enabled = !creating) {
                            public = option
                            refreshSuggestion()
                        }
                        .padding(horizontal = 8.dp, vertical = 2.dp),
                ) {
                    androidx.compose.material3.RadioButton(
                        selected = public == option,
                        onClick = {
                            public = option
                            refreshSuggestion()
                        },
                        enabled = !creating,
                    )
                    Column {
                        Text(t(if (option) "group_type.public" else "group_type.private"))
                        Text(
                            t(if (option) "group_type.public_desc" else "group_type.private_desc"),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
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
                            t("new_group.no_contacts"),
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
                        val checked = contact.request.messageId in picked.value && canPick
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
                                personKey = contact.request.fromPubkey,
                            )
                            Spacer(Modifier.width(12.dp))
                            Column(modifier = Modifier.weight(1f)) {
                                Text(resolved.primary, style = MaterialTheme.typography.titleMedium)
                                val line = when {
                                    // A contact with no display name is titled "@username"; the
                                    // line would repeat it.
                                    canPick -> resolved.verifiedUsername?.let { "@$it" }?.takeIf { it != resolved.primary }
                                    else -> t("chats.new_group.accept_again")
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
                enabled = canCreateGroupWithHandle(creating, name, wellFormed, public) && !visiblyTaken,
                onClick = {
                    val chosen: List<AcceptedContact> =
                        selectable.filter { it.request.messageId in picked.value }
                    creating = true
                    error = null
                    handleError = null
                    scope.launch {
                        // ADR-0087 4.5.3 — the photo first: a failed upload
                        // creates nothing, and the form stays filled.
                        var avatar: me.lakko.app.profile.AvatarRefs? = null
                        photo?.let { chosenPhoto ->
                            try {
                                avatar = uploadGroupPhoto(appCtx, chosenPhoto)
                            } catch (cause: Throwable) {
                                me.lakko.app.rethrowIfCancelled(cause) { creating = false }
                                photoError = newGroupPhotoFailure(cause)
                                creating = false
                                return@launch
                            }
                        }
                        // ADR-0087 sections 3 and 10 — the group starts with
                        // its creator; the @name is claimed before anything
                        // about the group leaves this device, so a held name
                        // creates nothing. Picks are added through the normal
                        // add path.
                        var claimed: me.lakko.app.chat.SignedGroupHandle? = null
                        val groupIdHex = try {
                            chatRepository.startGroupAlone { groupId, knock ->
                                val facts = if (public) {
                                    me.lakko.app.chat.PublicGroupFacts(
                                        name = name.trim(),
                                        description = "",
                                        memberCount = 1 + chosen.size,
                                        knockPublicKey = knock,
                                        avatar = avatar,
                                    )
                                } else {
                                    null
                                }
                                claimed = chatRepository.claimHandleForNewGroup(groupId, handle, public, facts)
                            }
                        } catch (cause: Throwable) {
                            me.lakko.app.rethrowIfCancelled(cause) {
                                creating = false
                            }
                            android.util.Log.w("NewGroupScreen", "group creation failed", cause)
                            val handleCopy = me.lakko.app.chat.handleWriteFailure(cause)
                            if (handleCopy.key == "group_handle.error.held") {
                                handleError = handleCopy
                            } else if (handleCopy.key != "group_handle.error.failed") {
                                error = handleCopy
                            } else {
                                error = UiText("chats.new_group.error")
                            }
                            creating = false
                            return@launch
                        }
                        claimed?.let { signed ->
                            try {
                                chatRepository.adoptSignedHandle(groupIdHex, signed)
                            } catch (cause: Throwable) {
                                me.lakko.app.rethrowIfCancelled(cause)
                                android.util.Log.w("NewGroupScreen", "group handle not stored", cause)
                            }
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
                        // 4.5.3 — the photo goes to the group right after the
                        // creating commit; a failed send is kept owed.
                        avatar?.let { refs ->
                            try {
                                chatRepository.setGroupPhoto(groupIdHex, refs)
                            } catch (cause: Throwable) {
                                me.lakko.app.rethrowIfCancelled(cause)
                                android.util.Log.w("NewGroupScreen", "group photo not sent", cause)
                            }
                        }
                        for (next in chosen) {
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
