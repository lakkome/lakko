// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

@file:OptIn(
    androidx.compose.foundation.ExperimentalFoundationApi::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
)

package me.lakko.app.ui.chat

import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.combinedClickable
import androidx.compose.animation.animateColorAsState
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import me.lakko.app.ui.components.submitOnHardwareEnter
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.role
import androidx.compose.ui.text.style.TextOverflow
import me.lakko.app.chat.ReplyArmed
import me.lakko.app.chat.ReplyRef
import me.lakko.app.chat.replyQuoteLines
import me.lakko.app.chat.ForwardRef
import me.lakko.app.chat.ForwardRequest
import me.lakko.app.chat.PendingMessageJump
import me.lakko.app.chat.forwardLineLabel
import androidx.compose.foundation.clickable
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.FilterChip
import androidx.compose.ui.unit.sp
import androidx.compose.foundation.layout.offset
import androidx.compose.ui.semantics.customActions
import me.lakko.app.chat.toFfiAllowed
import androidx.compose.material3.Icon
import androidx.compose.material3.Scaffold
import androidx.compose.material3.TopAppBar
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.LakkoAvatar
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.positionInParent
import androidx.compose.ui.layout.positionOnScreen
import androidx.compose.ui.unit.toSize
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.Dp
import java.time.ZoneId
import java.util.Locale
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.border
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.selected
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.calls.CallSession
import me.lakko.app.calls.callOwnsCapture
import me.lakko.app.calls.quietOffer
import me.lakko.app.chat.AttachmentBridge
import androidx.compose.ui.text.style.TextDecoration
import me.lakko.app.chat.Ack
import me.lakko.app.chat.ConversationInfo
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.chat.GroupPollOutcome
import me.lakko.app.chat.UploadStage
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.ConversationInfoCapabilities
import uniffi.lakko_ffi_uniffi.ConversationInfoKind
import uniffi.lakko_ffi_uniffi.ConversationRole
import uniffi.lakko_ffi_uniffi.InfoSection
import uniffi.lakko_ffi_uniffi.callHoldsCapture
import uniffi.lakko_ffi_uniffi.conversationInfoSections
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize
import me.lakko.app.ui.components.TextAction

/**
 * 1×1 chat screen — `LazyColumn` of messages + composer.
 *
 * Mirror of `apps/web`'s eventual ChatRoute. Minimal Material3:
 * outgoing rows right-aligned in a primary-tinted bubble,
 * incoming rows left-aligned in a surface-variant bubble,
 * system rows centered + smaller font. The auto-scroll-to-
 * bottom on new messages (the canonical chat-UX touch) is
 * one `LaunchedEffect(messages.size)` away.
 *
 * No continuous polling — `LaunchedEffect(Unit)` fires one
 * refresh on first arrival; the user taps Refresh in the
 * header thereafter. Same posture as 1.15e6's
 * [me.lakko.app.ui.contacts.ContactsScreen] +
 * 1.15f4b's [ChatListScreen].
 *
 * Composer uses the standard `OutlinedTextField` with
 * multi-line support (`singleLine = false`) — paste a 5-line
 * blob and it renders as five lines, send wraps everything
 * into one Application-message. Send button is disabled when
 * the draft is blank or a send is in flight.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun ChatScreen(
    viewModel: ChatViewModel,
    onNavigateBack: () -> Unit,
    /** docs/ux/conversation-info.md — which person a one-to-one header opens
     *  is read off this device's contact list. */
    contactsRepository: me.lakko.app.contacts.ContactsRepository,
    /** Section 3, "Except a person" — push the contact screen for a person
     *  who is a contact, exactly as the contacts list pushes it. The chat stays
     *  on the back stack, so Back (and the screen's `Message`) return here. */
    onOpenContact: (contactKey: String) -> Unit,
    modifier: Modifier = Modifier,
    /** docs/ux/conversation-info.md section 2.4 — the people search the Add
     *  members screen asks the network with; `null` shows contacts only. */
    peopleSearch: me.lakko.app.search.PeopleSearchService? = null,
    /** The group's title (local name or member-derived) for a group chat;
     *  a 1x1 chat names the other person itself. Never a hex id. */
    groupTitle: String? = null,
    /** ADR-0052 decision 2 / UX 5.1 — Forward was chosen on a message: open
     *  the "Forward to" picker with it. */
    onForwardMessage: (ForwardRequest) -> Unit = {},
    /** UX section 8 — Forward was chosen on a selection: open the same
     *  picker with the requests, oldest first. */
    onForwardSelection: (List<ForwardRequest>) -> Unit = {},
    /** UX 5.1 "Tap to jump" — open another conversation this device holds,
     *  after the message to light in it has been published to
     *  [PendingMessageJump]. */
    onOpenConversation: (groupIdHex: String) -> Unit = {},
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    // docs/ux/media-messages.md section 11 — what is attached to the draft.
    val tray by viewModel.tray.collectAsStateWithLifecycle()
    // Saving an attachment asks the system where to put the file.
    val fileSaver = me.lakko.app.ui.components.rememberFileSaver()
    val listState = rememberLazyListState()
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    // ADR-0045 UX §6.2 — has this device's own membership ended, and (if so)
    // did it ask to go? Every rule and every string lives in
    // `me.lakko.app.chat.MembershipEnded`; this screen renders its verdicts.
    // The state is TERMINAL, so nothing here needs invalidating on a timer.
    val membership by viewModel.membership.collectAsStateWithLifecycle()
    val membershipEnded = me.lakko.app.chat.membershipEnded(membership)
    // ADR-0091 phase 2 / `docs/ux/conversation-rejoin.md` 2.2 — this device
    // missed an update to this chat and waits to be brought back: the
    // composer, the call buttons and every control that would change the chat
    // are absent (not disabled); earlier messages stay readable.
    val reentryGaps by viewModel.reentryGaps.collectAsStateWithLifecycle()

    // ADR-0087 section 9 — this account is out of the group: the dialog when
    // the person is told, straight back to Chats when they asked for it.
    val endDialog by viewModel.endDialog.collectAsStateWithLifecycle()
    val conversationGone by viewModel.conversationGone.collectAsStateWithLifecycle()
    LaunchedEffect(conversationGone) { if (conversationGone) onNavigateBack() }
    // The wipe behind the dialog deletes what a title is made from. The screen
    // keeps the title it had while the conversation was whole.
    val ending = endDialog != null || viewModel.conversationEnding()
    var heldGroupTitle by remember(viewModel) { mutableStateOf<String?>(null) }
    val shownGroupTitle = me.lakko.app.chat.titleBehindEnd(groupTitle, heldGroupTitle, ending)
    SideEffect {
        if (me.lakko.app.chat.holdsTitle(state.threadKnown, ending)) heldGroupTitle = groupTitle
    }
    endDialog?.let { dialog ->
        MembershipEndDialog(
            dialog = dialog,
            shownTitle = shownGroupTitle,
            onOk = viewModel::onEndNoticeAcknowledged,
        )
    }

    // 1.19c4 — preview-attachment sheet state. When the user
    // taps "Open" on an image chip we resolve the bytes off
    // the main thread + populate this, which surfaces an
    // dialog with the decoded bitmap.
    var previewImage by remember { mutableStateOf<ImageBitmap?>(null) }

    // ADR-0023 — the message being edited (the small pre-filled
    // dialog), or `null` when no edit is in progress.
    var editTarget by remember { mutableStateOf<ChatMessage?>(null) }
    // ADR-0052 / UX 3.1 — the armed reply (the strip above the composer).
    // Held by the view model so a rotation keeps it; cleared when this screen
    // leaves, because "a reply is not a draft that follows the person".
    val replyingTo by viewModel.replyingTo.collectAsStateWithLifecycle()
    DisposableEffect(viewModel) {
        onDispose { viewModel.onCancelReply() }
    }
    // UX section 8 — selection mode. The value and every rule live in the
    // view model / `MessageSelection.kt`; this screen swaps the header for
    // the selection bar and the composer for the action bar while it is set.
    // Back leaves the mode before it leaves the chat.
    val selection by viewModel.selection.collectAsStateWithLifecycle()
    // ADR-0065 section 4 — the account's saved messages, for the mark and
    // the menu row's word.
    val savedList by viewModel.saved.collectAsStateWithLifecycle()
    val bulkBusy by viewModel.bulkBusy.collectAsStateWithLifecycle()
    val bulkStatus by viewModel.bulkStatus.collectAsStateWithLifecycle()
    val selecting = selection != null
    var confirmBulkDelete by remember { mutableStateOf(false) }
    BackHandler(enabled = selecting) { viewModel.onCancelSelection() }
    // UX 3.1 "Tap to jump" — the row a tapped quote scrolled to, lit for about
    // a second and a half and then back to normal.
    var highlightedMessageId by remember { mutableStateOf<Long?>(null) }
    LaunchedEffect(highlightedMessageId) {
        if (highlightedMessageId != null) {
            delay(REPLY_HIGHLIGHT_MS)
            highlightedMessageId = null
        }
    }
    // ADR-0023 — our own IDENTITY (signature key) in this chat group, sampled
    // once per composition; the UI marks which reaction chips we contributed
    // (and can toggle off). Never our leaf — see `AuthorIdentity.kt`.
    val ownIdentity = remember(state.groupIdHex) { viewModel.ownChatIdentity() }
    // ADR-0024 — the live roster's other members. The Bubble's "Read" tick
    // needs every other member's watermark `>=` the message's id, and the
    // header counts them. Collected from the view model, which re-reads the
    // roster after every repository emission: keying a `remember` on the
    // read-watermark map missed a roster that grew with no receipt arriving.
    val otherMembers by viewModel.otherMemberIdentities.collectAsStateWithLifecycle()
    // ADR-0048 — resolved names update in place when a profile arrives or a
    // username verifies. The header of a two-member chat names the other
    // person; a group keeps its label, and its incoming bubbles carry author
    // names instead.
    val nameVersion by viewModel.nameVersion.collectAsStateWithLifecycle()
    val peerName = remember(state.groupIdHex, state.messages.size, nameVersion, otherMembers) {
        viewModel.oneToOnePeerName()
    }
    // UX section 3 / ADR-0062 — the kind is read from the group and decides
    // the header shape: a group made through the new-group flow is a group
    // even while it still has two people, and a one-to-one never grows. Keyed
    // on the roster only so a group loaded after first composition is read.
    val conversationKind = remember(state.groupIdHex, otherMembers) { viewModel.conversationKind() }
    val isGroupChat = conversationKind == ConversationKind.Group
    // ADR-0099 section 6 — the block register, for the collapsed group rows,
    // the one-to-one banner and the call tiles.
    val blockRegister by me.lakko.app.moderation.BlockRegister.state.collectAsStateWithLifecycle()
    val blockScope = rememberCoroutineScope()
    val oneToOnePeer = remember(state.groupIdHex, isGroupChat, state.messages.size) {
        if (isGroupChat) null else viewModel.oneToOnePeerPersonKey()
    }
    val blockedPeerKey = oneToOnePeer?.takeIf { blockRegister?.isBlocked(it) == true }
    // ADR-0099 step 4 / UX 3 — whether the other person in a one-to-one is
    // suspended, as the moderation log confirms; asked when the chat opens.
    val peerSuspended by me.lakko.app.ui.suspension.rememberAccountSuspended(oneToOnePeer)
    // ADR-0099 section 4 - while the account is suspended the chat is
    // read-only: no composer, no calls.
    val suspendedReadOnly by me.lakko.app.auth.SuspensionGate.readOnly.collectAsStateWithLifecycle()
    // UX 5.2 — the second header line of a 1x1 chat is the verified @username.
    val peerUsername = remember(state.groupIdHex, nameVersion, otherMembers) {
        viewModel.oneToOnePeerUsername()
    }
    // ADR-0098 / UX section 3 — the other person of this one-to-one deleted
    // their account: history stays, nothing can be sent or placed.
    val peerDeleted = remember(state.groupIdHex, nameVersion, otherMembers) {
        viewModel.oneToOnePeerDeleted()
    }
    LaunchedEffect(state.groupIdHex, otherMembers) { viewModel.noteOneToOnePeerOnScreen() }
    // docs/ux/conversation-info.md section 1 — WHICH info this conversation's
    // header opens, decided by what the conversation is. The rule is in
    // `me.lakko.app.chat` where the host suite runs it; this reads its answer
    // and never re-derives it from a flag.
    val contactsState by contactsRepository.state.collectAsStateWithLifecycle()
    val contactLinks = remember(contactsState.accepted) {
        contactsState.accepted.map {
            me.lakko.app.chat.ContactLink(
                routeKey = me.lakko.app.ui.contacts.contactRouteKey(it.request.fromPubkey),
                groupIdHex = it.groupIdHex,
            )
        }
    }
    val peerRouteKey = remember(state.groupIdHex, otherMembers) {
        viewModel.oneToOnePeerRouteKey()
    }
    val conversationInfo = remember(
        conversationKind, otherMembers, state.groupIdHex, peerRouteKey, contactLinks,
    ) {
        me.lakko.app.chat.conversationInfo(
            kind = conversationKind,
            groupIdHex = state.groupIdHex,
            peerRouteKey = peerRouteKey,
            contacts = contactLinks,
        )
    }
    // UX 5.2 — the timeline's clock: DS `created_at` rendered in the device's
    // 12/24-hour format; date separators relative to now.
    val zone = remember { ZoneId.systemDefault() }
    // ADR-0061: dates and times in the interface language.
    val strings = LocalStrings.current
    val locale = remember(strings.language) { Locale.forLanguageTag(strings.language) }
    val use24h = remember(context) { me.lakko.app.l10n.DeviceFormats.is24Hour(context) }
    // `docs/ux/message-actions.md` section 9 — a deleted message is not drawn,
    // except in its short hold when its deletion was applied in front of the
    // person.
    val heldDeleted by viewModel.heldDeleted.collectAsStateWithLifecycle()
    val heldDeletedIds = heldDeleted.keys
    val timeline = remember(state.messages, zone, use24h, strings, isGroupChat, reentryGaps, heldDeletedIds) {
        // UX second-device.md section 4 — a group draws its membership rows.
        val built = buildTimeline(
            state.messages,
            System.currentTimeMillis(),
            zone,
            use24h,
            locale,
            strings,
            isGroup = isGroupChat,
            heldDeleted = heldDeletedIds,
        )
        // ADR-0091 phase 2 / conversation-rejoin 2.4 — the gap lines.
        withReentryGapLines(built, reentryGaps) { _ -> reentryGapLabel(strings) }
    }
    // ADR-0052 decision 3 / UX 4.1 — the pins. The bar is DERIVED from the two
    // stores and the held messages, never stored: a pin whose message this
    // device does not hold, or holds deleted, is simply not in the list.
    val sharedPins by viewModel.sharedPins.collectAsStateWithLifecycle()
    val personalPins by viewModel.personalPins.collectAsStateWithLifecycle()
    val mayPinSharedRaw by viewModel.mayPinForEveryone.collectAsStateWithLifecycle()
    val mayPinShared = mayPinSharedRaw
    val pinEntries = remember(state.messages, sharedPins, personalPins) {
        me.lakko.app.chat.pinBarEntries(sharedPins, personalPins, state.messages)
    }
    // The bar's position: newest first, advanced by a tap, wrapping past the
    // oldest. A change in WHICH pins exist shows the newest again.
    var pinCursor by remember { mutableStateOf(0) }
    LaunchedEffect(pinEntries.map { it.messageId }) { pinCursor = 0 }
    // The scope sheet for one message (`null` when closed), and the list.
    var pinSheet by remember { mutableStateOf<Pair<Long, me.lakko.app.chat.PinMenu>?>(null) }
    var pinListOpen by remember { mutableStateOf(false) }
    // UX 3.1 — the target is found by its id in the timeline THIS device
    // holds; when it is not there the tap does nothing and nothing is said.
    // The timeline's rows come first in the lazy list, before the pending
    // media, so a timeline index is a list index.
    val jumpToMessage: (Long) -> Unit = { target ->
        // ADR-0073 rule 7 — a quote whose target is 0 has no jump (a held
        // reply that went out in the successor): never matched against a
        // row that has no id yet.
        val index = if (target == me.lakko.app.chat.REPLY_NO_JUMP_TARGET) {
            -1
        } else {
            // Section 9 — a deleted message is not a target, even in its hold.
            timeline.indexOfFirst { it is TimelineItem.Row && it.message.messageId == target && !it.message.deleted }
        }
        if (index >= 0) {
            scope.launch {
                listState.animateScrollToItem(index)
                highlightedMessageId = target
            }
        }
    }
    // UX 5.1 "Tap to jump" — a forward's attribution goes to the original
    // when this device holds the origin conversation AND the message in it.
    // In this conversation it is the reply jump; in another it is a
    // navigation with the message id left in [PendingMessageJump] for the
    // destination to consume below. Otherwise nothing happens and nothing is
    // said (section 5: an explanation would confirm the conversation exists).
    val jumpToForward: (ForwardRef) -> Unit = { forward ->
        val target = viewModel.forwardJumpTarget(forward)
        if (target != null) {
            val (originHex, messageId) = target
            if (originHex == state.groupIdHex) {
                jumpToMessage(messageId)
            } else {
                PendingMessageJump.publish(originHex, messageId)
                onOpenConversation(originHex)
            }
        }
    }
    // The destination half: once this screen's timeline holds the row a jump
    // named, scroll to it and light it. Keyed on the timeline so a screen
    // opened before its thread rehydrated still consumes the jump when the
    // rows arrive; a jump that names a row this timeline never shows is
    // spent the first time the timeline is non-empty, not kept forever.
    LaunchedEffect(timeline) {
        val pending = PendingMessageJump.peek(state.groupIdHex)
        if (pending != null && timeline.isNotEmpty()) {
            PendingMessageJump.take(state.groupIdHex)
            jumpToMessage(pending)
        }
    }
    // What the timeline calls a message's author: "You" for an own message,
    // the other person in a two-member chat, the resolved name otherwise.
    // One rule for the reply quote and the forward attribution, so the two
    // cannot name the same person differently.
    val timelineAuthorLabel: (ChatMessage) -> String = { msg ->
        when {
            msg.direction == Direction.Outgoing -> strings["chat.author.you"]
            !isGroupChat && peerName != null -> peerName
            else -> msg.authorKey?.let(viewModel::nameOf) ?: viewModel.nameOfKeyB64(null)
        }
    }
    // Whether the message a reply quotes is this account's own (sent from
    // this device or another of the account's): its quote reads "You" here.
    val replyTargetIsOwn: (Long) -> Boolean = { target ->
        state.messages.firstOrNull { it.messageId == target }?.direction == Direction.Outgoing
    }
    // The author a reply quote or a forward attribution CARRIES: the same
    // person, but never a placeholder (every receiver would show it forever);
    // with no real name the snapshot carries none.
    val snapshotAuthorLabel: (ChatMessage) -> String? = { msg ->
        me.lakko.app.chat.snapshotAuthorOf(
            message = msg,
            ownName = viewModel.ownSnapshotName(),
            isOneToOne = !isGroupChat,
            authorName = viewModel.snapshotNameOf(msg.authorKey),
            peerName = viewModel.oneToOnePeerSnapshotName(),
        )
    }
    // docs/ux/feed.md section 5 — Reply chosen on a feed item: arm the reply
    // strip once the timeline holds the row. A row this timeline never shows
    // spends the request the first time the timeline is non-empty.
    LaunchedEffect(state.messages) {
        val pending = me.lakko.app.chat.PendingReplyArm.peek(state.groupIdHex) ?: return@LaunchedEffect
        if (state.messages.isEmpty()) return@LaunchedEffect
        me.lakko.app.chat.PendingReplyArm.take(state.groupIdHex)
        state.messages.firstOrNull { it.messageId == pending }?.let { msg ->
            viewModel.onReplyToMessage(msg, snapshotAuthorLabel(msg).orEmpty())
        }
    }
    val maxBubbleWidth: Dp = (LocalConfiguration.current.screenWidthDp * 0.78f).dp

    // 1.19c4 — system-picker launchers. `ACTION_OPEN_DOCUMENT`
    // surfaces both files and photos through the same code path
    // (the user picks via the docs UI). Photo-specific picking
    // could be wired through `PickVisualMedia` separately;
    // 1.19c4 ships a single entry point for simplicity. The
    // launcher contract is `OpenMultipleDocuments` so the user
    // can select several files in one go — each resolves to a
    // separate `onAttachmentPicked` call.
    val pickFilesLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.OpenMultipleDocuments(),
    ) { uris: List<Uri> ->
        // media-messages section 11 / attachments UX section 4 — into the
        // composer tray as file chips, the bytes unchanged.
        viewModel.addToTray(uris, filesOnly = true)
    }
    // UX spec §10 — the gallery pick for photos and videos, next to the
    // document pick above. Both paths attach size + thumb when the MIME type
    // is an allowlisted image or video.
    val pickVisualLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.PickMultipleVisualMedia(),
    ) { uris: List<Uri> ->
        // media-messages section 11 / attachments UX section 3 — into the
        // composer tray; a photo is re-encoded with its metadata dropped.
        viewModel.addToTray(uris, filesOnly = false)
    }
    // ADR-0092 / attachments UX section 6 — the document picker filtered to audio.
    val pickAudioLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.OpenMultipleDocuments(),
    ) { uris: List<Uri> -> viewModel.onPickedAudio(uris) }
    var attachMenuOpen by remember { mutableStateOf(false) }
    var contentScreen by remember { mutableStateOf<ContentScreen?>(null) }

    // The screen's poll runs while the app is started or a call is up
    // ([me.lakko.app.chat.screenPollRuns]); in the background the push wake
    // reads the group instead.
    val pollLifecycle = androidx.lifecycle.compose.LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(Unit) {
        // Claimed for this effect's lifetime, FIRST, so the app-level
        // all-groups poll hands this group to this screen (and stops holding
        // its in-flight slot) before the refresh below runs. Given up while
        // this device's membership has ended and taken back when it resumes
        // ([me.lakko.app.chat.runScreenPollLoop]).
        val pollClaim = me.lakko.app.chat.ScreenPollClaim(
            claim = { viewModel.claimScreenPoll() },
            release = { viewModel.releaseScreenPoll() },
        )
        pollClaim.ensure()
        // ADR-0092 / UX 7.5 — an anonymous vote left unsent goes again at open.
        viewModel.retryFailedBallots()
        try {
            // ADR-0040 (G-35) — re-publish any pending commit that crashed
            // mid-window on a previous run BEFORE the first poll, re-arming the
            // own-echo filter for the re-published commit so the refresh below
            // drops it instead of hitting CannotDecryptOwnMessage. Best-effort —
            // a re-publish failure leaves the entry un-acked for the next open.
            viewModel.sweepPendingCommitsOnOpen()
            // First tick: one immediate fetch.
            viewModel.pollNow()
            // Subsequent ticks: continuous long-poll. Cancels when
            // the composable leaves composition (back navigation,
            // logout), and pauses while the app is in the background with
            // no call up ([me.lakko.app.chat.screenPollRuns]): the running
            // long poll is cancelled then, and starts again on return.
            // Errors back off via [POLL_RETRY_BACKOFF_MS] so we don't
            // tight-loop against a 503.
            kotlinx.coroutines.flow.combine(
                pollLifecycle.currentStateFlow,
                viewModel.callOccupant,
            ) { lifecycleState, call ->
                me.lakko.app.chat.screenPollRuns(
                    appStarted = lifecycleState.isAtLeast(androidx.lifecycle.Lifecycle.State.STARTED),
                    callUp = call != null,
                )
            }.distinctUntilChanged().collectLatest { runs ->
                if (!runs) return@collectLatest
                // ADR-0045 UX §6.2.8 item 1 — no poll for a group this device
                // is no longer in (the repository refuses it at once, so a loop
                // around it would spin); the loop waits for the membership to
                // resume instead of ending, since a person can be added back
                // while this screen is open. A poll that finds another poll of
                // this group in flight JOINS it rather than returning at once,
                // so calling straight back neither spins nor sleeps blindly;
                // only UNAVAILABLE (no session / no record) and an error back off.
                me.lakko.app.chat.runScreenPollLoop(
                    ended = viewModel.membershipEnded,
                    claim = pollClaim,
                    poll = { viewModel.pollOnceLongPoll() },
                    // docs/ux/connection-state.md section 3: the network
                    // coming back ends the backoff at once.
                    backoff = { me.lakko.app.transport.ConnectionState.delayUnlessNetworkReturns(POLL_RETRY_BACKOFF_MS) },
                )
            }
        } finally {
            pollClaim.giveUp()
        }
    }
    // ADR-0045 S4b — the committer-liveness beacon emitter.
    //
    // `repeatOnLifecycle(RESUMED)` IS the foreground gate: the coroutine is
    // cancelled when the app stops and restarted when it resumes, so a
    // backgrounded device simply does not beacon. That matters because a beacon
    // means *able to sweep*, and a sweep runs only at the tail of the poll this
    // screen drives — the ADR-0041 / C-59 background decrypt is
    // read-only-throwaway and structurally cannot commit, so a beacon from the
    // background would make every other candidate defer to a device that is
    // never going to sweep.
    //
    // The tick is deliberately much shorter than the beacon INTERVAL, which is
    // the core's `shouldEmitBeacon` and is never hand-copied here: the tick only
    // bounds how long after a resume the first beacon waits.
    val lifecycleOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
    // The chat list's unread badge: a chat that is resumed on screen is being
    // read, so its incoming messages do not count and showing it clears them.
    DisposableEffect(lifecycleOwner) {
        val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
            when (event) {
                androidx.lifecycle.Lifecycle.Event.ON_RESUME -> viewModel.setOnScreen(true)
                androidx.lifecycle.Lifecycle.Event.ON_PAUSE -> viewModel.setOnScreen(false)
                else -> Unit
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose {
            lifecycleOwner.lifecycle.removeObserver(observer)
            viewModel.setOnScreen(false)
        }
    }
    // Group privacy spec section 8.1 — a tapped request-to-join notification
    // for this group opens its info at `Join requests`.
    val joinInfoRequested by me.lakko.app.chat.JoinRequestsIntake.infoRequested.collectAsStateWithLifecycle()
    LaunchedEffect(joinInfoRequested) {
        if (joinInfoRequested != null && me.lakko.app.chat.JoinRequestsIntake.takeInfoRequest(viewModel.groupIdHex)) {
            viewModel.openJoinRequests()
        }
    }
    LaunchedEffect(lifecycleOwner) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.RESUMED) {
            while (isActive) {
                // ADR-0045 UX §6.2.8 item 2 — the ticker stops for good once
                // this device's membership has ended, and it stays stopped
                // across a resume because the flag lives in the view model
                // rather than in this coroutine. A beacon means *able to
                // sweep*, and an evicted device can never sweep again — the
                // same rule that keeps a backgrounded device from emitting
                // (which is what `repeatOnLifecycle` is), reached from the
                // other direction: there the device could sweep and is not
                // running; here it is running and never can.
                if (!viewModel.beaconTickerRunning.value) break
                viewModel.emitCommitterBeaconTick(foreground = true)
                delay(me.lakko.app.chat.BEACON_TICK_MS)
            }
        }
    }



    // Phase 3a10 — call surface. Every call (any chat's 1x1 call, a group call,
    // a call-link call) is app-level: the ringing banner and the in-call screens
    // are drawn from `MainActivity`'s root, above every screen, so they survive
    // leaving this chat. This screen only reads the one call slot to hide its
    // call buttons and keep media capture out of the way.
    val callOccupant by viewModel.callOccupant.collectAsStateWithLifecycle()

    // docs/ux/calls.md section 2.1.1 — with ONE exception. A call in a MUTED
    // conversation is not allowed to take the screen over, so the app-level
    // surface draws nothing for it; it is offered here instead, as a panel in
    // this screen's own layout. The verdict is the shared rule's, and it
    // answers for this conversation only: a muted call ringing in some other
    // chat is that chat's panel and this chat's business not at all.
    val oneToOneCall by viewModel.oneToOneCall.collectAsStateWithLifecycle()
    // Hoisted, not written inline behind the `?:`, so the `remember` is reached
    // on every composition: a slot that only exists while the slot is empty
    // changes the composition's shape the moment a call arrives.
    val noOneToOneCall = remember { MutableStateFlow(CallSession.WebCallState.Done) }
    val oneToOneCallState by (oneToOneCall?.session?.state ?: noOneToOneCall)
        .collectAsStateWithLifecycle()
    val quietCallOffer = remember(oneToOneCall, oneToOneCallState, state.groupIdHex) {
        oneToOneCall?.quietOffer(oneToOneCallState)?.takeIf { it.isFor(state.groupIdHex) }
    }

    // Every way into a call asks for the microphone (and, for video, the
    // camera) first; the denial dialogs are mounted by the gate itself.
    val callPermissions = me.lakko.app.ui.call.rememberCallPermissionGate()
    val joinableGroupCall by viewModel.joinableGroupCall.collectAsStateWithLifecycle()
    val callNotice by viewModel.callNotice.collectAsStateWithLifecycle()
    val callNoticeOffersEnd by viewModel.callNoticeOffersEndCurrentCall.collectAsStateWithLifecycle()

    // ADR-0047 — the media runtime for this chat: decrypted files, the one
    // voice player, the played marks. Scoped to this screen, so every
    // decrypted file is deleted when the chat closes.
    val mediaHub = remember { me.lakko.app.media.ChatMediaHub(context, scope) }
    DisposableEffect(mediaHub) {
        onDispose { mediaHub.dispose() }
    }
    mediaHub.voiceQueue = {
        state.messages.map { m ->
            val voiceRef = m.attachments.singleOrNull()?.takeIf {
                me.lakko.app.media.renderDecision(it, mediaHub.allowlist).bubble ==
                    me.lakko.app.media.MediaBubble.VOICE
            }
            me.lakko.app.media.VoiceQueueItem(
                key = voiceRef?.let { me.lakko.app.media.ChatMediaHub.mediaKey(it.blobId) } ?: "msg-${m.messageId}",
                isVoice = voiceRef != null,
                senderKey = m.authorKey,
                played = m.direction != Direction.Incoming,
            )
        }
    }
    // docs/ux/connection-state.md section 7.3 — upload progress of the
    // conversation's media rows in flight (they are rows of the timeline).
    val outboxProgress by viewModel.outboxProgress.collectAsStateWithLifecycle()
    val recorder = rememberRecordController(mediaHub) { take -> viewModel.sendRecordedTake(take) }
    // `docs/ux/media-messages.md` — the recorder stands aside while a call
    // HOLDS the microphone and camera. This used to be `callOccupant != null`
    // alone, which answers a different question: an offer merely being rung
    // takes the slot and takes no microphone, so a voice message was refused at
    // exactly the moment it was the only reply that fitted. The rule that tells
    // the two apart is `crates/lakko-ui-rules/src/capture.rs`; both facts are
    // the reads this screen already has.
    //
    // `callOccupant` is the ONE app-wide slot, not this chat's call, and that is
    // deliberate: `docs/ux/calls.md` section 4.5 collapses a call into a bar, so
    // a person can stand in this chat with a call running from another one — and
    // the microphone is just as busy here.
    val callOwnsMedia = callOwnsCapture(
        occupant = callOccupant,
        call = oneToOneCall,
        state = oneToOneCallState,
        holdsCapture = ::callHoldsCapture,
    )
    SideEffect { recorder.inCall = callOwnsMedia }
    // UX spec §4 — the picker: composer mode (insert, stays open) or reaction
    // mode for one message (react, closes). `null` when closed.
    var picker by remember { mutableStateOf<PickerRequest?>(null) }
    // reactions-and-emoji — what every bubble needs to draw and answer
    // reactions, and the open reactions list (section 8.1).
    var reactorsFor by remember { mutableStateOf<Pair<Long, String?>?>(null) }
    val capNoticeId by viewModel.reactionCapNotice.collectAsStateWithLifecycle()
    // `docs/ux/message-actions.md` section 9 — what was open on a message
    // closes when its deletion is applied: the reaction picker, the reactions
    // list, the pin sheet; an edit of it in progress ends as Cancel would. An
    // armed reply strip stays (it sends with its snapshot).
    LaunchedEffect(state.messages) {
        val deleted = state.messages.asSequence()
            .filter { it.deleted && it.messageId != 0L }
            .map { it.messageId }
            .toHashSet()
        if (deleted.isEmpty()) return@LaunchedEffect
        if (picker?.messageId?.let { it in deleted } == true) picker = null
        if (reactorsFor?.first?.let { it in deleted } == true) reactorsFor = null
        if (pinSheet?.first?.let { it in deleted } == true) pinSheet = null
        if (editTarget?.messageId?.let { it in deleted } == true) editTarget = null
    }
    val quickChosen by me.lakko.app.chat.QuickReaction.chosen.collectAsStateWithLifecycle()
    val settingsVersion by me.lakko.app.chat.GroupSettingsStore.version.collectAsStateWithLifecycle()
    // ADR-0098 — no React in a one-to-one whose other person is deleted.
    val allowedReactions = remember(state.groupIdHex, settingsVersion, peerDeleted) {
        if (peerDeleted) me.lakko.app.chat.AllowedReactions.Off else viewModel.allowedReactions()
    }
    val reactionContext = remember(allowedReactions, quickChosen, capNoticeId, conversationKind, nameVersion) {
        BubbleReactionContext(
            allowed = allowedReactions,
            quick = viewModel.quickReaction(),
            recent = mediaHub.prefs.recentEmoji,
            chipConversation = if (conversationKind == ConversationKind.Group) {
                uniffi.lakko_ffi_uniffi.FfiChipConversation.GROUP
            } else {
                uniffi.lakko_ffi_uniffi.FfiChipConversation.ONE_TO_ONE
            },
            capNoticeId = capNoticeId,
            onQuickReact = { id -> viewModel.onQuickReact(id) },
            onShowReactors = { id, emoji -> reactorsFor = id to emoji },
            nameOf = { key -> viewModel.nameOf(key) },
        )
    }
    reactorsFor?.let { (id, emoji) ->
        val target = state.messages.firstOrNull { it.messageId == id }
        if (target == null || target.reactions.isEmpty()) {
            reactorsFor = null
        } else {
            ReactorsSheet(
                reactions = target.reactions,
                ownIdentity = ownIdentity,
                initialEmoji = emoji,
                nameOf = { key -> viewModel.nameOf(key) },
                onDismiss = { reactorsFor = null },
            )
        }
    }
    var emojiDataset by remember { mutableStateOf<me.lakko.app.media.EmojiDataset?>(null) }
    // reactions-and-emoji section 13 — also read for the `:` strip, so loaded
    // with the screen rather than with the picker.
    LaunchedEffect(Unit) {
        if (emojiDataset == null) {
            emojiDataset = withContext(Dispatchers.IO) { me.lakko.app.media.EmojiAssets.load(context) }
        }
    }
    // A sequence number rides with each insert so two quick taps on the same
    // emoji are two inserts, not one value written twice.
    var pendingEmojiInsert by remember { mutableStateOf<Pair<Long, String>?>(null) }
    var emojiInsertSeq by remember { mutableStateOf(0L) }

    // Whether the timeline has been positioned at its newest row once. The
    // first positioning is a jump, every later one an animation: on a cold
    // start the rows arrive before the list has laid anything out, and
    // `animateScrollToItem` against a list with no visible items returns
    // without moving, which is how a long chat used to open at the top of
    // its history. `scrollToItem` sets the position directly and survives
    // the first layout.
    var positionedOnce by remember { mutableStateOf(false) }
    LaunchedEffect(timeline.size) {
        // Auto-scroll to the latest row on every new message
        // (Outgoing send + Incoming poll both bump the count, and so does an
        // outgoing media bubble entering the timeline).
        val total = timeline.size
        if (total > 0) {
            if (positionedOnce) {
                listState.animateScrollToItem(total - 1)
            } else {
                listState.scrollToItem(total - 1)
                positionedOnce = true
            }
        }
    }

    // `docs/ux/navigation.md` section 2.4 — no title before this device's
    // conversations were read: it would be the fallback word, then the name.
    val headerTitle = when {
        !state.messagesKnown -> ""
        isGroupChat -> shownGroupTitle ?: peerName ?: t("chat.group.fallback_name")
        else -> peerName ?: shownGroupTitle ?: t("chat.group.fallback_name")
    }
    // ADR-0050 — people, not leaves: this device's other phone is not a member.
    val peopleCount by viewModel.peopleCount.collectAsStateWithLifecycle()
    val callControls = me.lakko.app.chat.showsCallControls(isGroupChat, peopleCount)
    // UX 5.2 — "typing..." while a typing indicator is live (ADR-0024; never on
    // a dead group), else "@username" in a 1x1, else "N members" in a group.
    val typingLine = state.typingText?.text()
    // docs/ux/connection-state.md: while this device cannot reach the servers
    // the line says so, ahead of a (stale) typing indicator.
    val connectionLine = me.lakko.app.ui.connectionHeaderText()
    val headerSubtitle = when {
        connectionLine != null -> connectionLine
        !state.messagesKnown -> null
        typingLine != null && !membershipEnded -> typingLine
        isGroupChat -> t("chat.members.count", emptyMap(), mapOf("count" to (peopleCount ?: (otherMembers.size + 1)).toULong()))
        // A contact with no display name already reads "@username" as the
        // title; the line under it would repeat it (seen 2026-11-10).
        peerUsername != null && "@$peerUsername" != headerTitle -> "@$peerUsername"
        else -> null
    }
    // docs/ux/conversation-info.md section 3 — a group's info is a SCREEN
    // pushed over the conversation, not a layer on it: while it (or a screen
    // pushed over it) is up it takes the conversation's place. The
    // conversation's state outlives it — the timeline's scroll position is
    // `listState` above and the draft is the view model's — so its back
    // control returns to the conversation as it was left.
    //
    // Gated on the conversation's KIND, not on the flag alone: a conversation
    // between two people has no group info, and no caller of
    // `toggleMembersPanel` may put one in front of them (section 0).
    val membersPanelOpen by viewModel.membersPanelOpen.collectAsStateWithLifecycle()
    val addMembersOpen by viewModel.addMembersOpen.collectAsStateWithLifecycle()
    val transferStepOpen by viewModel.transferStepOpen.collectAsStateWithLifecycle()
    val infoLayer = me.lakko.app.chat.groupInfoLayer(
        infoOpen = membersPanelOpen,
        isGroup = conversationInfo == ConversationInfo.Group,
        addMembersOpen = addMembersOpen,
        transferOpen = transferStepOpen,
    )
    if (infoLayer == me.lakko.app.chat.GroupInfoLayer.ADD_MEMBERS) {
        AddMembersScreen(
            viewModel = viewModel,
            contactsRepository = contactsRepository,
            peopleSearch = peopleSearch,
            modifier = modifier,
        )
    } else if (infoLayer != me.lakko.app.chat.GroupInfoLayer.CONVERSATION) {
        MembersSheet(
            viewModel = viewModel,
            onChatDeleted = onNavigateBack,
            onOpenContact = onOpenContact,
            contactsRepository = contactsRepository,
            modifier = modifier,
        )
    } else
    Scaffold(
        modifier = modifier.fillMaxSize(),
        // Edge-to-edge: the top bar takes the status bar, the composer sits
        // above the navigation bar and the keyboard.
        contentWindowInsets = WindowInsets.safeDrawing,
        topBar = {
        val liveSelection = selection
        if (liveSelection != null) {
            SelectionBar(
                countLabel = liveSelection.countLabel.text(),
                onCancel = viewModel::onCancelSelection,
            )
        } else {
        me.lakko.app.ui.ConnectionAnnouncer()
        Header(
            title = headerTitle,
            subtitle = headerSubtitle,
            isGroup = isGroupChat,
            avatarSeed = state.groupIdHex,
            // ADR-0084 — a one-to-one's header shows the other person's photo.
            personKey = if (isGroupChat) null else me.lakko.app.ui.components.personKeyBytes(peerRouteKey),
            // ADR-0045 UX §6.2.8 item 4 / §6.2.11 — the call buttons are GONE
            // once this device's membership has ended, not merely failing. The
            // per-call SFrame seed comes from `exportSecret("lakko/call/v1", …)`
            // and now raises, so they fail closed — which is correct, and is not
            // enough: a call control asserts a live E2EE session, and there is
            // not one. Folded into `callActive` because the reasons to hide
            // them are the same rendering decision; a group of one is the third
            // (docs/ux/group-and-channel-privacy.md section 10).
            // ADR-0091 UX 2.6 — the buttons stay while this device is behind;
            // the call hosts fail a start then with the ordinary notice.
            // ADR-0099 section 4 - a suspended account makes no calls.
            callActive = me.lakko.app.chat.callButtonsHidden(
                callOccupied = callOccupant != null,
                membershipEnded = membershipEnded,
                callControls = callControls,
                peerDeleted = peerDeleted,
            ) || suspendedReadOnly,
            onNavigateBack = onNavigateBack,
            // A live group call on this chat is JOINED rather than
            // duplicated; otherwise a group chat (>2 MLS members) starts a
            // multi-party SFU call and a 1x1 chat the P2P CallSession. The
            // buttons are hidden (callActive) while any call occupies the slot.
            onStartCall = {
                callPermissions.request(video = false) { v -> viewModel.onCallButton(video = v) }
            },
            // Video call — publishes the local camera. Group chats
            // SFrame-seal the video through the SFU; 1x1 chats open a
            // P2P video call (the prejoin still lets the user toggle
            // the camera before joining).
            onStartVideoCall = {
                callPermissions.request(video = true) { v -> viewModel.onCallButton(video = v) }
            },
            // Section 1 — the ONE way into this conversation's info. Which
            // one opens, and how, is `conversationInfo`'s and
            // `headerInfoPresentation`'s answer, never this screen's. A
            // one-to-one whose record cannot be resolved has nothing to
            // open, so the tap does nothing.
            onShowInfo = {
                when (val open = me.lakko.app.chat.headerInfoPresentation(conversationInfo)) {
                    me.lakko.app.chat.HeaderInfoPresentation.GroupLayer ->
                        if (!viewModel.membersPanelOpen.value) viewModel.toggleMembersPanel()
                    is me.lakko.app.chat.HeaderInfoPresentation.ContactScreen ->
                        onOpenContact(open.contactKey)
                    null -> Unit
                }
            },
        )
        }
        },
    ) { scaffoldPadding ->
    Box(modifier = Modifier.fillMaxSize().padding(scaffoldPadding)) {
    Column(modifier = Modifier.fillMaxSize()) {

        // ADR-0099 step 4 / UX 3 — a suspended person's one-to-one carries a
        // banner at the top; the chat still works. A blocked person's chat
        // shows the block banner only.
        if (oneToOnePeer != null &&
            me.lakko.app.moderation.suspendedBannerShows(blocked = blockedPeerKey != null, suspended = peerSuspended)
        ) {
            me.lakko.app.ui.suspension.SuspendedPeerBanner(
                name = peerName ?: viewModel.nameOf(oneToOnePeer),
            )
        }

        // docs/ux/calls.md section 2.1.1 — a muted conversation's incoming
        // call, offered rather than thrown. FIRST child of the column, so it
        // sits in the layout above the timeline instead of over it: nothing
        // already on screen is covered, nothing takes focus, and the composer
        // keeps whatever was being typed. It leaves when the ring window runs
        // out, because the host ends the call and clears the slot it reads.
        quietCallOffer?.let { offer ->
            me.lakko.app.ui.call.QuietIncomingCallPanel(
                offer = offer,
                // The resolved peer name — the same value this screen's own
                // header shows for a one-to-one chat, and the same one the
                // full-screen ring resolves for its title, so the two surfaces
                // cannot name one caller two ways (and fall back to the same
                // word when this device has never learned a name).
                callerName = peerName,
                personKey = me.lakko.app.ui.components.personKeyBytes(peerRouteKey),
                // The same two entry points the full-screen ring screen drives.
                onAccept = { withVideo -> viewModel.acceptIncomingCall(withVideo) },
                onDecline = { viewModel.declineIncomingCall() },
            )
        }

        // The ongoing-group-call banner: a live call on this chat that this
        // device is not in. Hidden once this device's membership has ended —
        // the call key can no longer be derived, so Join could only fail.
        val joinable = joinableGroupCall
        // ADR-0099 section 4 — and while the account is suspended: calls are
        // not offered.
        if (joinable != null && !membershipEnded && !suspendedReadOnly) {
            GroupCallBanner(
                call = joinable,
                onJoin = {
                    callPermissions.request(video = false) { v ->
                        viewModel.joinGroupCall(video = v)
                    }
                },
                onJoinWithVideo = {
                    callPermissions.request(video = true) { v ->
                        viewModel.joinGroupCall(video = v)
                    }
                },
            )
        }

        // Group privacy spec section 8.1 — the requests-to-join bar, for the
        // owner and admins, directly under the group call bar.
        val joinRequestBar by viewModel.joinRequestBar.collectAsStateWithLifecycle()
        joinRequestBar?.let { bar ->
            JoinRequestsBanner(
                bar = bar,
                onReview = viewModel::openJoinRequests,
                onShown = viewModel::onJoinRequestsShown,
            )
        }

        // ADR-0052 decision 3 / UX 4.1 — the pinned bar: one pin at a time,
        // shared before personal, newest first. Tap = jump with the reply
        // highlight, then advance (wrapping). No pins, no bar.
        if (pinEntries.isNotEmpty()) {
            val pinIndex = me.lakko.app.chat.clampPinIndex(pinCursor, pinEntries.size)
            val pinEntry = pinEntries[pinIndex]
            val pinnedMessage = state.messages.firstOrNull { it.messageId == pinEntry.messageId }
            if (pinnedMessage != null) {
                PinnedBar(
                    // A personal pin says `For you` in place of the author, so
                    // a message pinned only for me never looks like one the
                    // room can see.
                    author = if (pinEntry.scope == me.lakko.app.chat.PinScope.PERSONAL) {
                        me.lakko.app.chat.PIN_FOR_YOU_LABEL.text()
                    } else {
                        timelineAuthorLabel(pinnedMessage)
                    },
                    excerpt = me.lakko.app.chat.pinExcerpt(pinnedMessage),
                    position = pinIndex,
                    count = pinEntries.size,
                    onTap = {
                        jumpToMessage(pinEntry.messageId)
                        pinCursor = me.lakko.app.chat.advancePinIndex(pinIndex, pinEntries.size)
                    },
                    onOpenList = { pinListOpen = true },
                )
            }
        }

        // ADR-0082 / translation UX 3.6 — `Messages here are in Spanish.`
        val recentIncomingKeys = remember(state.messages) {
            state.messages
                .asReversed()
                .asSequence()
                .filter { it.direction == me.lakko.app.chat.Direction.Incoming && it.text.isNotBlank() && !it.deleted }
                .take(uniffi.lakko_ffi_uniffi.translationBarWindow().toInt())
                .map { me.lakko.app.translation.TranslationStoreState.messageKey(viewModel.groupIdHex, it.messageId) }
                .toList()
        }
        me.lakko.app.ui.translation.ConversationTranslateBar(viewModel.groupIdHex, recentIncomingKeys)
        me.lakko.app.ui.translation.TranslationDialogs(mediaHub)
        if (pinListOpen) {
            PinListSheet(
                entries = pinEntries,
                authorOf = { entry ->
                    if (entry.scope == me.lakko.app.chat.PinScope.PERSONAL) {
                        me.lakko.app.chat.PIN_FOR_YOU_LABEL.render()
                    } else {
                        state.messages.firstOrNull { it.messageId == entry.messageId }
                            ?.let(timelineAuthorLabel).orEmpty()
                    }
                },
                excerptOf = { entry ->
                    state.messages.firstOrNull { it.messageId == entry.messageId }
                        ?.let { me.lakko.app.chat.pinExcerpt(it) }.orEmpty()
                },
                onPick = { entry ->
                    pinListOpen = false
                    jumpToMessage(entry.messageId)
                },
                onDismiss = { pinListOpen = false },
            )
        }
        pinSheet?.let { (messageId, menu) ->
            PinScopeSheet(
                menu = menu,
                oneToOne = !isGroupChat,
                onPick = { action ->
                    pinSheet = null
                    viewModel.onPinAction(messageId, action)
                },
                onDismiss = { pinSheet = null },
            )
        }

        callNotice?.let { notice ->
            LakkoDialog(
                onDismissRequest = viewModel::dismissCallNotice,
                title = t("chat.call.voice"),
                body = notice.text(),
                confirm = if (callNoticeOffersEnd) {
                    LakkoDialogAction(t("call.link.end_current"), onClick = viewModel::onEndCurrentCall)
                } else {
                    LakkoDialogAction(t("common.ok"), onClick = viewModel::dismissCallNotice)
                },
                cancel = if (callNoticeOffersEnd) {
                    LakkoDialogAction(t("common.cancel"), LakkoDialogTone.Ghost, onClick = viewModel::dismissCallNotice)
                } else {
                    null
                },
            )
        }

        // No call is drawn here: the 1x1, group and call-link in-call screens
        // are mounted app-wide by `MainActivity`, so leaving this chat never
        // hides or ends a call and returning to it shows the live call.

        if (me.lakko.app.chat.showsSettingUpLine(state.threadKnown, ending, state.messagesKnown)) {
            // The chat list opened this screen for a group that
            // the repository doesn't know about yet (post-restart
            // before snapshot rehydrate built the handle, or a
            // bug in the route argument). Surface the missing
            // state without crashing — the composer below still
            // works for the user to type but sendMessage will
            // surface a clear error.
            ThreadMissingHint()
        }

        // Message list takes the available height; composer
        // pinned to the bottom.
        Box(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .padding(horizontal = 12.dp),
        ) {
            if (showsEmptyTimeline(timeline, state.messages.isEmpty(), true, state.messagesKnown)) {
                // ADR-0045 UX §6.2 — the empty-timeline copy invites a send,
                // and once the membership has ended that is an invitation the
                // bar below can no longer accept. The terminal row carries the
                // whole of what there is to say.
                if (!membershipEnded) {
                    if (isGroupChat) {
                        SoloGroupBlock(
                            viewModel = viewModel,
                            onAddMembers = viewModel::openAddMembersFromConversation,
                            alone = otherMembers.isEmpty(),
                        )
                    }
                    if (!isGroupChat || otherMembers.isNotEmpty()) EmptyTimelineHint()
                }
            } else {
                val contentBubbleContext = rememberContentBubbleContext(
                    viewModel = viewModel,
                    contactsRepository = contactsRepository,
                    ownKey = ownIdentity,
                    isOneToOne = !isGroupChat,
                    onScreen = { contentScreen = it },
                    onOpenContact = onOpenContact,
                    mediaHub = mediaHub,
                )
                androidx.compose.runtime.CompositionLocalProvider(
                    LocalBubbleReactions provides reactionContext,
                    LocalContentBubble provides contentBubbleContext,
                    LocalReplyWithdrawn provides peerDeleted,
                    LocalQuotedAuthorDeleted provides { target -> nameVersion.let { viewModel.messageAuthorDeleted(target) } },
                ) {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(
                        vertical = 8.dp,
                    ),
                ) {
                    // ADR-0087 section 10 — a group of one opens with its
                    // creation row and the two ways to bring people in.
                    if (isGroupChat && !membershipEnded) {
                        item(key = "solo-group") {
                            SoloGroupBlock(
                                viewModel = viewModel,
                                onAddMembers = viewModel::openAddMembersFromConversation,
                                alone = otherMembers.isEmpty(),
                            )
                        }
                    }
                    items(items = timeline, key = { it.key }) { item ->
                        if (item is TimelineItem.DateSeparator) {
                            DateSeparatorPill(item.label)
                            return@items
                        }
                        // ADR-0047 / connection-state section 7.3 — an own
                        // voice message, video note or GIF not sent yet is a
                        // row of the timeline in its place, drawn as a pending
                        // bubble (upload ring, waiting, Not sent).
                        val pendingMediaMsg = (item as TimelineItem.Row).message
                        val pendingPreview = pendingMediaMsg.localMedia
                        if (pendingPreview != null && pendingMediaMsg.ack != Ack.SENT) {
                            val pendingRow = pendingMediaRowOf(
                                msg = pendingMediaMsg,
                                preview = pendingPreview,
                                progress = outboxProgress[
                                    me.lakko.app.chat.OutboxQueue.progressKey(viewModel.groupIdHex, pendingMediaMsg.localId),
                                ],
                            )
                            Row(
                                modifier = Modifier.fillMaxWidth(),
                                horizontalArrangement = Arrangement.End,
                            ) {
                                PendingMediaBubble(
                                    row = pendingRow,
                                    onRetry = { viewModel.onRetrySend(pendingMediaMsg.localId) },
                                    onDiscard = { viewModel.deleteUnsent(pendingMediaMsg.localId) },
                                )
                            }
                            return@items
                        }
                        androidx.compose.runtime.CompositionLocalProvider(
                            me.lakko.app.ui.translation.LocalTranslationConversation provides viewModel.groupIdHex,
                        ) {
                        val row = item as TimelineItem.Row
                        val msg = row.message
                        // reactions-and-emoji section 12 — an own message with
                        // reactions on screen is seen.
                        if (msg.direction == Direction.Outgoing && msg.reactions.isNotEmpty()) {
                            LaunchedEffect(msg.messageId, msg.reactions.size) {
                                me.lakko.app.chat.UnseenReactions.seen(state.groupIdHex, msg.messageId)
                            }
                        }
                        val pinMenu = me.lakko.app.chat.pinMenuFor(
                            sharedPinned = sharedPins[msg.messageId]?.pinned == true,
                            personalPinned = msg.messageId in personalPins,
                            mayShared = me.lakko.app.chat.sharedPinOffered(mayPinShared, suspendedReadOnly),
                        )
                        // UX section 8 — while selecting, the whole row is the
                        // toggle and a mark sits at its leading edge; a row that
                        // cannot be acted on shows no mark and answers nothing.
                        val selectable = me.lakko.app.chat.canSelectMessage(msg)
                        val isSelected = selection?.contains(msg.messageId) == true
                        // `docs/ux/message-actions.md` section 9 — a held
                        // deleted row reads "Message deleted", then collapses.
                        DeletedRowCollapse(
                            deleted = msg.deleted,
                            collapsing = heldDeleted[msg.messageId] == me.lakko.app.chat.DeletedRowPhase.COLLAPSE,
                        ) {
                        SelectableTimelineRow(
                            selecting = selecting,
                            selectable = selectable,
                            selected = isSelected,
                            markLabel = me.lakko.app.chat.selectionMarkLabel(
                                messageLabel = selectionRowLabel(msg),
                                selected = isSelected,
                            ).text(),
                            onToggle = { viewModel.onToggleSelection(msg) },
                        ) {
                        MessageRow(
                            msg = msg,
                            // UX 5.2 — a run's first row gets the wide gap, the
                            // name and the avatar; the rest sit tight under it.
                            firstInRun = row.firstInRun,
                            time = row.time,
                            isGroup = isGroupChat,
                            maxBubbleWidth = maxBubbleWidth,
                            mediaHub = mediaHub,
                            onOpenReactionPicker = {
                                picker = PickerRequest(PickerMode.REACTION, msg.messageId)
                            },
                            // docs/ux/calls.md section 6 — tapping a call row
                            // calls back, through the same permission gate and
                            // the same routing as the header buttons.
                            onCallBack = { video ->
                                if (callControls) {
                                    callPermissions.request(video = video) { granted ->
                                        viewModel.onCallButton(video = granted)
                                    }
                                }
                            },
                            ownIdentity = ownIdentity,
                            // ADR-0048 UX §5 — author labels in groups only;
                            // a 1x1 bubble needs no name. `nameVersion` is
                            // read so the label updates in place.
                            authorName = if (isGroupChat && msg.direction == Direction.Incoming) {
                                nameVersion.let { msg.authorKey?.let(viewModel::nameOf) }
                            } else {
                                null
                            },
                            // UX second-device.md section 4 — "VG3 joined",
                            // names resolved now; `nameVersion` is read so the
                            // row updates in place.
                            systemLine = if (isGroupChat) {
                                nameVersion.let {
                                    me.lakko.app.chat.membershipRowText(msg, viewModel::nameOf, strings)
                                }
                            } else {
                                null
                            },
                            // ADR-0045 UX §6.2.5 — the same predicate the
                            // composer is gated on, two `when`s below.
                            // UX section 8 — and never while selecting: the
                            // reaction row and the action card do not open.
                            actionsAvailable = !selecting && me.lakko.app.chat.messageActionsState(
                                membership,
                            ) == me.lakko.app.chat.MessageActions.AVAILABLE,
                            // ADR-0024 — read-state inputs (own
                            // bubbles render "Sent" / "Read").
                            otherMembers = otherMembers,
                            readUpToByMember = state.readUpToByMember,
                            onPreview = { ref ->
                                scope.launch {
                                    // UX 4.1 / 4.2 — the failure is recorded on
                                    // the hub, so the chip says gone or limited.
                                    val bytes = mediaHub.loadBytes(ref).getOrNull() ?: return@launch
                                    val bitmap = withContext(Dispatchers.Default) {
                                        decodeImageBitmap(bytes)
                                    }
                                    if (bitmap != null) previewImage = bitmap
                                }
                            },
                            onSave = { ref ->
                                scope.launch {
                                    // A photo or video already opened in this
                                    // chat is in the media cache; reuse it
                                    // instead of downloading it again.
                                    val bytes = mediaHub.loadBytes(ref).getOrNull() ?: return@launch
                                    // A file leaves the app through the
                                    // system's save picker, never a share
                                    // sheet. The name is the sender's, so it
                                    // is sanitized before it is suggested.
                                    fileSaver.save(
                                        name = AttachmentBridge.safeFileName(
                                            ref.name,
                                            fallback = "attachment-${AttachmentBridge.hex(ref.blobId).take(8)}",
                                        ),
                                        mime = ref.mime,
                                        bytes = bytes,
                                    )
                                }
                            },
                            // ADR-0023 — open the pre-filled edit dialog.
                            // UX 3.1 — and drop an armed reply: one thing at
                            // a time.
                            onEdit = {
                                viewModel.onEditStarted()
                                editTarget = msg
                            },
                            onDelete = {
                                // Section 7.3 — an unsent row is deleted here
                                // only: nothing reached anyone.
                                if (msg.messageId == 0L && msg.localId != 0L && msg.ack != Ack.SENT) {
                                    viewModel.deleteUnsent(msg.localId)
                                } else {
                                    viewModel.onDeleteMessage(msg.messageId)
                                }
                            },
                            // ADR-0052 / UX 3.1 — Reply ends an open edit
                            // (its draft is discarded, as Cancel would) and
                            // arms the strip. The name is what the timeline
                            // calls the author: "You" for an own message, the
                            // resolved name otherwise.
                            onReply = {
                                editTarget = null
                                viewModel.onReplyToMessage(msg, snapshotAuthorLabel(msg).orEmpty())
                            },
                            onJumpToReply = jumpToMessage,
                            replyTargetIsOwn = replyTargetIsOwn,
                            // ADR-0052 decision 2 / UX 5.1 — Forward hands the
                            // row, with the name the timeline shows for its
                            // author, to the picker. The name is decided HERE
                            // because this is where that resolution lives; the
                            // picker never re-derives it.
                            onForward = {
                                onForwardMessage(
                                    ForwardRequest(
                                        sourceGroupIdHex = state.groupIdHex,
                                        message = msg,
                                        authorLabel = snapshotAuthorLabel(msg),
                                        // ADR-0053 section 6 — no origin out of a one-to-one.
                                        sourceIsOneToOne = viewModel.conversationKind() ==
                                            me.lakko.app.chat.ConversationKind.Chat,
                                    ),
                                )
                            },
                            onJumpToForward = jumpToForward,
                            highlighted = msg.messageId != 0L && highlightedMessageId == msg.messageId,
                            // ADR-0052 decision 3 / UX 4.1 — the row's word
                            // and the sheet's choices, decided once here from
                            // the collected stores so a pin arriving from the
                            // poll re-labels the row in place. Section 4.1: a
                            // Pin always opens the sheet, even a one-row one (the
                            // missing row is the explanation); only an Unpin in
                            // one scope acts at once.
                            pinLabel = pinMenu.label.text(),
                            pinUndoes = pinMenu.label.key == "chat.message.unpin",
                            // ADR-0065 / saved section 2.
                            savedMessage = viewModel.isSaved(msg, savedList),
                            onToggleSaved = { viewModel.onToggleSave(msg) },
                            onPin = {
                                val single = pinMenu.actions.singleOrNull()
                                if (single != null && pinMenu.label.key == "chat.message.unpin") {
                                    viewModel.onPinAction(msg.messageId, single)
                                } else {
                                    pinSheet = msg.messageId to pinMenu
                                }
                            },
                            onReact = { emoji ->
                                viewModel.onReactToMessage(msg.messageId, emoji)
                            },
                            onRetrySend = { viewModel.onRetrySend(it) },
                            // UX section 8 — enter selection mode on this row.
                            onSelect = { viewModel.onEnterSelection(msg) },
                            // ADR-0099 — Report on someone else's message: its
                            // author, with this message included.
                            onReport = {
                                msg.authorKey?.let { author ->
                                    me.lakko.app.ui.moderation.ModerationRequests.openReport(
                                        me.lakko.app.ui.moderation.ReportRequest(
                                            me.lakko.app.moderation.messageReportSubject(
                                                authorKeyB64 = author,
                                                authorName = viewModel.nameOf(author),
                                                groupIdHex = msg.historyOf ?: state.groupIdHex,
                                                oneToOne = !isGroupChat,
                                                message = me.lakko.app.moderation.ReportableMessage(
                                                    messageId = if (msg.isHistory) msg.historyMessageId else msg.messageId,
                                                    groupIdHex = msg.historyOf ?: state.groupIdHex,
                                                    evidence = msg.evidence,
                                                ),
                                            ),
                                        ),
                                    )
                                }
                            },
                            // ADR-0099 / UX 1.2 — a blocked person's group
                            // message, collapsed until tapped.
                            collapsedBlocked = msg.direction == Direction.Incoming &&
                                blockRegister?.isBlocked(msg.authorKey) == true &&
                                me.lakko.app.moderation.receivedMessageVerdict(
                                    oneToOne = !isGroupChat,
                                    senderKeyB64 = msg.authorKey,
                                    isOwn = false,
                                    isBlocked = { true },
                                ) == uniffi.lakko_ffi_uniffi.BlockVerdict.COLLAPSE,
                        )
                        }
                        }
                        }
                    }
                }
                }
                // reactions-and-emoji section 12 — the unseen-reactions
                // button: a heart and a count; a tap scrolls to the oldest
                // such message and lights it.
                val unseenVersion by me.lakko.app.chat.UnseenReactions.version.collectAsStateWithLifecycle()
                val unseen = remember(unseenVersion, state.groupIdHex) {
                    me.lakko.app.chat.UnseenReactions.of(state.groupIdHex)
                }
                if (unseen.isNotEmpty()) {
                    val unseenLabel = strings.format(
                        "chat.reaction.unseen_a11y",
                        counts = mapOf("count" to unseen.size.toULong()),
                    )
                    Surface(
                        shape = CircleShape,
                        color = MaterialTheme.colorScheme.surface,
                        contentColor = MaterialTheme.colorScheme.primary,
                        shadowElevation = 4.dp,
                        modifier = Modifier
                            .align(Alignment.BottomEnd)
                            .padding(8.dp)
                            .semantics { contentDescription = unseenLabel }
                            .clickable {
                                val first = unseen.first()
                                me.lakko.app.chat.UnseenReactions.seen(state.groupIdHex, first)
                                jumpToMessage(first)
                            },
                    ) {
                        Text(
                            text = "\u2764\uFE0F ${unseen.size}",
                            style = MaterialTheme.typography.labelLarge,
                            modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                        )
                    }
                }
            }
        }

        // ADR-0045 UX §6.2.5 — the terminal row.
        //
        // It goes in the TIMELINE, not in a banner, and §10 already says why:
        // "only outcomes belong in the timeline" — and this is an outcome, one
        // that happened at a definite point in the transcript. A row marks
        // WHERE the history stops, survives scrollback, needs no dismissal
        // state, and reuses the centred muted treatment system rows already
        // have.
        //
        // It renders after the list rather than inside it because it is not a
        // `ChatMessage`: nothing produced it on the wire (the removing commit
        // returns the same content-less `Commit` as any other), and the state
        // is terminal, so "after everything" is the only place it can be.
        me.lakko.app.chat.terminalRowBehindEnd(membership, ending)?.let { row ->
            TerminalMembershipRow(row)
        }
        // ADR-0024 — the typing indicator lives in the header's second line
        // (UX 5.2), not above the input bar; see `headerSubtitle`.

        // UX section 8 — the action bar in place of the composer while
        // selecting. Forward hands the selection, oldest first, to the picker;
        // Delete asks first, because a bulk delete is irreversible and
        // multiplied.
        val liveSelection = selection
        if (liveSelection != null) {
            // ADR-0092 / UX 11a — a selection holding a poll, a checklist or a
            // card that may not be shared is not forwarded at all (a forward
            // that silently left a message out would not be what was chosen).
            val selectionForwardable = viewModel.selectedMessagesInOrder()
                .all { me.lakko.app.chat.isForwardableMessage(it, cardShareableOf(context)) }
            SelectionActionBar(
                selection = liveSelection,
                busy = bulkBusy,
                status = bulkStatus,
                forwardAllowed = selectionForwardable,
                onForward = onForward@{
                    // A channel invitation card is never forwarded
                    // (maintainer, 2026-11-13), nor a poll, a checklist or a
                    // card that may not be shared (ADR-0092 / UX 11a): Forward
                    // is disabled for such a selection; this is the floor.
                    if (!liveSelection.forwardEnabled || !selectionForwardable) return@onForward
                    val requests = viewModel.selectedMessagesInOrder().map { msg ->
                        ForwardRequest(
                            sourceGroupIdHex = state.groupIdHex,
                            message = msg,
                            authorLabel = snapshotAuthorLabel(msg),
                            // ADR-0053 section 6 — no origin out of a one-to-one.
                            sourceIsOneToOne = viewModel.conversationKind() ==
                                me.lakko.app.chat.ConversationKind.Chat,
                        )
                    }
                    if (requests.isNotEmpty()) onForwardSelection(requests)
                },
                onDelete = { confirmBulkDelete = true },
                onSave = { viewModel.onSaveSelected() },
            )
                } else if (suspendedReadOnly) {
                    // ADR-0099 section 4 - the account is suspended: one muted
                    // line in place of the composer, no send control.
                    me.lakko.app.ui.suspension.SuspendedComposerLine()
                } else if (blockedPeerKey != null && !peerDeleted) {
                    // ADR-0099 / UX 1.2 — a banner replaces the composer in the
                    // one-to-one with a person this account blocked.
                    BlockedBanner(
                        name = viewModel.oneToOnePeerName() ?: viewModel.nameOf(blockedPeerKey),
                        onUnblock = {
                            blockScope.launch {
                                me.lakko.app.moderation.BlockRegister.setBlocked(blockedPeerKey, blocked = false)
                            }
                        },
                    )
                } else when (val slot = me.lakko.app.chat.composerState(membership, state.draft, peerDeleted)) {
            me.lakko.app.chat.ComposerSlot.Composer -> Composer(
                recorder = recorder,
                emojiInsert = pendingEmojiInsert,
                onEmojiInserted = { pendingEmojiInsert = null },
                colonSuggest = { word ->
                    val data = emojiDataset ?: return@Composer emptyList()
                    val found = me.lakko.app.media.searchEmoji(data, word).map { it.emoji }
                    val recent = mediaHub.prefs.recentEmoji.filter { it in found }
                    (recent + found).distinct()
                },
                onOpenPicker = { picker = PickerRequest(PickerMode.COMPOSER, null) },
                draft = state.draft,
                draftRevision = state.draftRevision,
                sending = state.sending,
                sendError = state.sendError,
                tray = tray,
                onDraftChange = viewModel::onDraftChange,
                onSendClicked = viewModel::onSendClicked,
                onFocusChanged = viewModel::onComposerFocusChanged,
                replyStrip = replyingTo,
                replyStripIsOwn = replyingTo?.let { replyTargetIsOwn(it.messageId) } == true,
                onCancelReply = viewModel::onCancelReply,
                onPickMedia = {
                    pickVisualLauncher.launch(
                        androidx.activity.result.PickVisualMediaRequest(
                            ActivityResultContracts.PickVisualMedia.ImageAndVideo,
                        ),
                    )
                },
                onAttachClicked = { attachMenuOpen = true },
                onRemoveTrayItem = viewModel::onRemoveTrayItem,
                onRetryTrayItem = viewModel::onRetryTrayItem,
                trayLocalFile = viewModel::trayLocalFile,
                onPasteMedia = viewModel::onPasteMedia,
            )

            is me.lakko.app.chat.ComposerSlot.Withdrawn -> WithdrawnComposer(slot)
        }
    }
    // ADR-0047 §3 — the round live preview over the chat while a video note
    // records. Draws no pointer handlers of its own outside the circle, so the
    // held record button keeps its gesture.
    VideoNoteOverlay(recorder)
    }
    }

    // UX section 8 — "Delete 12 messages?" with a destructive Delete and Cancel.
    val confirmCount = selection?.count ?: 0
    if (confirmBulkDelete && confirmCount > 0) {
        LakkoDialog(
            onDismissRequest = { confirmBulkDelete = false },
            title = me.lakko.app.chat.confirmDeleteLabel(confirmCount).text(),
            confirm = LakkoDialogAction(t("common.delete"), LakkoDialogTone.Danger) {
                confirmBulkDelete = false
                viewModel.onDeleteSelected()
            },
        )
    } else if (confirmBulkDelete) {
        // The selection emptied under the dialog (another device deleted the
        // rows); there is nothing left to confirm.
        confirmBulkDelete = false
    }

    AttachmentSurfaces(
        viewModel = viewModel,
        contactsRepository = contactsRepository,
        menuOpen = attachMenuOpen,
        onMenuDismiss = { attachMenuOpen = false },
        screen = contentScreen,
        onScreen = { contentScreen = it },
        onPickVisual = {
            pickVisualLauncher.launch(
                androidx.activity.result.PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo),
            )
        },
        onPickFiles = { pickFilesLauncher.launch(arrayOf("*/*")) },
        onPickAudio = { pickAudioLauncher.launch(arrayOf("audio/*")) },
        onOpenContact = onOpenContact,
        callHoldsCamera = callOwnsMedia,
        mediaHub = mediaHub,
    )
    picker?.let { request ->
        EmojiPickerSheet(
            mode = request.mode,
            dataset = emojiDataset,
            prefs = mediaHub.prefs,
            gifModel = if (request.mode == PickerMode.COMPOSER) viewModel.gifPicker else null,
            loadGifPreview = viewModel::gifPreview,
            onEmoji = { emoji ->
                val target = request.messageId
                if (request.mode == PickerMode.REACTION && target != null) {
                    viewModel.onReactToMessage(target, emoji)
                } else {
                    emojiInsertSeq += 1
                    pendingEmojiInsert = emojiInsertSeq to emoji
                }
            },
            onGif = viewModel::sendGif,
            onDismiss = {
                viewModel.gifPicker.close()
                picker = null
            },
            only = if (request.mode == PickerMode.REACTION) {
                (allowedReactions as? me.lakko.app.chat.AllowedReactions.Only)?.emoji
            } else {
                null
            },
            // reactions-and-emoji section 10 — with 3 held, the picker opens
            // with the same rule as the row.
            capOwn = if (request.mode == PickerMode.REACTION) {
                state.messages.firstOrNull { it.messageId == request.messageId }
                    ?.let { me.lakko.app.chat.ownReactionEmoji(it.reactions, ownIdentity).toSet() }
                    ?.takeIf { it.size >= MAX_OWN_REACTIONS }
            } else {
                null
            },
        )
    }

    // Inline image preview — surfaced as a full-screen
    // dialog with the decoded bitmap. Dismiss via tap
    // outside or the Close button.
    val preview = previewImage
    if (preview != null) {
        LakkoDialog(
            onDismissRequest = { previewImage = null },
            title = null,
            confirm = LakkoDialogAction(t("common.close")) { previewImage = null },
            cancel = null,
        ) {
            androidx.compose.foundation.Image(
                bitmap = preview,
                contentDescription = t("chat.attachment.image_alt"),
            )
        }
    }

    // ADR-0023 — edit dialog: a small text field pre-filled with
    // the target's current text + Save / Cancel.
    val editing = editTarget
    if (editing != null) {
        EditMessageDialog(
            initialText = editing.text,
            onCancel = { editTarget = null },
            onSave = { newText ->
                viewModel.onEditMessage(editing.messageId, newText)
                editTarget = null
            },
        )
    }
}

// =============================================================================
// Header
// =============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun Header(
    /** ADR-0048 UX §5 — the other person's resolved name in a 1x1 chat, the
     *  group's title otherwise. Never an id. */
    title: String,
    /** "@username" / "N members" / "typing..." — `null` for nothing. */
    subtitle: String?,
    isGroup: Boolean,
    avatarSeed: String,
    personKey: ByteArray? = null,
    callActive: Boolean,
    onNavigateBack: () -> Unit,
    onStartCall: () -> Unit,
    onStartVideoCall: () -> Unit,
    /** docs/ux/conversation-info.md section 1 — open this conversation's info.
     *  The only way in: the overflow menu's duplicate entry is gone, because
     *  two routes to one screen means two things to keep in step and the one
     *  in the menu is the one that drifts. */
    onShowInfo: () -> Unit,
) {
    TopAppBar(
        navigationIcon = {
            IconButton(onClick = onNavigateBack) {
                LkIcon(R.drawable.ic_lk_chevron_back, contentDescription = t("chat.header.back"))
            }
        },
        title = {
            // The name and the avatar together are the entry point: tapping
            // them opens info about this conversation, and which info that is
            // depends on what the conversation is.
            // `docs/ux/navigation.md` section 2.4 — an empty title is a
            // conversation not read yet: nothing of it is drawn.
            if (title.isNotEmpty()) Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.clickable(onClick = onShowInfo),
            ) {
                LakkoAvatar(
                    name = title,
                    seed = avatarSeed,
                    size = 32.dp,
                    group = isGroup,
                    personKey = personKey,
                    groupIdHex = avatarSeed.takeIf { isGroup },
                    // UX profile-screen section 7: the chat header plays a
                    // profile video always on a touch device.
                    playVideo = true,
                )
                Spacer(Modifier.width(10.dp))
                Column {
                    Text(
                        text = title,
                        style = MaterialTheme.typography.titleMedium,
                        maxLines = 1,
                        overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                    )
                    if (subtitle != null) {
                        Text(
                            text = subtitle,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                        )
                    }
                }
            }
        },
        actions = {
            // Phase 3a10 — call buttons. Hidden when a call is already active
            // (the overlay carries the Accept / Decline / Hang up affordances).
            if (!callActive) {
                IconButton(onClick = onStartCall) {
                    LkIcon(R.drawable.ic_lk_phone, contentDescription = t("chat.call.voice"))
                }
                IconButton(onClick = onStartVideoCall) {
                    LkIcon(R.drawable.ic_lk_video, contentDescription = t("chat.call.video"))
                }
            }
            // The overflow menu held one item, `Members` / `Details`, and it
            // was a second route to what the title above already opens. It is
            // gone rather than repointed. Nothing else lived in it; when
            // search, mute or wallpaper arrive, they come back here and the
            // button with them.
        },
    )
}

// =============================================================================
// Ongoing group call banner
// =============================================================================

@Composable
private fun GroupCallBanner(
    call: JoinableGroupCall,
    onJoin: () -> Unit,
    onJoinWithVideo: () -> Unit,
) {
    val joinedLine = when (call.joinedCount) {
        0 -> t("chat.call.group_starting")
        else -> t("chat.call.group_people", emptyMap(), mapOf("count" to call.joinedCount.toULong()))
    }
    Surface(
        color = MaterialTheme.colorScheme.secondaryContainer,
        modifier = Modifier
            .fillMaxWidth()
            .semantics { liveRegion = LiveRegionMode.Polite },
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = t("chat.call.group_in_progress"),
                    style = MaterialTheme.typography.titleSmall,
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                )
                Text(
                    text = joinedLine,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                )
            }
            TextButton(onClick = onJoin) { Text(t("chat.call.join_short")) }
            TextButton(onClick = onJoinWithVideo) { Text(t("chat.call.join_video")) }
        }
    }
    HorizontalDivider()
}

// =============================================================================
// Requests-to-join bar (group privacy spec section 8.1)
// =============================================================================

@Composable
private fun JoinRequestsBanner(
    bar: me.lakko.app.chat.JoinRequestBannerText,
    onReview: () -> Unit,
    /** The bar is drawn in front of the person: its requests are shown. */
    onShown: () -> Unit,
) {
    val lifecycleOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
    LaunchedEffect(bar, lifecycleOwner) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.RESUMED) {
            onShown()
        }
    }
    val text = bar.count?.let { count ->
        t(bar.key, emptyMap(), mapOf("count" to count.toULong()))
    } ?: t(bar.key)
    Surface(
        color = MaterialTheme.colorScheme.secondaryContainer,
        modifier = Modifier
            .fillMaxWidth()
            .semantics { liveRegion = LiveRegionMode.Polite },
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
        ) {
            Text(
                text = text,
                style = MaterialTheme.typography.titleSmall,
                color = MaterialTheme.colorScheme.onSecondaryContainer,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = onReview) { Text(t("chat.join_requests.review")) }
        }
    }
    HorizontalDivider()
}

// =============================================================================
// Timeline rows
// =============================================================================

@Composable
private fun MessageRow(
    msg: ChatMessage,
    /** UX 5.2 — first row of a run: wide gap above, sender name + avatar in groups. */
    firstInRun: Boolean,
    /** UX 5.2 — the corner time ("14:05") from the DS row, or `null` for none. */
    time: String?,
    /** `true` in a group: incoming rows get the avatar gutter. */
    isGroup: Boolean,
    /** About 78 percent of the screen. */
    maxBubbleWidth: Dp,
    /** ADR-0047 — the chat's media runtime, for voice / video-note / GIF bubbles. */
    mediaHub: me.lakko.app.media.ChatMediaHub,
    /** ADR-0047 — the reaction bar's `+`: the picker in reaction mode. */
    onOpenReactionPicker: () -> Unit,
    ownIdentity: me.lakko.app.chat.MemberIdentity?,
    /** ADR-0048 — the resolved author name for an incoming group bubble, else
     *  `null` (no label). */
    authorName: String?,
    /** UX second-device.md section 4 — the drawn line of a membership row
     *  (`VG3 joined`), else `null` and a system row shows its own text. */
    systemLine: String? = null,
    /** ADR-0045 UX §6.2.5 — whether Edit / Delete / React are offered at all.
     *  `false` once this device's membership has ended: all three compose an
     *  application message, so all three are refused by the same core gate
     *  that refuses Send. Threaded down rather than read here so the bubble
     *  stays a pure renderer of what `MembershipEnded.kt` decided. */
    actionsAvailable: Boolean,

    /** ADR-0024 — live roster minus self; read-state inputs. Keyed on the
     *  member's signature key, never the MLS leaf. */
    otherMembers: Set<me.lakko.app.chat.MemberIdentity>,
    readUpToByMember: Map<me.lakko.app.chat.MemberIdentity, Long>,
    onPreview: (AttachmentRef) -> Unit,
    onSave: (AttachmentRef) -> Unit,
    onEdit: () -> Unit,
    onDelete: () -> Unit,
    onReact: (String) -> Unit,
    /** Send this row again, by its client-side id. Only ever reached from a
     *  row whose send failed. */
    onRetrySend: (Long) -> Unit,
    /** `docs/ux/calls.md` section 6 — tapping a finished call's row calls back,
     *  with the same kind of call the row records. */
    onCallBack: (video: Boolean) -> Unit,
    /** ADR-0052 / UX 3.1 — Reply was chosen on this message. */
    onReply: () -> Unit = {},
    /** UX 3.1 — this row's quote was tapped; the argument is the target id. */
    onJumpToReply: (Long) -> Unit = {},
    /** Whether a reply's quoted message is this account's own. */
    replyTargetIsOwn: (Long) -> Boolean = { false },
    /** UX 3.1 — this row is the one a tapped quote just scrolled to. */
    highlighted: Boolean = false,
    /** ADR-0052 decision 2 / UX 5.1 — Forward was chosen on this message. */
    onForward: () -> Unit = {},
    /** UX 5.1 — this row's "Forwarded from" line was tapped. */
    onJumpToForward: (ForwardRef) -> Unit = {},
    /** ADR-0052 decision 3 / UX 4.1 — Pin / Unpin was chosen on this message. */
    onPin: () -> Unit = {},
    /** UX 4.1 — the row's word, `Pin` or `Unpin`, decided by the caller. */
    pinLabel: String = me.lakko.app.l10n.Localization.strings.value["chat.message.pin"],
    /** ADR-0073 R.5 — the row's pin row would UNDO a pin: the one thing a
     *  read-only row of an older segment still offers. */
    pinUndoes: Boolean = false,
    /** UX section 8 — Select was chosen on this message. */
    onSelect: () -> Unit = {},
    /** ADR-0065 / saved section 2 — the message is saved (the mark, and the
     *  menu row reads `Remove from saved`). */
    savedMessage: Boolean = false,
    /** Saved section 2 — Save / Remove from saved was chosen. */
    onToggleSaved: () -> Unit = {},
    /** ADR-0099 — Report was chosen on someone else's message. */
    onReport: () -> Unit = {},
    /** ADR-0099 / UX 1.2 — a group message from someone this account blocked:
     *  drawn as one muted row until tapped. */
    collapsedBlocked: Boolean = false,
) {
    val gapAbove = if (firstInRun) 8.dp else 2.dp
    var revealedBlocked by remember(msg.messageId) { mutableStateOf(false) }
    if (collapsedBlocked && !revealedBlocked) {
        TextAction(
            onClick = { revealedBlocked = true },
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = gapAbove),
        ) {
            Text(
                t("block.group_row"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            )
        }
        return
    }
    // UX 3.1 — "the surface tint used for the pressed state": Material's
    // pressed state layer is `onSurface` at 12 percent, so the lit row reads as
    // a held row rather than a selected one, and it fades back on its own.
    val highlightTint by animateColorAsState(
        targetValue = if (highlighted) {
            MaterialTheme.colorScheme.onSurface.copy(alpha = 0.12f)
        } else {
            Color.Transparent
        },
        label = "reply-jump-highlight",
    )
    when (msg.direction) {
        Direction.Outgoing -> Row(
            modifier = Modifier
                .fillMaxWidth()
                .background(highlightTint)
                .padding(top = gapAbove),
            horizontalArrangement = Arrangement.End,
        ) {
            Bubble(
                msg = msg,
                time = time,
                maxBubbleWidth = maxBubbleWidth,
                mediaHub = mediaHub,
                onOpenReactionPicker = onOpenReactionPicker,
                ownIdentity = ownIdentity,
                actionsAvailable = actionsAvailable,
                isOwn = true,
                onReply = onReply,
                onJumpToReply = onJumpToReply,
                replyTargetIsOwn = replyTargetIsOwn,
                onForward = onForward,
                onJumpToForward = onJumpToForward,
                onPin = onPin,
                pinLabel = pinLabel,
                savedMessage = savedMessage,
                onToggleSaved = onToggleSaved,
                pinUndoes = pinUndoes,
                onSelect = onSelect,
                // ADR-0024 — own bubble: compute the read tick.
                readState = ownReadStateFor(
                    messageId = msg.messageId,
                    own = ownIdentity,
                    others = otherMembers,
                    readUpToByMember = readUpToByMember,
                ),
                // UX section 4 — outgoing on the accent.
                background = MaterialTheme.colorScheme.primary,
                contentColor = MaterialTheme.colorScheme.onPrimary,
                onPreview = onPreview,
                onSave = onSave,
                onEdit = onEdit,
                onDelete = onDelete,
                onReact = onReact,
                onRetrySend = onRetrySend,
            )
        }

        Direction.Incoming -> Row(
            modifier = Modifier
                .fillMaxWidth()
                .background(highlightTint)
                .padding(top = gapAbove),
            verticalAlignment = Alignment.Bottom,
        ) {
            if (isGroup) {
                // UX 5.2 — the avatar once per run, in a fixed gutter so the
                // bubbles of one run line up.
                if (firstInRun) {
                    LakkoAvatar(
                        name = authorName ?: "?",
                        seed = msg.authorKey ?: msg.messageId.toString(),
                        size = 28.dp,
                        personKey = me.lakko.app.ui.components.personKeyBytes(msg.authorKey),
                        modifier = Modifier.padding(bottom = 2.dp),
                    )
                } else {
                    Spacer(Modifier.width(28.dp))
                }
                Spacer(Modifier.width(8.dp))
            }
            Column(horizontalAlignment = Alignment.Start) {
                if (authorName != null && firstInRun) {
                    me.lakko.app.ui.theme.ChatTextScale {
                        Text(
                            text = authorName,
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary,
                            maxLines = 1,
                            modifier = Modifier.padding(start = 4.dp, bottom = 2.dp),
                        )
                    }
                }
                Bubble(
                    msg = msg,
                    time = time,
                    maxBubbleWidth = maxBubbleWidth,
                    mediaHub = mediaHub,
                    onOpenReactionPicker = onOpenReactionPicker,
                    ownIdentity = ownIdentity,
                    actionsAvailable = actionsAvailable,
                    isOwn = false,
                    onReply = onReply,
                    onJumpToReply = onJumpToReply,
                    replyTargetIsOwn = replyTargetIsOwn,
                    onForward = onForward,
                    onJumpToForward = onJumpToForward,
                    onPin = onPin,
                    pinLabel = pinLabel,
                    savedMessage = savedMessage,
                    onToggleSaved = onToggleSaved,
                    pinUndoes = pinUndoes,
                    onSelect = onSelect,
                    onReport = onReport,
                    // Incoming bubbles never show our read state.
                    readState = OwnReadState.None,
                    // UX section 4 — incoming on the sunken surface.
                    background = MaterialTheme.colorScheme.surfaceVariant,
                    contentColor = MaterialTheme.colorScheme.onSurface,
                    onPreview = onPreview,
                    onSave = onSave,
                    onEdit = onEdit,
                    onDelete = onDelete,
                    onReact = onReact,
                    onRetrySend = onRetrySend,
                )
            }
        }

        // System rows stay centred and small (UX 5.2), in a quiet pill. A
        // finished call is a system row too, but a tappable one.
        Direction.System -> {
            val callEntry = msg.call
            if (callEntry != null) {
                CallHistoryRow(entry = callEntry, time = time, onCallBack = onCallBack)
            } else {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 8.dp),
                    horizontalArrangement = Arrangement.Center,
                ) {
                    Surface(
                        shape = RoundedCornerShape(percent = 50),
                        color = MaterialTheme.colorScheme.surfaceVariant,
                        contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                    ) {
                        Text(
                            text = systemLine ?: msg.text,
                            style = MaterialTheme.typography.labelMedium,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
                        )
                    }
                }
            }
        }
    }
}

/**
 * `docs/ux/calls.md` section 6 — what a finished call leaves in the
 * conversation: `Outgoing call, 12:04`, `Incoming call, 12:04`, `Missed call`,
 * `Declined`. Tapping it calls back, with the kind of call it records.
 *
 * Centred like the other system rows so it reads as an event rather than as
 * something somebody said, but on a tappable pill with a leading call icon. A
 * missed or declined call takes the danger colour — the one thing on this row a
 * user scans for — and carries its meaning in words as well, so nothing depends
 * on the colour (section 8).
 */
@Composable
private fun CallHistoryRow(
    entry: me.lakko.app.calls.CallTranscriptEntry,
    time: String?,
    onCallBack: (video: Boolean) -> Unit,
) {
    val missed = me.lakko.app.calls.callHistoryIsUnanswered(entry.outcome)
    val tint = if (missed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary
    val strings = me.lakko.app.l10n.LocalStrings.current
    val spoken = remember(entry, strings) { me.lakko.app.calls.callRowSpokenLabel(entry, strings) }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 8.dp),
        horizontalArrangement = Arrangement.Center,
    ) {
        Surface(
            shape = RoundedCornerShape(percent = 50),
            color = MaterialTheme.colorScheme.surfaceVariant,
            contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier
                .clickable { onCallBack(entry.video) }
                .semantics(mergeDescendants = true) { contentDescription = spoken },
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 6.dp),
            ) {
                LkIcon(
                    icon = if (entry.video) R.drawable.ic_lk_video else R.drawable.ic_lk_phone,
                    contentDescription = null,
                    tint = tint,
                    size = LkIconSize.Mark,
                )
                Text(
                    text = me.lakko.app.calls.callRowText(entry, strings),
                    style = MaterialTheme.typography.labelMedium,
                    color = tint,
                    textAlign = TextAlign.Center,
                )
                if (time != null) {
                    Text(
                        text = time,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

/** ADR-0047 — what the emoji picker was opened for. */
private data class PickerRequest(val mode: PickerMode, val messageId: Long?)

/** ADR-0023 — the per-message action menu's emoji picker set. */
internal val REACTION_EMOJIS: List<String> =
    listOf("👍", "❤️", "😂", "😮", "😢", "🙏")

/** UX 5.2 — the centred day pill between two days of the timeline. */
@Composable
internal fun DateSeparatorPill(label: String) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 12.dp, bottom = 4.dp),
        horizontalArrangement = Arrangement.Center,
    ) {
        Surface(
            shape = RoundedCornerShape(percent = 50),
            color = MaterialTheme.colorScheme.surfaceVariant,
            contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
        ) {
            Text(
                text = label,
                style = MaterialTheme.typography.labelMedium,
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
            )
        }
    }
}

/**
 * How a call invitation's `Join` opens the call
 * (`docs/ux/call-links.md` section 3): handed the link as the one string the
 * app's own `/c/` handling takes, the same a tapped or a pasted link arrives
 * as. `null`, the default, means nothing here can open a call, and an
 * invitation is then drawn as the text it is rather than with a button that
 * leads nowhere.
 */
internal val LocalOpenCallLink = androidx.compose.runtime.staticCompositionLocalOf<((String) -> Unit)?> { null }

/**
 * A call invitation in a conversation (`docs/ux/call-links.md` section 3): a
 * phone glyph, `Call invitation`, and `Join`, in the bubble's own colours.
 *
 * It is drawn for a message that is exactly one call link of this deployment
 * ([me.lakko.app.calls.callInviteOf]); a link to another host, or a link with
 * other text around it, stays ordinary text. `Join` opens the call inside the
 * app. A long press still opens the message's menu ([bubbleControl]).
 */
@Composable
private fun CallInviteCard(
    bubbleColor: Color,
    contentColor: Color,
    onLongPress: (() -> Unit)?,
    onJoin: () -> Unit,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        modifier = Modifier.widthIn(min = 220.dp),
    ) {
        Box(
            modifier = Modifier
                .size(36.dp)
                .background(contentColor.copy(alpha = 0.14f), CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            LkIcon(
                R.drawable.ic_lk_phone_fill,
                contentDescription = null,
                tint = contentColor,
                size = LkIconSize.Row,
            )
        }
        Text(
            text = t("chat.call_invite.title"),
            style = MaterialTheme.typography.bodyMedium,
            fontWeight = androidx.compose.ui.text.font.FontWeight.SemiBold,
            modifier = Modifier.weight(1f),
        )
        Box(
            modifier = Modifier
                .clip(RoundedCornerShape(percent = 50))
                .background(contentColor)
                .bubbleControl(onLongPress = onLongPress, role = Role.Button, onClick = onJoin)
                .padding(horizontal = 16.dp, vertical = 8.dp),
        ) {
            Text(
                text = t("chat.call.join_short"),
                style = MaterialTheme.typography.labelLarge,
                color = bubbleColor,
            )
        }
    }
}

/**
 * `internal` rather than `private` only so a UI test can render one bubble.
 * The long press that opens the action menu starts on this composable's own
 * Surface, so "a long press opens the menu and nothing else does" cannot be
 * asserted anywhere below here.
 *
 * It does not END there: `docs/ux/client-redesign.md` requires the gesture to
 * reach the menu over the bubble's own controls too, and a control that answers
 * a tap consumes the press before this Surface ever sees it. So [openMenu] is
 * threaded down to every control inside the bubble — see
 * [me.lakko.app.ui.chat.bubbleControl] for why that is the fix rather than
 * arranging for the press to pass through.
 */
@Composable
internal fun Bubble(
    msg: ChatMessage,
    /** UX 5.2 — the corner time, or `null` when the row carries no usable time. */
    time: String?,
    maxBubbleWidth: Dp,
    mediaHub: me.lakko.app.media.ChatMediaHub,
    onOpenReactionPicker: () -> Unit,
    /** ADR-0023 — our own signature key, for the own-reaction highlight. */
    ownIdentity: me.lakko.app.chat.MemberIdentity?,
    isOwn: Boolean,
    /** ADR-0045 UX §6.2.5 — whether Edit / Delete / React are offered at all.
     *  `false` once this device's membership has ended: all three compose an
     *  application message, so all three are refused by the same core gate
     *  that refuses Send. Threaded down rather than read here so the bubble
     *  stays a pure renderer of what `MembershipEnded.kt` decided. */
    actionsAvailable: Boolean,

    /** ADR-0024 — read state for an OWN bubble ([OwnReadState.None]
     *  for incoming / un-acked rows). Renders a "Sent" / "Read"
     *  line under the bubble. */
    readState: OwnReadState,
    background: Color,
    contentColor: Color,
    onPreview: (AttachmentRef) -> Unit,
    onSave: (AttachmentRef) -> Unit,
    onEdit: () -> Unit,
    onDelete: () -> Unit,
    onReact: (String) -> Unit,
    /** Send this row again, by its client-side id. Only ever reached from a
     *  row whose send failed. */
    onRetrySend: (Long) -> Unit,
    /** ADR-0052 / UX 3.1 — Reply was chosen on this message. */
    onReply: () -> Unit = {},
    /** UX 3.1 — the quote block was tapped; the argument is the target id. */
    onJumpToReply: (Long) -> Unit = {},
    /** Whether a reply's quoted message is this account's own: then the
     *  quote reads "You" here, whatever name the snapshot carries. */
    replyTargetIsOwn: (Long) -> Boolean = { false },
    /** ADR-0052 decision 2 / UX 5.1 — Forward was chosen on this message. */
    onForward: () -> Unit = {},
    /** UX 5.1 — the "Forwarded from" line was tapped. */
    onJumpToForward: (ForwardRef) -> Unit = {},
    /** ADR-0052 decision 3 / UX 4.1 — Pin / Unpin was chosen on this message. */
    onPin: () -> Unit = {},
    /** UX 4.1 — the row's word, `Pin` or `Unpin`, decided by the caller. */
    pinLabel: String = me.lakko.app.l10n.Localization.strings.value["chat.message.pin"],
    /** ADR-0073 R.5 — the row's pin row would UNDO a pin: the one thing a
     *  read-only row of an older segment still offers. */
    pinUndoes: Boolean = false,
    /** UX section 8 — Select was chosen on this message. */
    onSelect: () -> Unit = {},
    /** ADR-0065 / saved section 2 — the message is saved (the mark, and the
     *  menu row reads `Remove from saved`). */
    savedMessage: Boolean = false,
    /** Saved section 2 — Save / Remove from saved was chosen. */
    onToggleSaved: () -> Unit = {},
    /** ADR-0087 section 12 — a channel post: no Reply, Pin or Select; Edit
     *  and Delete for the owner and publishers whoever wrote it. `null` in a
     *  chat. */
    channelPost: ChannelPostActions? = null,
    /** ADR-0099 — Report was chosen on someone else's message. */
    onReport: (() -> Unit)? = null,
) {
    // ADR-0023 — every affordance is gated on the message having a
    // real DS messageId (0 ⇒ a not-yet-acked outgoing row that no
    // op can target). Edit / Delete are own-message-only; React is
    // offered on any message with an id.
    val hasId = msg.messageId != 0L
    var menuOpen by remember { mutableStateOf(false) }
    // `docs/ux/message-actions.md` §2 — the menu is drawn around the bubble in
    // window coordinates (reaction row above, card below, scrim everywhere
    // else), so the bubble's own rectangle is what the presentation is built
    // from. Null until the bubble has been laid out once.
    var bubbleBounds by remember { mutableStateOf<Rect?>(null) }
    val context = LocalContext.current

    // ADR-0082 — translation of this message, and the transcript of its voice
    // or video message, on this device. A row of an older segment takes none.
    val tr = rememberBubbleTranslation(msg, isOwn)
    // ADR-0087 section 14 — a message carrying a channel invitation card that
    // opens is the card, not its text (the `/g/` link with its secret): the
    // bubble leaves the text out, and Copy and Translate have nothing to take.
    // A link sent as text alone that resolves to a channel's is the same once
    // the card says so.
    val cardReplacesText = remember(msg.channelInvite, msg.deleted) {
        !msg.deleted && channelPost == null &&
            me.lakko.app.ui.channels.ChannelInviteActionsHolder.actions != null &&
            me.lakko.app.ui.channels.channelInviteLine(msg.channelInvite) != null
    }
    var linkCardResolved by remember(msg.messageId, msg.text) { mutableStateOf(false) }
    val textHidden = cardReplacesText || (channelPost == null && linkCardResolved)
    // A bare `/g/` link with no card (`BareGroupLink.kt`): Copy takes the
    // words the bubble shows in its place, never the secret.
    val bareCopyLink = remember(msg.text, msg.channelInvite) {
        if (channelPost == null && msg.channelInvite?.isEmpty() != false) {
            me.lakko.app.ui.channels.bareGroupLinkOf(msg.text, me.lakko.app.config.ApiEndpoints.webOrigin)
        } else {
            null
        }
    }

    // docs/ux/text-size.md — the message's text, its meta line, the quote,
    // the forwarded line, link previews and the reaction chips follow the
    // device's text size; media frames are dp and do not.
    me.lakko.app.ui.theme.ChatTextScale {
    Column(
        horizontalAlignment = if (isOwn) Alignment.End else Alignment.Start,
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        // ADR-0047 §3 — a video note is a circle with no bubble chrome.
        // UX spec §10 — a lone photo or video is the media itself, edge to
        // edge, with the time / ticks pill drawn on it.
        // UX spec §2 — a lone voice message is a ROW, and its own time belongs
        // on that row rather than on a line of its own below it.
        // ADR-0052 — a reply is never "nothing but one piece of media": its
        // quote block needs the bubble's chrome to sit in, so a quoted photo
        // or voice message keeps the ordinary bubble around it.
        // ADR-0052 decision 2 — a forward's attribution line needs the same
        // chrome, for the same reason.
        val soleBubble = remember(msg) {
            if (msg.reply != null || msg.forward != null || msg.content != null) {
                null
            } else {
                me.lakko.app.media.soleMediaBubble(
                    deleted = msg.deleted,
                    text = msg.text,
                    attachments = msg.attachments,
                    allowlist = mediaHub.allowlist,
                )
            }
        }
        // Where this bubble's meta line goes — ONE rule, in the media module,
        // rather than the three conditionals that used to encode it here and
        // could disagree (the voice case fell through all three and got a
        // second row below the bubble).
        // docs/ux/media-messages.md section 11 — the photos and videos of the
        // message as one mosaic at the top of the bubble, then the other
        // attachments, then the text. Laid out by the shared rule.
        val mosaicSplit = remember(msg) {
            if (msg.deleted || msg.content != null || msg.unsupportedVersion != null) {
                null
            } else {
                splitForMosaic(msg.attachments, mediaHub.allowlist, mosaicMaxItems()).takeIf { it.media.isNotEmpty() }
            }
        }
        val mosaicLayout = remember(mosaicSplit) { mosaicSplit?.let { layoutMosaic(it.media) } }
        // Section 11 — the mosaic alone: no text, no other attachment, no
        // quote or forward line; drawn with no bubble behind it.
        val mosaicBare = mosaicSplit != null && msg.text.isEmpty() && mosaicSplit.others.isEmpty() &&
            msg.reply == null && msg.forward == null
        val metaSlot = when {
            // Without text the time rides a pill on the mosaic; with text it
            // sits at the end of the text, drawn by the mosaic branch.
            mosaicSplit != null && msg.text.isEmpty() -> me.lakko.app.media.BubbleMetaSlot.MEDIA_PILL
            mosaicSplit != null -> me.lakko.app.media.BubbleMetaSlot.BUBBLE_ROW
            else -> me.lakko.app.media.bubbleMetaSlot(soleBubble)
        }
        val mediaOnly = metaSlot == me.lakko.app.media.BubbleMetaSlot.MEDIA_PILL
        // reactions-and-emoji section 13 — one to three emoji and nothing
        // else (no attachment, quote or forward line) are drawn large, with
        // no bubble behind them. The count is the core's rule.
        val emojiOnly = remember(msg.text, msg.deleted, msg.attachments.size, msg.reply, msg.forward) {
            if (msg.deleted || msg.attachments.isNotEmpty() || msg.reply != null || msg.forward != null) {
                0
            } else {
                try {
                    uniffi.lakko_ffi_uniffi.emojiOnlyCount(msg.text).toInt()
                } catch (_: Throwable) {
                    0
                }
            }
        }
        val chromeless = soleBubble == me.lakko.app.media.MediaBubble.VIDEO_NOTE ||
            (mediaOnly && (mosaicSplit == null || mosaicBare)) || emojiOnly > 0
        val reactionCtx = LocalBubbleReactions.current
        // ADR-0099 section 4 — a suspended account reacts to nothing (the
        // reaction would be refused), and joins no call from a link.
        val bubbleSuspended by me.lakko.app.auth.SuspensionGate.readOnly.collectAsStateWithLifecycle()
        val contentBubbleCtx = LocalContentBubble.current
        val ownEmoji = remember(msg.reactions, ownIdentity) {
            me.lakko.app.chat.ownReactionEmoji(msg.reactions, ownIdentity).toSet()
        }
        // Section 6 — a double tap sends the quick reaction; nothing on a
        // message not sent, failed, deleted, ended, or while selecting.
        val quickReactable = actionsAvailable && hasId && !msg.deleted && !msg.isHistory &&
            msg.ack == Ack.SENT && msg.unsupportedVersion == null && reactionCtx != null && !bubbleSuspended
        // ADR-0061 section 6 — a newer-version row has no content this build
        // can read, so nothing to copy, edit, quote or forward: no menu.
        // ADR-0073 R.5 — a row of an older segment is read-only history: its
        // card exists only to undo a pin.
        // docs/ux/connection-state.md section 7.3 — an own row that never
        // reached the server (waiting, or "Not sent") has a menu too: Copy
        // and Delete, nothing that needs a server id.
        val unsentOwnRow = !msg.isHistory && msg.localId != 0L && msg.messageId == 0L &&
            (msg.ack == Ack.FAILED || msg.ack == Ack.WAITING)
        val menuAvailable = if (msg.isHistory) {
            actionsAvailable && pinUndoes
        } else if (unsentOwnRow) {
            actionsAvailable
        } else {
            actionsAvailable && hasId && !msg.deleted && msg.unsupportedVersion == null
        }
        val openMenu: (() -> Unit)? = if (menuAvailable) {
            { menuOpen = true }
        } else {
            null
        }
        Box {
            Surface(
                // UX section 4 — radius 18 for bubbles.
                shape = RoundedCornerShape(18.dp),
                color = if (chromeless) Color.Transparent else background,
                contentColor = if (emojiOnly > 0) MaterialTheme.colorScheme.onSurfaceVariant else contentColor,
                modifier = Modifier
                    .widthIn(max = maxBubbleWidth)
                    .onGloballyPositioned {
                        // On SCREEN, not in the window: the menu is a
                        // popup, which is a window of its own, and screen
                        // space is the only one the two share.
                        bubbleBounds = Rect(it.positionOnScreen(), it.size.toSize())
                    }
                    .then(
                        // ADR-0045 UX §6.2.5 — no long press once the
                        // membership has ended: the menu it opens offers
                        // three ops the core now refuses, permanently.
                        if (menuAvailable) {
                            val quickLabel = me.lakko.app.l10n.LocalStrings.current.format(
                                "chat.message.react_with",
                                mapOf("emoji" to (reactionCtx?.quick ?: "")),
                            )
                            Modifier
                                .combinedClickable(
                                    onClick = {},
                                    onLongClick = { menuOpen = true },
                                    onDoubleClick = if (quickReactable) {
                                        { reactionCtx?.onQuickReact?.invoke(msg.messageId) }
                                    } else {
                                        null
                                    },
                                )
                                .semantics {
                                    // Section 6 — TalkBack keeps its own double
                                    // tap; the quick reaction is a named action.
                                    if (quickReactable) {
                                        customActions = listOf(
                                            androidx.compose.ui.semantics.CustomAccessibilityAction(quickLabel) {
                                                reactionCtx?.onQuickReact?.invoke(msg.messageId)
                                                true
                                            },
                                        )
                                    }
                                }
                        } else {
                            Modifier
                        },
                    ),
            ) {
                Column(
                    // The meta line right-aligns itself with `fillMaxWidth`,
                    // which takes the MAXIMUM width the parent allows - so
                    // every bubble was stretched to `maxBubbleWidth` and a
                    // two-word message was as wide as a paragraph. Sizing this
                    // column to its widest child makes that fill resolve to
                    // the content instead, so the bubble hugs what is in it
                    // and the time still sits in the trailing corner.
                    modifier = (if (mediaOnly || mosaicSplit != null) Modifier else Modifier.padding(horizontal = 12.dp, vertical = 8.dp))
                        .width(IntrinsicSize.Max),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    if (msg.deleted) {
                        // ADR-0023 — muted tombstone, no text / attachments.
                        Text(
                            text = t("chat.message.deleted"),
                            style = MaterialTheme.typography.bodyMedium,
                            color = contentColor.copy(alpha = 0.6f),
                            fontStyle = androidx.compose.ui.text.font.FontStyle.Italic,
                        )
                    } else if (msg.unsupportedVersion != null) {
                        // ADR-0061 section 6 — a message from a newer wire
                        // version: the client's own sentence, never core text.
                        Text(
                            text = t(UNSUPPORTED_MESSAGE_KEY),
                            style = MaterialTheme.typography.bodyMedium,
                            color = contentColor.copy(alpha = 0.6f),
                            fontStyle = androidx.compose.ui.text.font.FontStyle.Italic,
                        )
                    } else if (msg.content != null) {
                        // ADR-0092 — a poll, checklist, location, contact card
                        // or article: a card of its own kind. UX 11a — a
                        // forwarded one carries the ordinary forward line.
                        msg.forward?.let { forward ->
                            ForwardLine(
                                forward = forward,
                                contentColor = contentColor,
                                onClick = { onJumpToForward(forward) },
                            )
                        }
                        msg.reply?.let { reply ->
                            ReplyQuote(
                                reply = reply,
                                quotesOwnAccount = replyTargetIsOwn(reply.target),
                                contentColor = contentColor,
                                onClick = { onJumpToReply(reply.target) },
                            )
                        }
                        ContentBubble(msg, contentColor, isOwn)
                    } else if (mosaicSplit != null) {
                        MosaicMessageBody(
                            msg = msg,
                            split = mosaicSplit,
                            mosaic = mosaicLayout,
                            bare = mosaicBare,
                            hub = mediaHub,
                            isOwn = isOwn,
                            time = time,
                            readState = readState,
                            saved = savedMessage,
                            background = background,
                            contentColor = contentColor,
                            shownText = tr.shownText,
                            captionLine = tr.captionLine?.let { line ->
                                {
                                    me.lakko.app.ui.translation.TranslateQuietLine(
                                        line = line,
                                        contentColor = contentColor,
                                        onShowOriginal = tr.onShowOriginal,
                                        onTranslate = tr.onTranslate,
                                        onNotLanguage = tr.onNotLanguage,
                                        onCancel = tr.onCancel,
                                    )
                                }
                            },
                            onSave = onSave,
                            onPreview = onPreview,
                            openMenu = openMenu,
                            onRetry = if (msg.ack == Ack.FAILED || msg.ack == Ack.WAITING) {
                                { onRetrySend(msg.localId) }
                            } else {
                                null
                            },
                            onJumpToForward = onJumpToForward,
                            onJumpToReply = onJumpToReply,
                            replyTargetIsOwn = replyTargetIsOwn,
                        )
                    } else {
                        // ADR-0052 / UX 3.1 — the quote above the message's
                        // own text, in the bubble's own colours, drawn from
                        // the snapshot the envelope carried and nothing else.
                        // ADR-0052 decision 2 / UX 5.1 — one line above the
                        // content: "Forwarded from <name>" or "Forwarded".
                        // The forwarder stays the row's author for everything
                        // else; the name is a label, not a person.
                        val forward = msg.forward
                        if (forward != null) {
                            ForwardLine(
                                forward = forward,
                                contentColor = contentColor,
                                onClick = { onJumpToForward(forward) },
                            )
                        }
                        val reply = msg.reply
                        if (reply != null) {
                            ReplyQuote(
                                reply = reply,
                                quotesOwnAccount = replyTargetIsOwn(reply.target),
                                contentColor = contentColor,
                                onClick = { onJumpToReply(reply.target) },
                            )
                        }
                        // docs/ux/call-links.md section 3 — a message whose
                        // whole text is one call link of this deployment is an
                        // invitation: a card with a Join button, not a line of
                        // characters to copy.
                        val openCallLink = LocalOpenCallLink.current
                        val callInvite = remember(msg.text, openCallLink != null) {
                            if (openCallLink == null) {
                                null
                            } else {
                                me.lakko.app.calls.callInviteOf(
                                    msg.text,
                                    me.lakko.app.calls.CALL_INVITE_DEFAULT_ORIGIN,
                                )
                            }
                        }
                        if (callInvite != null && !bubbleSuspended) {
                            CallInviteCard(
                                bubbleColor = background,
                                contentColor = contentColor,
                                onLongPress = openMenu,
                                onJoin = { openCallLink?.invoke(callInvite.argument) },
                            )
                        } else if (emojiOnly > 0) {
                            Text(
                                text = msg.text.trim(),
                                fontSize = largeEmojiSize(emojiOnly),
                                lineHeight = largeEmojiSize(emojiOnly) * 1.15f,
                            )
                        } else if (msg.text.isNotEmpty()) {
                            // A bare `/g/` link with no card (an older message,
                            // `BareGroupLink.kt`): until it is drawn as its card,
                            // or opened as a group's live link, the words in
                            // its place, never its secret.
                            val bareLink = remember(msg.text, msg.channelInvite) {
                                if (channelPost == null && msg.channelInvite?.isEmpty() != false) {
                                    me.lakko.app.ui.channels.bareGroupLinkOf(
                                        msg.text,
                                        me.lakko.app.config.ApiEndpoints.webOrigin,
                                    )
                                } else {
                                    null
                                }
                            }
                            val bareOpenings = me.lakko.app.ui.channels.BareGroupLinks.openings
                                .collectAsStateWithLifecycle().value
                            LaunchedEffect(bareLink) { bareLink?.let(me.lakko.app.ui.channels.BareGroupLinks::start) }
                            val bareWords = bareLink?.let {
                                me.lakko.app.ui.channels.bareGroupLinkBubbleWords(it, bareOpenings[it])
                            }
                            // ADR-0087 section 14.1 — a bubble whose channel
                            // invitation card is drawn shows the card alone:
                            // the text is the `/g/` link with its secret.
                            if (bareWords != null && me.lakko.app.ui.channels.bubbleDrawsMessageText(
                                    cardDrawn = channelPost == null,
                                    cardOpens = cardReplacesText,
                                    linkCardResolved = linkCardResolved,
                                )
                            ) {
                                Text(text = bareWords, style = MaterialTheme.typography.bodyMedium)
                            } else if (me.lakko.app.ui.channels.bubbleDrawsMessageText(
                                    cardDrawn = channelPost == null,
                                    cardOpens = cardReplacesText,
                                    linkCardResolved = linkCardResolved,
                                )
                            ) {
                                // docs/ux/message-links.md section 1 — the accent on
                                // an incoming bubble; on an outgoing one the accent
                                // is the background, so the bubble's own text colour.
                                me.lakko.app.ui.components.MessageLinkText(
                                    // Translation UX 3.2 — the translation takes the
                                    // place of the text inside the same bubble.
                                    text = tr.shownText,
                                    style = MaterialTheme.typography.bodyMedium,
                                    linkColor = if (isOwn) contentColor else MaterialTheme.colorScheme.primary,
                                    onLongPress = openMenu,
                                )
                            }
                            // ADR-0087 section 14.1 — a channel invitation:
                            // the link drawn as a card with Subscribe / Open.
                            if (channelPost == null) {
                                me.lakko.app.ui.channels.ChannelInviteBubbleCard(
                                    msg.text,
                                    msg.channelInvite,
                                    msg.messageId,
                                    bubbleColor = background,
                                    contentColor = contentColor,
                                    outgoing = isOwn,
                                    onResolved = { drawn -> linkCardResolved = drawn },
                                )
                            }
                            tr.captionLine?.let { line ->
                                me.lakko.app.ui.translation.TranslateQuietLine(
                                    line = line,
                                    contentColor = contentColor,
                                    onShowOriginal = tr.onShowOriginal,
                                    onTranslate = tr.onTranslate,
                                    onNotLanguage = tr.onNotLanguage,
                                    onCancel = tr.onCancel,
                                )
                            }
                        }
                        if (msg.attachments.isNotEmpty()) {
                            androidx.compose.runtime.CompositionLocalProvider(
                                me.lakko.app.ui.translation.LocalTranscriptTarget provides tr.transcriptTarget,
                            ) {
                            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                for (ref in msg.attachments) {
                                    AttachmentView(
                                        ref = ref,
                                        hub = mediaHub,
                                        isOwn = isOwn,
                                        contentColor = contentColor,
                                        bubbleColor = background,
                                        onSave = onSave,
                                        onLongPress = openMenu,
                                        mediaOverlay = if (mediaOnly) {
                                            {
                                                MediaMetaPill(
                                                    edited = msg.edited,
                                                    saved = savedMessage,
                                                    time = time,
                                                    readState = if (isOwn) readState else OwnReadState.None,
                                                    modifier = Modifier
                                                        .align(Alignment.BottomEnd)
                                                        .padding(6.dp),
                                                )
                                            }
                                        } else {
                                            null
                                        },
                                        // UX spec §2 — the message's own time,
                                        // edited marker and delivery ticks ride
                                        // the voice row's quiet line. Only for a
                                        // voice message that is alone in its
                                        // bubble: one with a caption, or beside
                                        // another attachment, keeps the row
                                        // below.
                                        voiceMeta = if (metaSlot == me.lakko.app.media.BubbleMetaSlot.VOICE_LINE) {
                                            {
                                                BubbleMeta(
                                                    edited = msg.edited,
                                                    saved = savedMessage,
                                                    time = time,
                                                    readState = if (isOwn) readState else OwnReadState.None,
                                                    contentColor = contentColor,
                                                    fillWidth = false,
                                                    onRetry = if (msg.ack == Ack.FAILED || msg.ack == Ack.WAITING) {
                                                        { onRetrySend(msg.localId) }
                                                    } else {
                                                        null
                                                    },
                                                    waiting = msg.ack == Ack.WAITING,
                                                )
                                            }
                                        } else {
                                            null
                                        },
                                        // UX spec §3 (2026-11-22) — a lone video
                                        // note carries its time, edited marker and
                                        // ticks on a pill inside the circle; under
                                        // it they read as loose text floating under
                                        // the video (seen on a Pixel).
                                        noteMeta = if (metaSlot == me.lakko.app.media.BubbleMetaSlot.NOTE_PILL) {
                                            {
                                                BubbleMeta(
                                                    edited = msg.edited,
                                                    saved = savedMessage,
                                                    time = time,
                                                    readState = if (isOwn) readState else OwnReadState.None,
                                                    contentColor = Color.White,
                                                    fillWidth = false,
                                                    onRetry = if (msg.ack == Ack.FAILED || msg.ack == Ack.WAITING) {
                                                        { onRetrySend(msg.localId) }
                                                    } else {
                                                        null
                                                    },
                                                    waiting = msg.ack == Ack.WAITING,
                                                )
                                            }
                                        } else {
                                            null
                                        },
                                    ) {
                                        AttachmentChipRow(
                                            ref = ref,
                                            hub = mediaHub,
                                            onPreview = { onPreview(ref) },
                                            onSave = { onSave(ref) },
                                        )
                                    }
                                }
                            }
                            }
                        }
                    }
                    // UX 5.2 — the time in the lower corner, with the edited
                    // marker before it and, on own bubbles, the delivery glyph
                    // after it. A voice row carries it on its own quiet line
                    // and chromeless media below the bubble, so neither adds a
                    // row here.
                    if (metaSlot == me.lakko.app.media.BubbleMetaSlot.BUBBLE_ROW && mosaicSplit == null) {
                        BubbleMeta(
                            edited = msg.edited && !msg.deleted,
                            saved = savedMessage && !msg.deleted,
                            time = time,
                            readState = if (isOwn) readState else OwnReadState.None,
                            contentColor = contentColor,
                            onRetry = if (msg.ack == Ack.FAILED || msg.ack == Ack.WAITING) {
                                { onRetrySend(msg.localId) }
                            } else {
                                null
                            },
                            waiting = msg.ack == Ack.WAITING,
                        )
                    }
                }
            }

            // ADR-0023 + `docs/ux/message-actions.md` §2 — the long-press menu.
            // The reactions are NOT a row in it: they are a row of their own
            // above the bubble, one tap each, because reacting is the commonest
            // thing a person does here and used to cost two taps.
            val bounds = bubbleBounds
            // `docs/ux/message-actions.md` section 9 — an open card on a
            // message closes when its deletion is applied.
            LaunchedEffect(menuAvailable) { if (!menuAvailable) menuOpen = false }
            if (menuOpen && menuAvailable && bounds != null) {
                MessageActionMenuOverlay(
                    bubbleBounds = bounds,
                    alignEnd = isOwn,
                    actions = if (unsentOwnRow) {
                        unsentRowActions(text = if (textHidden || msg.content != null) "" else tr.copyText)
                    } else if (msg.isHistory) {
                        // ADR-0073 R.5 — a row of an older segment is
                        // read-only: `Unpin` alone, when it is pinned; Save,
                        // which sends nothing to the chat, stays.
                        historyRowActions(rowIsPinned = pinUndoes, pinLabel = pinLabel)
                            .copy(canSave = !msg.deleted, saved = savedMessage)
                    } else {
                        messageActionsFor(
                            isOwn = isOwn,
                            deleted = msg.deleted,
                            // Translation UX 3.2 — Copy copies the text shown;
                            // an invitation card shows none.
                            text = if (textHidden) {
                                ""
                            } else {
                                bareCopyLink?.let {
                                    me.lakko.app.ui.channels.bareGroupLinkBubbleWords(
                                        it,
                                        me.lakko.app.ui.channels.BareGroupLinks.openings.value[it],
                                    )
                                } ?: tr.copyText
                            },
                            menuAvailable = menuAvailable,
                            // UX 5.1 — no Forward on a row still sending or
                            // one that failed.
                            sendComplete = msg.ack == Ack.SENT,
                            // UX 4.1 — `Pin` or `Unpin`, decided by the screen.
                            pinLabel = pinLabel,
                            saved = savedMessage,
                            invitationCard = textHidden || me.lakko.app.chat.carriesInvitationCard(msg),
                        ).copy(
                            canTranslate = menuAvailable && tr.menuOffered && !textHidden,
                            translateLabel = tr.menuLabel,
                        ).let { base ->
                            // ADR-0092 — a content message has no text to copy
                            // or edit; its own rows replace those (UX 7.3: no
                            // Edit on a poll).
                            val contentCtx = contentBubbleCtx
                            if (msg.content == null || contentCtx == null) {
                                base
                            } else {
                                base.copy(
                                    canCopy = false,
                                    canEdit = false,
                                    canTranslate = false,
                                    contentRows = contentMenuRows(
                                        msg,
                                        contentCtx,
                                        me.lakko.app.l10n.Localization.strings.value,
                                        System.currentTimeMillis(),
                                    ).map { row -> row.copy(onClick = { menuOpen = false; row.onClick() }) },
                                )
                            }
                        }.let { base ->
                            if (channelPost == null) {
                                // ADR-0099 — Report only where a host can open it.
                                base.copy(canReport = base.canReport && onReport != null)
                            } else {
                                base.copy(
                                    canReply = false,
                                    canPin = false,
                                    canSelect = false,
                                    canForward = menuAvailable && channelPost.canForward && !msg.deleted,
                                    canSave = channelPost.canSave && !msg.deleted,
                                    canEdit = menuAvailable && channelPost.publisher && !msg.deleted,
                                    canDelete = menuAvailable && channelPost.publisher && !msg.deleted,
                                    // ADR-0099 — a post reports the channel.
                                    canReport = base.canReport && onReport != null,
                                )
                            }
                        }.let { base ->
                            // ADR-0092 / UX 11a and 10.1 — no Forward on a poll
                            // or checklist, nor on a card of a person who does
                            // not let others share it.
                            if (!base.canForward || me.lakko.app.chat.isForwardableMessage(msg, cardShareableOf(context))) {
                                base
                            } else {
                                base.copy(canForward = false)
                            }
                        }
                    },
                    reactions = if (msg.isHistory) emptyList() else reactionCtx?.rowFor(msg.reactions) ?: REACTION_EMOJIS,
                    ownReactions = ownEmoji,
                    capReached = ownEmoji.size >= MAX_OWN_REACTIONS,
                    showExpand = reactionCtx?.showExpand ?: true,
                    showRow = !msg.isHistory && !unsentOwnRow && reactionCtx?.allowed != me.lakko.app.chat.AllowedReactions.Off &&
                        !bubbleSuspended && channelPost?.canReact != false,
                    onReact = { emoji ->
                        menuOpen = false
                        onReact(emoji)
                    },
                    // ADR-0047 / UX spec §4 — any emoji, through the picker in
                    // reaction mode (Emoji tab only; a tap reacts).
                    onOpenReactionPicker = {
                        menuOpen = false
                        onOpenReactionPicker()
                    },
                    onReply = {
                        menuOpen = false
                        onReply()
                    },
                    onForward = {
                        menuOpen = false
                        onForward()
                    },
                    onPin = {
                        menuOpen = false
                        onPin()
                    },
                    onSave = {
                        menuOpen = false
                        onToggleSaved()
                    },
                    onSelect = {
                        menuOpen = false
                        onSelect()
                    },
                    onCopy = {
                        menuOpen = false
                        copyMessageText(context, tr.copyText)
                    },
                    onTranslate = {
                        menuOpen = false
                        tr.onMenuTranslate()
                    },
                    onEdit = {
                        menuOpen = false
                        onEdit()
                    },
                    onDelete = {
                        menuOpen = false
                        onDelete()
                    },
                    onReport = {
                        menuOpen = false
                        onReport?.invoke()
                    },
                    onDismiss = { menuOpen = false },
                )
            }
        }

        // ADR-0023 — reaction chips with counts under the bubble.
        // Tapping a chip we contributed toggles it off.
        if (!msg.deleted && msg.reactions.isNotEmpty()) {
            // A chip carries BOTH a fact and an affordance, and §6.2.5
            // separates them: the count is transcript, which §6.2.6 keeps
            // whole, while tapping it is the React op, which dies with Send.
            // So the chips stay and stop being tappable — not greyed, for the
            // same reason the composer is replaced rather than disabled.
            ReactionChips(
                reactions = msg.reactions,
                ownIdentity = ownIdentity,
                onToggle = { emoji -> if (actionsAvailable && hasId && !bubbleSuspended) onReact(emoji) },
                interactive = actionsAvailable && !bubbleSuspended,
                onShowReactors = { emoji -> reactionCtx?.onShowReactors?.invoke(msg.messageId, emoji) },
            )
        }
        // Sections 6, 7 and 10 — the 3-reactions line, about three seconds.
        if (reactionCtx != null && reactionCtx.capNoticeId == msg.messageId) {
            Text(
                text = t("chat.reaction.cap_reached"),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .padding(horizontal = 4.dp)
                    .semantics { liveRegion = LiveRegionMode.Polite },
            )
        }

    }
    }
}

/** ADR-0087 section 12 — what a channel post's menu offers beyond React and
 *  Copy. */
internal data class ChannelPostActions(
    val publisher: Boolean,
    /** UX 12 — the post menu's Forward (re-encrypted into a conversation). */
    val canForward: Boolean = false,
    /** UX 12 — the post menu's Save (the saved register, kind channel). */
    val canSave: Boolean = false,
    /** ADR-0099 step 5 — the menu's reaction row; off for a channel
     *  moderators removed. */
    val canReact: Boolean = true,
)

/** The edit dialog's text field, for the UI tests. */
internal const val EDIT_FIELD_TAG: String = "edit-message-field"

/** UX 3.1 — how long a jumped-to row stays lit: "about a second and a half". */
internal const val REPLY_HIGHLIGHT_MS: Long = 1_500L

/** Where an attachment handed to another app used to be written, in
 *  plaintext, for the share sheet. Nothing writes here any more (a saved file
 *  goes straight to the document the person picks); the name is kept so
 *  forget device still deletes what an older build left behind. */
internal const val SHARED_ATTACHMENTS_DIR: String = "shared-attachments"

/** The strip's close control, by name, for TalkBack and the UI tests. */
internal const val CANCEL_REPLY_LABEL: String = "chat.reply.cancel"

/**
 * ADR-0052 / `docs/ux/message-actions.md` section 3.1 — the strip above the
 * composer: one line, a vertical rule in the accent colour, the quoted
 * author's name, the excerpt, and a close control at the trailing end. Both
 * strings are the snapshot the next send will carry, so what is shown is what
 * the recipient will see. Theme colours only, so it reads in dark mode too.
 */
@Composable
internal fun ReplyStrip(
    reply: ReplyRef,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
    quotesOwnAccount: Boolean = false,
) {
    val (name, excerpt) = replyQuoteLines(reply, quotesOwnAccount = quotesOwnAccount)
    Row(
        modifier = modifier
            .fillMaxWidth()
            .padding(start = 12.dp, end = 4.dp, top = 6.dp)
            .height(IntrinsicSize.Min),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier
                .width(3.dp)
                .fillMaxHeight()
                .clip(RoundedCornerShape(2.dp))
                .background(MaterialTheme.colorScheme.primary),
        )
        Spacer(Modifier.width(10.dp))
        if (name.isNotEmpty()) {
            Text(
                text = name,
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.primary,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(end = 8.dp),
            )
        }
        Text(
            text = excerpt,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        IconButton(onClick = onClose) {
            LkIcon(
                icon = R.drawable.ic_lk_xmark,
                contentDescription = t(CANCEL_REPLY_LABEL),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                size = LkIconSize.Row,
            )
        }
    }
}

/**
 * UX 5.1 — the attribution line inside a forwarded message's bubble, above
 * its content: a small forward arrow and "Forwarded from <name>", or
 * "Forwarded" when no name was carried, in the muted tone of the bubble's own
 * content colour. Plain text — no avatar, no username, no verification mark,
 * no link styling (section 5: a label, not a person).
 *
 * It is a button for accessibility, named after the line itself; tapping it
 * jumps to the original when this device holds it and does nothing otherwise.
 */
@Composable
internal fun ForwardLine(
    forward: ForwardRef,
    contentColor: Color,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val label = forwardLineLabel(forward).text()
    Row(
        modifier = modifier
            .clip(RoundedCornerShape(6.dp))
            .clickable(onClick = onClick)
            .semantics {
                role = Role.Button
                contentDescription = label
            }
            .padding(vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        LkIcon(
            icon = R.drawable.ic_lk_forward,
            contentDescription = null,
            tint = contentColor.copy(alpha = 0.7f),
            size = LkIconSize.Mark,
        )
        Spacer(Modifier.width(4.dp))
        Text(
            text = label,
            style = MaterialTheme.typography.labelMedium,
            color = contentColor.copy(alpha = 0.7f),
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/**
 * UX 3.1 — the quote block inside a reply's bubble, above its own text: the
 * same rule, the name on the first line, the excerpt on the second, both
 * single-line and truncated, in the bubble's own colours. Own and incoming
 * bubbles draw it alike. It is a button named "Reply to <name>"; tapping it
 * jumps to the quoted message when this device holds it.
 *
 * A reply that arrived with no snapshot draws the name line empty and the
 * excerpt "Message" ([replyQuoteLines]); the name is a claim by the replier,
 * so nothing here dresses it as verified.
 */
@Composable
internal fun ReplyQuote(
    reply: ReplyRef,
    contentColor: Color,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    quotesOwnAccount: Boolean = false,
) {
    val strings = LocalStrings.current
    val quotesDeleted = !quotesOwnAccount && LocalQuotedAuthorDeleted.current(reply.target)
    val (name, excerpt) = replyQuoteLines(reply, strings, quotesOwnAccount, quotesDeleted)
    val label = if (name.isEmpty()) strings["chat.reply.quote"] else strings.format("chat.reply.quote_to", mapOf("name" to name))
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(contentColor.copy(alpha = 0.10f))
            .clickable(onClick = onClick)
            .semantics {
                role = Role.Button
                contentDescription = label
            }
            .height(IntrinsicSize.Min),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier
                .width(3.dp)
                .fillMaxHeight()
                .background(contentColor),
        )
        Column(Modifier.padding(horizontal = 8.dp, vertical = 4.dp)) {
            // The line is kept even when empty, so a snapshot-less reply keeps
            // the block's two-line shape rather than collapsing to one word.
            Text(
                text = name,
                style = MaterialTheme.typography.labelMedium,
                color = contentColor,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = excerpt,
                style = MaterialTheme.typography.bodySmall,
                color = contentColor.copy(alpha = 0.8f),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/**
 * UX 5.2 — the bubble's corner line: "(edited)", the time, and for an own
 * message the delivery glyph (ADR-0024): one check = sent, two checks = read
 * (every other member's watermark has reached this id). Colours come from the
 * bubble's content colour so the glyph reads on the accent and on the sunken
 * surface alike; no text.
 */
@Composable
private fun BubbleMeta(
    edited: Boolean,
    time: String?,
    readState: OwnReadState,
    contentColor: Color,
    modifier: Modifier = Modifier,
    /** Saved section 2 — the saved mark beside the time. */
    saved: Boolean = false,
    /** `false` inside the §10 media pill, which wraps its content. */
    fillWidth: Boolean = true,
    /** Send this row again. `null` unless this row's send failed - the action
     *  sits next to the message it belongs to, which is the only place that
     *  can say WHICH message failed. */
    onRetry: (() -> Unit)? = null,
    /** `docs/ux/connection-state.md` section 7.1 — the row waits for the
     *  network: a clock before [time] (which then reads "Waiting for
     *  network"), no Retry word, and a tap on the two sends it now
     *  ([onRetry]). */
    waiting: Boolean = false,
) {
    if (!edited && time == null && readState == OwnReadState.None && onRetry == null && !saved) return
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        modifier = if (fillWidth) modifier.fillMaxWidth() else modifier,
    ) {
        if (fillWidth) Spacer(Modifier.weight(1f))
        if (edited) {
            Text(
                text = t("chat.message.edited"),
                style = MaterialTheme.typography.labelSmall,
                color = contentColor.copy(alpha = 0.7f),
            )
        }
        if (saved) {
            // Saved section 2 — a small mark, only the person sees it.
            LkIcon(
                icon = R.drawable.ic_lk_bookmark_fill,
                contentDescription = t("chat.message.saved_mark"),
                tint = contentColor.copy(alpha = 0.7f),
                size = LkIconSize.Mark,
            )
        }
        if (waiting && time != null) {
            val waitingLine: @Composable () -> Unit = {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(2.dp),
                ) {
                    LkIcon(
                        icon = R.drawable.ic_lk_clock,
                        contentDescription = null,
                        tint = contentColor.copy(alpha = 0.7f),
                        size = LkIconSize.Mark,
                    )
                    Text(
                        text = time,
                        style = MaterialTheme.typography.labelSmall,
                        color = contentColor.copy(alpha = 0.7f),
                    )
                }
            }
            if (onRetry != null) TextAction(onClick = onRetry) { waitingLine() } else waitingLine()
        } else if (time != null) {
            Text(
                text = time,
                style = MaterialTheme.typography.labelSmall,
                color = contentColor.copy(alpha = 0.7f),
            )
        }
        if (onRetry != null && !waiting) {
            val sendAgain = t("common.retry")
            // A 48dp target around the small word, drawn as before.
            TextAction(onClick = onRetry) {
                Text(
                    text = t("common.retry"),
                    style = MaterialTheme.typography.labelSmall,
                    textDecoration = TextDecoration.Underline,
                    color = contentColor,
                    modifier = Modifier.semantics { contentDescription = sendAgain },
                )
            }
        }
        if (readState != OwnReadState.None) {
            val read = readState == OwnReadState.Read
            LkIcon(
                icon = if (read) R.drawable.ic_lk_checkmark_double else R.drawable.ic_lk_checkmark,
                contentDescription = t(if (read) "chat.message.read" else "chat.message.sent"),
                tint = if (read) contentColor else contentColor.copy(alpha = 0.7f),
                size = LkIconSize.Mark,
            )
        }
    }
}

/** UX spec §10 — time, ticks and the edited marker on a photo or video, in
 *  a small translucent pill that reads on any image in either theme. */
@Composable
private fun MediaMetaPill(
    edited: Boolean,
    time: String?,
    readState: OwnReadState,
    modifier: Modifier = Modifier,
    saved: Boolean = false,
    /** `docs/ux/connection-state.md` section 7.1 — the clock and "Waiting for
     *  network" in place of the time; [onWaitingTap] sends it now. */
    waiting: Boolean = false,
    onWaitingTap: (() -> Unit)? = null,
) {
    if (!edited && time == null && readState == OwnReadState.None && !saved) return
    Box(
        modifier = modifier
            .background(Color.Black.copy(alpha = 0.45f), RoundedCornerShape(percent = 50))
            .padding(horizontal = 8.dp, vertical = 2.dp),
    ) {
        BubbleMeta(
            edited = edited,
            time = time,
            readState = readState,
            contentColor = Color.White,
            fillWidth = false,
            saved = saved,
            onRetry = onWaitingTap,
            waiting = waiting,
        )
    }
}

/**
 * docs/ux/media-messages.md section 11 — a message carrying photos or videos:
 * the quote or forward line, the mosaic edge to edge, the other attachments,
 * then the text, all in one bubble. With text the time sits at the end of the
 * text; without it, on a pill over the mosaic.
 */
@Composable
private fun MosaicMessageBody(
    msg: ChatMessage,
    split: MosaicSplit,
    mosaic: uniffi.lakko_ffi_uniffi.MediaMosaic?,
    /** The mosaic alone, with no bubble behind it. */
    bare: Boolean,
    hub: me.lakko.app.media.ChatMediaHub,
    isOwn: Boolean,
    time: String?,
    readState: OwnReadState,
    saved: Boolean,
    background: Color,
    contentColor: Color,
    shownText: String,
    captionLine: (@Composable () -> Unit)?,
    onSave: (AttachmentRef) -> Unit,
    onPreview: (AttachmentRef) -> Unit,
    openMenu: (() -> Unit)?,
    onRetry: (() -> Unit)?,
    onJumpToForward: (ForwardRef) -> Unit,
    onJumpToReply: (Long) -> Unit,
    replyTargetIsOwn: (Long) -> Boolean,
) {
    val hasText = msg.text.isNotEmpty()
    val tail = hasText || split.others.isNotEmpty()
    val mosaicWidth = mosaic?.width?.toInt() ?: MOSAIC_BUBBLE_WIDTH_DP
    // A narrow lone item keeps a readable text column under it.
    val columnWidth = if (tail) maxOf(mosaicWidth, MOSAIC_TEXT_MIN_WIDTH_DP) else mosaicWidth
    val pad = Modifier.padding(horizontal = 12.dp)
    Column(
        modifier = Modifier.width(columnWidth.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        val forward = msg.forward
        val reply = msg.reply
        if (forward != null || reply != null) {
            Column(pad.padding(top = 8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                if (forward != null) {
                    ForwardLine(forward = forward, contentColor = contentColor, onClick = { onJumpToForward(forward) })
                }
                if (reply != null) {
                    ReplyQuote(
                        reply = reply,
                        quotesOwnAccount = replyTargetIsOwn(reply.target),
                        contentColor = contentColor,
                        onClick = { onJumpToReply(reply.target) },
                    )
                }
            }
        }
        MediaMosaicView(
            refs = split.media,
            mosaic = mosaic,
            hub = hub,
            onSave = onSave,
            onLongPress = openMenu,
            squareBottom = tail,
            squareTop = forward != null || reply != null,
            overlay = if (!hasText) {
                {
                    MediaMetaPill(
                        edited = msg.edited,
                        saved = saved,
                        time = time,
                        readState = if (isOwn) readState else OwnReadState.None,
                        modifier = Modifier.align(Alignment.BottomEnd).padding(6.dp),
                        waiting = msg.ack == Ack.WAITING,
                        onWaitingTap = onRetry.takeIf { msg.ack == Ack.WAITING },
                    )
                }
            } else {
                null
            },
        )
        if (split.others.isNotEmpty()) {
            Column(pad.padding(top = 4.dp, bottom = if (hasText) 0.dp else 8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                for (ref in split.others) {
                    AttachmentView(
                        ref = ref,
                        hub = hub,
                        isOwn = isOwn,
                        contentColor = contentColor,
                        bubbleColor = background,
                        onSave = onSave,
                        onLongPress = openMenu,
                    ) {
                        AttachmentChipRow(ref = ref, hub = hub, onPreview = { onPreview(ref) }, onSave = { onSave(ref) })
                    }
                }
            }
        }
        if (hasText) {
            Column(pad.padding(top = 4.dp, bottom = 8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                me.lakko.app.ui.components.MessageLinkText(
                    text = shownText,
                    style = MaterialTheme.typography.bodyMedium,
                    linkColor = if (isOwn) contentColor else MaterialTheme.colorScheme.primary,
                    onLongPress = openMenu,
                )
                captionLine?.invoke()
                BubbleMeta(
                    edited = msg.edited,
                    saved = saved,
                    time = time,
                    readState = if (isOwn) readState else OwnReadState.None,
                    contentColor = contentColor,
                    onRetry = onRetry,
                    waiting = msg.ack == Ack.WAITING,
                )
            }
        } else if (onRetry != null && msg.ack != Ack.WAITING) {
            // A failed send still offers Retry next to the message. A waiting
            // one says so on the pill over the mosaic.
            BubbleMeta(
                edited = false,
                time = null,
                readState = OwnReadState.None,
                contentColor = if (bare) MaterialTheme.colorScheme.onSurfaceVariant else contentColor,
                modifier = pad.padding(bottom = 4.dp),
                onRetry = onRetry,
            )
        }
    }
}

/** A lone narrow photo with text under it keeps at least this text column. */
private const val MOSAIC_TEXT_MIN_WIDTH_DP: Int = 200

/** reactions-and-emoji sections 5-10 — what a bubble needs to draw and
 *  answer reactions, provided once by the chat screen. */
internal class BubbleReactionContext(
    /** What the conversation takes (section 9). */
    val allowed: me.lakko.app.chat.AllowedReactions,
    /** The account's quick reaction (section 6). */
    val quick: String,
    /** The picker's Recent list, newest first (section 5). */
    val recent: List<String>,
    /** One-to-one, group (avatars) or channel (counts). */
    val chipConversation: uniffi.lakko_ffi_uniffi.FfiChipConversation,
    /** The message whose 3-reactions line is showing, or `null`. */
    val capNoticeId: Long?,
    val onQuickReact: (Long) -> Unit,
    /** Open the reactions list on a message, on a tab (`null` = All). */
    val onShowReactors: (Long, String?) -> Unit,
    /** A person's display name, by account key (base64). */
    val nameOf: (me.lakko.app.chat.MemberIdentity) -> String,
) {
    /** Section 5 — whether the row ends with the expand control. */
    val showExpand: Boolean = try {
        uniffi.lakko_ffi_uniffi.rowHasExpand(allowed.toFfiAllowed())
    } catch (_: Throwable) {
        true
    }

    /** Section 5 — the six of the row for a message holding [held]. */
    fun rowFor(held: List<me.lakko.app.chat.Reaction>): List<String> = try {
        uniffi.lakko_ffi_uniffi.quickReactionRow(
            quick,
            recent,
            allowed.toFfiAllowed(),
            me.lakko.app.chat.reactionKinds(held),
        )
    } catch (_: Throwable) {
        REACTION_EMOJIS
    }
}

/** Provided by [ChatScreen]; `null` outside a chat (no reactions offered). */
internal val LocalBubbleReactions = androidx.compose.runtime.staticCompositionLocalOf<BubbleReactionContext?> { null }

/** Section 10 — reactions one person gives one message. */
internal const val MAX_OWN_REACTIONS: Int = 3

/** Section 13 — 56 / 44 / 36 for one / two / three emoji (chosen). */
internal fun largeEmojiSize(count: Int): androidx.compose.ui.unit.TextUnit = when (count) {
    1 -> 56.sp
    2 -> 44.sp
    else -> 36.sp
}

/** reactions-and-emoji section 7 — the chips under a message, decided by the
 *  core (`reactionChips`: order, face, the 11-kind cap and `+N`). Own chip:
 *  accent-soft fill and a 1 dp accent border. Tap adds or takes off; a long
 *  press opens the reactions list. */
@Composable
private fun ReactionChips(
    reactions: List<me.lakko.app.chat.Reaction>,
    ownIdentity: me.lakko.app.chat.MemberIdentity?,
    onToggle: (String) -> Unit,
    /** ADR-0045 UX §6.2.5 — `false` once the membership has ended: the chip
     *  still renders its count and stops being a control. */
    interactive: Boolean = true,
    onShowReactors: (String?) -> Unit = {},
) {
    val ctx = LocalBubbleReactions.current
    val strings = LocalStrings.current
    // The own reactor's circle: the person's own photo, else the initials of
    // their own name (a contact directory never holds the own profile).
    val ownAvatar = me.lakko.app.ui.components.rememberOwnAvatar()
    val ownAvatarName = me.lakko.app.ui.components.rememberOwnAvatarName(
        ownIdentity?.let { ctx?.nameOf?.invoke(it) } ?: "?",
    )
    val row = remember(reactions, ownIdentity, ctx?.allowed, ctx?.chipConversation) {
        chipRowOf(reactions, ownIdentity, ctx)
    }
    // docs/ux/text-size.md — every chip value scales together; the `sp`
    // values already do through the scaled font size.
    val f = me.lakko.app.ui.theme.LocalChatTextFactor.current
    val chipHeight = REACTION_CHIP_HEIGHT * f
    val chipPadding = REACTION_CHIP_PADDING * f
    val innerGap = REACTION_CHIP_INNER_GAP * f
    val spacing = REACTION_CHIP_SPACING * f
    val avatarSize = REACTION_AVATAR_SIZE * f
    val overlap = REACTION_AVATAR_OVERLAP * f
    // design-system.md "Reaction chip": 6 between chips and between lines.
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(spacing),
        verticalArrangement = Arrangement.spacedBy(spacing),
    ) {
        for (chip in row.chips) {
            val label = if (chip.mine) {
                strings.format("chat.reaction.chip_yours_a11y", mapOf("emoji" to chip.emoji))
            } else {
                strings.format("chat.reaction.chip_a11y", mapOf("emoji" to chip.emoji), mapOf("count" to chip.count.toULong()))
            }
            Surface(
                shape = RoundedCornerShape(percent = 50),
                color = if (chip.mine) {
                    MaterialTheme.colorScheme.primaryContainer
                } else {
                    MaterialTheme.colorScheme.surfaceVariant
                },
                border = if (chip.mine) {
                    androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.primary)
                } else {
                    null
                },
                contentColor = MaterialTheme.colorScheme.onSurface,
                modifier = Modifier
                    .height(chipHeight)
                    .semantics { contentDescription = label }
                    .then(
                        if (interactive) {
                            Modifier.combinedClickable(
                                onClick = { onToggle(chip.emoji) },
                                onLongClickLabel = strings["chat.reaction.show_who_a11y"],
                                onLongClick = { onShowReactors(chip.emoji) },
                            )
                        } else {
                            Modifier
                        },
                    ),
            ) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(innerGap),
                    modifier = Modifier.fillMaxHeight().padding(horizontal = chipPadding),
                ) {
                    Text(text = chip.emoji, style = reactionChipEmojiStyle())
                    when (val face = chip.face) {
                        is uniffi.lakko_ffi_uniffi.FfiChipFace.Count ->
                            Text(
                                text = face.count.toString(),
                                style = reactionChipCountStyle(),
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        // Overlapping by 4: a negative spacing, so the row's
                        // width is what is drawn (an offset would leave the
                        // overlap as empty space at the end of the chip).
                        is uniffi.lakko_ffi_uniffi.FfiChipFace.Avatars -> Row(
                            horizontalArrangement = Arrangement.spacedBy(-overlap),
                        ) {
                            face.reactors.take(REACTION_CHIP_MAX_AVATARS).forEach { key ->
                                val b64 = java.util.Base64.getEncoder().encodeToString(key)
                                val mine = b64 == ownIdentity
                                LakkoAvatar(
                                    name = if (mine) ownAvatarName else ctx?.nameOf?.invoke(b64) ?: "?",
                                    seed = b64,
                                    size = avatarSize,
                                    personKey = key,
                                    avatar = if (mine) ownAvatar else null,
                                    // A 1 dp ring in the surface colour, as on
                                    // iOS: the circle stays visible on the own
                                    // chip's accent fill and where avatars overlap.
                                    modifier = Modifier
                                        .border(1.dp, MaterialTheme.colorScheme.surface, CircleShape),
                                )
                            }
                        }
                        uniffi.lakko_ffi_uniffi.FfiChipFace.EmojiOnly -> Unit
                    }
                }
            }
        }
        if (row.overflowKinds > 0u) {
            Surface(
                shape = RoundedCornerShape(percent = 50),
                color = MaterialTheme.colorScheme.surfaceVariant,
                contentColor = MaterialTheme.colorScheme.onSurface,
                modifier = Modifier
                    .height(chipHeight)
                    .combinedClickable(onClick = { onShowReactors(null) }),
            ) {
                Box(
                    contentAlignment = Alignment.Center,
                    modifier = Modifier.fillMaxHeight().padding(horizontal = chipPadding),
                ) {
                    Text(
                        text = "+${row.overflowKinds}",
                        style = reactionChipCountStyle(),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

/*
 * design-system.md "Reaction chip" (maintainer, 2026-11-04): a capsule 32 tall,
 * 10 of horizontal padding; emoji 18; reactor avatars 22 with a 1 ring in the
 * surface colour, overlapping by 4 (at 7 the second initial of a covered
 * avatar was hidden), at most 3; 4 between the emoji and the
 * avatars or the count; count 14, muted; 6 between chips and wrapped lines.
 */
internal val REACTION_CHIP_HEIGHT: Dp = 32.dp
internal val REACTION_CHIP_PADDING: Dp = 10.dp
internal val REACTION_CHIP_INNER_GAP: Dp = 4.dp
internal val REACTION_CHIP_SPACING: Dp = 6.dp
internal val REACTION_AVATAR_SIZE: Dp = 22.dp
internal val REACTION_AVATAR_OVERLAP: Dp = 4.dp
internal const val REACTION_CHIP_MAX_AVATARS: Int = 3
internal val REACTION_CHIP_EMOJI_SP = 18.sp
internal val REACTION_CHIP_COUNT_SP = 14.sp

@Composable
private fun reactionChipEmojiStyle(): androidx.compose.ui.text.TextStyle =
    MaterialTheme.typography.bodyLarge.copy(fontSize = REACTION_CHIP_EMOJI_SP, lineHeight = 22.sp)

@Composable
private fun reactionChipCountStyle(): androidx.compose.ui.text.TextStyle =
    MaterialTheme.typography.bodyMedium.copy(fontSize = REACTION_CHIP_COUNT_SP, lineHeight = 18.sp)

/** The core's chip row, or a plain tally when the core is unavailable. */
private fun chipRowOf(
    reactions: List<me.lakko.app.chat.Reaction>,
    ownIdentity: me.lakko.app.chat.MemberIdentity?,
    ctx: BubbleReactionContext?,
): uniffi.lakko_ffi_uniffi.FfiChipRow {
    val ownKey = ownIdentity?.let {
        try {
            java.util.Base64.getDecoder().decode(it)
        } catch (_: IllegalArgumentException) {
            null
        }
    } ?: ByteArray(32)
    return try {
        val held = reactions.mapNotNull { r ->
            val person = try {
                java.util.Base64.getDecoder().decode(r.reactor)
            } catch (_: IllegalArgumentException) {
                null
            }?.takeIf { it.size == 32 } ?: return@mapNotNull null
            uniffi.lakko_ffi_uniffi.FfiHeldReaction(person = person, emoji = r.emoji, at = r.at)
        }
        val sources = uniffi.lakko_ffi_uniffi.chipSourcesFromHeld(held, ownKey)
        uniffi.lakko_ffi_uniffi.reactionChips(
            sources,
            ctx?.chipConversation ?: uniffi.lakko_ffi_uniffi.FfiChipConversation.GROUP,
            (ctx?.allowed ?: me.lakko.app.chat.AllowedReactions.All).toFfiAllowed(),
        )
    } catch (_: Throwable) {
        val order = reactions.map { it.emoji }.distinct()
        uniffi.lakko_ffi_uniffi.FfiChipRow(
            chips = order.map { e ->
                val n = reactions.count { it.emoji == e }
                uniffi.lakko_ffi_uniffi.FfiReactionChip(
                    emoji = e,
                    count = n.toUInt(),
                    mine = reactions.any { it.emoji == e && it.reactor == ownIdentity },
                    face = uniffi.lakko_ffi_uniffi.FfiChipFace.Count(n.toUInt()),
                )
            },
            overflowKinds = 0u,
        )
    }
}

/**
 * reactions-and-emoji section 8.1 — who reacted: a bottom sheet with a tab row
 * (`All n`, then one per emoji in chip order, hidden with one emoji) and one
 * row per reaction; the person's own row reads `You` and comes first. Built
 * from what the device holds; nothing is fetched.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ReactorsSheet(
    reactions: List<me.lakko.app.chat.Reaction>,
    ownIdentity: me.lakko.app.chat.MemberIdentity?,
    initialEmoji: String?,
    nameOf: (me.lakko.app.chat.MemberIdentity) -> String,
    onDismiss: () -> Unit,
) {
    val ownAvatar = me.lakko.app.ui.components.rememberOwnAvatar()
    val ownAvatarName = me.lakko.app.ui.components.rememberOwnAvatarName(
        ownIdentity?.let(nameOf) ?: "?",
    )
    val strings = LocalStrings.current
    val kinds = remember(reactions) {
        reactions.groupBy { it.emoji }.entries
            .sortedWith(compareByDescending<Map.Entry<String, List<me.lakko.app.chat.Reaction>>> { it.value.size }
                .thenBy { e -> e.value.minOf { it.at } })
            .map { it.key }
    }
    var tab by remember { mutableStateOf(initialEmoji?.takeIf { it in kinds }) }
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.padding(horizontal = 16.dp).padding(bottom = 24.dp)) {
            Text(
                text = strings["chat.reaction.list_title"],
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(bottom = 8.dp).semantics { heading() },
            )
            if (kinds.size > 1) {
                Row(
                    modifier = Modifier.horizontalScroll(rememberScrollState()).padding(bottom = 8.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    FilterChip(
                        selected = tab == null,
                        onClick = { tab = null },
                        label = { Text(strings.format("chat.reaction.list_all", counts = mapOf("count" to reactions.size.toULong()))) },
                    )
                    for (k in kinds) {
                        FilterChip(
                            selected = tab == k,
                            onClick = { tab = k },
                            label = { Text("$k ${reactions.count { it.emoji == k }}") },
                        )
                    }
                }
            }
            val rows = reactions
                .filter { tab == null || it.emoji == tab }
                .sortedWith(compareBy<me.lakko.app.chat.Reaction>({ it.reactor != ownIdentity }, { it.at }))
            for (r in rows) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
                ) {
                    val mine = r.reactor == ownIdentity
                    val name = if (mine) strings["chat.author.you"] else nameOf(r.reactor)
                    // The own row's circle is the person's own photo or
                    // initials, never the "Y" of the "You" label.
                    LakkoAvatar(
                        name = if (mine) ownAvatarName else name,
                        seed = r.reactor,
                        size = 32.dp,
                        personKey = me.lakko.app.ui.components.personKeyBytes(r.reactor),
                        avatar = if (mine) ownAvatar else null,
                    )
                    Spacer(Modifier.width(12.dp))
                    Text(text = name, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
                    if (tab == null) Text(text = r.emoji, style = MaterialTheme.typography.titleMedium)
                }
            }
        }
    }
}

/** ADR-0023 — the pre-filled edit dialog (Save / Cancel). */
@Composable
internal fun EditMessageDialog(
    initialText: String,
    onCancel: () -> Unit,
    onSave: (String) -> Unit,
) {
    // Base walk 2026-11-04 — the field takes focus with the caret at the end
    // and raises the keyboard as soon as Edit is chosen, so typing goes in
    // without a tap on the field first.
    var field by remember {
        mutableStateOf(androidx.compose.ui.text.input.TextFieldValue(initialText, androidx.compose.ui.text.TextRange(initialText.length)))
    }
    val draft = field.text
    val editFocus = remember { FocusRequester() }
    val keyboard = androidx.compose.ui.platform.LocalSoftwareKeyboardController.current
    LaunchedEffect(Unit) {
        runCatching { editFocus.requestFocus() }
        keyboard?.show()
    }
    val canSave = draft.trim().isNotEmpty() && draft.trim() != initialText.trim()
    LakkoDialog(
        onDismissRequest = onCancel,
        title = t("chat.edit.title"),
        // The field has the focus, keyboard or not: typing is why it opened.
        focusCancelOnOpen = false,
        confirm = LakkoDialogAction(
            t("common.save"),
            enabled = draft.trim().isNotEmpty() && draft.trim() != initialText.trim(),
        ) { onSave(draft) },
        cancel = LakkoDialogAction(t("common.cancel"), LakkoDialogTone.Ghost, onClick = onCancel),
    ) {
        OutlinedTextField(
            value = field,
            onValueChange = { field = it },
            singleLine = false,
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.Sentences,
            ),
            modifier = Modifier
                .fillMaxWidth()
                .testTag(EDIT_FIELD_TAG)
                .focusRequester(editFocus)
                // Base scenario 10 — Enter saves, Shift+Enter is a new line.
                .submitOnHardwareEnter { if (canSave) onSave(draft) },
        )
    }
}

/** Single attachment chip rendered inside a bubble. Hosts the
 *  Save action on every chip and an Open action on image
 *  MIMEs. The two callbacks are owned by [ChatScreen] so they
 *  can drive coroutine work + the screen-level preview sheet. */
@Composable
private fun AttachmentChipRow(
    ref: AttachmentRef,
    hub: me.lakko.app.media.ChatMediaHub,
    onPreview: () -> Unit,
    onSave: () -> Unit,
) {
    val preview = AttachmentBridge.isInlinePreviewable(ref)
    // UX 4.1 / 4.2 — a file that is gone loses its Open / Save (trying again
    // cannot help); a limit keeps them as Try again.
    val failures by hub.failures.collectAsStateWithLifecycle()
    val failure = failures[me.lakko.app.media.ChatMediaHub.mediaKey(ref.blobId)]
    val gone = failure == me.lakko.app.media.MediaFailure.GONE
    Surface(
        shape = RoundedCornerShape(8.dp),
        color = Color.White.copy(alpha = 0.12f),
        contentColor = MaterialTheme.colorScheme.onSurface,
        modifier = Modifier.padding(vertical = 2.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(horizontal = 8.dp, vertical = 6.dp),
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = ref.name ?: t("chat.attachment.unnamed"),
                    style = MaterialTheme.typography.labelMedium,
                    maxLines = 1,
                )
                Text(
                    text = AttachmentBridge.formatSize(ref.size, LocalStrings.current),
                    style = MaterialTheme.typography.labelSmall,
                )
                if (failure != null && failure != me.lakko.app.media.MediaFailure.FAILED) {
                    MediaFailureLabel(failure, generic = "", onMedia = false)
                }
            }
            if (!gone) {
                if (preview) {
                    TextButton(onClick = onPreview) { Text(t("chat.attachment.open")) }
                }
                TextButton(onClick = onSave) {
                    Text(if (failure == me.lakko.app.media.MediaFailure.LIMIT) t("common.retry") else t("common.save"))
                }
            }
        }
    }
}

@Composable
private fun EmptyTimelineHint() {
    Box(
        modifier = Modifier.fillMaxSize(),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = t("chat.timeline.empty"),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun ThreadMissingHint() {
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 8.dp),
    ) {
        Text(
            text = t("chat.timeline.setting_up"),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

// =============================================================================
// Composer
// =============================================================================

/**
 * The composer row — `docs/ux/client-redesign.md` UX 5.2: attach, a text field
 * growing to five lines, the emoji or GIF picker, and a trailing button that is
 * a microphone with an empty draft and a send arrow otherwise.
 *
 * `internal` rather than private so `ComposerUiTest` drives the real thing.
 * Every parameter is already a plain value or a callback, so nothing is
 * loosened to reach it beyond the visibility itself.
 */
@Composable
internal fun Composer(
    /** ADR-0047 — the hold-to-record controller for this chat. */
    recorder: RecordController,
    /** ADR-0047 — an emoji the picker asked to insert at the caret. */
    emojiInsert: Pair<Long, String>?,
    onEmojiInserted: () -> Unit,
    onOpenPicker: () -> Unit,
    draft: String,
    /** Moves when something other than the person's typing changed [draft]. */
    draftRevision: Int = 0,
    sending: Boolean,
    sendError: UiText?,
    /** docs/ux/media-messages.md section 11 — what is attached to the
     *  message being written. */
    tray: ComposerTray,
    onDraftChange: (String) -> Unit,
    onSendClicked: () -> Unit,
    /** ADR-0024 — fired with the composer's focus state. On
     *  focus-loss the ViewModel publishes `typing:false` once. */
    onFocusChanged: (Boolean) -> Unit,
    /** UX spec §10 — the gallery pick (photos and videos). */
    onPickMedia: () -> Unit,
    onAttachClicked: () -> Unit,
    /** Section 11 — the (x) on a tray item. */
    onRemoveTrayItem: (Long) -> Unit,
    /** Section 11 — tap on a failed tray item. */
    onRetryTrayItem: (Long) -> Unit = {},
    /** Section 11 — the prepared bytes of a tray photo or video, for the viewer. */
    trayLocalFile: (Long) -> java.io.File? = { null },
    /** Section 11 Send — whether the tray lets Send go (a channel post needs
     *  every item uploaded; a chat message does not). */
    trayCanSend: (ComposerTray, String) -> Boolean = { t, text -> t.canSend(text) },
    /** ADR-0052 / UX 3.1 — the armed reply the strip shows, or `null`. */
    replyStrip: ReplyArmed? = null,
    /** The armed reply quotes this account's own message ("You" here). */
    replyStripIsOwn: Boolean = false,
    /** The strip's close control. */
    onCancelReply: () -> Unit = {},
    /** reactions-and-emoji section 13 — the emoji matching a `:word`, Recent
     *  first. Empty when the emoji data is not loaded. */
    colonSuggest: (String) -> List<String> = { emptyList() },
    /** docs/ux/attachments.md 2.5 — a photo or video pasted into the field or
     *  inserted from the keyboard; opens the send preview. Null where
     *  nothing can be attached: a paste is then text only. */
    onPasteMedia: ((List<me.lakko.app.chat.PastedMedia>) -> Unit)? = null,
) {
    // The caret has to be known to insert an emoji AT it (UX spec §4), which a
    // bare `String` value cannot carry. The view model still owns the text;
    // this mirrors it and follows an external change (a cleared draft after
    // Send) without losing the caret on the user's own edits.
    //
    // Only [draftRevision] resets the field. The draft itself reaches this
    // screen late while the person types, so treating a change of it as an
    // outside edit put an older text back over what was just typed and the
    // letters after it were lost.
    //
    // The field is state-based (see [ComposerTextField]), so the person's own
    // edits reach the view model through the text flow below; [reported]
    // remembers the last text handed on, so a reset from [draftRevision] is
    // not echoed back as an edit.
    val fieldState = remember { androidx.compose.foundation.text.input.TextFieldState(draft, TextRange(draft.length)) }
    val reported = remember { arrayOf(draft) }
    var lastRevision by remember { mutableStateOf(draftRevision) }
    val currentDraft by androidx.compose.runtime.rememberUpdatedState(draft)
    LaunchedEffect(draftRevision) {
        if (draftRevision != lastRevision) {
            lastRevision = draftRevision
            val outside = currentDraft
            if (fieldState.text.toString() != outside) {
                reported[0] = outside
                fieldState.edit {
                    replace(0, length, outside)
                    selection = TextRange(outside.length)
                }
            }
        }
    }
    val currentOnDraftChange by androidx.compose.runtime.rememberUpdatedState(onDraftChange)
    LaunchedEffect(fieldState) {
        androidx.compose.runtime.snapshotFlow { fieldState.text.toString() }.collect { text ->
            if (text != reported[0]) {
                reported[0] = text
                currentOnDraftChange(text)
            }
        }
    }
    // Hands the field's text on at once when the flow above has not yet: a
    // send right after the last keystroke must carry that keystroke.
    val flushDraft = {
        val text = fieldState.text.toString()
        if (text != reported[0]) {
            reported[0] = text
            onDraftChange(text)
        }
    }
    LaunchedEffect(emojiInsert) {
        val emoji = emojiInsert?.second ?: return@LaunchedEffect
        val sel = fieldState.selection
        val text = fieldState.text.toString().replaceRange(sel.min, sel.max, emoji)
        val caret = sel.min + emoji.length
        reported[0] = text
        fieldState.edit {
            replace(0, length, text)
            selection = TextRange(caret)
        }
        onDraftChange(text)
        onEmojiInserted()
    }

    // UX 3.1 — "Choosing Reply ... focuses the composer." Keyed on the armed
    // reply itself, which carries a sequence number so a second Reply on the
    // same message focuses again.
    val fieldFocus = remember { FocusRequester() }
    LaunchedEffect(replyStrip) {
        if (replyStrip != null) {
            runCatching { fieldFocus.requestFocus() }
        }
    }

    val recState = recorder.state
    // ADR-0047 §3.1 — the recording UI replaces the composer only once
    // something is actually being captured. A press still being judged as
    // tap-or-hold, and a permission being asked for, both leave the composer
    // exactly as it was: swapping it for either made a plain tap flash the
    // whole screen (the recording bar in, the tray out, the hint line shifting
    // the layout, and in camera mode the full-screen round preview) for the few
    // tens of milliseconds a tap lasts. The rule is
    // `me.lakko.app.media.showsRecordingUi`.
    val recording = me.lakko.app.media.showsRecordingUi(recState)

    val showSend = draft.isNotBlank() || tray.items.isNotEmpty()

    Column {
        HorizontalDivider()
        if (sendError != null) {
            Text(
                text = sendError.text(),
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
            )
        }
        // ADR-0047 §6 — microphone / camera refused: the card with Open Settings.
        recorder.denied?.let { kind ->
            MediaDeniedCard(kind = kind, onDismiss = recorder::dismissDenied)
        }
        (recorder.notice ?: recorder.disabledReason?.takeIf { !showSend })?.let { note ->
            Text(
                text = t(note),
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .padding(horizontal = 16.dp, vertical = 2.dp)
                    .semantics { liveRegion = LiveRegionMode.Polite },
            )
        }
        // ADR-0052 / UX 3.1 — the reply strip, above the tray and the field.
        if (replyStrip != null && !recording) {
            me.lakko.app.ui.theme.ChatTextScale {
                ReplyStrip(reply = replyStrip.snapshot, onClose = onCancelReply, quotesOwnAccount = replyStripIsOwn)
            }
        }
        // reactions-and-emoji section 13 — the `:` suggestions strip.
        val fieldText = fieldState.text.toString()
        val fieldSelection = fieldState.selection
        val colonQuery = me.lakko.app.chat.colonQueryAt(
            fieldText,
            fieldSelection.end,
            collapsed = fieldSelection.collapsed,
        )
        val colonEmoji = remember(colonQuery?.word) {
            colonQuery?.let { colonSuggest(it.word).take(me.lakko.app.chat.COLON_SUGGESTIONS_MAX) }.orEmpty()
        }
        if (colonQuery != null && colonEmoji.isNotEmpty() && !recording) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState())
                    .padding(horizontal = 12.dp, vertical = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                for (emoji in colonEmoji) {
                    Box(
                        contentAlignment = Alignment.Center,
                        modifier = Modifier
                            .size(40.dp)
                            .clickable(onClickLabel = emoji) {
                                val (text, caret) = me.lakko.app.chat.applyColonPick(
                                    fieldState.text.toString(),
                                    fieldState.selection.end,
                                    colonQuery,
                                    emoji,
                                )
                                reported[0] = text
                                fieldState.edit {
                                    replace(0, length, text)
                                    selection = TextRange(caret)
                                }
                                onDraftChange(text)
                            },
                    ) {
                        Text(text = emoji, style = MaterialTheme.typography.titleLarge)
                    }
                }
            }
        }
        // docs/ux/media-messages.md section 11 — the tray directly above the
        // field: thumbnails and file chips, each with its (x), its upload ring
        // and, when it failed, a retry glyph. Hidden when empty.
        if (tray.items.isNotEmpty() && !recording) {
            ComposerTrayStrip(
                tray = tray,
                onRemove = onRemoveTrayItem,
                onRetry = onRetryTrayItem,
                localFile = trayLocalFile,
            )
        }
        Row(
            verticalAlignment = Alignment.Bottom,
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 8.dp, vertical = 8.dp),
        ) {
            if (recording) {
                // §2 flow 2 — the recording bar replaces the text field. The
                // record button stays composed at the same spot so the held
                // pointer keeps its gesture.
                RecordingBar(recorder, Modifier.weight(1f).heightIn(min = 48.dp))
            } else {
                // 1.19c4 — Attach button. Drives the OS-level
                // ACTION_OPEN_DOCUMENT picker via the launcher
                // wired in [ChatScreen]; the contract surfaces both
                // photos and arbitrary files behind a single tap.
                // ADR-0092 / docs/ux/attachments.md section 2 — the paperclip
                // opens the one attachment menu (a LakkoMenu sheet) owned by
                // ChatScreen; never a stock dropdown.
                IconButton(
                    onClick = onAttachClicked,
                    enabled = !sending,
                    modifier = Modifier.testTag("composer-attach"),
                ) {
                    LkIcon(R.drawable.ic_lk_paperclip, contentDescription = t("chat.composer.attach"), size = LkIconSize.Row)
                }
                // UX 5.2 — a pill field on the sunken surface that grows to
                // five lines and then scrolls; the IME resizes the screen
                // (adjustResize + the Scaffold's safe-drawing insets).
                // Not disabled while a message is on its way: the draft was
                // cleared when it was sent, and a disabled field loses the
                // focus, so the keyboard closed after every message and what
                // was typed next went nowhere (base scenario 17, seen
                // 2026-11-11). Send stays off until the message is out.
                // docs/ux/attachments.md 2.5 — a pasted or keyboard-inserted
                // photo or video attaches (see ComposerTextField).
                ComposerTextField(
                    state = fieldState,
                    placeholder = t("chat.composer.placeholder"),
                    onPasteMedia = onPasteMedia,
                    // docs/ux/text-size.md — the composer's text follows the
                    // device's text size; the field's frame does not.
                    textStyle = MaterialTheme.typography.bodyLarge.let { base ->
                        val f = me.lakko.app.ui.theme.chatTextFactor()
                        base.copy(fontSize = base.fontSize * f, lineHeight = base.lineHeight * f)
                    },
                    modifier = Modifier
                        .weight(1f)
                        .focusRequester(fieldFocus)
                        // Base scenario 10 — a hardware keyboard's Enter sends
                        // (Shift+Enter is a new line); the on-screen keyboard's
                        // Enter stays a new line.
                        .submitOnHardwareEnter {
                            flushDraft()
                            if (!sending && trayCanSend(tray, fieldState.text.toString())) {
                                onSendClicked()
                            }
                        }
                        // ADR-0024 — surface focus to the ViewModel so a
                        // blur clears any outstanding typing signal.
                        .onFocusChanged { onFocusChanged(it.isFocused) },
                )
                // UX spec §4 — the smiley opens the picker on the Emoji tab
                // (UX 5.2: attach, field, emoji or GIF, trailing button).
                IconButton(
                    onClick = onOpenPicker,
                    enabled = !sending,
                ) {
                    LkIcon(R.drawable.ic_lk_face_smiling, contentDescription = t("chat.composer.emoji_gifs"), size = LkIconSize.Row)
                }
            }
            Spacer(Modifier.width(4.dp))
            if (showSend && !recording) {
                // UX 5.2 — the trailing button is the send arrow once there is
                // a draft or an attachment; the mic otherwise (below).
                val sendLabel = t("chat.composer.send")
                FilledIconButton(
                    onClick = {
                        flushDraft()
                        onSendClicked()
                    },
                    enabled = !sending && trayCanSend(tray, draft),
                    colors = IconButtonDefaults.filledIconButtonColors(
                        containerColor = MaterialTheme.colorScheme.primary,
                        contentColor = MaterialTheme.colorScheme.onPrimary,
                    ),
                    // The label lives on the BUTTON node (the clickable,
                    // focusable one TalkBack lands on and UI tests find by
                    // `content-desc`), not on the icon inside it, so it is the
                    // same node and the same text whether the icon is showing
                    // or the in-flight spinner has replaced it. The mic
                    // (`RecordButton`) labels its own clickable node the same
                    // way.
                    modifier = Modifier
                        .height(48.dp)
                        .width(48.dp)
                        .semantics { contentDescription = sendLabel },
                ) {
                    if (sending) {
                        CircularProgressIndicator(
                            modifier = Modifier
                                .height(20.dp)
                                .width(20.dp),
                            strokeWidth = 2.dp,
                            color = MaterialTheme.colorScheme.onPrimary,
                        )
                    } else {
                        LkIcon(R.drawable.ic_lk_send_fill, contentDescription = null, size = LkIconSize.Row)
                    }
                }
            } else if (recState !is me.lakko.app.media.RecState.Preview) {
                // UX spec §1 — with nothing to send the right-hand button is
                // the mic (or camera, after a tap toggles the mode).
                RecordButton(recorder)
            }
        }
    }
}

/**
 * ADR-0045 UX §6.2.5 — the terminal timeline row.
 *
 * Two lines, the same shape as §5.1's pending notice: the fact, then the
 * standing consequence. Centred and muted, matching the treatment system rows
 * already have, and carried by TEXT rather than by hue or an icon.
 *
 * Announced POLITELY rather than assertively: it arrives unbidden, possibly
 * mid-sentence, and interrupting someone to tell them they have been removed
 * from a group is the wrong register for a fact they cannot act on.
 */
@Composable
private fun TerminalMembershipRow(row: me.lakko.app.chat.TerminalRowCopy) {
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 24.dp, vertical = 8.dp)
            .semantics { liveRegion = LiveRegionMode.Polite },
    ) {
        Text(
            text = row.headline.text(),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        Text(
            text = row.consequence.text(),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
    }
}

/**
 * ADR-0045 UX §6.2.5 — what stands where the composer was.
 *
 * **Replaced, not disabled, and the distinction is principled.** §5.3 argues a
 * pending row's `Remove` must stay visible-but-disabled, because hiding it
 * would read as the permission having vanished when it is merely suspended.
 * The permission here has vanished, permanently and by design, so the rule
 * inverts: disable while the state is TRANSIENT, remove when it is TERMINAL. A
 * disabled Send button on a group you will never be in again is a control
 * promising a future it does not have.
 *
 * The bar occupies the composer's own slot so the layout does not jump.
 *
 * **The unsent draft is preserved, and NOT in a text field.** The commit lands
 * during a poll, which can be mid-sentence, and silently discarding what the
 * user typed is a small data loss with no upside. But the obvious fix — leave
 * the `OutlinedTextField` there, `enabled = false` — is wrong on exactly this
 * platform: a disabled Compose text field is not selectable, so the user would
 * see their words and be unable to copy them out. Hence a `SelectionContainer`
 * around a plain `Text`, which is Compose's expression of the cross-platform
 * shape §6.2.5 specifies: SELECTABLE, non-editable text.
 */
@Composable
private fun WithdrawnComposer(slot: me.lakko.app.chat.ComposerSlot.Withdrawn) {
    Column {
        HorizontalDivider()
        Column(modifier = Modifier.padding(12.dp)) {
            if (slot.draft != null) {
                Text(
                    text = (slot.draftLabel ?: me.lakko.app.chat.UNSENT_DRAFT_LABEL).text(),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(2.dp))
                SelectionContainer {
                    Text(
                        text = slot.draft,
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
                Spacer(Modifier.height(8.dp))
            }
            Text(
                text = slot.notice.text(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** Back-off delay between failed long-poll iterations. 5s
 *  matches the canonical chat-app retry cadence — see the
 *  parallel constant in `ContactsScreen`. */
private const val POLL_RETRY_BACKOFF_MS: Long = 5_000L


// =============================================================================
// 1.19c4 — picker / share helpers
// =============================================================================

/** Decode an arbitrary byte buffer to a Compose `ImageBitmap`
 *  for the inline preview sheet. Uses Android's built-in
 *  BitmapFactory so we don't take on Coil / Glide as a
 *  dependency for the one preview-image use case. Returns
 *  `null` on a non-image / corrupt blob. */
private fun decodeImageBitmap(bytes: ByteArray): ImageBitmap? {
    return runCatching {
        android.graphics.BitmapFactory.decodeByteArray(bytes, 0, bytes.size)
            ?.asImageBitmap()
    }.getOrNull()
}

// =============================================================================
// A4 — group-membership sheet: add a member by @username (G-4-verified) and
// view / remove the live MLS roster. Names resolve through the accepted
// contacts list (ADR-0021), falling back to a short key fingerprint.
// =============================================================================

/**
 * ADR-0045 (UX §6) — the `Leave group` confirmation.
 *
 * The confirm is the LAST EXIT, not a courtesy: a published request cannot be
 * withdrawn (there is no wire withdraw, and evicting it locally would leave it
 * live in every peer's store), so this dialog is the only place the user can
 * still change their mind. The body carries the one fact they need — that they
 * remain in the group, receiving messages, until somebody applies it.
 */
@Composable
private fun LeaveGroupDialog(
    onCancel: () -> Unit,
    onLeave: () -> Unit,
) {
    LakkoDialog(
        onDismissRequest = onCancel,
        title = t("chat.leave.title"),
        body = t("chat.leave.body"),
        confirm = LakkoDialogAction(t("chat.leave.confirm"), LakkoDialogTone.Danger, onClick = onLeave),
        cancel = LakkoDialogAction(t("common.cancel"), LakkoDialogTone.Ghost, onClick = onCancel),
    )
}

/**
 * ADR-0045 (UX §6.2.4) — the `Delete this chat` confirmation.
 *
 * The only protection there is, which is why the body is exact rather than
 * sweeping.
 *
 * Copy from [me.lakko.app.chat.DELETE_CHAT_TITLE] /
 * [me.lakko.app.chat.DELETE_CHAT_BODY] rather than inline, so the three clients
 * cannot drift on the one string that describes an irreversible deletion.
 */
/**
 * ADR-0087 UX 9.1 — this account is out of the group the person has open. One
 * button, which goes back to Chats; it never says who. What the composer held
 * is shown selectable: the conversation it was typed in is gone.
 */
@Composable
private fun MembershipEndDialog(
    dialog: me.lakko.app.chat.EndDialog,
    /** The title on screen, for a group with no shared name. */
    shownTitle: String?,
    onOk: () -> Unit,
) {
    val fallback = shownTitle ?: t("chat.group.fallback_name")
    LakkoDialog(
        onDismissRequest = {},
        title = me.lakko.app.chat.endNoticeLine(dialog.name, fallback).text(),
        body = t("chat.ended.notice_body"),
        confirm = LakkoDialogAction(t("common.ok"), LakkoDialogTone.Primary, onClick = onOk),
        cancel = null,
        content = dialog.draft?.let { draft ->
            {
                val clipboard = LocalClipboardManager.current
                Text(
                    text = t("chat.ended.unsent_draft"),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                androidx.compose.foundation.text.selection.SelectionContainer {
                    Text(text = draft, style = MaterialTheme.typography.bodyMedium)
                }
                TextButton(onClick = { clipboard.setText(androidx.compose.ui.text.AnnotatedString(draft)) }) {
                    Text(t("common.copy"))
                }
            }
        },
    )
}

/** ADR-0045 UX 6.2.4 — `internal` so the person layer raises the same one. */
@Composable
internal fun DeleteChatDialog(
    onCancel: () -> Unit,
    onDelete: () -> Unit,
) {
    LakkoDialog(
        onDismissRequest = onCancel,
        title = me.lakko.app.chat.DELETE_CHAT_TITLE.text(),
        body = me.lakko.app.chat.DELETE_CHAT_BODY.text(),
        confirm = LakkoDialogAction(t("common.delete"), LakkoDialogTone.Danger, onClick = onDelete),
        cancel = LakkoDialogAction(t("common.cancel"), LakkoDialogTone.Ghost, onClick = onCancel),
    )
}

/**
 * ADR-0045 (UX §6.1.9) — the new-owner notice.
 *
 * Rendered once, dismissible, at panel level, and announced politely (§13).
 * Best-effort and FAIL-OPEN by construction: nothing on the wire announces a
 * transfer, so this is a client-side inference from an observed role-table
 * change, structurally unable to fire on a device that first observes the table
 * already naming it owner. It must never be the only way a user learns their
 * role — the panel's affordances stay the primary signal, exactly as they are
 * for admin today.
 */
@Composable
private fun NewOwnerNoticeCard(
    notice: me.lakko.app.chat.NewOwnerNotice,
    onDismiss: () -> Unit,
) {
    Surface(
        modifier = Modifier.fillMaxWidth(),
        shape = MaterialTheme.shapes.small,
        color = MaterialTheme.colorScheme.surfaceVariant,
    ) {
        Column(modifier = Modifier.padding(10.dp)) {
            Text(text = notice.headline.text(), style = MaterialTheme.typography.bodyMedium)
            // The second line names the exit deliberately: it is the one
            // sentence that stops the new owner from arriving where the old one
            // was.
            Text(
                text = notice.body.text(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            TextButton(onClick = onDismiss) { Text(notice.dismiss.text()) }
        }
    }
}

/**
 * ADR-0045 (UX §6.1.4 / §6.1.5) — the transfer step.
 *
 * ## The container, which §6.1.11 leaves to each platform
 *
 * An INLINE expansion inside the members `Dialog`, not a second `Dialog` on top
 * of it. Android is the platform §6.1.11 flags as genuinely open, because unlike
 * web's side panel the members panel here is already a full-width modal, so the
 * obvious reading of "a step" — another `Dialog` — stacks two modals. Three
 * things decided it against stacking:
 *
 * 1. §6.1.5's own argument against a second confirmation says a modal-over-modal
 *    is idiomatic on none of the three platforms. It is written about an
 *    `Are you sure?`, but the objection is to the stacking, and it does not stop
 *    applying because the top modal happens to be useful.
 * 2. **Adjacency is the whole of §6.1.1.** The disabled `Leave group` is a
 *    signposted route *because* its reason names an action sitting directly
 *    above it. A `Dialog` covers that block behind a scrim, so the user who
 *    tapped `Transfer ownership` loses sight of the thing that sent them there
 *    at the exact moment they are asked to commit to it.
 * 3. The roster the owner has just read stays on screen, which matters because
 *    the successor they are about to name is in it.
 *
 * The cost is height, and it is paid explicitly: the picker is bounded, and the
 * caller shrinks the roster list while this is open, so the action button cannot
 * be pushed off a small screen.
 *
 * ## The shape, which §6.1 does NOT leave open
 *
 * Irreversibility statement, then picker, then role choice, then the trailing
 * "applied" line, then the action. **There is no second confirmation dialog** —
 * this step IS the confirmation, which happens to require two choices to be
 * well-formed. Stacking an `Are you sure?` on a step that already asked two
 * questions buys friction of the kind that gets dismissed reflexively. What
 * replaces it is friction that cannot be discharged without reading: the action
 * names the person, is disabled until one is picked, and uses the picked row's
 * label verbatim.
 *
 * **Ordering is load-bearing:** the irreversibility statement sits ABOVE the
 * role choice. Below it, the pre-selected option would read as the safe one and
 * the warning would read as being about the other one.
 */
@Composable
private fun TransferOwnershipStep(
    candidates: List<me.lakko.app.chat.TransferCandidate>,
    pickedKeyB64: String?,
    outgoing: uniffi.lakko_ffi_uniffi.OutgoingOwner,
    busy: Boolean,
    onPick: (String) -> Unit,
    onOutgoing: (uniffi.lakko_ffi_uniffi.OutgoingOwner) -> Unit,
    onCancel: () -> Unit,
    onTransfer: (me.lakko.app.chat.TransferCandidate) -> Unit,
) {
    val selected = candidates.firstOrNull { it.signatureKeyB64 == pickedKeyB64 }
    val action = me.lakko.app.chat.transferActionLabel(selected)
    Surface(
        modifier = Modifier.fillMaxWidth(),
        shape = MaterialTheme.shapes.small,
        color = MaterialTheme.colorScheme.surfaceVariant,
    ) {
        Column(modifier = Modifier.padding(12.dp)) {
            Text(
                text = me.lakko.app.chat.TRANSFER_IRREVERSIBLE_HEADLINE.text(),
                style = MaterialTheme.typography.titleSmall,
            )
            Text(
                text = me.lakko.app.chat.TRANSFER_IRREVERSIBLE_BODY.text(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(10.dp))
            Text(text = t("chat.transfer.new_owner_legend"), style = MaterialTheme.typography.labelMedium)
            // §6.1.3 — every member except the owner, in the roster's own order,
            // with a muted `Admin` MARKER rather than a filter. An admins-only
            // picker is EMPTY in the commonest small group (owner plus one
            // member, no admins ever appointed) — the exact dead end this
            // feature exists to close, reappearing as a UI rule. The selection
            // value is the 32-byte signature key, never the leaf index.
            LazyColumn(modifier = Modifier.heightIn(max = 180.dp)) {
                items(candidates, key = { it.signatureKeyB64 }) { row ->
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .fillMaxWidth()
                            .selectable(
                                selected = pickedKeyB64 == row.signatureKeyB64,
                                enabled = !busy,
                                role = androidx.compose.ui.semantics.Role.RadioButton,
                                onClick = { onPick(row.signatureKeyB64) },
                            )
                            // UX §13 — a picker row announces the roster label
                            // plus `, admin` where the marker shows. The explicit
                            // description is what keeps that a comma rather than
                            // the two texts running together.
                            .semantics(mergeDescendants = true) {
                                contentDescription =
                                    if (row.isAdmin) {
                                        me.lakko.app.l10n.Localization.strings.value.format(
                                            "chat.transfer.candidate_admin",
                                            mapOf("name" to row.label),
                                        )
                                    } else {
                                        row.label
                                    }
                            }
                            .padding(vertical = 2.dp),
                    ) {
                        RadioButton(
                            selected = pickedKeyB64 == row.signatureKeyB64,
                            onClick = null,
                            enabled = !busy,
                        )
                        Spacer(Modifier.width(6.dp))
                        // The members list's own two lines (§6.1.3).
                        Column {
                            Text(text = row.label, style = MaterialTheme.typography.bodyMedium)
                            row.usernameLine?.let {
                                Text(
                                    text = it,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                        // MARK, do not filter and do not sort. Who is already an
                        // admin is information the owner assigned and is worth
                        // having when choosing a successor; it is not a reason to
                        // hide anyone, and hoisting admins to the top would be a
                        // soft version of the filter §6.1.3 rejects.
                        if (row.isAdmin) {
                            Spacer(Modifier.width(6.dp))
                            Text(
                                text = t("chat.members.admin.marker"),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }
            Spacer(Modifier.height(10.dp))
            // §6.1.4 — a real decision, pre-selected but never defaulted. A
            // single-select control, NEVER a checkbox: a checkbox has an unstated
            // default state and this has none. The core refuses to default it
            // because either silent choice is wrong in one direction, and the UI
            // must not undo with a layout choice what the core protected with a
            // type.
            Text(text = t("chat.transfer.role_legend"), style = MaterialTheme.typography.labelMedium)
            me.lakko.app.chat.OUTGOING_OWNER_OPTIONS.forEach { option ->
                Row(
                    verticalAlignment = Alignment.Top,
                    modifier = Modifier
                        .fillMaxWidth()
                        .selectable(
                            selected = outgoing == option.value,
                            enabled = !busy,
                            role = androidx.compose.ui.semantics.Role.RadioButton,
                            onClick = { onOutgoing(option.value) },
                        )
                        .padding(vertical = 2.dp),
                ) {
                    RadioButton(
                        selected = outgoing == option.value,
                        onClick = null,
                        enabled = !busy,
                    )
                    Spacer(Modifier.width(6.dp))
                    Column {
                        Text(text = option.label.text(), style = MaterialTheme.typography.bodyMedium)
                        Text(
                            text = option.consequence.text(),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
            Spacer(Modifier.height(8.dp))
            // Note APPLIED, not NOW: the transfer is a proposal, and asserting
            // the exit is open before the commit lands would be the optimistic
            // update §3 forbids.
            Text(
                text = me.lakko.app.chat.TRANSFER_UNLOCKS_LEAVE.text(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(
                    onClick = { selected?.let(onTransfer) },
                    enabled = !busy && action.enabled,
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                ) { Text(action.label.text()) }
                // `Cancel` is a PRE-PUBLISH exit, and it is the last one.
                // Nothing here is a withdraw: once the proposal is published it
                // is in every member's store and dies only when some commit
                // lands, so no client may offer a cancel afterwards —
                // including, and especially, for a transfer.
                TextButton(onClick = onCancel, enabled = !busy) { Text(t("common.cancel")) }
            }
        }
    }
}

/**
 * ADR-0051 / docs/ux/group-and-channel-names.md section 3.1 — the `Group name`
 * row of the members panel. Owner / admin: a field with Save (an empty name
 * clears it). Plain member: the name read-only, no edit affordance.
 */
@Composable
private fun GroupNameRow(viewModel: ChatViewModel, canRename: Boolean) {
    val name by viewModel.groupName.collectAsStateWithLifecycle()
    val draft by viewModel.groupNameDraft.collectAsStateWithLifecycle()
    val busy by viewModel.groupNameBusy.collectAsStateWithLifecycle()
    val error by viewModel.groupNameError.collectAsStateWithLifecycle()
    Column {
        if (canRename) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = draft,
                    onValueChange = viewModel::setGroupNameDraft,
                    label = { Text(me.lakko.app.chat.GROUP_NAME_LABEL.text()) },
                    singleLine = true,
                    enabled = !busy,
                    isError = error != null,
                    keyboardOptions = KeyboardOptions(
                        capitalization = KeyboardCapitalization.Sentences,
                    ),
                    modifier = Modifier.weight(1f),
                )
                Spacer(Modifier.width(8.dp))
                Button(
                    onClick = viewModel::saveGroupName,
                    enabled = !busy && draft.trim() != name.trim(),
                ) { Text(if (busy) t("common.saving") else me.lakko.app.chat.GROUP_NAME_SAVE.text()) }
            }
        } else {
            Text(
                text = me.lakko.app.chat.GROUP_NAME_LABEL.text(),
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                text = name.ifBlank { t("chat.group_name.empty") },
                style = MaterialTheme.typography.bodyLarge,
                color = if (name.isBlank()) {
                    MaterialTheme.colorScheme.onSurfaceVariant
                } else {
                    MaterialTheme.colorScheme.onSurface
                },
            )
        }
        error?.let { message ->
            Spacer(Modifier.height(4.dp))
            Text(
                text = message.text(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
    }
}

/**
 * The group's info (`docs/ux/conversation-info.md` sections 2.2 and 3): a
 * screen pushed over the conversation with a back arrow in its top bar, and
 * the system back doing the same. The transfer step is pushed over it the
 * same way (its own back returns here).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MembersSheet(
    viewModel: ChatViewModel,
    /** ADR-0045 UX §6.2.4 — invoked after `Delete this chat` has removed the
     *  registry row, so the screen leaves a thread it can no longer open. */
    onChatDeleted: () -> Unit,
    /** Section 2.2 — a member who is a contact opens the contact screen,
     *  pushed over this one; its back returns here. */
    onOpenContact: (contactKey: String) -> Unit,
    contactsRepository: me.lakko.app.contacts.ContactsRepository,
    modifier: Modifier = Modifier,
) {
    val members by viewModel.members.collectAsStateWithLifecycle()
    // ADR-0048 / ADR-0098 — a profile arriving, a username verifying or a
    // directory answer (which decides whether a `@username` may show) bumps
    // the names; the rows below read it so they show the change in place.
    val membersNameVersion by viewModel.nameVersion.collectAsStateWithLifecycle()
    val contactsState by contactsRepository.state.collectAsStateWithLifecycle()
    val contactKeysB64 = remember(contactsState.accepted) {
        contactsState.accepted.map { me.lakko.app.chat.outboxB64(it.request.fromPubkey) }.toSet()
    }
    val busy by viewModel.membershipBusy.collectAsStateWithLifecycle()
    val error by viewModel.membershipError.collectAsStateWithLifecycle()
    val canManageRaw by viewModel.canManageMembers.collectAsStateWithLifecycle()
    // ADR-0091 phase 2 / conversation-rejoin 2.2 — a waiting chat hides every
    // control that would change it: no member changes, no Remove, no rename.
    val canManage = canManageRaw
    val isOwner by viewModel.isOwner.collectAsStateWithLifecycle()
    val roleTable by viewModel.roleTable.collectAsStateWithLifecycle()
    val pendingJoins by viewModel.pendingJoins.collectAsStateWithLifecycle()
    val unkeptJoinRequest by viewModel.unkeptJoinRequest.collectAsStateWithLifecycle()
    val pendingRequests by viewModel.pendingRequests.collectAsStateWithLifecycle()
    val notApplied by viewModel.notApplied.collectAsStateWithLifecycle()
    val pendingNotice by viewModel.pendingNotice.collectAsStateWithLifecycle()
    val joinAttributions by viewModel.joinAttributions.collectAsStateWithLifecycle()
    val inviteLink by viewModel.inviteLink.collectAsStateWithLifecycle()
    val notice by viewModel.membershipNotice.collectAsStateWithLifecycle()
    val newOwner by viewModel.newOwnerNotice.collectAsStateWithLifecycle()
    val clipboard = LocalClipboardManager.current
    val context = LocalContext.current
    val ownLeaf = viewModel.ownLeafIndex()
    // ADR-0045 (UX §6) — the `Leave group` confirmation. Local to the sheet: it
    // is a presentation concern with no bearing on the request, and nothing
    // outside this composable may open it.
    var confirmingLeave by remember { mutableStateOf(false) }
    // ADR-0045 (UX §6.1.5) — the transfer step. Unlike `confirmingLeave` above,
    // this state lives in the VIEW MODEL, and the reason is the async path: the
    // step must close only once the proposal is published, so a refusal — the
    // C-65 collision most of all — leaves the picked successor and the role
    // choice intact for a retry. The step IS the confirmation, so there is
    // deliberately nothing behind it.
    val transferOpen by viewModel.transferStepOpen.collectAsStateWithLifecycle()
    val transferPick by viewModel.transferPick.collectAsStateWithLifecycle()
    val transferOutgoing by viewModel.transferOutgoing.collectAsStateWithLifecycle()
    // ADR-0045 UX §6.2 — has this device's membership ended?
    val membershipEnded by viewModel.membershipEnded.collectAsStateWithLifecycle()
    // ADR-0087 section 9 — under "You are no longer in {name}" this screen keeps
    // the name and the members and nothing that acts.
    val infoEndDialog by viewModel.endDialog.collectAsStateWithLifecycle()
    val behindEnd = me.lakko.app.chat.infoBehindEnd(
        ending = infoEndDialog != null || viewModel.conversationEnding(),
        membershipEnded = membershipEnded,
    )
    // ADR-0045 UX §6.2.4 — the `Delete this chat` confirmation. Local to the
    // sheet, like `confirmingLeave`: it is a presentation concern, and nothing
    // outside this composable may open it.
    var confirmingDelete by remember { mutableStateOf(false) }
    // ADR-0045 (UX §6) — the `Remove this member?` confirmation, holding the
    // leaf awaiting it. Local to the sheet like `confirmingLeave`, and a
    // nullable leaf rather than a boolean because the confirmation names WHICH
    // member: a shared boolean plus a separately-stored target is how a
    // confirmation ends up removing somebody the user did not look at.

    // docs/ux/conversation-info.md section 2.2 — WHAT this panel holds is
    // asked, never decided here. The one copy of the rule is
    // `crates/lakko-ui-rules/src/info.rs`, reached over UniFFI; this panel used
    // to branch on `canManage` and `isOwner` inline and consumed no section
    // list at all, which is the same class of divergence that put a group's
    // panel in front of a person.
    //
    // The three owner-or-admin fields are fed from ONE role read because on
    // this client they are one question: `refreshMembers` reads the live role
    // table once (`role != Role.MEMBER && !ended`), and renaming, adding and
    // minting a link all turn on exactly that. They stay three fields across
    // the boundary because they are three different ADRs (0051 / 0022 / 0028)
    // and a client that later learns to tell them apart must not have to widen
    // the rule to say so.
    //
    // Remembered because asking now crosses the FFI: this is a native call,
    // and its answer changes only when one of its inputs does.
    //
    // `canLeave` is the same pair of conditions `exitBlock` hides the whole
    // block on: a group carrying no ADR-0022 role table has no ADR-0045
    // proposal path, so a member has nothing to leave WITH, and a torn
    // own-leaf read is not a licence to offer an exit. It is a field of the
    // shared rule because the rule used to offer `Leave group` regardless and
    // this client and web both overrode it in their own view - which is a
    // rule nobody was following.
    val canLeave = roleTable != null && ownLeaf != null

    // ADR-0045 UX §6.1 — the exit block's inputs, read as Compose state so the
    // block recomposes when any of them moves.
    val rosterViews = remember(members) { viewModel.rosterViews() }
    // ADR-0099 step 4 / UX 3 — members whose account the moderation log
    // confirms suspended, marked as the answers arrive. Keyed on the
    // ACCOUNT key (one person, however many devices); a leaf whose
    // credential did not resolve carries none and is not asked about.
    val memberAccountKeys = remember(members) {
        members.mapNotNullTo(HashSet()) { m ->
            m.accountKey.takeIf { it.size == 32 }?.let { java.util.Base64.getEncoder().encodeToString(it) }
        }
    }
    val suspendedMembers by me.lakko.app.ui.suspension.rememberSuspendedAmong(memberAccountKeys)
    val ownerKeyB64 = roleTable?.owner?.let { me.lakko.app.chat.outboxB64(it) }
    val adminKeysB64 = roleTable?.admins?.map { me.lakko.app.chat.outboxB64(it) }?.toSet()
        ?: emptySet()
    val transferCandidates = remember(rosterViews, ownerKeyB64, adminKeysB64) {
        me.lakko.app.chat.transferCandidates(rosterViews, ownerKeyB64, adminKeysB64)
    }
    // ADR-0087 / group-and-channel-privacy section 18 — the sections come from
    // `lakko_ui_rules::conversation_info_sections`: `Handle`, `GroupType` and
    // `DeleteGroup` included; `alone` and `can_transfer` are inputs. An ended
    // membership is answered as a plain member who cannot leave, which takes
    // every editing and exit section away and leaves the name readable.
    val settingsVersionForInfo by me.lakko.app.chat.GroupSettingsStore.version.collectAsStateWithLifecycle()
    val hasHandle = remember(settingsVersionForInfo) { viewModel.groupHandle() != null }
    // ADR-0099 section 4 - no calls and no adding people while suspended.
    val suspendedReadOnlyInfo by me.lakko.app.auth.SuspensionGate.readOnly.collectAsStateWithLifecycle()
    val sections = remember(canManage, isOwner, canLeave, membershipEnded, transferCandidates, hasHandle, suspendedReadOnlyInfo) {
        me.lakko.app.ui.suspension.readOnlySections(suspended = suspendedReadOnlyInfo, sections =
        conversationInfoSections(
            ConversationInfoCapabilities(
                kind = ConversationInfoKind.GROUP,
                role = when {
                    membershipEnded -> ConversationRole.MEMBER
                    isOwner -> ConversationRole.OWNER
                    canManage -> ConversationRole.ADMIN
                    else -> ConversationRole.MEMBER
                },
                alone = transferCandidates.isEmpty(),
                canTransfer = transferCandidates.isNotEmpty(),
                canLeave = canLeave && !membershipEnded,
                hasHandle = hasHandle,
            ),
        ))
    }
    // §6.1.6 / §6.1.8 — the role change already WAITING to apply, if any.
    // `waiting` and not merely pending is the whole point: below the quiet window
    // the control stays ENABLED and a second tap is refused by the repository's
    // C-65 pre-check, because a request that is not yet rendered must not
    // silently disable a control with nothing to explain it.
    //
    // The successor label is resolved through the LIVE roster HERE, at render
    // time, never from a stored label: a successor who has since left degrades to
    // the unnamed variant rather than to a name the roster no longer carries.
    val waitingRoleChange = pendingRequests
        .firstOrNull {
            it.kind == me.lakko.app.chat.PendingRequestKind.ROLE_CHANGE &&
                it.state == me.lakko.app.chat.RequestState.WAITING
        }
        ?.let { hit ->
            me.lakko.app.chat.WaitingRoleChange(
                // Our own PROMOTE is not our own transfer — only a record
                // carrying a successor key is a transfer, because
                // `ProposalKind::RoleChange` carries no payload and the two are
                // otherwise indistinguishable.
                ownTransfer = hit.own && hit.transferToB64 != null,
                successorLabel = hit.transferToB64?.let { key ->
                    rosterViews.firstOrNull { it.signatureKeyB64 == key }?.label
                },
            )
        }
    // Behind the end's dialog there is no exit to offer: the group is gone.
    val exit = if (!behindEnd.exitBlock) me.lakko.app.chat.ExitBlock.Hidden else me.lakko.app.chat.exitBlock(
        me.lakko.app.chat.ExitBlockInput(
            usesProposalPath = roleTable != null,
            isOwner = isOwner,
            ownLeafKnown = ownLeaf != null,
            candidateCount = transferCandidates.size,
            waitingRoleChange = waitingRoleChange,
            ownRemovalPending = ownLeaf != null &&
                me.lakko.app.chat.rowAnnotation(pendingRequests, ownLeaf, ownLeaf) != null,
            // §6.2.7 — beats every other arm. Distinct from
            // `ownRemovalPending` above in the way that matters: that is a
            // request in flight over a group we are still fully in, this is the
            // terminal state after it landed.
            membershipEnded = membershipEnded,
        ),
    )

    if (confirmingLeave) {
        LeaveGroupDialog(
            onCancel = { confirmingLeave = false },
            onLeave = {
                confirmingLeave = false
                viewModel.onLeaveGroup()
            },
        )
    }

    if (confirmingDelete) {
        DeleteChatDialog(
            onCancel = { confirmingDelete = false },
            onDelete = {
                confirmingDelete = false
                viewModel.onDeleteChat()
                // Close the panel first, then leave the thread: the registry
                // row is gone, so the screen behind this sheet no longer has
                // anything to render.
                viewModel.toggleMembersPanel()
                onChatDeleted()
            },
        )
    }

    // Section 3 / ADR-0045 UX §6.1.5 — the transfer step is pushed over the
    // info; its back control (and the system back) cancels it and returns
    // here, which is what its Cancel button does.
    if (transferOpen && exit is me.lakko.app.chat.ExitBlock.Owner) {
        BackHandler { viewModel.cancelTransferStep() }
        Scaffold(
            modifier = modifier.fillMaxSize(),
            contentWindowInsets = WindowInsets.safeDrawing,
            topBar = {
                TopAppBar(
                    title = { Text(t("chat.transfer.action")) },
                    navigationIcon = {
                        IconButton(onClick = viewModel::cancelTransferStep) {
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
                    .padding(16.dp),
            ) {
                TransferOwnershipStep(
                    candidates = transferCandidates,
                    pickedKeyB64 = transferPick,
                    outgoing = transferOutgoing,
                    busy = busy,
                    onPick = viewModel::setTransferPick,
                    onOutgoing = viewModel::setTransferOutgoing,
                    onCancel = viewModel::cancelTransferStep,
                    onTransfer = { successor ->
                        viewModel.onTransferOwnership(successor, transferOutgoing)
                    },
                )
                // A refusal keeps the step open with the pick intact, so its
                // sentence shows here, where the person is.
                error?.let { message ->
                    Spacer(Modifier.height(8.dp))
                    Text(
                        text = message.text(),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        }
        return
    }

    BackHandler { viewModel.toggleMembersPanel() }
    // Group privacy spec section 8.1 — `Review` (or a tapped notification)
    // opens this screen scrolled to `Join requests`.
    val infoScroll = rememberScrollState()
    var joinRequestsTop by remember { mutableStateOf<Int?>(null) }
    val joinRequestScroll by viewModel.joinRequestScrollRequested.collectAsStateWithLifecycle()
    LaunchedEffect(joinRequestScroll, joinRequestsTop) {
        if (!joinRequestScroll) return@LaunchedEffect
        val top = joinRequestsTop
        if (top != null) infoScroll.animateScrollTo(top)
        viewModel.consumeJoinRequestScroll()
    }
    Scaffold(
        modifier = modifier.fillMaxSize(),
        contentWindowInsets = WindowInsets.safeDrawing,
        topBar = {
            TopAppBar(
                // conversation-info.md section 3 — the screen is the group's
                // info, titled as on web and iOS; the member count heads the
                // members list below.
                title = { Text(t("chat.info.group")) },
                navigationIcon = {
                    IconButton(onClick = viewModel::toggleMembersPanel) {
                        LkIcon(R.drawable.ic_lk_chevron_back, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { scaffoldPadding ->
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(scaffoldPadding)
                    .verticalScroll(infoScroll)
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            ) {
                // ADR-0045 (UX §6.1.9) — tell the new owner they are the new
                // owner. Panel level, dismissible, rendered once.
                //
                // Placement DIVERGES from the web reference, which puts it
                // directly below the pending notice "so §5's 'pending notice,
                // top of the panel, above everything' ordering stays literally
                // intact". That constraint does not bind here: this panel
                // already renders the add form, the invite block, the action
                // notice and the error ABOVE the pending notice, so §5's
                // ordering is not intact on Android with or without this. What
                // web was protecting — put it as high as it can go without
                // displacing the pending notice — therefore lands it directly
                // under the title, which is also above the add form and the
                // invite block, i.e. above the very affordances that just
                // appeared and that the notice exists to explain.
                newOwner?.let {
                    Spacer(Modifier.height(8.dp))
                    NewOwnerNoticeCard(notice = it, onDismiss = viewModel::dismissNewOwnerNotice)
                }
                Spacer(Modifier.height(12.dp))
                // ADR-0051 — the shared group name: visible to everyone,
                // editable by the owner and admins (the LIVE role, read by
                // `refreshMembers`), read-only for a plain member.
                //
                // Two sections, not one: `GROUP_NAME` is whether the name is
                // here at all and `RENAME_GROUP` is whether it is editable, so
                // an ended membership leaves the name READABLE and takes the
                // editing away rather than removing both.
                if (InfoSection.GROUP_NAME in sections) {
                    // ADR-0087 4.5.2 — the 96 dp header photo; the owner and
                    // admins (the people who rename) set it here.
                    val headerName by viewModel.groupName.collectAsStateWithLifecycle()
                    GroupPhotoHeader(
                        viewModel = viewModel,
                        title = headerName.ifBlank { t("chat.group.fallback_name") },
                        canSet = InfoSection.RENAME_GROUP in sections,
                    )
                    Spacer(Modifier.height(8.dp))
                    // Section 4 — the header line and the @name row
                    // (`Handle`), the owner's Who can join (`GroupType`).
                    GroupHandleRow(
                        viewModel = viewModel,
                        memberCount = members.size,
                        isOwner = InfoSection.GROUP_TYPE in sections,
                        showHandle = InfoSection.HANDLE in sections,
                    )
                    GroupNameRow(
                        viewModel = viewModel,
                        canRename = InfoSection.RENAME_GROUP in sections,
                    )
                    // reactions-and-emoji section 9 — the core's `Reactions`
                    // section (owner and admins of a group).
                    if (InfoSection.REACTIONS in sections) {
                        GroupReactionsRow(viewModel = viewModel)
                    }
                    Spacer(Modifier.height(12.dp))
                    // docs/ux/who-can-find-me.md section 5 — this group's
                    // `Members can find me` switch (absent unless the setting
                    // includes groups).
                    if (behindEnd.membersCanFindMe) {
                        me.lakko.app.ui.settings.MembersCanFindMeRow(
                            reach = me.lakko.app.reach.ReachManager.get(androidx.compose.ui.platform.LocalContext.current),
                            kind = me.lakko.app.ui.settings.FindMeScopeKind.GROUP,
                            idHex = viewModel.groupIdHex,
                        )
                    }
                }
                // ADR-0082 / translation UX 3.7 — this device only.
                me.lakko.app.ui.translation.AutoTranslateRow(viewModel.groupIdHex)
                // ADR-0028 (G-3) — owner/admin "create invite link" affordance.
                // Mints a /g/<slug> link conveying this group's id; a non-member
                // opens it to request to join, approved via the pending list.
                if (InfoSection.INVITE_LINK in sections) {
                    Spacer(Modifier.height(8.dp))
                    TextButton(
                        onClick = viewModel::onCreateInviteLink,
                        enabled = !busy,
                    ) { Text(t("chat.invite_link.create")) }
                    inviteLink?.let { link ->
                        // The caption names the link and its actions below it
                        // (web: the group labelled "Invite link"); TalkBack can
                        // jump to it as a heading.
                        Column {
                            Text(
                                text = t("chat.invite_link.label"),
                                style = MaterialTheme.typography.labelMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier
                                    .padding(horizontal = 8.dp)
                                    .semantics { heading() },
                            )
                            Text(
                                text = link,
                                style = MaterialTheme.typography.bodySmall,
                                modifier = Modifier.padding(horizontal = 8.dp),
                            )
                            Row {
                                TextButton(onClick = { clipboard.setText(AnnotatedString(link)) }) {
                                    Text(t("chat.invite_link.copy"))
                                }
                                TextButton(
                                    onClick = viewModel::onRevokeInviteLink,
                                    enabled = !busy,
                                ) { Text(t("chat.invite_link.revoke")) }
                            }
                        }
                    }
                }
                // ADR-0045 — the request accepted locally and published. Kept
                // beside the error and, like it, UNGATED: a plain member now has
                // a membership action (`Leave group`), so the feedback for one
                // must reach a member who cannot manage anybody.
                notice?.let { message ->
                    Spacer(Modifier.height(6.dp))
                    Text(
                        text = message.text(),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                error?.let { message ->
                    Spacer(Modifier.height(6.dp))
                    Text(
                        text = message.text(),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
                Spacer(Modifier.height(12.dp))
                // ADR-0045 / UX §5.1 — the pending-membership notice. Renders
                // only once something has been outstanding past the quiet
                // window, because the owner sweeps its own request immediately
                // and an affordance that appears and vanishes teaches the user
                // that membership is slow when it is not. The visibility gate
                // was already applied in the view model (UX §10): owner and
                // admins see the set, every other member sees only their own.
                //
                // ONE liveness line, not an escalating pair: ADR-0045 gives a
                // waiting request no deadline, so there is no moment at which
                // the client learns something new to say.
                pendingNotice?.let { notice ->
                    Text(text = notice.count.text(), style = MaterialTheme.typography.titleSmall)
                    Text(
                        text = notice.liveness.text(),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    // UX — a person who has been REQUESTED is not a member, so
                    // they must not appear in the roster above; they get their
                    // own line here instead.
                    val waitingAdds = me.lakko.app.chat.waitingToBeAdded(pendingRequests)
                    if (waitingAdds.isNotEmpty()) {
                        Spacer(Modifier.height(6.dp))
                        Text(
                            text = t(
                                "chat.members.pending.adds_heading",
                                emptyMap(),
                                mapOf("count" to waitingAdds.size.toULong()),
                            ),
                            style = MaterialTheme.typography.titleSmall,
                        )
                        // UX §5.2 — one row per pending add, with the same identity
                        // treatment the join-request rows already use (the short
                        // fingerprint; a prospective member has no roster entry to
                        // read a display name from), plus the secondary line.
                        waitingAdds.forEach { request ->
                            val addName = viewModel.nameOfKeyB64(request.addSignatureKeyB64)
                            // ADR-0084 — the person's photo beside their name.
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                LakkoAvatar(
                                    // Initials from the name alone, never
                                    // the ` · @username` label.
                                    name = viewModel.avatarNameOfKeyB64(request.addSignatureKeyB64),
                                    seed = request.addSignatureKeyB64 ?: addName,
                                    size = 24.dp,
                                    personKey = me.lakko.app.ui.components.personKeyBytes(request.addSignatureKeyB64),
                                )
                                Spacer(Modifier.width(8.dp))
                                Text(text = addName, style = MaterialTheme.typography.bodySmall)
                            }
                            Text(
                                text = t("chat.members.pending.waiting_added"),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                    Spacer(Modifier.height(12.dp))
                }
                // ADR-0045 S7 residual 1 — `Not applied ({n})`.
                //
                // A sweep does not necessarily carry everything it finds, and an
                // excluded request used to vanish without a trace: OpenMLS
                // empties the proposal store on every merged commit whether or
                // not the commit carried the proposal. For an approved join it
                // is worse, because a successful approve CONSUMED the retained
                // request — the admin cannot even re-approve.
                //
                // This section says the one thing that is knowable, and no more:
                // the request did not apply. The core reports no reason for an
                // exclusion (`CreateCommitError` names no proposal), so neither
                // does the copy. A TOP-LEVEL block rather than a child of the
                // pending notice above, because a `Not applied` row is exactly
                // the case where nothing is pending any more.
                if (notApplied.isNotEmpty()) {
                    Text(
                        text = me.lakko.app.chat.unappliedHeading(notApplied.size).text(),
                        style = MaterialTheme.typography.titleSmall,
                    )
                    Text(
                        text = me.lakko.app.chat.UNAPPLIED_EXPLANATION.text(),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Spacer(Modifier.height(6.dp))
                    notApplied.forEach { item ->
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(vertical = 2.dp),
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    text = viewModel.nameOfKeyB64(
                                        item.subjectKeyB64,
                                    ),
                                    style = MaterialTheme.typography.bodySmall,
                                )
                                Text(
                                    text = me.lakko.app.chat.unappliedLine(item).text(),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            TextButton(
                                onClick = {
                                    viewModel.onDismissNotApplied(item.rowKey)
                                },
                            ) { Text(t("common.dismiss")) }
                        }
                    }
                    Spacer(Modifier.height(12.dp))
                }
                // ADR-0027 (G-3) — pending external-join requests. Owner/admin
                // only (same gate as add/remove); hidden when there are none.
                // Approve drives the role-gated MLS approve + posts the commit &
                // Welcome rendezvous; Reject drops the retained proposal.
                val queueFull = remember(pendingJoins, unkeptJoinRequest) { viewModel.knockQueueFull() }
                val joinSectionShown = canManage && (pendingJoins.isNotEmpty() || unkeptJoinRequest || queueFull)
                if (!joinSectionShown && joinRequestsTop != null) joinRequestsTop = null
                if (joinSectionShown) {
                    // Section 8.1 — the requests listed here are shown.
                    if (pendingJoins.isNotEmpty()) {
                        LaunchedEffect(pendingJoins.size) { viewModel.onJoinRequestsShown() }
                    }
                    var confirmRejectAll by remember { mutableStateOf(false) }
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.onGloballyPositioned { coords ->
                            val y = coords.positionInParent().y.toInt()
                            if (joinRequestsTop != y) joinRequestsTop = y
                        },
                    ) {
                        Text(
                            text = if (queueFull || pendingJoins.size >= 64) {
                                t("chat.members.join.heading_full")
                            } else {
                                t("chat.members.join.heading", emptyMap(), mapOf("count" to pendingJoins.size.toULong()))
                            },
                            style = MaterialTheme.typography.titleSmall,
                            modifier = Modifier.weight(1f),
                        )
                        // ADR-0087 section 5 — Reject all, behind a confirmation.
                        TextButton(
                            onClick = { confirmRejectAll = true },
                            enabled = !busy,
                            colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
                        ) { Text(t("chat.members.join.reject_all")) }
                    }
                    if (queueFull) {
                        Text(
                            text = t("chat.members.join.more_waiting"),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    if (confirmRejectAll) {
                        LakkoDialog(
                            onDismissRequest = { confirmRejectAll = false },
                            title = t("chat.members.join.reject_all_title"),
                            body = t("chat.members.join.reject_all_body"),
                            confirm = LakkoDialogAction(t("chat.members.join.reject_all"), LakkoDialogTone.Danger) {
                                confirmRejectAll = false
                                viewModel.onRejectAllJoins()
                            },
                            cancel = LakkoDialogAction(t("common.cancel"), LakkoDialogTone.Ghost) { confirmRejectAll = false },
                        )
                    }
                    // ADR-0027 / UX §7.1 — a request the quarantine turned
                    // away. It is in no row below, because the core never kept
                    // it, and it gets no Approve for the same reason: there is
                    // nothing to approve. Informational only, which is the most
                    // `externalJoinAffordance` allows.
                    if (unkeptJoinRequest) {
                        Text(
                            text = me.lakko.app.chat.JOIN_REQUEST_NOT_KEPT.text(),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Spacer(Modifier.height(6.dp))
                    pendingJoins.forEach { ref ->
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(vertical = 2.dp),
                        ) {
                            // ADR-0084 — the joiner's photo, through the same
                            // key their name resolves by.
                            LakkoAvatar(
                                name = viewModel.pendingJoinAvatarName(ref),
                                seed = java.util.Base64.getEncoder().encodeToString(ref),
                                size = 28.dp,
                                personKey = viewModel.pendingJoinPersonKey(ref),
                            )
                            Spacer(Modifier.width(8.dp))
                            Column(modifier = Modifier.weight(1f)) {
                                // ADR-0048 — a join request names the joiner
                                // through the one resolver (usually the
                                // fingerprint: a stranger's profile has not
                                // reached this device).
                                Text(
                                    // ADR-0053 — named through the joiner's
                                    // account key, where contacts live.
                                    text = viewModel.pendingJoinLabel(ref),
                                    style = MaterialTheme.typography.bodySmall,
                                )
                                // group-and-channel-privacy.md section 8 — one
                                // line: `Asked 2 hours ago`, or from a link
                                // `Invited by Anna · 2 hours ago`. The sealed
                                // knock row gave both; an older attribution
                                // envelope names the issuer only.
                                val attr = joinAttributions[
                                    ref.joinToString("") { "%02x".format(it.toInt() and 0xff) },
                                ]
                                val strings = LocalStrings.current
                                val settingsVersion by me.lakko.app.chat.GroupSettingsStore.version.collectAsStateWithLifecycle()
                                val origin = remember(ref.contentHashCode(), settingsVersion) {
                                    viewModel.joinRequestOrigin(ref)
                                }
                                val line = me.lakko.app.chat.joinRequestLine(
                                    origin = origin,
                                    attributionIssuerB64 = attr?.issuerKeyB64,
                                    nameOf = viewModel::nameOf,
                                    whenOf = { at ->
                                        me.lakko.app.ui.channels.relativeTimeLabel(
                                            at,
                                            System.currentTimeMillis(),
                                            java.time.ZoneId.systemDefault(),
                                            java.util.Locale.getDefault(),
                                            strings,
                                        )
                                    },
                                )
                                if (line != null) {
                                    Text(
                                        text = line.text(),
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                            }
                            TextButton(
                                onClick = { viewModel.onApproveJoin(ref) },
                                enabled = !busy,
                            ) { Text(t("chat.members.join.approve")) }
                            TextButton(
                                onClick = { viewModel.onRejectJoin(ref) },
                                enabled = !busy,
                            ) { Text(t("chat.members.join.reject")) }
                        }
                    }
                    Spacer(Modifier.height(12.dp))
                }
                // ADR-0045 UX §6.2.7 — the roster STAYS, read-only, and carries
                // one line saying so.
                //
                // It stays because it is the last thing you knew about who you
                // were in a group with, it is genuinely informative next to a
                // frozen transcript, and it is still readable: `members()` is a
                // plain read of the public group with no state gate.
                //
                // The line is a statement about the LIST, not about any row in
                // it, and that is load-bearing rather than stylistic. A
                // `Remove` rewrites no `GroupContext`, so the role table is
                // untouched and a removed admin's frozen roster still names
                // them an admin — nothing here may present that as a live role.
                // "Stopped updating" is the literal truth (the local view is
                // frozen at the removing epoch and never moves again) rather
                // than a euphemism for it, and it is deliberately neutral
                // between having left and having been removed.
                if (behindEnd.frozenRosterLine) {
                    Text(
                        text = me.lakko.app.chat.FROZEN_ROSTER_HEADER.text(),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Spacer(Modifier.height(6.dp))
                }
                // docs/ux/conversation-info.md 2.2 — the roster is a SECTION,
                // and it is the one action-bearing block an ended membership
                // KEEPS: `members()` is a plain read of the public group with
                // no state gate, and the last thing you knew about who you were
                // in a group with is worth keeping next to a frozen transcript.
                // The per-row `Remove` below stays on `canManage` rather than on
                // a section, because the rule names the LIST and says nothing
                // about what a row carries.
                if (InfoSection.MEMBER_LIST in sections) {
                    Spacer(Modifier.height(8.dp))
                    Text(
                        text = t("chat.members.heading", emptyMap(), mapOf("count" to members.size.toULong())),
                        style = MaterialTheme.typography.titleSmall,
                        modifier = Modifier.semantics { heading() },
                    )
                    // Section 2.4 — the one way to add people: the FIRST row of
                    // the members list, for whoever may add (the same section the
                    // rule grants the add to).
                    if (InfoSection.ADD_MEMBER in sections) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable(enabled = !busy, onClick = viewModel::openAddMembers)
                                .padding(vertical = 8.dp),
                        ) {
                            LkIcon(
                                R.drawable.ic_lk_person_plus,
                                contentDescription = null,
                                tint = MaterialTheme.colorScheme.primary,
                                modifier = Modifier.size(32.dp).padding(4.dp),
                            )
                            Spacer(Modifier.width(10.dp))
                            Text(
                                text = t("chat.members.add.action"),
                                color = MaterialTheme.colorScheme.primary,
                                style = MaterialTheme.typography.bodyLarge,
                            )
                        }
                    }
                    // The whole screen scrolls, so the roster is a plain column
                    // (a group's roster is bounded by MLS, and a lazy list inside
                    // a scrolling column would need a fixed height).
                    // The own row's initials come from the person's own name
                    // (their profile), never from the word "You" the row reads.
                    val ownInitialsName = me.lakko.app.ui.components.rememberOwnAvatarName("?")
                    Column {
                        members.forEach { member ->
                            // Section 2.2 — a member who is a contact (and not
                            // this account) opens the contact screen.
                            val memberKeyB64 = me.lakko.app.chat.outboxB64(member.signaturePublicKey)
                            val opensContact = me.lakko.app.chat.memberOpensContact(
                                memberKeyB64 = memberKeyB64,
                                ownRow = ownLeaf == member.leafIndex,
                                contactKeysB64 = contactKeysB64,
                            )
                            Row(
                                verticalAlignment = Alignment.CenterVertically,
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .then(
                                        if (opensContact) {
                                            Modifier.clickable {
                                                onOpenContact(
                                                    me.lakko.app.ui.contacts.contactRouteKey(member.signaturePublicKey),
                                                )
                                            }
                                        } else {
                                            Modifier
                                        },
                                    )
                                    .padding(vertical = 4.dp),
                            ) {
                                // ADR-0045 — NO optimistic update: a member with a
                                // pending removal STAYS in the roster, annotated.
                                // Dropping the row would assert they are gone while
                                // they are still cryptographically in the group —
                                // still receiving every message, still able to
                                // decrypt — until a committer sweeps. That is the
                                // membership equivalent of a green E2EE pill over an
                                // unencrypted call.
                                val annotation = me.lakko.app.chat.rowAnnotation(
                                    pendingRequests,
                                    member.leafIndex,
                                    ownLeaf,
                                )
                                // ADR-0084 — a person's photo beside their name;
                                // the own row shows the owner's own photo.
                                val memberRow = membersNameVersion.let { viewModel.memberRowText(member) }
                                val memberName = memberRow.title
                                val isOwnRow = ownLeaf == member.leafIndex
                                // ADR-0098 section 2 — a visible member row: the
                                // directory is asked about them when a check is due.
                                // Keyed on the account id their profile claims, so a
                                // member who is not a contact and whose profile
                                // arrives after the row was drawn is asked about then.
                                val checkKey = membersNameVersion.let {
                                    if (isOwnRow) null else viewModel.memberCheckKey(memberKeyB64)
                                }
                                LaunchedEffect(checkKey) {
                                    if (checkKey != null) viewModel.notePersonOnScreen(checkKey.personKeyB64)
                                }
                                LakkoAvatar(
                                    // Initials from the name alone, never
                                    // the ` · @username` label.
                                    name = if (isOwnRow) ownInitialsName else viewModel.memberAvatarName(member),
                                    seed = java.util.Base64.getEncoder().encodeToString(member.signaturePublicKey),
                                    size = 32.dp,
                                    personKey = if (isOwnRow) null else member.signaturePublicKey,
                                    avatar = if (isOwnRow) me.lakko.app.ui.components.rememberOwnAvatar() else null,
                                )
                                Spacer(Modifier.width(10.dp))
                                // conversation-info.md section 2.2 / ADR-0022 — the
                                // row's role mark and which of Remove / Make admin
                                // this reader gets on it; never anything on the
                                // owner's row (`memberRowActions`).
                                val rowActions = me.lakko.app.chat.memberRowActions(
                                    viewerRole = when {
                                        roleTable == null -> null
                                        isOwner -> me.lakko.app.chat.MemberRole.OWNER
                                        canManage -> me.lakko.app.chat.MemberRole.ADMIN
                                        else -> me.lakko.app.chat.MemberRole.MEMBER
                                    },
                                    rowRole = me.lakko.app.chat.rowRoleOf(
                                        member.signaturePublicKey,
                                        roleTable?.owner,
                                        roleTable?.admins.orEmpty(),
                                    ),
                                    ownRow = isOwnRow,
                                    membershipEnded = membershipEnded,
                                )
                                Column(modifier = Modifier.weight(1f)) {
                                    val memberSuspended = !isOwnRow && member.accountKey.size == 32 &&
                                        java.util.Base64.getEncoder().encodeToString(member.accountKey) in suspendedMembers
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        Text(text = memberName, modifier = Modifier.weight(1f, fill = false))
                                        // Section 2.2 — "Owner" / "Admin" after the name, muted.
                                        rowActions.mark?.let { mark ->
                                            Spacer(Modifier.width(6.dp))
                                            Text(
                                                text = t(
                                                    if (mark == me.lakko.app.chat.MemberRole.OWNER) {
                                                        "chat.members.owner.marker"
                                                    } else {
                                                        "chat.members.admin.marker"
                                                    },
                                                ),
                                                style = MaterialTheme.typography.bodySmall,
                                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                            )
                                        }
                                        // UX 3 — "Suspended" after the name, muted.
                                        if (memberSuspended) {
                                            Spacer(Modifier.width(6.dp))
                                            Text(
                                                text = t("suspended.member"),
                                                style = MaterialTheme.typography.bodySmall,
                                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                            )
                                        }
                                    }
                                    // profile-names.md §3 — the verified username
                                    // under the name, once.
                                    memberRow.subtitle?.let {
                                        Text(
                                            text = it,
                                            style = MaterialTheme.typography.bodyMedium,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                    }
                                    annotation?.let {
                                        Text(
                                            text = it.text(),
                                            style = MaterialTheme.typography.bodySmall,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                    }
                                }
                                // ADR-0022 — owner-only promote / demote toggle;
                                // never the owner's own row. `ROLES` is OWNER-only
                                // in the shared rule and must never widen to
                                // admins: a role-table change is owner-only and
                                // every peer refuses an admin's. Which ROW carries
                                // one is `memberRowActions`.
                                val toggle = rowActions.adminToggle
                                if (InfoSection.ROLES in sections && toggle != null) {
                                    val isAdmin = toggle == me.lakko.app.chat.AdminToggle.TAKE
                                    TextButton(
                                        onClick = { viewModel.onToggleAdmin(member, !isAdmin) },
                                        enabled = !busy,
                                    ) { Text(t(if (isAdmin) "chat.members.admin.remove" else "chat.members.admin.make")) }
                                }
                                // ADR-0022 — the owner removes anyone else, an
                                // admin plain members only, a member nobody; an
                                // open group anyone (`memberRowActions`).
                                if (canManage && rowActions.remove) {
                                    TextButton(
                                        // proposal-based-membership.md section 6,
                                        // "No confirmation" (2026-11-04): Remove
                                        // acts on the tap; a removal that waits for
                                        // another committer says so on the row.
                                        onClick = { viewModel.onRemoveMember(member.leafIndex) },
                                        // UX §5.3 — a member with a pending removal keeps
                                        // Remove VISIBLE but DISABLED. Hiding it would read
                                        // as the permission having vanished; the annotation
                                        // above explains the disabled state.
                                        enabled = !busy && annotation == null,
                                    ) { Text(t("common.remove")) }
                                }
                            }
                        }
                    }
                }
                // ADR-0059 section 6 — share the listed channels this person
                // follows with this group. Not a block of the shared info rule
                // (`lakko-ui-rules`), like web: shown in every group this
                // device is still a member of.
                if (!membershipEnded) {
                    ShareChannelsSwitch(viewModel)
                }
                // ADR-0045 (UX §6.1.1) — THE EXIT BLOCK: one block at the
                // panel's bottom whose meaning is *the exit*, holding two
                // ROLE-DISJOINT controls of which exactly one is ever live.
                //
                // `Transfer ownership` is owner-only and comes FIRST, because it
                // is the precondition and reads first. It is NOT
                // destructive-styled at rest: red would file it with `Remove` and
                // `Leave group`, and an owner scanning for a way out should not
                // have to read a warning colour to find one. Transfer is not a
                // destruction; it is a handover that happens to be final, and
                // finality is a property of the confirmation below it, not of the
                // button that opens it.
                //
                // The owner's `Leave group` used to be HIDDEN, and this reverses
                // that — not because the old argument (a control that always
                // refuses is worse than no control) was wrong, but because it was
                // right only while transfer was unreachable. A disabled control
                // whose reason names the action directly above it is a signposted
                // route, and it is the same visible-but-disabled pattern §5.3
                // already uses for a pending row's `Remove`. Hiding it now would
                // leave an owner looking for the exit finding nothing at all
                // about leaving.
                //
                // The block is computed above from Compose state reads so it
                // recomposes; the rule itself lives in `me.lakko.app.chat` where
                // the host suite can run it.
                //
                // ## Why `sections` is NOT the gate here, alone in this panel
                //
                // Every other block above asks `groupInfoSections` whether it
                // belongs. This one does not, and the reason is arithmetic
                // rather than preference: `group_info_sections` pushes
                // `LEAVE_GROUP` on every live path where `canLeave` holds and
                // `DELETE_CHAT` on every ended one, so on every input that
                // reaches a non-`Hidden` arm the list holds at least one of
                // this block's three sections.
                // Wrapping the `when` in a test of them would be a condition
                // that is true on every input — a gate in shape only, and one
                // that would read as protection it does not provide.
                //
                // Per-arm gating adds nothing either, because on every arm the
                // machine is already at least as narrow as the rule: `Ended`
                // fires on exactly the `membershipEnded` that produces
                // `DELETE_CHAT`, and `Owner` on exactly the `isOwner` that
                // produces `TRANSFER_OWNERSHIP`.
                //
                // `Hidden` used to be a real difference: a group with no role
                // table, or a torn own-leaf read, where the rule said
                // `LEAVE_GROUP` and this machine showed nothing. It was
                // reported rather than absorbed, and the report is why the
                // rule gained `canLeave` — fed a few lines above from exactly
                // these two reads. A rule both clients had to override in
                // their own view was a rule nobody was following; now the two
                // agree, and `Hidden` is precisely where the rule returns
                // neither section.
                //
                // What the machine decides on top — which of the two controls
                // is live, and the sentence explaining the dead one — the
                // section rule says nothing about and never will: a section
                // list answers "is this block here", not "is this control
                // enabled and why not".
                // ADR-0099 — Report, first in the exit block, for anyone but
                // the owner and only for a group with an @name (the rule).
                if (InfoSection.REPORT in sections) {
                    val handle = viewModel.groupHandle()?.handle
                    if (handle != null) {
                        Spacer(Modifier.height(12.dp))
                        TextButton(
                            onClick = {
                                me.lakko.app.ui.moderation.ModerationRequests.openReport(
                                    me.lakko.app.ui.moderation.ReportRequest(
                                        subject = me.lakko.app.moderation.groupReportSubject(
                                            handle = handle,
                                            name = viewModel.groupName.value.ifBlank { "@$handle" },
                                            groupIdHex = viewModel.groupIdHex,
                                        ),
                                        leaveOrUnsubscribe = { viewModel.onLeaveGroup() },
                                    ),
                                )
                            },
                            enabled = !busy,
                            colors = ButtonDefaults.textButtonColors(
                                contentColor = MaterialTheme.colorScheme.error,
                            ),
                        ) { Text(t("report.action")) }
                    }
                }
                when (exit) {
                    me.lakko.app.chat.ExitBlock.Hidden -> Unit
                    // ADR-0045 UX §6.2.4 / §6.2.7 — once the membership has
                    // ended, both of the other controls are gone (one names a
                    // group the reader is not in, the other is owner-only and
                    // they are not) and the block holds `Delete this chat`
                    // instead.
                    //
                    // This slot is the right home by §6.1.1's own argument: the
                    // block's meaning is *the exit*, and once you are out the
                    // only exit-shaped action left is closing the door behind
                    // you. A permanently dead row with no action available is
                    // its own defect — everything else about this state is "you
                    // cannot", and this is the one thing they still can.
                    me.lakko.app.chat.ExitBlock.Ended -> {
                        Spacer(Modifier.height(12.dp))
                        TextButton(
                            onClick = { confirmingDelete = true },
                            enabled = !busy,
                            colors = ButtonDefaults.textButtonColors(
                                contentColor = MaterialTheme.colorScheme.error,
                            ),
                        ) { Text(me.lakko.app.chat.DELETE_CHAT_CONTROL.text()) }
                    }
                    is me.lakko.app.chat.ExitBlock.Owner -> if (InfoSection.DELETE_GROUP in sections) {
                        // ADR-0087 section 10 — an owner alone deletes the
                        // group instead of transferring and leaving.
                        Spacer(Modifier.height(12.dp))
                        DeleteGroupControl(viewModel = viewModel)
                    } else {
                        Spacer(Modifier.height(12.dp))
                        TextButton(
                            onClick = viewModel::openTransferStep,
                            enabled = !busy && exit.transfer.enabled && !transferOpen,
                        ) { Text(t("chat.transfer.action")) }
                        // The reason is part of the control's description, not a
                        // decoration: a reader who cannot see the greyed button
                        // must still be told why.
                        exit.transfer.disabledReason?.let {
                            Text(
                                text = it.text(),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        TextButton(
                            onClick = { },
                            enabled = false,
                            colors = ButtonDefaults.textButtonColors(
                                contentColor = MaterialTheme.colorScheme.error,
                            ),
                        ) { Text(t("chat.leave.action")) }
                        exit.leave.disabledReason?.let {
                            Text(
                                text = it.text(),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                    is me.lakko.app.chat.ExitBlock.Member -> {
                        Spacer(Modifier.height(12.dp))
                        TextButton(
                            onClick = { confirmingLeave = true },
                            // UX §5.3 — visible but disabled once we have asked.
                            // Hiding it then would read as the permission having
                            // vanished, where our own row's "Waiting to leave."
                            // annotation explains a disabled control.
                            enabled = !busy && exit.leave.enabled,
                            colors = ButtonDefaults.textButtonColors(
                                contentColor = MaterialTheme.colorScheme.error,
                            ),
                        ) { Text(t("chat.leave.action")) }
                        exit.leave.disabledReason?.let {
                            Text(
                                text = it.text(),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
                // ADR-0045 (UX §6.1.4 / §6.1.5) — the transfer step, inline
                // beneath the control that opened it. See
                // [TransferOwnershipStep] for why it is not a second `Dialog`.
                Spacer(Modifier.height(16.dp))
            }
    }
}

// =============================================================================
// UX section 8 — selection mode
// =============================================================================

/**
 * What the selection mark names a row by: the message's text, or its kind
 * when it has none (a photo, a voice message), so a screen reader can tell
 * one mark from the next.
 */
private fun selectionRowLabel(msg: ChatMessage): String =
    if (msg.unsupportedVersion != null) {
        me.lakko.app.l10n.Localization.strings.value[UNSUPPORTED_MESSAGE_KEY]
    } else msg.text.ifEmpty {
        msg.attachments.firstOrNull()
            ?.let { mediaPreviewLabel(PreviewAttachment(kind = it.kind, mime = it.mime)) }
            ?: me.lakko.app.l10n.Localization.strings.value["chat.reply.fallback_excerpt"]
    }

/** Diameter of the selection mark; also its tap-friendly footprint inside the
 *  row (the whole row is the target, so the mark itself needs no 40dp). */
private val SELECTION_MARK_SIZE: Dp = 26.dp

/** Gap between the mark and the row it belongs to. */
private val SELECTION_MARK_GAP: Dp = 8.dp

/**
 * Section 8 — a timeline row while selecting: the mark at the leading edge
 * and the WHOLE row as the toggle, a tap and a long press alike. Outside
 * selection mode it is the row itself, untouched.
 *
 * The toggle is a transparent layer drawn OVER the row rather than a click
 * modifier on it: the bubble's own controls (an attachment, a quote, a
 * forward line) each answer a tap, and a layer above them is what keeps a
 * tap anywhere on the row a toggle and nothing else. A row that cannot be
 * acted on gets the layer too, so its controls stay quiet while selecting,
 * but no mark and no answer.
 */
@Composable
private fun SelectableTimelineRow(
    selecting: Boolean,
    selectable: Boolean,
    selected: Boolean,
    markLabel: String,
    onToggle: () -> Unit,
    content: @Composable () -> Unit,
) {
    if (!selecting) {
        content()
        return
    }
    Box(Modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (selectable) {
                SelectionMark(selected = selected)
            } else {
                Spacer(Modifier.width(SELECTION_MARK_SIZE))
            }
            Spacer(Modifier.width(SELECTION_MARK_GAP))
            Box(Modifier.weight(1f)) { content() }
        }
        Box(
            modifier = Modifier
                .matchParentSize()
                .then(
                    if (selectable) {
                        Modifier
                            .semantics {
                                role = Role.Checkbox
                                contentDescription = markLabel
                                this.selected = selected
                                stateDescription = me.lakko.app.l10n.Localization.strings.value[
                                    if (selected) {
                                        me.lakko.app.chat.SELECTED_STATE_KEY
                                    } else {
                                        me.lakko.app.chat.NOT_SELECTED_STATE_KEY
                                    },
                                ]
                            }
                            .combinedClickable(
                                onClick = onToggle,
                                onLongClick = onToggle,
                            )
                    } else {
                        // Swallows the tap so nothing under it answers; says
                        // nothing, because there is nothing to select.
                        Modifier.combinedClickable(
                            interactionSource = remember { MutableInteractionSource() },
                            indication = null,
                            onClick = {},
                            onLongClick = {},
                        )
                    },
                ),
        )
    }
}

/** Section 8 — an empty circle, filled with a check when selected. */
@Composable
private fun SelectionMark(selected: Boolean) {
    if (selected) {
        LkIcon(
            icon = R.drawable.ic_lk_checkmark_circle_fill,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.primary,
            size = SELECTION_MARK_SIZE,
        )
    } else {
        LkIcon(
            icon = R.drawable.ic_lk_circle,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            size = SELECTION_MARK_SIZE,
        )
    }
}

/**
 * Section 8 — the selection bar in place of the header: a leading `Cancel`,
 * the count in the middle, nothing trailing.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SelectionBar(
    countLabel: String,
    onCancel: () -> Unit,
) {
    TopAppBar(
        navigationIcon = {
            TextButton(onClick = onCancel) { Text(me.lakko.app.chat.SELECTION_CANCEL_LABEL.text()) }
        },
        title = {
            Text(
                text = countLabel,
                style = MaterialTheme.typography.titleMedium,
                maxLines = 1,
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            )
        },
    )
}

/**
 * Section 8 — the action bar in place of the composer: `Forward` and
 * `Delete`, each with its icon and label, and one helper line under them
 * — "Only your own messages can be deleted." while Delete is held back, or
 * the progress copy after a run stopped short.
 *
 * The same elevated surface and hairline top as the composer it stands in
 * for (design system: composer = elevated surface, hairline top).
 */
/** ADR-0092 section 7 — whether the person behind a card's account key lets
 *  others share their card, per the newest profile this device holds. */
private fun cardShareableOf(context: android.content.Context): (ByteArray) -> Boolean = { key ->
    me.lakko.app.profile.ProfileDirectory.get(context)
        .isCardShareable(java.util.Base64.getEncoder().encodeToString(key))
}

@Composable
private fun SelectionActionBar(
    selection: me.lakko.app.chat.MessageSelection,
    busy: Boolean,
    status: UiText?,
    onForward: () -> Unit,
    onDelete: () -> Unit,
    /** Saved section 2 — Select mode's Save, beside Delete and Forward. */
    onSave: () -> Unit = {},
    /** ADR-0092 / UX 11a — `false` when the selection holds a message that is
     *  never forwarded (a poll, a checklist, a card that may not be shared). */
    forwardAllowed: Boolean = true,
) {
    Surface(color = MaterialTheme.colorScheme.surface) {
        Column(Modifier.fillMaxWidth()) {
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            if (busy) {
                LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
            }
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 8.dp, vertical = 4.dp),
                horizontalArrangement = Arrangement.SpaceEvenly,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextButton(
                    onClick = onForward,
                    enabled = selection.forwardEnabled && forwardAllowed && !busy,
                    modifier = Modifier.weight(1f),
                ) {
                    LkIcon(
                        icon = R.drawable.ic_lk_forward,
                        contentDescription = null,
                        size = LkIconSize.Row,
                    )
                    Spacer(Modifier.width(8.dp))
                    Text(t("chat.message.forward"))
                }
                TextButton(
                    onClick = onSave,
                    enabled = selection.saveEnabled && !busy,
                    modifier = Modifier.weight(1f),
                ) {
                    LkIcon(
                        icon = R.drawable.ic_lk_bookmark,
                        contentDescription = null,
                        size = LkIconSize.Row,
                    )
                    Spacer(Modifier.width(8.dp))
                    Text(t("chat.select.save"))
                }
                TextButton(
                    onClick = onDelete,
                    enabled = selection.deleteEnabled && !busy,
                    modifier = Modifier.weight(1f),
                    // The destructive action in the platform's destructive
                    // colour; Material's own disabled colour when held back.
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                ) {
                    LkIcon(
                        icon = R.drawable.ic_lk_trash,
                        contentDescription = null,
                        size = LkIconSize.Row,
                    )
                    Spacer(Modifier.width(8.dp))
                    Text(t("common.delete"))
                }
            }
            // One helper slot: the progress copy after a failure outranks the
            // gate's explanation, because it is the newer fact.
            val helper = status ?: selection.deleteHelper
            if (helper != null) {
                Text(
                    text = helper.text(),
                    style = MaterialTheme.typography.bodySmall,
                    color = if (status != null) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 4.dp)
                        .semantics { liveRegion = LiveRegionMode.Polite },
                )
            }
        }
    }
}


/**
 * "Share my channels with this group" (docs/ux/feed.md 6.3, ADR-0059 section
 * 6). Off by default. Turning it on sends this person's listed channels to the
 * group, end-to-end encrypted; off sends an empty list. The switch changes only
 * once the send succeeded. Mirror of web `ShareChannelsSwitch.tsx`.
 */
@Composable
private fun ShareChannelsSwitch(viewModel: ChatViewModel) {
    val isGroup = remember(viewModel.groupIdHex) { viewModel.isGroupConversation() }
    if (!isGroup) return
    LaunchedEffect(viewModel.groupIdHex) { viewModel.openSharedChannels() }
    val row by viewModel.sharedChannelsRow.collectAsStateWithLifecycle()
    val busy by viewModel.shareChannelsBusy.collectAsStateWithLifecycle()
    val error by viewModel.shareChannelsError.collectAsStateWithLifecycle()
    val on = viewModel.groupIdHex in row.shareOn
    Column(modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
            Text(
                me.lakko.app.discover.DiscoverCopy.SHARE_CHANNELS_LABEL.text(),
                style = MaterialTheme.typography.bodyLarge,
                modifier = Modifier.weight(1f),
            )
            androidx.compose.material3.Switch(
                checked = on,
                enabled = !busy,
                onCheckedChange = { viewModel.setShareChannels(it) },
            )
        }
        Text(
            me.lakko.app.discover.DiscoverCopy.SHARE_CHANNELS_LINE.text(),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        error?.let {
            Text(it.text(), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
        }
    }
}

/** ADR-0099 / UX 1.2 — "You blocked {name}." with Unblock, in place of the composer. */
@Composable
private fun BlockedBanner(name: String, onUnblock: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .navigationBarsPadding()
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        Text(
            t("block.banner", mapOf("name" to name)),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.weight(1f),
        )
        TextButton(onClick = onUnblock) { Text(t("block.unblock")) }
    }
}

/** ADR-0098 — whether the author of the message with this DS id is shown as
 *  Deleted account (a quote then names them so). `false` outside a chat. */
internal val LocalQuotedAuthorDeleted =
    androidx.compose.runtime.staticCompositionLocalOf<(Long) -> Boolean> { { false } }
