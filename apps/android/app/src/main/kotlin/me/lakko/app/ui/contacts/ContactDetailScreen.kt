// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import kotlinx.coroutines.launch
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.ui.components.LakkoAvatar
import uniffi.lakko_ffi_uniffi.InfoSection
import me.lakko.app.contacts.CONTACT_ACCOUNT_ID_LEN
import me.lakko.app.ui.settings.toModel
import uniffi.lakko_ffi_uniffi.PersonInfoCapabilities
import uniffi.lakko_ffi_uniffi.deletedPersonInfoSections
import uniffi.lakko_ffi_uniffi.personInfoSections
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/** The line under the Message button of a contact with no chat on this
 *  device. One whose account is not known cannot be messaged from here; for
 *  any other, Message founds a new one-to-one (maintainer, 2026-11-05), so
 *  there is no line (`null`). */
internal fun contactChatLineKey(accountId: ByteArray?): String? =
    if (accountId != null && accountId.size == CONTACT_ACCOUNT_ID_LEN) {
        null
    } else {
        "contacts.detail.message_unavailable"
    }

/** A line under Message on a known person with no chat on this device. */
internal data class FoundLine(val key: String, val isError: Boolean, val namesPerson: Boolean = false)

/**
 * The line under Message for a person with no chat here
 * (`docs/ux/who-can-find-me.md` section 6.4): nothing while it runs; then what
 * the last run left ([MessageNotice]); else "Request sent" while a request to
 * them waits in Sent.
 */
internal fun foundLine(starting: Boolean, notice: MessageNotice?, waiting: Boolean): FoundLine? = when {
    starting -> null
    notice == MessageNotice.START_FAILED -> FoundLine("contacts.error.start_chat", isError = true)
    notice == MessageNotice.REQUEST_FAILED -> FoundLine("contacts.detail.request_failed", isError = true)
    notice == MessageNotice.CANNOT_MESSAGE -> FoundLine("contacts.detail.cannot_message", isError = false, namesPerson = true)
    waiting -> FoundLine("contacts.detail.request_sent", isError = false, namesPerson = true)
    else -> null
}

/**
 * Contact detail — docs/ux/client-redesign.md section 5.3, and the person a
 * one-to-one conversation opens (docs/ux/conversation-info.md section 2.1).
 *
 * A 72 dp avatar, the name and the verified username; Message, Call and Video
 * actions (the last two only when a chat exists on this device — a call rides
 * the chat's group, so without one there is nothing to dial and the buttons
 * are disabled rather than faking a state); then rows for the safety number,
 * a rename and removal. No key fingerprint anywhere: the safety number is the
 * verification surface.
 *
 * ONE screen, one presentation (docs/ux/conversation-info.md section 3, "Except
 * a person"): the contacts list and a one-to-one's header both push this, with
 * its own top bar and Back, and with the same controls in the same order.
 * `Message` is always offered; reached from the chat, it simply returns to it.
 * Until 2026-09-26 this had a second, sheet-hosted presentation without
 * `Message`, and a person looking at both saw two cards for one contact.
 * Which rows this holds is [personInfoSections], not a flag read here — the
 * defect that replaced was exactly a panel reading flags.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun ContactDetailScreen(
    /** The route key: base64url of the contact's signature key. */
    contactKey: String,
    contactsRepository: ContactsRepository,
    viewModel: ContactsViewModel,
    onBack: () -> Unit,
    /** Open the contact's chat. From the chat's own header this is a return
     *  to it; the host decides whether that is a pop or a push. */
    onOpenChat: (groupIdHex: String) -> Unit,
    /** Start a 1x1 call on the contact's chat. `null` hides the actions. */
    onStartCall: ((groupIdHex: String, video: Boolean) -> Unit)?,
    onVerifySafetyNumber: (contactKey: String) -> Unit,
    /** Has this device's membership in the contact's chat ended (ADR-0045 UX
     *  6.2)? Asked per chat, because the same screen serves a contact with no
     *  chat on this device yet. The host reads the core's own predicate. */
    chatMembershipEnded: (groupIdHex: String) -> Boolean,
    /** ADR-0045 UX 6.2.4 — forget a chat this device is out of. The host owns
     *  where to land afterwards. */
    onDeleteChat: (groupIdHex: String) -> Unit,
    /** The contact's one-to-one chat on THIS device: the linked id only while
     *  the chat still exists here, else the roster scan, else `null`. The
     *  default trusts the link, which is what every caller did until a stale
     *  link opened a dead chat (2026-09-26); the host passes
     *  `ChatRepository.chatGroupIdHexFor`. */
    resolveChat: (linkedGroupIdHex: String?, peerKey: ByteArray) -> String? = { linked, _ -> linked },
    /** The one-to-one this device holds with the person holding [peerKey],
     *  for a person with no contact row: one the viewer removed, whose
     *  conversation remains (docs/ux/conversation-info.md section 2.1,
     *  amended 2026-10-24). `null` when there is none; the default finds
     *  none. */
    removedPeerChat: (peerKey: ByteArray) -> String? = { null },
    modifier: Modifier = Modifier,
) {
    val contactsState by contactsRepository.state.collectAsStateWithLifecycle()
    val listed: AcceptedContact? = remember(contactsState.accepted, contactKey) {
        contactsRepository.contactByRouteKey(contactKey)
    }
    // A person the viewer removed keeps the conversation; their card is this
    // same page, built from that conversation, and offers `Add to contacts`
    // in place of Rename and Remove. Never listed.
    val removedPeer: AcceptedContact? = remember(listed, contactKey) {
        if (listed != null) {
            null
        } else {
            removedPeerContact(contactKey, removedPeerChat)
        }
    }
    val contact: AcceptedContact? = listed ?: removedPeer
    val isContact = listed != null
    // Removed (here or on another screen) while open: nothing to show.
    LaunchedEffect(contact) { if (contact == null) onBack() }

    val appContext = LocalContext.current.applicationContext
    val directory = remember(appContext) { ProfileDirectory.get(appContext) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    // Opening the detail retries a stalled username verification.
    LaunchedEffect(contactKey) {
        contact?.let { directory.ensureVerified(java.util.Base64.getEncoder().encodeToString(it.request.fromPubkey)) }
    }

    var renaming by remember { mutableStateOf(false) }
    var draftName by remember { mutableStateOf("") }
    var confirmingRemove by remember { mutableStateOf(false) }
    var confirmingDelete by remember { mutableStateOf(false) }
    val blockScope = androidx.compose.runtime.rememberCoroutineScope()

    // Section 4 — an ended membership turns the actions over. Read when the
    // screen opens (and again if the contact's chat link changes): the state
    // is terminal, so there is nothing to re-check on a timer.
    val groupIdHex = contact?.let { resolveChat(it.groupIdHex, it.request.fromPubkey) }
    val membershipEnded = remember(groupIdHex) {
        groupIdHex?.let(chatMembershipEnded) ?: false
    }

    // Section 2.1 — which rows this holds is asked, never decided here. The
    // one copy of the rule is `crates/lakko-ui-rules/src/info.rs`; this screen
    // renders whichever blocks it is handed.
    // Remembered because asking now crosses the FFI: the rule is a native
    // call, and its answer changes only when one of its inputs does.
    val reach = remember(contactsRepository) { me.lakko.app.reach.ReachManager.get(appContext) }
    val visibility by reach.visibility.collectAsStateWithLifecycle()
    val contactsOfContactsOn = visibility?.toModel()?.contactsPartOn == true
    // ADR-0099 section 6 — the block register's answer for this person.
    val blockRegister by me.lakko.app.moderation.BlockRegister.state.collectAsStateWithLifecycle()
    val contactKeyB64 = contact?.request?.fromPubkey?.let { java.util.Base64.getEncoder().encodeToString(it) }
    val isBlocked = blockRegister?.isBlocked(contactKeyB64) == true
    // ADR-0099 step 4 / UX 3 — whether the moderation log confirms this
    // person's account is suspended; asked when the card opens.
    val isSuspended by me.lakko.app.ui.suspension.rememberAccountSuspended(contactKeyB64)
    // ADR-0099 section 4 - no calls and no adding people while suspended.
    val suspendedReadOnlyInfo by me.lakko.app.auth.SuspensionGate.readOnly.collectAsStateWithLifecycle()
    // ADR-0098 — a person whose account was deleted: the shared rule's own
    // section list (open / delete the chat, remove the contact; no block, no
    // report, nothing else).
    val personKeyB64 = contactKeyB64
    val deleted = personKeyB64 != null && nameVersion.let { directory.isDeleted(personKeyB64) }
    LaunchedEffect(personKeyB64) { personKeyB64?.let(directory::noteOnScreen) }
    // UX 16.11 — a key whose account started over is never added back.
    val startedOverKeys by contactsRepository.startedOverKeys.collectAsStateWithLifecycle()
    val keyStartedOver = personKeyB64 != null && personKeyB64 in startedOverKeys
    val sections = remember(membershipEnded, isContact, contactsOfContactsOn, isBlocked, suspendedReadOnlyInfo, deleted, groupIdHex, keyStartedOver) {
        withoutAddForStartedOver(keyStartedOver = keyStartedOver, sections = if (deleted) {
            deletedPersonInfoSections(hasOneToOne = groupIdHex != null, isContact = isContact)
        } else {
            me.lakko.app.ui.suspension.readOnlySections(suspended = suspendedReadOnlyInfo, sections =
            personInfoSections(
                PersonInfoCapabilities(
                    membershipEnded = membershipEnded,
                    isContact = isContact,
                    contactsOfContactsOn = contactsOfContactsOn,
                    isBlocked = isBlocked,
                ),
            ))
        })
    }
    // Read through `nameVersion` so a profile or a username arriving later
    // renames in place.
    val resolvedName = contact?.let { c -> nameVersion.let { directory.resolve(c.request.fromPubkey) } }

    // `Message` on a contact with no chat here founds one (maintainer,
    // 2026-11-05). The founded chat opens only while this card is still on
    // screen; leaving it does not cancel the founding.
    val startingChat by viewModel.startingChat.collectAsStateWithLifecycle()
    val messageNotices by viewModel.messageNotices.collectAsStateWithLifecycle()
    // `docs/ux/who-can-find-me.md` 6.4 — a request to them waits in Sent
    // (read off the collected state so the line follows it).
    val requestWaiting = isContact && contact?.request?.fromAccountId?.let { id ->
        me.lakko.app.contacts.requestWaiting(contactsState.sent, java.util.Base64.getEncoder().encodeToString(id))
    } == true
    val onScreen = remember { java.util.concurrent.atomic.AtomicBoolean(true) }
    DisposableEffect(Unit) {
        onScreen.set(true)
        onDispose { onScreen.set(false) }
    }
    val messageAction = messageAction(
        chatGroupIdHex = groupIdHex,
        // Only a listed contact founds; a removed person's card is built
        // from the conversation they still have.
        accountId = if (isContact) contact?.request?.fromAccountId else null,
    )

    val body: @Composable (Modifier) -> Unit = { bodyModifier ->
        val c = contact
        if (c != null && resolvedName != null) {
            ContactDetailBody(
                contactKey = contactKey,
                accountId = contact?.request?.fromAccountId,
                name = resolvedName,
                groupIdHex = groupIdHex,
                sections = sections,
                messageAction = messageAction,
                startingChat = contactKey in startingChat,
                foundLine = foundLine(
                    starting = contactKey in startingChat,
                    notice = messageNotices[contactKey],
                    waiting = requestWaiting,
                ),
                onStartChat = {
                    viewModel.onStartChatClicked(contactKey, c) { founded ->
                        if (onScreen.get()) onOpenChat(founded)
                    }
                },
                onOpenChat = onOpenChat,
                onStartCall = onStartCall,
                onVerifySafetyNumber = onVerifySafetyNumber,
                onRename = {
                    draftName = c.displayName ?: ""
                    renaming = true
                },
                onRemove = { confirmingRemove = true },
                onAddContact = {
                    val chat = c.groupIdHex
                    if (chat != null) viewModel.onAddContactBack(c.request.fromPubkey, chat)
                },
                onDeleteChat = { confirmingDelete = true },
                isBlocked = isBlocked,
                isSuspended = isSuspended,
                onBlock = {
                    contactKeyB64?.let {
                        me.lakko.app.ui.moderation.ModerationRequests.askBlock(
                            me.lakko.app.ui.moderation.BlockRequest(it, resolvedName.primary),
                        )
                    }
                },
                onUnblock = {
                    contactKeyB64?.let { key ->
                        blockScope.launch { me.lakko.app.ui.moderation.applyBlock(contactsRepository, key, blocked = false) }
                    }
                },
                onReport = {
                    contactKeyB64?.let { key ->
                        me.lakko.app.ui.moderation.ModerationRequests.openReport(
                            me.lakko.app.ui.moderation.ReportRequest(
                                me.lakko.app.moderation.personReportSubject(
                                    accountKeyB64 = key,
                                    name = resolvedName.primary,
                                    conversationKind = me.lakko.app.moderation.ReportConversationKind.CARD,
                                    groupIdHex = groupIdHex,
                                ),
                            ),
                        )
                    }
                },
                theirContactsRow = {
                    // A contact with no known account id cannot be chosen.
                    c.request.fromAccountId?.takeIf { it.size == CONTACT_ACCOUNT_ID_LEN }?.let { id ->
                        me.lakko.app.ui.settings.TheirContactsCanFindMeRow(
                            reach = reach,
                            accountId = id,
                            allContactIds = contactsState.accepted
                                .mapNotNull { it.request.fromAccountId }
                                .filter { it.size == CONTACT_ACCOUNT_ID_LEN },
                        )
                    }
                },
                modifier = bodyModifier,
            )
        }
    }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = {},
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        LkIcon(R.drawable.ic_lk_chevron_back, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        body(
            Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState()),
        )
    }

    // The dialogs belong to the screen rather than to the body.
    if (confirmingDelete && groupIdHex != null) {
        me.lakko.app.ui.chat.DeleteChatDialog(
            onCancel = { confirmingDelete = false },
            onDelete = {
                confirmingDelete = false
                onDeleteChat(groupIdHex)
            },
        )
    }
    if (contact != null && resolvedName != null) {
        RenameContactDialog(
            open = renaming,
            draft = draftName,
            onDraftChange = { draftName = it },
            onDismiss = { renaming = false },
            onSave = {
                viewModel.onRenameContactClicked(contact.request.messageId, draftName)
                renaming = false
            },
        )
        RemoveContactDialog(
            open = confirmingRemove,
            name = resolvedName.primary,
            onDismiss = { confirmingRemove = false },
            onRemove = {
                confirmingRemove = false
                viewModel.onRemoveContactClicked(contact.request.messageId)
                onBack()
            },
        )
    }
}

/**
 * The person: identity, then the actions, then the detail — the common shape
 * docs/ux/conversation-info.md section 2 asks the three info surfaces to share.
 */
@Composable
private fun ContactDetailBody(
    contactKey: String,
    accountId: ByteArray?,
    name: me.lakko.app.profile.ResolvedName,
    groupIdHex: String?,
    sections: List<InfoSection>,
    messageAction: MessageAction,
    startingChat: Boolean,
    foundLine: FoundLine?,
    onStartChat: () -> Unit,
    onOpenChat: (String) -> Unit,
    onStartCall: ((groupIdHex: String, video: Boolean) -> Unit)?,
    onVerifySafetyNumber: (contactKey: String) -> Unit,
    onRename: () -> Unit,
    onRemove: () -> Unit,
    onAddContact: () -> Unit,
    onDeleteChat: () -> Unit,
    theirContactsRow: @Composable () -> Unit,
    modifier: Modifier = Modifier,
    isBlocked: Boolean = false,
    /** ADR-0099 step 4 — the moderation log confirms this account suspended. */
    isSuspended: Boolean = false,
    onBlock: () -> Unit = {},
    onUnblock: () -> Unit = {},
    onReport: () -> Unit = {},
) {
        Column(
            modifier = modifier,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Spacer(Modifier.height(8.dp))
            LakkoAvatar(
                name = if (name.isPlaceholder) "?" else name.primary,
                seed = contactKey,
                size = 72.dp,
                personKey = me.lakko.app.ui.components.personKeyBytes(contactKey),
                // UX §7: a profile video plays in the large views.
                large = true,
            )
            Spacer(Modifier.height(12.dp))
            Text(name.primary, style = MaterialTheme.typography.headlineSmall, textAlign = TextAlign.Center)
            if (name.deleted) {
                // ADR-0098 / UX section 3 — one line, no button.
                Text(
                    t("person.deleted.body"),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }
            name.verifiedUsername?.takeIf { name.primary != "@$it" }?.let { user ->
                Text(
                    "@$user",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            name.theirName?.let { theirs ->
                Text(
                    t("contacts.detail.their_name", mapOf("name" to theirs)),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (name.usernameNotVerified) {
                Text(
                    t("contacts.detail.username_not_verified"),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            // ADR-0099 / UX 1.4 — a muted line under a blocked person's name.
            if (isBlocked) {
                Text(
                    t("block.card_line"),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            // ADR-0099 step 4 / UX 3 — a muted line for a suspended account;
            // the block line wins when both apply.
            if (me.lakko.app.moderation.suspendedCardLineShows(blocked = isBlocked, suspended = isSuspended)) {
                Text(
                    t("suspended.card_line"),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.height(20.dp))

            Row(
                horizontalArrangement = Arrangement.spacedBy(12.dp),
                modifier = Modifier.padding(horizontal = 16.dp),
            ) {
                // Section 2.1 — always, wherever the card was opened from;
                // from the conversation itself the host makes this a return.
                if (InfoSection.MESSAGE in sections) {
                    ActionButton(
                        label = t("contacts.detail.message"),
                        icon = R.drawable.ic_lk_chat_fill,
                        enabled = when (messageAction) {
                            is MessageAction.Open -> true
                            MessageAction.Found -> !startingChat
                            MessageAction.Unavailable -> false
                        },
                        busy = messageAction == MessageAction.Found && startingChat,
                        onClick = {
                            when (messageAction) {
                                is MessageAction.Open -> onOpenChat(messageAction.groupIdHex)
                                MessageAction.Found -> onStartChat()
                                MessageAction.Unavailable -> Unit
                            }
                        },
                        modifier = Modifier.weight(1f),
                    )
                }
                // Section 4 — a conversation this device's membership has
                // ended in has no call to place, so the tiles are gone rather
                // than dimmed: the key comes from a group we are not in.
                if (InfoSection.CALL in sections) {
                    ActionButton(
                        label = t("chat.call.voice"),
                        icon = R.drawable.ic_lk_phone_fill,
                        enabled = groupIdHex != null && onStartCall != null,
                        onClick = { if (groupIdHex != null) onStartCall?.invoke(groupIdHex, false) },
                        modifier = Modifier.weight(1f),
                    )
                    ActionButton(
                        label = t("chat.media.video"),
                        icon = R.drawable.ic_lk_video_fill,
                        enabled = groupIdHex != null && onStartCall != null,
                        onClick = { if (groupIdHex != null) onStartCall?.invoke(groupIdHex, true) },
                        modifier = Modifier.weight(1f),
                    )
                }
            }
            // The line exists to explain a dimmed tile, so it goes when there
            // is no tile to explain.
            val hasConversationActions =
                InfoSection.CALL in sections || InfoSection.MESSAGE in sections
            val noChatLine = if (groupIdHex == null) contactChatLineKey(accountId) else null
            if (noChatLine != null && hasConversationActions) {
                Spacer(Modifier.height(8.dp))
                Text(
                    t(noChatLine),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            // What Message on a person with no chat here led to (section
            // 6.4): a request waiting, nothing sendable, or a failure (Message
            // stays usable).
            if (messageAction == MessageAction.Found && foundLine != null) {
                Spacer(Modifier.height(8.dp))
                Text(
                    if (foundLine.namesPerson) t(foundLine.key, mapOf("name" to name.primary)) else t(foundLine.key),
                    style = MaterialTheme.typography.bodySmall,
                    color = if (foundLine.isError) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                )
            }
            Spacer(Modifier.height(24.dp))
            HorizontalDivider()

            if (InfoSection.SAFETY_NUMBER in sections) {
                DetailRow(
                    title = t("contacts.safety.title"),
                    supporting = t("contacts.safety.row_sub"),
                    icon = R.drawable.ic_lk_shield_check,
                    onClick = { onVerifySafetyNumber(contactKey) },
                )
            }
            // ADR-0082 / translation UX 3.7 — `Translate automatically`, for
            // this device only; absent with no chat or no engine.
            if (InfoSection.MESSAGE in sections) {
                me.lakko.app.ui.translation.AutoTranslateRow(groupIdHex)
            }
            if (InfoSection.RENAME_CONTACT in sections) {
                DetailRow(
                    title = t("contacts.rename.action"),
                    supporting = t("contacts.rename.sub_local"),
                    icon = R.drawable.ic_lk_pencil,
                    onClick = onRename,
                )
            }
            // Section 2.1 (amended 2026-10-24) — a person the viewer removed:
            // back on the list with the conversation they have. Local; sends
            // nothing.
            if (InfoSection.ADD_CONTACT in sections) {
                ListItem(
                    headlineContent = { Text(t("contacts.add_back.control")) },
                    supportingContent = {
                        Text(t("contacts.add_back.sub", mapOf("name" to name.primary)))
                    },
                    leadingContent = {
                        LkIcon(
                            R.drawable.ic_lk_person_plus,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            size = LkIconSize.Row,
                        )
                    },
                    trailingContent = {
                        TextButton(onClick = onAddContact) { Text(t("contacts.add_back.action")) }
                    },
                    colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
                    modifier = Modifier.clickable(onClick = onAddContact),
                )
            }
            // Section 2.1 — who-can-find-me 5.1.
            if (InfoSection.THEIR_CONTACTS_CAN_FIND_ME in sections) theirContactsRow()
            // ADR-0099 — the danger block: Report, then Block or Unblock, then
            // Remove contact (the order the rule returns).
            if (InfoSection.REPORT in sections) {
                HorizontalDivider()
                DetailRow(
                    title = t("report.action"),
                    supporting = null,
                    icon = R.drawable.ic_lk_flag,
                    onClick = onReport,
                    danger = true,
                )
            }
            if (InfoSection.BLOCK in sections) {
                DetailRow(
                    title = t("block.action"),
                    supporting = null,
                    icon = R.drawable.ic_lk_nosign,
                    onClick = onBlock,
                    danger = true,
                )
            }
            if (InfoSection.UNBLOCK in sections) {
                DetailRow(
                    title = t("block.unblock"),
                    supporting = null,
                    icon = R.drawable.ic_lk_nosign,
                    onClick = onUnblock,
                )
            }
            if (InfoSection.REMOVE_CONTACT in sections) {
                HorizontalDivider()
                DetailRow(
                    title = t("contacts.remove.control"),
                    supporting = null,
                    icon = R.drawable.ic_lk_person_minus,
                    onClick = onRemove,
                    danger = true,
                )
            }
            // Section 4 — on a one-to-one exactly as on a group: the one thing
            // still open once this device's membership has ended.
            if (InfoSection.DELETE_CHAT in sections && groupIdHex != null) {
                HorizontalDivider()
                DetailRow(
                    title = me.lakko.app.chat.DELETE_CHAT_CONTROL.text(),
                    supporting = null,
                    icon = R.drawable.ic_lk_trash,
                    onClick = onDeleteChat,
                    danger = true,
                )
            }
            Spacer(Modifier.height(24.dp))
        }
}

/**
 * The local rename — the viewer's own name for a contact, which nobody else
 * ever sees.
 */
@Composable
private fun RenameContactDialog(
    open: Boolean,
    draft: String,
    onDraftChange: (String) -> Unit,
    onDismiss: () -> Unit,
    onSave: () -> Unit,
) {
    if (!open) return
    LakkoDialog(
        onDismissRequest = onDismiss,
        title = t("contacts.rename.action"),
        body = t("contacts.rename.placeholder"),
        confirm = LakkoDialogAction(t("common.save"), onClick = onSave),
        cancel = LakkoDialogAction(t("common.cancel"), LakkoDialogTone.Ghost, onClick = onDismiss),
    ) {
        OutlinedTextField(
            value = draft,
            onValueChange = onDraftChange,
            singleLine = true,
            label = { Text(t("contacts.rename.field")) },
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/** Removing a contact, which deliberately leaves any existing chat behind. */
@Composable
private fun RemoveContactDialog(
    open: Boolean,
    name: String,
    onDismiss: () -> Unit,
    onRemove: () -> Unit,
) {
    if (!open) return
    LakkoDialog(
        onDismissRequest = onDismiss,
        title = t("contacts.remove.title"),
        body = t(
            "common.sentence_pair",
            mapOf(
                "first" to t("contacts.remove.sub", mapOf("name" to name)),
                "second" to t("contacts.remove.body"),
            ),
        ),
        confirm = LakkoDialogAction(t("common.remove"), LakkoDialogTone.Danger, onClick = onRemove),
    )
}

/** One action tile. `internal` so the unknown-person layer draws the same one. */
@Composable
internal fun ActionButton(
    label: String,
    @androidx.annotation.DrawableRes icon: Int,
    enabled: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    /** Work behind the tile is running: a spinner in place of the icon. */
    busy: Boolean = false,
) {
    // Three tiles share one row on a 360 dp phone; the default button padding
    // left "Message" wrapping onto two lines, so the tile keeps a single line
    // and trims its horizontal padding instead.
    FilledTonalButton(
        onClick = onClick,
        enabled = enabled,
        modifier = modifier,
        contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 8.dp, vertical = 10.dp),
    ) {
        if (busy) {
            androidx.compose.material3.CircularProgressIndicator(
                modifier = Modifier.size(18.dp),
                strokeWidth = 2.dp,
            )
        } else {
            LkIcon(icon, contentDescription = null, size = LkIconSize.Row)
        }
        Spacer(Modifier.width(6.dp))
        Text(label, maxLines = 1, softWrap = false)
    }
}

@Composable
private fun DetailRow(
    title: String,
    supporting: String?,
    @androidx.annotation.DrawableRes icon: Int,
    onClick: () -> Unit,
    danger: Boolean = false,
) {
    val tint = if (danger) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant
    ListItem(
        headlineContent = {
            Text(title, color = if (danger) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
        },
        supportingContent = supporting?.let { { Text(it) } },
        leadingContent = { LkIcon(icon, contentDescription = null, tint = tint, size = LkIconSize.Row) },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
        modifier = Modifier.clickable(onClick = onClick),
    )
}

// =============================================================================
// Safety number
// =============================================================================

private sealed interface SafetyNumberUi {
    data object Loading : SafetyNumberUi
    data class Ready(val groups: List<String>) : SafetyNumberUi
    data object Error : SafetyNumberUi
}

/**
 * The safety number between this account and a contact — ADR-0005 section 13.5
 * — in five-digit groups, large, with a one-paragraph explanation. Both people
 * see the same number; comparing it out of band confirms nobody sits between
 * them. There is no "verified" toggle: this client keeps no per-contact
 * verification mark yet, so none is shown.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun SafetyNumberScreen(
    contactKey: String,
    contactsRepository: ContactsRepository,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val contactsState by contactsRepository.state.collectAsStateWithLifecycle()
    val contact = remember(contactsState.accepted, contactKey) {
        contactsRepository.contactByRouteKey(contactKey)
    }
    // A person the viewer removed has no row but keeps the safety number: it
    // is computed over the key the route names (section 2.1, amended
    // 2026-10-24).
    val peerKey: ByteArray? = contact?.request?.fromPubkey ?: routeKeyBytes(contactKey)
    val appContext = LocalContext.current.applicationContext
    val directory = remember(appContext) { ProfileDirectory.get(appContext) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val name = peerKey?.let { nameVersion.let { _ -> directory.resolve(it).primary } }

    var ui: SafetyNumberUi by remember { mutableStateOf(SafetyNumberUi.Loading) }
    LaunchedEffect(contactKey, contact) {
        val key = peerKey ?: return@LaunchedEffect
        ui = try {
            // Derives the own identity from the seed; keep it off the main thread.
            val raw = withContext(Dispatchers.Default) { contactsRepository.safetyNumber(key) }
            SafetyNumberUi.Ready(safetyNumberGroups(raw))
        } catch (cause: Throwable) {
            me.lakko.app.rethrowIfCancelled(cause)
            android.util.Log.w("LakkoContacts", "WARN safety number failed: ${cause.javaClass.simpleName}")
            SafetyNumberUi.Error
        }
    }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("contacts.safety.number")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        LkIcon(R.drawable.ic_lk_chevron_back, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 24.dp, vertical = 16.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            when (val s = ui) {
                SafetyNumberUi.Loading -> CircularProgressIndicator(modifier = Modifier.padding(32.dp))
                SafetyNumberUi.Error -> Text(
                    t("contacts.safety.error"),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
                is SafetyNumberUi.Ready -> {
                    Spacer(Modifier.height(8.dp))
                    for (row in safetyNumberRows(s.groups)) {
                        Row(horizontalArrangement = Arrangement.spacedBy(20.dp)) {
                            for (group in row) {
                                Text(
                                    text = group,
                                    fontFamily = FontFamily.Monospace,
                                    fontWeight = FontWeight.SemiBold,
                                    fontSize = 24.sp,
                                    letterSpacing = 1.sp,
                                )
                            }
                        }
                        Spacer(Modifier.height(10.dp))
                    }
                }
            }
            Spacer(Modifier.height(16.dp))
            Text(
                text = if (name != null) {
                    t("contacts.safety.body", mapOf("name" to name))
                } else {
                    t("contacts.safety.body_unnamed")
                },
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }
    }
}
