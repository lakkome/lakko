// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

@file:OptIn(
    androidx.compose.foundation.ExperimentalFoundationApi::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
)

package me.lakko.app.ui.chat

import android.content.Context
import android.content.Intent
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
import androidx.compose.foundation.combinedClickable
import androidx.compose.animation.animateColorAsState
import androidx.compose.material.icons.filled.Close
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
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
import androidx.compose.material.icons.automirrored.filled.Forward
import androidx.compose.foundation.clickable
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.AttachFile
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.DoneAll
import androidx.compose.material.icons.filled.Mood
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.Scaffold
import androidx.compose.material3.TopAppBar
import me.lakko.app.ui.components.LakkoAvatar
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
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
import androidx.compose.ui.layout.positionOnScreen
import androidx.compose.ui.unit.toSize
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
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
import android.text.format.DateFormat
import java.time.ZoneId
import java.util.Locale
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.selected
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
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
import uniffi.lakko_ffi_uniffi.GroupInfoCapabilities
import uniffi.lakko_ffi_uniffi.InfoSection
import uniffi.lakko_ffi_uniffi.callHoldsCapture
import uniffi.lakko_ffi_uniffi.groupInfoSections

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
    val listState = rememberLazyListState()
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    // ADR-0045 UX §6.2 — has this device's own membership ended, and (if so)
    // did it ask to go? Every rule and every string lives in
    // `me.lakko.app.chat.MembershipEnded`; this screen renders its verdicts.
    // The state is TERMINAL, so nothing here needs invalidating on a timer.
    val membership by viewModel.membership.collectAsStateWithLifecycle()
    val membershipEnded = me.lakko.app.chat.membershipEnded(membership)

    // 1.19c4 — preview-attachment sheet state. When the user
    // taps "Open" on an image chip we resolve the bytes off
    // the main thread + populate this, which surfaces an
    // AlertDialog with the decoded bitmap.
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
    // UX section 3 — the persisted kind decides the header shape: a group made
    // through the new-group flow is a group even while it still has two people.
    val conversationKind = remember(state.groupIdHex, otherMembers) { viewModel.conversationKind() }
    val isGroupChat = conversationKind == ConversationKind.Group || otherMembers.size > 1
    // UX 5.2 — the second header line of a 1x1 chat is the verified @username.
    val peerUsername = remember(state.groupIdHex, nameVersion, otherMembers) {
        viewModel.oneToOnePeerUsername()
    }
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
            otherMemberCount = otherMembers.size,
            groupIdHex = state.groupIdHex,
            peerRouteKey = peerRouteKey,
            contacts = contactLinks,
        )
    }
    // UX 5.2 — the timeline's clock: DS `created_at` rendered in the device's
    // 12/24-hour format; date separators relative to now.
    val zone = remember { ZoneId.systemDefault() }
    val locale = remember { Locale.getDefault() }
    val use24h = remember(context) { DateFormat.is24HourFormat(context) }
    val timeline = remember(state.messages, zone, use24h) {
        buildTimeline(state.messages, System.currentTimeMillis(), zone, use24h, locale)
    }
    // ADR-0052 decision 3 / UX 4.1 — the pins. The bar is DERIVED from the two
    // stores and the held messages, never stored: a pin whose message this
    // device does not hold, or holds deleted, is simply not in the list.
    val sharedPins by viewModel.sharedPins.collectAsStateWithLifecycle()
    val personalPins by viewModel.personalPins.collectAsStateWithLifecycle()
    val mayPinShared by viewModel.mayPinForEveryone.collectAsStateWithLifecycle()
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
        val index = timeline.indexOfFirst { it is TimelineItem.Row && it.message.messageId == target }
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
            msg.direction == Direction.Outgoing -> "You"
            !isGroupChat && peerName != null -> peerName
            else -> msg.authorKey?.let(viewModel::nameOf) ?: viewModel.nameOfKeyB64(null)
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
        // Each item gets its upload chip at once in the view model, which
        // reads, previews and uploads it (UX spec §10).
        viewModel.onPickedUris(uris)
    }
    // UX spec §10 — the gallery pick for photos and videos, next to the
    // document pick above. Both paths attach size + thumb when the MIME type
    // is an allowlisted image or video.
    val pickVisualLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.PickMultipleVisualMedia(),
    ) { uris: List<Uri> ->
        // Each item gets its upload chip at once in the view model, which
        // reads, previews and uploads it (UX spec §10).
        viewModel.onPickedUris(uris)
    }

    LaunchedEffect(Unit) {
        // Claimed for this effect's lifetime, FIRST, so the app-level
        // all-groups poll hands this group to this screen (and stops holding
        // its in-flight slot) before the refresh below runs.
        viewModel.claimScreenPoll()
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
            // logout). Errors back off via [POLL_RETRY_BACKOFF_MS]
            // so we don't tight-loop against a 503.
            while (isActive) {
                // ADR-0045 UX §6.2.8 item 1 — the poll STOPS for a group this
                // device is no longer in. The repository gates `pollGroup` too,
                // which is the authority; this loop must break as well because that
                // gate makes the call return IMMEDIATELY, and a `while (isActive)`
                // around an instant return is a spin, not a poll. Checked at the
                // top so a membership that ends mid-window exits on the next pass.
                if (viewModel.membershipEnded.value) break
                try {
                    // A poll that finds another poll of this group in flight
                    // JOINS it (waits for its apply) instead of returning at
                    // once, so calling straight back in neither spins on the
                    // latch nor sleeps a blind interval. Only an UNAVAILABLE
                    // answer (no session / no record) would return instantly
                    // again, so that one backs off.
                    if (viewModel.pollOnceLongPoll() == GroupPollOutcome.UNAVAILABLE) {
                        delay(POLL_RETRY_BACKOFF_MS)
                    }
                } catch (cancel: kotlinx.coroutines.CancellationException) {
                    throw cancel
                } catch (_: Throwable) {
                    delay(POLL_RETRY_BACKOFF_MS)
                }
            }
        } finally {
            viewModel.releaseScreenPoll()
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
    val pendingMedia by viewModel.pendingMedia.collectAsStateWithLifecycle()
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
    var emojiDataset by remember { mutableStateOf<me.lakko.app.media.EmojiDataset?>(null) }
    LaunchedEffect(picker != null) {
        if (picker != null && emojiDataset == null) {
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
    LaunchedEffect(timeline.size, pendingMedia.size) {
        // Auto-scroll to the latest row on every new message
        // (Outgoing send + Incoming poll both bump the count, and so does an
        // outgoing media bubble entering the timeline).
        val total = timeline.size + pendingMedia.size
        if (total > 0) {
            if (positionedOnce) {
                listState.animateScrollToItem(total - 1)
            } else {
                listState.scrollToItem(total - 1)
                positionedOnce = true
            }
        }
    }

    val headerTitle = if (isGroupChat) {
        groupTitle ?: peerName ?: "Group"
    } else {
        peerName ?: groupTitle ?: "Group"
    }
    // UX 5.2 — "typing..." while a typing indicator is live (ADR-0024; never on
    // a dead group), else "@username" in a 1x1, else "N members" in a group.
    val headerSubtitle = when {
        !state.typingText.isNullOrEmpty() && !membershipEnded -> state.typingText
        isGroupChat -> "${otherMembers.size + 1} members"
        peerUsername != null -> "@$peerUsername"
        else -> null
    }
    Scaffold(
        modifier = modifier.fillMaxSize(),
        // Edge-to-edge: the top bar takes the status bar, the composer sits
        // above the navigation bar and the keyboard.
        contentWindowInsets = WindowInsets.safeDrawing,
        topBar = {
        val liveSelection = selection
        if (liveSelection != null) {
            SelectionBar(
                countLabel = liveSelection.countLabel,
                onCancel = viewModel::onCancelSelection,
            )
        } else {
        Header(
            title = headerTitle,
            subtitle = headerSubtitle,
            isGroup = isGroupChat,
            avatarSeed = state.groupIdHex,
            // ADR-0045 UX §6.2.8 item 4 / §6.2.11 — the call buttons are GONE
            // once this device's membership has ended, not merely failing. The
            // per-call SFrame seed comes from `exportSecret("lakko/call/v1", …)`
            // and now raises, so they fail closed — which is correct, and is not
            // enough: a call control asserts a live E2EE session, and there is
            // not one. Folded into `callActive` because the two reasons to hide
            // them are the same rendering decision.
            callActive = callOccupant != null || membershipEnded,
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
                // The same two entry points the full-screen ring screen drives.
                onAccept = { withVideo -> viewModel.acceptIncomingCall(withVideo) },
                onDecline = { viewModel.declineIncomingCall() },
            )
        }

        // The ongoing-group-call banner: a live call on this chat that this
        // device is not in. Hidden once this device's membership has ended —
        // the call key can no longer be derived, so Join could only fail.
        val joinable = joinableGroupCall
        if (joinable != null && !membershipEnded) {
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
                        me.lakko.app.chat.PIN_FOR_YOU_LABEL
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
        if (pinListOpen) {
            PinListSheet(
                entries = pinEntries,
                authorOf = { entry ->
                    if (entry.scope == me.lakko.app.chat.PinScope.PERSONAL) {
                        me.lakko.app.chat.PIN_FOR_YOU_LABEL
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
            AlertDialog(
                onDismissRequest = viewModel::dismissCallNotice,
                title = { Text("Call") },
                text = { Text(notice) },
                confirmButton = {
                    if (callNoticeOffersEnd) {
                        TextButton(onClick = viewModel::onEndCurrentCall) { Text("End current call") }
                    } else {
                        TextButton(onClick = viewModel::dismissCallNotice) { Text("OK") }
                    }
                },
                dismissButton = if (callNoticeOffersEnd) {
                    { TextButton(onClick = viewModel::dismissCallNotice) { Text("Cancel") } }
                } else {
                    null
                },
            )
        }

        // The GROUP's info: the roster, the name, the invite link, the roles
        // and the exit, as a Dialog over the conversation while
        // `membersPanelOpen` is true.
        //
        // The second half of that condition is the guard, not a nicety: this
        // sheet is about a group, and a conversation between two people has
        // none. Gating the render on the conversation's kind rather than on
        // the flag alone means no future caller of `toggleMembersPanel` can
        // put `Group name`, `Create invite link`, `Make admin` and `Transfer
        // ownership` in front of somebody chatting with one other person,
        // which is the defect docs/ux/conversation-info.md section 0 records.
        val membersPanelOpen by viewModel.membersPanelOpen.collectAsStateWithLifecycle()
        if (membersPanelOpen && conversationInfo == ConversationInfo.Group) {
            MembersSheet(viewModel = viewModel, onChatDeleted = onNavigateBack)
        }

        // No call is drawn here: the 1x1, group and call-link in-call screens
        // are mounted app-wide by `MainActivity`, so leaving this chat never
        // hides or ends a call and returning to it shows the live call.

        if (!state.threadKnown) {
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
            if (state.messages.isEmpty() && pendingMedia.isEmpty()) {
                // ADR-0045 UX §6.2 — the empty-timeline copy invites a send,
                // and once the membership has ended that is an invitation the
                // bar below can no longer accept. The terminal row carries the
                // whole of what there is to say.
                if (!membershipEnded) EmptyTimelineHint()
            } else {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(
                        vertical = 8.dp,
                    ),
                ) {
                    items(items = timeline, key = { it.key }) { item ->
                        if (item is TimelineItem.DateSeparator) {
                            DateSeparatorPill(item.label)
                            return@items
                        }
                        val row = item as TimelineItem.Row
                        val msg = row.message
                        val pinMenu = me.lakko.app.chat.pinMenuFor(
                            sharedPinned = sharedPins[msg.messageId]?.pinned == true,
                            personalPinned = msg.messageId in personalPins,
                            mayShared = mayPinShared,
                        )
                        // UX section 8 — while selecting, the whole row is the
                        // toggle and a mark sits at its leading edge; a row that
                        // cannot be acted on shows no mark and answers nothing.
                        val selectable = me.lakko.app.chat.canSelectMessage(msg)
                        val isSelected = selection?.contains(msg.messageId) == true
                        SelectableTimelineRow(
                            selecting = selecting,
                            selectable = selectable,
                            selected = isSelected,
                            markLabel = me.lakko.app.chat.selectionMarkLabel(
                                messageLabel = selectionRowLabel(msg),
                                selected = isSelected,
                            ),
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
                                callPermissions.request(video = video) { granted ->
                                    viewModel.onCallButton(video = granted)
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
                                    val bytes = withContext(Dispatchers.IO) {
                                        runCatching {
                                            AttachmentBridge.downloadAndDecrypt(ref)
                                        }.getOrNull()
                                    } ?: return@launch
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
                                    val cached = mediaHub.fileFor(
                                        me.lakko.app.media.ChatMediaHub.mediaKey(ref.blobId),
                                    )
                                    val bytes = withContext(Dispatchers.IO) {
                                        runCatching {
                                            cached?.readBytes() ?: AttachmentBridge.downloadAndDecrypt(ref)
                                        }.getOrNull()
                                    } ?: return@launch
                                    withContext(Dispatchers.IO) {
                                        shareDecryptedBytes(context, ref, bytes)
                                    }
                                }
                            },
                            // ADR-0023 — open the pre-filled edit dialog.
                            // UX 3.1 — and drop an armed reply: one thing at
                            // a time.
                            onEdit = {
                                viewModel.onEditStarted()
                                editTarget = msg
                            },
                            onDelete = { viewModel.onDeleteMessage(msg.messageId) },
                            // ADR-0052 / UX 3.1 — Reply ends an open edit
                            // (its draft is discarded, as Cancel would) and
                            // arms the strip. The name is what the timeline
                            // calls the author: "You" for an own message, the
                            // resolved name otherwise.
                            onReply = {
                                editTarget = null
                                viewModel.onReplyToMessage(msg, timelineAuthorLabel(msg))
                            },
                            onJumpToReply = jumpToMessage,
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
                                        authorLabel = timelineAuthorLabel(msg),
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
                            pinLabel = pinMenu.label,
                            onPin = {
                                val single = pinMenu.actions.singleOrNull()
                                if (single != null && pinMenu.label == "Unpin") {
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
                        )
                        }
                    }
                    // ADR-0047 — outgoing media still uploading or not sent.
                    items(items = pendingMedia, key = { "pending-${it.tempId}" }) { row ->
                        Row(
                            modifier = Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.End,
                        ) {
                            PendingMediaBubble(
                                row = row,
                                onRetry = { viewModel.retryMediaSend(row.tempId) },
                                onDiscard = { viewModel.discardMediaSend(row.tempId) },
                            )
                        }
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
        me.lakko.app.chat.terminalRow(membership)?.let { row ->
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
            SelectionActionBar(
                selection = liveSelection,
                busy = bulkBusy,
                status = bulkStatus,
                onForward = {
                    val requests = viewModel.selectedMessagesInOrder().map { msg ->
                        ForwardRequest(
                            sourceGroupIdHex = state.groupIdHex,
                            message = msg,
                            authorLabel = timelineAuthorLabel(msg),
                            // ADR-0053 section 6 — no origin out of a one-to-one.
                            sourceIsOneToOne = viewModel.conversationKind() ==
                                me.lakko.app.chat.ConversationKind.Chat,
                        )
                    }
                    if (requests.isNotEmpty()) onForwardSelection(requests)
                },
                onDelete = { confirmBulkDelete = true },
            )
        } else when (val slot = me.lakko.app.chat.composerState(membership, state.draft)) {
            me.lakko.app.chat.ComposerSlot.Composer -> Composer(
                recorder = recorder,
                emojiInsert = pendingEmojiInsert,
                onEmojiInserted = { pendingEmojiInsert = null },
                onOpenPicker = { picker = PickerRequest(PickerMode.COMPOSER, null) },
                draft = state.draft,
                sending = state.sending,
                sendError = state.sendError,
                pendingAttachments = state.pendingAttachments,
                uploading = state.uploading,
                inFlightUploads = state.inFlightUploads,
                pendingPreviews = state.pendingPreviews,
                onDraftChange = viewModel::onDraftChange,
                onSendClicked = viewModel::onSendClicked,
                onFocusChanged = viewModel::onComposerFocusChanged,
                replyStrip = replyingTo,
                onCancelReply = viewModel::onCancelReply,
                onPickMedia = {
                    pickVisualLauncher.launch(
                        androidx.activity.result.PickVisualMediaRequest(
                            ActivityResultContracts.PickVisualMedia.ImageAndVideo,
                        ),
                    )
                },
                onAttachClicked = {
                    // ACTION_OPEN_DOCUMENT — pick any MIME. The
                    // contract returns `Uri` per pick.
                    pickFilesLauncher.launch(arrayOf("*/*"))
                },
                onRemoveAttachment = viewModel::onRemovePendingAttachment,
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
        AlertDialog(
            onDismissRequest = { confirmBulkDelete = false },
            title = { Text(me.lakko.app.chat.confirmDeleteLabel(confirmCount)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmBulkDelete = false
                        viewModel.onDeleteSelected()
                    },
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                ) { Text("Delete") }
            },
            dismissButton = {
                TextButton(onClick = { confirmBulkDelete = false }) { Text("Cancel") }
            },
        )
    } else if (confirmBulkDelete) {
        // The selection emptied under the dialog (another device deleted the
        // rows); there is nothing left to confirm.
        confirmBulkDelete = false
    }

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
        )
    }

    // Inline image preview — surfaced as a full-screen
    // AlertDialog with the decoded bitmap. Dismiss via tap
    // outside or the Close button.
    val preview = previewImage
    if (preview != null) {
        AlertDialog(
            onDismissRequest = { previewImage = null },
            confirmButton = {
                TextButton(onClick = { previewImage = null }) { Text("Close") }
            },
            text = {
                androidx.compose.foundation.Image(
                    bitmap = preview,
                    contentDescription = "attachment preview",
                )
            },
        )
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
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
            }
        },
        title = {
            // The name and the avatar together are the entry point: tapping
            // them opens info about this conversation, and which info that is
            // depends on what the conversation is.
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.clickable(onClick = onShowInfo),
            ) {
                LakkoAvatar(name = title, seed = avatarSeed, size = 32.dp, group = isGroup)
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
                    Icon(Icons.Filled.Call, contentDescription = "Call")
                }
                IconButton(onClick = onStartVideoCall) {
                    Icon(Icons.Filled.Videocam, contentDescription = "Video call")
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
        0 -> "Starting now"
        1 -> "1 person in the call"
        else -> "${call.joinedCount} people in the call"
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
                    text = "Group call in progress",
                    style = MaterialTheme.typography.titleSmall,
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                )
                Text(
                    text = joinedLine,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                )
            }
            TextButton(onClick = onJoin) { Text("Join") }
            TextButton(onClick = onJoinWithVideo) { Text("Join with video") }
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
    /** UX 3.1 — this row is the one a tapped quote just scrolled to. */
    highlighted: Boolean = false,
    /** ADR-0052 decision 2 / UX 5.1 — Forward was chosen on this message. */
    onForward: () -> Unit = {},
    /** UX 5.1 — this row's "Forwarded from" line was tapped. */
    onJumpToForward: (ForwardRef) -> Unit = {},
    /** ADR-0052 decision 3 / UX 4.1 — Pin / Unpin was chosen on this message. */
    onPin: () -> Unit = {},
    /** UX 4.1 — the row's word, `Pin` or `Unpin`, decided by the caller. */
    pinLabel: String = "Pin",
    /** UX section 8 — Select was chosen on this message. */
    onSelect: () -> Unit = {},
) {
    val gapAbove = if (firstInRun) 8.dp else 2.dp
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
                onForward = onForward,
                onJumpToForward = onJumpToForward,
                onPin = onPin,
                pinLabel = pinLabel,
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
                        modifier = Modifier.padding(bottom = 2.dp),
                    )
                } else {
                    Spacer(Modifier.width(28.dp))
                }
                Spacer(Modifier.width(8.dp))
            }
            Column(horizontalAlignment = Alignment.Start) {
                if (authorName != null && firstInRun) {
                    Text(
                        text = authorName,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.primary,
                        maxLines = 1,
                        modifier = Modifier.padding(start = 4.dp, bottom = 2.dp),
                    )
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
                    onForward = onForward,
                    onJumpToForward = onJumpToForward,
                    onPin = onPin,
                    pinLabel = pinLabel,
                    onSelect = onSelect,
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
                            text = msg.text,
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
    val missed = entry.outcome == me.lakko.app.calls.CallOutcome.MISSED ||
        entry.outcome == me.lakko.app.calls.CallOutcome.DECLINED
    val tint = if (missed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary
    val spoken = remember(entry) { me.lakko.app.calls.callRowSpokenLabel(entry) }
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
                Icon(
                    imageVector = if (entry.video) Icons.Filled.Videocam else Icons.Filled.Call,
                    contentDescription = null,
                    tint = tint,
                    modifier = Modifier.size(16.dp),
                )
                Text(
                    text = me.lakko.app.calls.callRowText(entry),
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
private val REACTION_EMOJIS: List<String> =
    listOf("👍", "❤️", "😂", "😮", "😢", "🙏")

/** UX 5.2 — the centred day pill between two days of the timeline. */
@Composable
private fun DateSeparatorPill(label: String) {
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
    /** ADR-0052 decision 2 / UX 5.1 — Forward was chosen on this message. */
    onForward: () -> Unit = {},
    /** UX 5.1 — the "Forwarded from" line was tapped. */
    onJumpToForward: (ForwardRef) -> Unit = {},
    /** ADR-0052 decision 3 / UX 4.1 — Pin / Unpin was chosen on this message. */
    onPin: () -> Unit = {},
    /** UX 4.1 — the row's word, `Pin` or `Unpin`, decided by the caller. */
    pinLabel: String = "Pin",
    /** UX section 8 — Select was chosen on this message. */
    onSelect: () -> Unit = {},
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
            if (msg.reply != null || msg.forward != null) {
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
        val metaSlot = me.lakko.app.media.bubbleMetaSlot(soleBubble)
        val mediaOnly = metaSlot == me.lakko.app.media.BubbleMetaSlot.MEDIA_PILL
        val chromeless = soleBubble == me.lakko.app.media.MediaBubble.VIDEO_NOTE || mediaOnly
        val menuAvailable = actionsAvailable && hasId && !msg.deleted
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
                contentColor = contentColor,
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
                            Modifier.combinedClickable(
                                onClick = {},
                                onLongClick = { menuOpen = true },
                            )
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
                    modifier = (if (mediaOnly) Modifier else Modifier.padding(horizontal = 12.dp, vertical = 8.dp))
                        .width(IntrinsicSize.Max),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    if (msg.deleted) {
                        // ADR-0023 — muted tombstone, no text / attachments.
                        Text(
                            text = "message deleted",
                            style = MaterialTheme.typography.bodyMedium,
                            color = contentColor.copy(alpha = 0.6f),
                            fontStyle = androidx.compose.ui.text.font.FontStyle.Italic,
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
                                contentColor = contentColor,
                                onClick = { onJumpToReply(reply.target) },
                            )
                        }
                        if (msg.text.isNotEmpty()) {
                            Text(
                                text = msg.text,
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        }
                        if (msg.attachments.isNotEmpty()) {
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
                                                    time = time,
                                                    readState = if (isOwn) readState else OwnReadState.None,
                                                    contentColor = contentColor,
                                                    fillWidth = false,
                                                    onRetry = if (msg.ack == Ack.FAILED) {
                                                        { onRetrySend(msg.localId) }
                                                    } else {
                                                        null
                                                    },
                                                )
                                            }
                                        } else {
                                            null
                                        },
                                    ) {
                                        AttachmentChipRow(
                                            ref = ref,
                                            onPreview = { onPreview(ref) },
                                            onSave = { onSave(ref) },
                                        )
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
                    if (metaSlot == me.lakko.app.media.BubbleMetaSlot.BUBBLE_ROW) {
                        BubbleMeta(
                            edited = msg.edited && !msg.deleted,
                            time = time,
                            readState = if (isOwn) readState else OwnReadState.None,
                            contentColor = contentColor,
                            onRetry = if (msg.ack == Ack.FAILED) {
                                { onRetrySend(msg.localId) }
                            } else {
                                null
                            },
                        )
                    }
                }
            }

            // ADR-0023 + `docs/ux/message-actions.md` §2 — the long-press menu.
            // The reactions are NOT a row in it: they are a row of their own
            // above the bubble, one tap each, because reacting is the commonest
            // thing a person does here and used to cost two taps.
            val bounds = bubbleBounds
            if (menuOpen && bounds != null) {
                MessageActionMenuOverlay(
                    bubbleBounds = bounds,
                    alignEnd = isOwn,
                    actions = messageActionsFor(
                        isOwn = isOwn,
                        deleted = msg.deleted,
                        text = msg.text,
                        menuAvailable = menuAvailable,
                        // UX 5.1 — no Forward on a row still sending or
                        // one that failed.
                        sendComplete = msg.ack == Ack.SENT,
                        // UX 4.1 — `Pin` or `Unpin`, decided by the screen.
                        pinLabel = pinLabel,
                    ),
                    reactions = REACTION_EMOJIS,
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
                    onSelect = {
                        menuOpen = false
                        onSelect()
                    },
                    onCopy = {
                        menuOpen = false
                        copyMessageText(context, msg.text)
                    },
                    onEdit = {
                        menuOpen = false
                        onEdit()
                    },
                    onDelete = {
                        menuOpen = false
                        onDelete()
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
                onToggle = { emoji -> if (actionsAvailable && hasId) onReact(emoji) },
                interactive = actionsAvailable,
            )
        }

        // A chromeless (video note) bubble has no surface to carry the meta
        // line, so it sits under the circle. A photo or video carries it in
        // the pill on the media (UX spec §10), so nothing is drawn here.
        if (metaSlot == me.lakko.app.media.BubbleMetaSlot.BELOW_BUBBLE) {
            BubbleMeta(
                edited = msg.edited,
                time = time,
                readState = if (isOwn) readState else OwnReadState.None,
                contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 4.dp),
            )
        }
    }
}

/** UX 3.1 — how long a jumped-to row stays lit: "about a second and a half". */
internal const val REPLY_HIGHLIGHT_MS: Long = 1_500L

/** The strip's close control, by name, for TalkBack and the UI tests. */
internal const val CANCEL_REPLY_LABEL: String = "Cancel reply"

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
) {
    val (name, excerpt) = replyQuoteLines(reply)
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
            Icon(
                imageVector = Icons.Filled.Close,
                contentDescription = CANCEL_REPLY_LABEL,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
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
    val label = forwardLineLabel(forward)
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
        Icon(
            imageVector = Icons.AutoMirrored.Filled.Forward,
            contentDescription = null,
            tint = contentColor.copy(alpha = 0.7f),
            modifier = Modifier.size(14.dp),
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
) {
    val (name, excerpt) = replyQuoteLines(reply)
    val label = "Reply to $name"
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
    /** `false` inside the §10 media pill, which wraps its content. */
    fillWidth: Boolean = true,
    /** Send this row again. `null` unless this row's send failed - the action
     *  sits next to the message it belongs to, which is the only place that
     *  can say WHICH message failed. */
    onRetry: (() -> Unit)? = null,
) {
    if (!edited && time == null && readState == OwnReadState.None && onRetry == null) return
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        modifier = if (fillWidth) modifier.fillMaxWidth() else modifier,
    ) {
        if (fillWidth) Spacer(Modifier.weight(1f))
        if (edited) {
            Text(
                text = "edited",
                style = MaterialTheme.typography.labelSmall,
                color = contentColor.copy(alpha = 0.7f),
            )
        }
        if (time != null) {
            Text(
                text = time,
                style = MaterialTheme.typography.labelSmall,
                color = contentColor.copy(alpha = 0.7f),
            )
        }
        if (onRetry != null) {
            Text(
                text = "Retry",
                style = MaterialTheme.typography.labelSmall,
                textDecoration = TextDecoration.Underline,
                color = contentColor,
                modifier = Modifier
                    .clickable(onClick = onRetry)
                    .semantics { contentDescription = "Send again" },
            )
        }
        if (readState != OwnReadState.None) {
            val read = readState == OwnReadState.Read
            Icon(
                imageVector = if (read) Icons.Filled.DoneAll else Icons.Filled.Check,
                contentDescription = if (read) "Read" else "Sent",
                tint = if (read) contentColor else contentColor.copy(alpha = 0.7f),
                modifier = Modifier.height(14.dp),
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
) {
    if (!edited && time == null && readState == OwnReadState.None) return
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
        )
    }
}

/** ADR-0023 — aggregate reactions into `emoji ×count` chips. A
 *  chip the local user contributed is tinted + tappable to
 *  toggle off. */
@Composable
private fun ReactionChips(
    reactions: List<me.lakko.app.chat.Reaction>,
    ownIdentity: me.lakko.app.chat.MemberIdentity?,
    onToggle: (String) -> Unit,
    /** ADR-0045 UX §6.2.5 — `false` once the membership has ended: the chip
     *  still renders its count and stops being a control. */
    interactive: Boolean = true,
) {
    // Aggregate by emoji preserving first-seen order; track whether WE are in
    // each bucket — by signature key, so a newcomer taking a slot we once held
    // never lights the "mine" highlight on their reaction.
    val mineKey = ownIdentity ?: me.lakko.app.chat.OWN_REACTION_SENTINEL
    val order = ArrayList<String>()
    val counts = LinkedHashMap<String, Int>()
    val mine = HashSet<String>()
    for (r in reactions) {
        if (!counts.containsKey(r.emoji)) order.add(r.emoji)
        counts[r.emoji] = (counts[r.emoji] ?: 0) + 1
        if (r.reactor == mineKey) mine.add(r.emoji)
    }
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        for (emoji in order) {
            val count = counts[emoji] ?: 0
            val isMine = mine.contains(emoji)
            Surface(
                shape = RoundedCornerShape(10.dp),
                color = if (isMine) {
                    MaterialTheme.colorScheme.primary.copy(alpha = 0.18f)
                } else {
                    MaterialTheme.colorScheme.surfaceVariant
                },
                contentColor = MaterialTheme.colorScheme.onSurface,
                modifier = if (interactive) {
                    Modifier.combinedClickable(
                        onClick = { onToggle(emoji) },
                        onLongClick = {},
                    )
                } else {
                    Modifier
                },
            ) {
                Text(
                    text = "$emoji $count",
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 3.dp),
                )
            }
        }
    }
}

/**
 * ADR-0045 UX §6 — the `Remove this member?` confirmation.
 *
 * Same idiom as [LeaveGroupDialog] deliberately: the two sit in the same panel
 * and a user reading one after the other should be answering the same kind of
 * question. The copy comes from `me.lakko.app.chat.removeMemberConfirmation`
 * rather than being inlined, so the three clients cannot drift on the sentence
 * that tells a user whether the person they are removing can still read their
 * messages.
 *
 * The body is absent on a direct-commit group, where §6's sentence would be
 * false — see the module for why it is dropped rather than reworded.
 */
@Composable
private fun RemoveMemberDialog(
    usesProposalPath: Boolean,
    onCancel: () -> Unit,
    onRemove: () -> Unit,
) {
    val copy = me.lakko.app.chat.removeMemberConfirmation(usesProposalPath)
    AlertDialog(
        onDismissRequest = onCancel,
        title = { Text(copy.title) },
        text = copy.body?.let { body -> { Text(body) } },
        confirmButton = {
            TextButton(
                onClick = onRemove,
                colors = ButtonDefaults.textButtonColors(
                    contentColor = MaterialTheme.colorScheme.error,
                ),
            ) { Text(copy.confirmLabel) }
        },
        dismissButton = {
            TextButton(onClick = onCancel) { Text(copy.cancelLabel) }
        },
    )
}

/** ADR-0023 — the pre-filled edit dialog (Save / Cancel). */
@Composable
private fun EditMessageDialog(
    initialText: String,
    onCancel: () -> Unit,
    onSave: (String) -> Unit,
) {
    var draft by remember { mutableStateOf(initialText) }
    AlertDialog(
        onDismissRequest = onCancel,
        title = { Text("Edit message") },
        text = {
            OutlinedTextField(
                value = draft,
                onValueChange = { draft = it },
                singleLine = false,
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.Sentences,
                ),
                modifier = Modifier.fillMaxWidth(),
            )
        },
        confirmButton = {
            TextButton(
                onClick = { onSave(draft) },
                enabled = draft.trim().isNotEmpty() && draft.trim() != initialText.trim(),
            ) { Text("Save") }
        },
        dismissButton = {
            TextButton(onClick = onCancel) { Text("Cancel") }
        },
    )
}

/** Single attachment chip rendered inside a bubble. Hosts the
 *  Save action on every chip and an Open action on image
 *  MIMEs. The two callbacks are owned by [ChatScreen] so they
 *  can drive coroutine work + the screen-level preview sheet. */
@Composable
private fun AttachmentChipRow(
    ref: AttachmentRef,
    onPreview: () -> Unit,
    onSave: () -> Unit,
) {
    val preview = AttachmentBridge.isInlinePreviewable(ref)
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
                    text = ref.name ?: "(unnamed)",
                    style = MaterialTheme.typography.labelMedium,
                    maxLines = 1,
                )
                Text(
                    text = AttachmentBridge.formatSize(ref.size),
                    style = MaterialTheme.typography.labelSmall,
                )
            }
            if (preview) {
                TextButton(onClick = onPreview) { Text("Open") }
            }
            TextButton(onClick = onSave) { Text("Save") }
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
            text = "No messages yet. Say hi.",
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
            text = "This conversation is not ready on this device yet. " +
                "It will appear once the other members are online.",
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
    sending: Boolean,
    sendError: String?,
    pendingAttachments: List<AttachmentRef>,
    uploading: Boolean,
    inFlightUploads: List<UploadProgress>,
    pendingPreviews: Map<String, ByteArray>,
    onDraftChange: (String) -> Unit,
    onSendClicked: () -> Unit,
    /** ADR-0024 — fired with the composer's focus state. On
     *  focus-loss the ViewModel publishes `typing:false` once. */
    onFocusChanged: (Boolean) -> Unit,
    /** UX spec §10 — the gallery pick (photos and videos). */
    onPickMedia: () -> Unit,
    onAttachClicked: () -> Unit,
    onRemoveAttachment: (Int) -> Unit,
    /** ADR-0052 / UX 3.1 — the armed reply the strip shows, or `null`. */
    replyStrip: ReplyArmed? = null,
    /** The strip's close control. */
    onCancelReply: () -> Unit = {},
) {
    // The caret has to be known to insert an emoji AT it (UX spec §4), which a
    // bare `String` value cannot carry. The view model still owns the text;
    // this mirrors it and follows an external change (a cleared draft after
    // Send) without losing the caret on the user's own edits.
    //
    // Only a CHANGE of `draft` resets the field — the same rule the `String`
    // overload of the text field applies internally. Comparing against the
    // field's own text instead would reset it to a stale draft whenever the
    // view model's state lags one keystroke behind, dropping what was typed.
    var field by remember { mutableStateOf(TextFieldValue(draft, TextRange(draft.length))) }
    var lastDraft by remember { mutableStateOf(draft) }
    if (draft != lastDraft) {
        lastDraft = draft
        if (field.text != draft) field = TextFieldValue(draft, TextRange(draft.length))
    }
    LaunchedEffect(emojiInsert) {
        val emoji = emojiInsert?.second ?: return@LaunchedEffect
        val sel = field.selection
        val text = field.text.replaceRange(sel.min, sel.max, emoji)
        val caret = sel.min + emoji.length
        field = TextFieldValue(text, TextRange(caret))
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
    val showSend = draft.isNotBlank() || pendingAttachments.isNotEmpty() || uploading

    Column {
        HorizontalDivider()
        if (sendError != null) {
            Text(
                text = sendError,
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
                text = note,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .padding(horizontal = 16.dp, vertical = 2.dp)
                    .semantics { liveRegion = LiveRegionMode.Polite },
            )
        }
        // ADR-0052 / UX 3.1 — the reply strip, above the tray and the field.
        if (replyStrip != null && !recording) {
            ReplyStrip(reply = replyStrip.snapshot, onClose = onCancelReply)
        }
        // 1.19c4 — pending-attachments tray. Horizontal scroll
        // of chips above the text field; each chip shows
        // `name · size` with a × to drop it. Plus an
        // "Encrypting…" placeholder chip while [uploading] is
        // true.
        if ((pendingAttachments.isNotEmpty() || uploading) && !recording) {
            PendingAttachmentsTray(
                attachments = pendingAttachments,
                inFlight = inFlightUploads,
                previews = pendingPreviews,
                onRemove = onRemoveAttachment,
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
                var attachMenu by remember { mutableStateOf(false) }
                Box {
                    IconButton(
                        onClick = { attachMenu = true },
                        enabled = !sending && !uploading,
                    ) {
                        Icon(Icons.Filled.AttachFile, contentDescription = "Attach")
                    }
                    DropdownMenu(expanded = attachMenu, onDismissRequest = { attachMenu = false }) {
                        DropdownMenuItem(
                            text = { Text("Photo or video") },
                            onClick = {
                                attachMenu = false
                                onPickMedia()
                            },
                        )
                        DropdownMenuItem(
                            text = { Text("File") },
                            onClick = {
                                attachMenu = false
                                onAttachClicked()
                            },
                        )
                    }
                }
                // UX 5.2 — a pill field on the sunken surface that grows to
                // five lines and then scrolls; the IME resizes the screen
                // (adjustResize + the Scaffold's safe-drawing insets).
                OutlinedTextField(
                    value = field,
                    onValueChange = { next ->
                        field = next
                        if (next.text != draft) onDraftChange(next.text)
                    },
                    placeholder = { Text("Message") },
                    singleLine = false,
                    maxLines = 5,
                    enabled = !sending,
                    shape = RoundedCornerShape(24.dp),
                    colors = OutlinedTextFieldDefaults.colors(
                        focusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
                        unfocusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
                        disabledContainerColor = MaterialTheme.colorScheme.surfaceVariant,
                        focusedBorderColor = Color.Transparent,
                        unfocusedBorderColor = Color.Transparent,
                        disabledBorderColor = Color.Transparent,
                    ),
                    textStyle = MaterialTheme.typography.bodyLarge,
                    keyboardOptions = KeyboardOptions(
                        capitalization = KeyboardCapitalization.Sentences,
                    ),
                    modifier = Modifier
                        .weight(1f)
                        .focusRequester(fieldFocus)
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
                    Icon(Icons.Filled.Mood, contentDescription = "Emoji and GIFs")
                }
            }
            Spacer(Modifier.width(4.dp))
            if (showSend && !recording) {
                // UX 5.2 — the trailing button is the send arrow once there is
                // a draft or an attachment; the mic otherwise (below).
                FilledIconButton(
                    onClick = onSendClicked,
                    enabled = !sending && !uploading
                        && (draft.isNotBlank() || pendingAttachments.isNotEmpty()),
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
                        .semantics { contentDescription = "Send" },
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
                        Icon(Icons.AutoMirrored.Filled.Send, contentDescription = null)
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
            text = row.headline,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        Text(
            text = row.consequence,
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
                    text = slot.draftLabel ?: me.lakko.app.chat.UNSENT_DRAFT_LABEL,
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
                text = slot.notice,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun PendingAttachmentsTray(
    attachments: List<AttachmentRef>,
    inFlight: List<UploadProgress>,
    previews: Map<String, ByteArray>,
    onRemove: (Int) -> Unit,
) {
    val scrollState = rememberScrollState()
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 6.dp)
            .horizontalScroll(scrollState),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        attachments.forEachIndexed { index, ref ->
            val preview = previews[AttachmentBridge.hex(ref.blobId)]
            AssistChip(
                onClick = { onRemove(index) },
                leadingIcon = preview?.let { bytes -> { TrayThumb(bytes, 24) } },
                label = {
                    Column {
                        Text(
                            text = ref.name ?: "(unnamed)",
                            style = MaterialTheme.typography.labelSmall,
                            maxLines = 1,
                        )
                        Text(
                            text = AttachmentBridge.formatSize(ref.size),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                },
                trailingIcon = {
                    Text("×", modifier = Modifier.semantics { contentDescription = "Remove ${ref.name ?: "attachment"}" })
                },
                colors = AssistChipDefaults.assistChipColors(),
            )
        }
        // UX spec §10 — one chip per picked item still in flight, shown from
        // the moment it was picked, with its thumb once one exists.
        for (item in inFlight) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.semantics(mergeDescendants = true) {},
            ) {
                item.previewBytes?.let { bytes ->
                    TrayThumb(bytes)
                    Spacer(Modifier.width(6.dp))
                }
                CircularProgressIndicator(
                    modifier = Modifier
                        .height(16.dp)
                        .width(16.dp),
                    strokeWidth = 2.dp,
                )
                Spacer(Modifier.width(6.dp))
                Text(
                    text = when (item.stage) {
                        null -> "Preparing ${item.fileName}…"
                        UploadStage.Encrypting -> "Encrypting ${item.fileName}…"
                        UploadStage.Uploading -> "Uploading ${item.fileName}…"
                    },
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
            }
        }
    }
}

/** UX spec §10 — the photo / video thumb in a composer chip. */
@Composable
private fun TrayThumb(bytes: ByteArray, sizeDp: Int = 32) {
    val bitmap = remember(bytes) { decodeImageBitmap(bytes) } ?: return
    androidx.compose.foundation.Image(
        bitmap = bitmap,
        contentDescription = null,
        contentScale = androidx.compose.ui.layout.ContentScale.Crop,
        modifier = Modifier
            .height(sizeDp.dp)
            .width(sizeDp.dp)
            .clip(RoundedCornerShape(6.dp)),
    )
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

/** Write `bytes` to the app's cache directory under the
 *  attachment's name (or a blob-id-derived fallback) and fire
 *  an ACTION_SEND chooser so the user can route to Files /
 *  Drive / etc. Best-effort: any IO / Intent failure is
 *  swallowed because we're already on a coroutine off the UI
 *  thread + can't surface a snackbar from this scope. */
private fun shareDecryptedBytes(
    context: Context,
    ref: AttachmentRef,
    bytes: ByteArray,
) {
    // `ref.name` is sender-controlled: sanitize it, write into a fresh
    // per-share directory so no existing file can be overwritten, and refuse
    // anything that still resolves outside that directory.
    val baseName = AttachmentBridge.safeFileName(
        ref.name,
        fallback = "attachment-${AttachmentBridge.hex(ref.blobId).take(8)}",
    )
    runCatching {
        val shareDir = java.io.File(
            java.io.File(context.cacheDir, "shared-attachments"),
            java.util.UUID.randomUUID().toString(),
        ).apply { mkdirs() }
        val outFile = java.io.File(shareDir, baseName)
        check(outFile.canonicalFile.parentFile == shareDir.canonicalFile) {
            "attachment name resolved outside the share directory"
        }
        outFile.writeBytes(bytes)
        val authority = "${context.packageName}.fileprovider"
        val uri = androidx.core.content.FileProvider
            .getUriForFile(context, authority, outFile)
        val send = Intent(Intent.ACTION_SEND).apply {
            putExtra(Intent.EXTRA_STREAM, uri)
            type = ref.mime ?: "application/octet-stream"
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        context.startActivity(
            Intent.createChooser(send, "Save attachment")
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }
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
    AlertDialog(
        onDismissRequest = onCancel,
        title = { Text("Leave this group?") },
        text = {
            Text(
                "You will keep receiving messages until an owner or an admin applies " +
                    "your request. You will need a new invite to rejoin.",
            )
        },
        confirmButton = {
            TextButton(
                onClick = onLeave,
                colors = ButtonDefaults.textButtonColors(
                    contentColor = MaterialTheme.colorScheme.error,
                ),
            ) { Text("Leave") }
        },
        dismissButton = {
            TextButton(onClick = onCancel) { Text("Cancel") }
        },
    )
}

/**
 * ADR-0045 (UX §6.2.4) — the `Delete this chat` confirmation.
 *
 * The only protection there is, which is why the body is exact rather than
 * sweeping: **the messages really are gone, and the dead MLS handle is not.**
 * `Group` exposes no delete, so the inactive group's state stays in the
 * provider snapshot as inert bytes — inert in the strong sense (it cannot send,
 * decrypt, export or process) but not nothing.
 *
 * Copy from [me.lakko.app.chat.DELETE_CHAT_TITLE] /
 * [me.lakko.app.chat.DELETE_CHAT_BODY] rather than inline, so the three clients
 * cannot drift on the one string that describes an irreversible deletion.
 */
/** ADR-0045 UX 6.2.4 — `internal` so the person layer raises the same one. */
@Composable
internal fun DeleteChatDialog(
    onCancel: () -> Unit,
    onDelete: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onCancel,
        title = { Text(me.lakko.app.chat.DELETE_CHAT_TITLE) },
        text = { Text(me.lakko.app.chat.DELETE_CHAT_BODY) },
        confirmButton = {
            TextButton(
                onClick = onDelete,
                colors = ButtonDefaults.textButtonColors(
                    contentColor = MaterialTheme.colorScheme.error,
                ),
            ) { Text("Delete") }
        },
        dismissButton = {
            TextButton(onClick = onCancel) { Text("Cancel") }
        },
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
            Text(text = notice.headline, style = MaterialTheme.typography.bodyMedium)
            // The second line names the exit deliberately: it is the one
            // sentence that stops the new owner from arriving where the old one
            // was.
            Text(
                text = notice.body,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            TextButton(onClick = onDismiss) { Text(notice.dismiss) }
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
                text = me.lakko.app.chat.TRANSFER_IRREVERSIBLE_HEADLINE,
                style = MaterialTheme.typography.titleSmall,
            )
            Text(
                text = me.lakko.app.chat.TRANSFER_IRREVERSIBLE_BODY,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(10.dp))
            Text(text = "New owner", style = MaterialTheme.typography.labelMedium)
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
                                    if (row.isAdmin) "${row.label}, admin" else row.label
                            }
                            .padding(vertical = 2.dp),
                    ) {
                        RadioButton(
                            selected = pickedKeyB64 == row.signatureKeyB64,
                            onClick = null,
                            enabled = !busy,
                        )
                        Spacer(Modifier.width(6.dp))
                        Text(text = row.label, style = MaterialTheme.typography.bodyMedium)
                        // MARK, do not filter and do not sort. Who is already an
                        // admin is information the owner assigned and is worth
                        // having when choosing a successor; it is not a reason to
                        // hide anyone, and hoisting admins to the top would be a
                        // soft version of the filter §6.1.3 rejects.
                        if (row.isAdmin) {
                            Spacer(Modifier.width(6.dp))
                            Text(
                                text = "Admin",
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
            Text(text = "Your role afterwards", style = MaterialTheme.typography.labelMedium)
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
                        Text(text = option.label, style = MaterialTheme.typography.bodyMedium)
                        Text(
                            text = option.consequence,
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
                text = me.lakko.app.chat.TRANSFER_UNLOCKS_LEAVE,
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
                ) { Text(action.label) }
                // `Cancel` is a PRE-PUBLISH exit, and it is the last one.
                // Nothing here is a withdraw: once the proposal is published it
                // is in every member's store and dies only when some commit
                // lands, so no client may offer a cancel afterwards —
                // including, and especially, for a transfer.
                TextButton(onClick = onCancel, enabled = !busy) { Text("Cancel") }
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
                    label = { Text(me.lakko.app.chat.GROUP_NAME_LABEL) },
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
                ) { Text(if (busy) "Sending..." else me.lakko.app.chat.GROUP_NAME_SAVE) }
            }
        } else {
            Text(
                text = me.lakko.app.chat.GROUP_NAME_LABEL,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                text = name.ifBlank { "No name yet" },
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
                text = message,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
    }
}

@Composable
private fun MembersSheet(
    viewModel: ChatViewModel,
    /** ADR-0045 UX §6.2.4 — invoked after `Delete this chat` has removed the
     *  registry row, so the screen leaves a thread it can no longer open. */
    onChatDeleted: () -> Unit,
) {
    val members by viewModel.members.collectAsStateWithLifecycle()
    val input by viewModel.addMemberInput.collectAsStateWithLifecycle()
    val busy by viewModel.membershipBusy.collectAsStateWithLifecycle()
    val error by viewModel.membershipError.collectAsStateWithLifecycle()
    val canManage by viewModel.canManageMembers.collectAsStateWithLifecycle()
    val isOwner by viewModel.isOwner.collectAsStateWithLifecycle()
    val roleTable by viewModel.roleTable.collectAsStateWithLifecycle()
    val pendingJoins by viewModel.pendingJoins.collectAsStateWithLifecycle()
    val unkeptJoinRequest by viewModel.unkeptJoinRequest.collectAsStateWithLifecycle()
    val pendingRequests by viewModel.pendingRequests.collectAsStateWithLifecycle()
    val unappliedAdds by viewModel.unappliedAdds.collectAsStateWithLifecycle()
    val pendingNotice by viewModel.pendingNotice.collectAsStateWithLifecycle()
    val joinAttributions by viewModel.joinAttributions.collectAsStateWithLifecycle()
    val inviteLink by viewModel.inviteLink.collectAsStateWithLifecycle()
    val notice by viewModel.membershipNotice.collectAsStateWithLifecycle()
    val newOwner by viewModel.newOwnerNotice.collectAsStateWithLifecycle()
    val clipboard = LocalClipboardManager.current
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
    // ADR-0045 UX §6.2.4 — the `Delete this chat` confirmation. Local to the
    // sheet, like `confirmingLeave`: it is a presentation concern, and nothing
    // outside this composable may open it.
    var confirmingDelete by remember { mutableStateOf(false) }
    // ADR-0045 (UX §6) — the `Remove this member?` confirmation, holding the
    // leaf awaiting it. Local to the sheet like `confirmingLeave`, and a
    // nullable leaf rather than a boolean because the confirmation names WHICH
    // member: a shared boolean plus a separately-stored target is how a
    // confirmation ends up removing somebody the user did not look at.
    var confirmingRemove by remember { mutableStateOf<UInt?>(null) }

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
    val sections = remember(canManage, isOwner, canLeave, membershipEnded) {
        groupInfoSections(
            GroupInfoCapabilities(
                canManageMembers = canManage,
                canRename = canManage,
                canMintInvite = canManage,
                isOwner = isOwner,
                canLeave = canLeave,
                membershipEnded = membershipEnded,
            ),
        )
    }

    // ADR-0045 UX §6.1 — the exit block's inputs, read as Compose state so the
    // block recomposes when any of them moves.
    val rosterViews = remember(members) { viewModel.rosterViews() }
    val ownerKeyB64 = roleTable?.owner?.let { me.lakko.app.chat.outboxB64(it) }
    val adminKeysB64 = roleTable?.admins?.map { me.lakko.app.chat.outboxB64(it) }?.toSet()
        ?: emptySet()
    val transferCandidates = remember(rosterViews, ownerKeyB64, adminKeysB64) {
        me.lakko.app.chat.transferCandidates(rosterViews, ownerKeyB64, adminKeysB64)
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
    val exit = me.lakko.app.chat.exitBlock(
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

    confirmingRemove?.let { leaf ->
        RemoveMemberDialog(
            usesProposalPath = roleTable != null,
            onCancel = { confirmingRemove = null },
            onRemove = {
                confirmingRemove = null
                viewModel.onRemoveMember(leaf)
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

    Dialog(
        onDismissRequest = viewModel::toggleMembersPanel,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Surface(
            modifier = Modifier
                .fillMaxWidth()
                .padding(16.dp),
            shape = MaterialTheme.shapes.large,
        ) {
            Column(modifier = Modifier.padding(16.dp)) {
                Text(
                    text = "Members (${members.size})",
                    style = MaterialTheme.typography.titleMedium,
                )
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
                    GroupNameRow(
                        viewModel = viewModel,
                        canRename = InfoSection.RENAME_GROUP in sections,
                    )
                    Spacer(Modifier.height(12.dp))
                    // docs/ux/who-can-find-me.md section 5 — this group's
                    // `Members can find me` switch (absent unless the setting
                    // includes groups).
                    me.lakko.app.ui.settings.MembersCanFindMeRow(
                        reach = me.lakko.app.reach.ReachManager.get(androidx.compose.ui.platform.LocalContext.current),
                        kind = me.lakko.app.ui.settings.FindMeScopeKind.GROUP,
                        idHex = viewModel.groupIdHex,
                    )
                }
                // ADR-0022 — only owner/admin (or an open/legacy group) sees the
                // add affordance; a plain member's add would be refused by peers.
                if (InfoSection.ADD_MEMBER in sections) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        OutlinedTextField(
                            value = input,
                            onValueChange = viewModel::setAddMemberInput,
                            label = { Text("@username") },
                            singleLine = true,
                            enabled = !busy,
                            keyboardOptions = KeyboardOptions(
                                capitalization = KeyboardCapitalization.None,
                            ),
                            modifier = Modifier.weight(1f),
                        )
                        Spacer(Modifier.width(8.dp))
                        Button(
                            onClick = viewModel::onAddMember,
                            enabled = !busy && input.trim().isNotEmpty(),
                        ) { Text("Add") }
                    }
                }
                // ADR-0028 (G-3) — owner/admin "create invite link" affordance.
                // Mints a /g/<slug> link conveying this group's id; a non-member
                // opens it to request to join, approved via the pending list.
                if (InfoSection.INVITE_LINK in sections) {
                    Spacer(Modifier.height(8.dp))
                    TextButton(
                        onClick = viewModel::onCreateInviteLink,
                        enabled = !busy,
                    ) { Text("Create invite link") }
                    inviteLink?.let { link ->
                        Text(
                            text = link,
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.padding(horizontal = 8.dp),
                        )
                        TextButton(onClick = { clipboard.setText(AnnotatedString(link)) }) {
                            Text("Copy link")
                        }
                        TextButton(
                            onClick = viewModel::onRevokeInviteLink,
                            enabled = !busy,
                        ) { Text("Revoke link") }
                    }
                }
                // ADR-0045 — the request accepted locally and published. Kept
                // beside the error and, like it, UNGATED: a plain member now has
                // a membership action (`Leave group`), so the feedback for one
                // must reach a member who cannot manage anybody.
                notice?.let { message ->
                    Spacer(Modifier.height(6.dp))
                    Text(
                        text = message,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                error?.let { message ->
                    Spacer(Modifier.height(6.dp))
                    Text(
                        text = message,
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
                    Text(text = notice.count, style = MaterialTheme.typography.titleSmall)
                    Text(
                        text = notice.liveness,
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
                            text = "Waiting to be added (${waitingAdds.size})",
                            style = MaterialTheme.typography.titleSmall,
                        )
                        // UX §5.2 — one row per pending add, with the same identity
                        // treatment the join-request rows already use (the short
                        // fingerprint; a prospective member has no roster entry to
                        // read a display name from), plus the secondary line.
                        waitingAdds.forEach { request ->
                            Text(
                                text = viewModel.nameOfKeyB64(request.addSignatureKeyB64),
                                style = MaterialTheme.typography.bodySmall,
                            )
                            Text(
                                text = "Waiting to be added.",
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
                if (unappliedAdds.isNotEmpty()) {
                    Text(
                        text = me.lakko.app.chat.unappliedHeading(unappliedAdds.size),
                        style = MaterialTheme.typography.titleSmall,
                    )
                    Text(
                        text = me.lakko.app.chat.UNAPPLIED_EXPLANATION,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Spacer(Modifier.height(6.dp))
                    unappliedAdds.forEach { item ->
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(vertical = 2.dp),
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    text = viewModel.nameOfKeyB64(
                                        item.addSignatureKeyB64,
                                    ),
                                    style = MaterialTheme.typography.bodySmall,
                                )
                                Text(
                                    text = me.lakko.app.chat.unappliedLine(item),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            TextButton(
                                onClick = {
                                    viewModel.onDismissUnapplied(item.addSignatureKeyB64)
                                },
                            ) { Text("Dismiss") }
                        }
                    }
                    Spacer(Modifier.height(12.dp))
                }
                // ADR-0027 (G-3) — pending external-join requests. Owner/admin
                // only (same gate as add/remove); hidden when there are none.
                // Approve drives the role-gated MLS approve + posts the commit &
                // Welcome rendezvous; Reject drops the retained proposal.
                if (canManage && (pendingJoins.isNotEmpty() || unkeptJoinRequest)) {
                    Text(
                        text = "Join requests (${pendingJoins.size})",
                        style = MaterialTheme.typography.titleSmall,
                    )
                    // ADR-0027 / UX §7.1 — a request the quarantine turned
                    // away. It is in no row below, because the core never kept
                    // it, and it gets no Approve for the same reason: there is
                    // nothing to approve. Informational only, which is the most
                    // `externalJoinAffordance` allows.
                    if (unkeptJoinRequest) {
                        Text(
                            text = me.lakko.app.chat.JOIN_REQUEST_NOT_KEPT,
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
                                // ADR-0028 OQ2 — which member's `/g/` link this
                                // request arrived on + whether they are still in
                                // the roster (advisory).
                                val attr = joinAttributions[
                                    ref.joinToString("") { "%02x".format(it.toInt() and 0xff) },
                                ]
                                if (attr != null) {
                                    Text(
                                        text = "via " +
                                            (attr.issuerKeyB64?.let(viewModel::nameOf)
                                                ?: attr.issuerFingerprint.take(11)) +
                                            if (attr.issuerInRoster) {
                                                " · member"
                                            } else {
                                                " · not a current member"
                                            },
                                        style = MaterialTheme.typography.labelSmall,
                                        color = if (attr.issuerInRoster) {
                                            MaterialTheme.colorScheme.onSurfaceVariant
                                        } else {
                                            MaterialTheme.colorScheme.error
                                        },
                                    )
                                } else {
                                    Text(
                                        text = "invite source unknown",
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                            }
                            TextButton(
                                onClick = { viewModel.onApproveJoin(ref) },
                                enabled = !busy,
                            ) { Text("Approve") }
                            TextButton(
                                onClick = { viewModel.onRejectJoin(ref) },
                                enabled = !busy,
                            ) { Text("Reject") }
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
                if (membershipEnded) {
                    Text(
                        text = me.lakko.app.chat.FROZEN_ROSTER_HEADER,
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
                    // The roster gives up height while the transfer step is open, so
                    // the step's action button cannot be pushed off a small screen.
                    // The alternative — a second `Dialog` with its own scroll — is
                    // the stacking [TransferOwnershipStep]'s doc rejects.
                    LazyColumn(
                        modifier = Modifier.heightIn(max = if (transferOpen) 140.dp else 360.dp),
                    ) {
                        items(members) { member ->
                            Row(
                                verticalAlignment = Alignment.CenterVertically,
                                modifier = Modifier
                                    .fillMaxWidth()
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
                                Column(modifier = Modifier.weight(1f)) {
                                    Text(text = viewModel.memberLabel(member))
                                    annotation?.let {
                                        Text(
                                            text = it,
                                            style = MaterialTheme.typography.bodySmall,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                    }
                                }
                                // ADR-0022 — owner-only promote / demote toggle;
                                // never the owner's own row.
                                //
                                // `ROLES` is OWNER-only in the shared rule and must
                                // never widen to admins: a role-table change is
                                // owner-only and every peer refuses an admin's, so
                                // an admin offered this would be offered a button
                                // whose every press is rejected. Which ROW it may
                                // not appear on is this client's, because the rule
                                // is about the block and knows nothing of rows.
                                val isMemberOwner = roleTable?.owner
                                    ?.contentEquals(member.signaturePublicKey) ?: false
                                if (InfoSection.ROLES in sections && !isMemberOwner) {
                                    val isAdmin = roleTable?.admins
                                        ?.any {
                                            it.contentEquals(member.signaturePublicKey)
                                        } ?: false
                                    TextButton(
                                        onClick = { viewModel.onToggleAdmin(member, !isAdmin) },
                                        enabled = !busy,
                                    ) { Text(if (isAdmin) "Remove admin" else "Make admin") }
                                }
                                // ADR-0022 — Remove only when we may manage members
                                // (owner / admin / open group) and not our own leaf.
                                if (canManage && ownLeaf != member.leafIndex) {
                                    TextButton(
                                        // ADR-0045 UX §6 — the confirmation is the
                                        // LAST exit: a published request cannot be
                                        // withdrawn, and a direct commit lands at
                                        // once. Never remove on a single tap.
                                        onClick = { confirmingRemove = member.leafIndex },
                                        // UX §5.3 — a member with a pending removal keeps
                                        // Remove VISIBLE but DISABLED. Hiding it would read
                                        // as the permission having vanished; the annotation
                                        // above explains the disabled state.
                                        enabled = !busy && annotation == null,
                                    ) { Text("Remove") }
                                }
                            }
                        }
                    }
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
                        ) { Text(me.lakko.app.chat.DELETE_CHAT_CONTROL) }
                    }
                    is me.lakko.app.chat.ExitBlock.Owner -> {
                        Spacer(Modifier.height(12.dp))
                        TextButton(
                            onClick = viewModel::openTransferStep,
                            enabled = !busy && exit.transfer.enabled && !transferOpen,
                        ) { Text("Transfer ownership") }
                        // The reason is part of the control's description, not a
                        // decoration: a reader who cannot see the greyed button
                        // must still be told why.
                        exit.transfer.disabledReason?.let {
                            Text(
                                text = it,
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
                        ) { Text("Leave group") }
                        exit.leave.disabledReason?.let {
                            Text(
                                text = it,
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
                        ) { Text("Leave group") }
                        exit.leave.disabledReason?.let {
                            Text(
                                text = it,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
                // ADR-0045 (UX §6.1.4 / §6.1.5) — the transfer step, inline
                // beneath the control that opened it. See
                // [TransferOwnershipStep] for why it is not a second `Dialog`.
                if (transferOpen && exit is me.lakko.app.chat.ExitBlock.Owner) {
                    Spacer(Modifier.height(8.dp))
                    TransferOwnershipStep(
                        candidates = transferCandidates,
                        pickedKeyB64 = transferPick,
                        outgoing = transferOutgoing,
                        busy = busy,
                        onPick = viewModel::setTransferPick,
                        onOutgoing = viewModel::setTransferOutgoing,
                        onCancel = viewModel::cancelTransferStep,
                        // The role choice is passed EXPLICITLY rather than read
                        // back inside the action, so the required-argument
                        // property the core protects with a type survives all
                        // the way to the tap: there is no call site here that
                        // can omit it.
                        onTransfer = { successor ->
                            viewModel.onTransferOwnership(successor, transferOutgoing)
                        },
                    )
                }
                Spacer(Modifier.height(12.dp))
                Row(modifier = Modifier.fillMaxWidth()) {
                    Spacer(Modifier.weight(1f))
                    TextButton(onClick = viewModel::toggleMembersPanel) { Text("Done") }
                }
            }
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
    msg.text.ifEmpty {
        msg.attachments.firstOrNull()
            ?.let { mediaPreviewLabel(PreviewAttachment(kind = it.kind, mime = it.mime)) }
            ?: "Message"
    }

/** Diameter of the selection mark; also its tap-friendly footprint inside the
 *  row (the whole row is the target, so the mark itself needs no 40dp). */
private val SELECTION_MARK_SIZE: Dp = 22.dp

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
                                stateDescription = if (selected) {
                                    me.lakko.app.chat.SELECTED_STATE_LABEL
                                } else {
                                    me.lakko.app.chat.NOT_SELECTED_STATE_LABEL
                                }
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
        Icon(
            imageVector = Icons.Filled.CheckCircle,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.primary,
            modifier = Modifier.size(SELECTION_MARK_SIZE),
        )
    } else {
        Box(
            modifier = Modifier
                .size(SELECTION_MARK_SIZE)
                .border(
                    width = 1.5.dp,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    shape = CircleShape,
                ),
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
            TextButton(onClick = onCancel) { Text(me.lakko.app.chat.SELECTION_CANCEL_LABEL) }
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
@Composable
private fun SelectionActionBar(
    selection: me.lakko.app.chat.MessageSelection,
    busy: Boolean,
    status: String?,
    onForward: () -> Unit,
    onDelete: () -> Unit,
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
                    enabled = selection.forwardEnabled && !busy,
                    modifier = Modifier.weight(1f),
                ) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.Forward,
                        contentDescription = null,
                        modifier = Modifier.size(18.dp),
                    )
                    Spacer(Modifier.width(8.dp))
                    Text("Forward")
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
                    Icon(
                        imageVector = Icons.Filled.Delete,
                        contentDescription = null,
                        modifier = Modifier.size(18.dp),
                    )
                    Spacer(Modifier.width(8.dp))
                    Text("Delete")
                }
            }
            // One helper slot: the progress copy after a failure outranks the
            // gate's explanation, because it is the newer fact.
            val helper = status ?: selection.deleteHelper
            if (helper != null) {
                Text(
                    text = helper,
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
