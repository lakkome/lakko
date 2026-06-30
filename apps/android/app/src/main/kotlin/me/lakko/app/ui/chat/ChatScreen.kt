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
import android.provider.OpenableColumns
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.foundation.background
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.ui.call.CallScreen
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.chat.AttachmentBridge
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.chat.UploadStage
import uniffi.lakko_ffi_uniffi.AttachmentRef

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
@Composable
public fun ChatScreen(
    viewModel: ChatViewModel,
    onNavigateBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val listState = rememberLazyListState()
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    // 1.19c4 — preview-attachment sheet state. When the user
    // taps "Open" on an image chip we resolve the bytes off
    // the main thread + populate this, which surfaces an
    // AlertDialog with the decoded bitmap.
    var previewImage by remember { mutableStateOf<ImageBitmap?>(null) }

    // ADR-0023 — the message being edited (the small pre-filled
    // dialog), or `null` when no edit is in progress.
    var editTarget by remember { mutableStateOf<ChatMessage?>(null) }
    // ADR-0023 — our own leaf in this chat group, sampled once
    // per composition; the UI marks which reaction chips we
    // contributed (and can toggle off).
    val ownLeaf = remember(state.groupIdHex) { viewModel.ownChatLeafIndex() }
    // ADR-0024 — the live roster's other leaves, re-sampled when
    // the membership / read-watermark map shifts. The Bubble's
    // "Read" tick needs every other member's watermark `>=` the
    // message's id.
    val otherLeaves = remember(state.groupIdHex, state.readUpToByLeaf) {
        viewModel.otherChatLeaves()
    }

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
        for (uri in uris) {
            // Hand off to a coroutine — the resolver work
            // (openInputStream + ContentResolver query) needs
            // to go off the main thread, and the encrypt +
            // upload is async too.
            scope.launch {
                val payload = withContext(Dispatchers.IO) {
                    readPickedUri(context, uri)
                } ?: return@launch
                viewModel.onAttachmentPicked(
                    bytes = payload.bytes,
                    mime = payload.mime,
                    name = payload.name,
                )
            }
        }
    }

    LaunchedEffect(Unit) {
        // First tick: manual-style refresh so the spinner
        // blips briefly while the immediate fetch resolves.
        viewModel.onRefreshClicked()
        // Subsequent ticks: continuous long-poll. Cancels when
        // the composable leaves composition (back navigation,
        // logout). Errors back off via [POLL_RETRY_BACKOFF_MS]
        // so we don't tight-loop against a 503.
        while (isActive) {
            try {
                viewModel.pollOnceLongPoll()
            } catch (_: Throwable) {
                delay(POLL_RETRY_BACKOFF_MS)
            }
        }
    }
    LaunchedEffect(state.messages.size) {
        // Auto-scroll to the latest row on every new message
        // (Outgoing send + Incoming poll both bump the count).
        if (state.messages.isNotEmpty()) {
            listState.animateScrollToItem(state.messages.size - 1)
        }
    }

    // Phase 3a10 — call surface.
    val activeCall by viewModel.activeCall.collectAsStateWithLifecycle()
    // SFU multi-party group call (>2 members). Mutually exclusive with
    // `activeCall` — the call button routes to exactly one.
    val activeSfuCall by viewModel.activeSfuCall.collectAsStateWithLifecycle()
    val callStateSnapshot = activeCall?.state?.collectAsStateWithLifecycle()?.value
    // G3-2-call-present — a "live" call (placed or accepted)
    // owns the full screen; `ringing` (incoming, pre-accept)
    // stays a lightweight inline banner.
    // G3-2-prejoin-join — `Idle` is included so a prepped-but-
    // undialed outgoing call presents the prejoin (mic / camera
    // preview) before the user taps Join. There's no `activeCall`
    // in `Idle` outside that prejoin window.
    val callIsLive = when (callStateSnapshot) {
        me.lakko.app.calls.CallSession.WebCallState.Idle,
        me.lakko.app.calls.CallSession.WebCallState.Outgoing,
        me.lakko.app.calls.CallSession.WebCallState.Connecting,
        me.lakko.app.calls.CallSession.WebCallState.Connected,
        me.lakko.app.calls.CallSession.WebCallState.Ending,
        me.lakko.app.calls.CallSession.WebCallState.Done,
        -> true
        else -> false
    }

    Column(modifier = modifier.fillMaxSize()) {
        Header(
            groupIdHex = state.groupIdHex,
            refreshing = state.refreshing,
            callActive = activeCall != null || activeSfuCall != null,
            onNavigateBack = onNavigateBack,
            onRefresh = viewModel::onRefreshClicked,
            // A group chat (>2 MLS members) routes through the
            // multi-party SFU path; a 1x1 chat stays on the P2P
            // CallSession. The button is disabled (callActive) while
            // either kind of call is already up.
            onStartCall = {
                if (viewModel.isGroupChat) {
                    viewModel.startGroupCall(video = false)
                } else {
                    viewModel.startCall(audio = true, video = false)
                }
            },
            // Video call — publishes the local camera. Group chats
            // SFrame-seal the video through the SFU; 1x1 chats open a
            // P2P video call (the prejoin still lets the user toggle
            // the camera before joining).
            onStartVideoCall = {
                if (viewModel.isGroupChat) {
                    viewModel.startGroupCall(video = true)
                } else {
                    viewModel.startCall(audio = true, video = true)
                }
            },
            onShowMembers = viewModel::toggleMembersPanel,
        )

        // A4 — the group-membership sheet (roster + add / remove),
        // presented as a Dialog while `membersPanelOpen` is true.
        val membersPanelOpen by viewModel.membersPanelOpen.collectAsStateWithLifecycle()
        if (membersPanelOpen) {
            MembersSheet(viewModel = viewModel)
        }

        // G3-2-call-present — the inline banner now only
        // handles the `ringing` incoming-call prompt
        // (Accept / Decline). Once the call is live it is
        // presented full-screen via the `Dialog` below.
        if (activeCall != null &&
            callStateSnapshot == me.lakko.app.calls.CallSession.WebCallState.Ringing
        ) {
            CallOverlay(
                state = me.lakko.app.calls.CallSession.WebCallState.Ringing,
                onAccept = { viewModel.acceptIncomingCall() },
                onDecline = { viewModel.declineIncomingCall() },
                onHangUp = { viewModel.hangUpCall() },
                onDismiss = { viewModel.clearFinishedCall() },
            )
        }

        // G3-2-call-present — present the full in-call UI over
        // the chat as a full-screen Dialog while the call is
        // live. `CallScreen` drives itself from the injected
        // `session` (CallUiState.driveFrom). The dialog is torn
        // down when the call clears (`clearFinishedCall` sets
        // `activeCall = null`); back-press / outside-tap
        // dismissal are disabled so the only exit is the
        // in-screen End button (→ hangUp → done → onExit).
        val liveCall = activeCall
        if (liveCall != null && callIsLive) {
            Dialog(
                onDismissRequest = { /* exit only via End button */ },
                properties = DialogProperties(
                    usePlatformDefaultWidth = false,
                    dismissOnBackPress = false,
                    dismissOnClickOutside = false,
                ),
            ) {
                Box(modifier = Modifier.fillMaxSize()) {
                    CallScreen(
                        onExit = { viewModel.clearFinishedCall() },
                        session = liveCall,
                        // G3-2-prejoin-join — Join dials the prepped
                        // session with the chosen mic / camera; Cancel
                        // abandons it.
                        onPrejoinJoin = { mic, cam ->
                            viewModel.dialActiveCall(audio = mic, video = cam)
                        },
                        onPrejoinCancel = { viewModel.cancelActiveCall() },
                    )
                }
            }
        }

        // SFU multi-party group call — presented full-screen over the
        // chat while live. Unlike the 1x1 path there is no prejoin: the
        // session auto-connects (phase is driven to Connecting before
        // the overlay appears), so `CallScreen` gets the externally
        // driven `sfuCallUiState` (NO `session` — the SfuCallUi adapter
        // owns the state) and no prejoin callbacks. End routes to
        // `hangUpGroupCall`, which clears `activeSfuCall` and drops this
        // Dialog.
        if (activeSfuCall != null) {
            Dialog(
                onDismissRequest = { /* exit only via End button */ },
                properties = DialogProperties(
                    usePlatformDefaultWidth = false,
                    dismissOnBackPress = false,
                    dismissOnClickOutside = false,
                ),
            ) {
                Box(modifier = Modifier.fillMaxSize()) {
                    CallScreen(
                        onExit = { viewModel.hangUpGroupCall() },
                        state = viewModel.sfuCallUiState,
                    )
                }
            }
        }

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
            if (state.messages.isEmpty()) {
                EmptyTimelineHint()
            } else {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize(),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(
                        vertical = 8.dp,
                    ),
                ) {
                    items(items = state.messages, key = { it.messageId }) { msg ->
                        MessageRow(
                            msg = msg,
                            ownLeaf = ownLeaf,
                            // ADR-0024 — read-state inputs (own
                            // bubbles render "Sent" / "Read").
                            otherLeaves = otherLeaves,
                            readUpToByLeaf = state.readUpToByLeaf,
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
                                    val bytes = withContext(Dispatchers.IO) {
                                        runCatching {
                                            AttachmentBridge.downloadAndDecrypt(ref)
                                        }.getOrNull()
                                    } ?: return@launch
                                    withContext(Dispatchers.IO) {
                                        shareDecryptedBytes(context, ref, bytes)
                                    }
                                }
                            },
                            // ADR-0023 — open the pre-filled edit dialog.
                            onEdit = { editTarget = msg },
                            onDelete = { viewModel.onDeleteMessage(msg.messageId) },
                            onReact = { emoji ->
                                viewModel.onReactToMessage(msg.messageId, emoji)
                            },
                        )
                    }
                }
            }
        }

        // ADR-0024 — typing indicator just above the input bar.
        // Auto-hides when `typingText` is null (nobody typing /
        // auto-expired).
        TypingIndicator(text = state.typingText)

        Composer(
            draft = state.draft,
            sending = state.sending,
            sendError = state.sendError,
            pendingAttachments = state.pendingAttachments,
            uploading = state.uploading,
            uploadStage = state.uploadStage,
            onDraftChange = viewModel::onDraftChange,
            onSendClicked = viewModel::onSendClicked,
            onFocusChanged = viewModel::onComposerFocusChanged,
            onAttachClicked = {
                // ACTION_OPEN_DOCUMENT — pick any MIME. The
                // contract returns `Uri` per pick. Wildcard
                // `*/*` keeps both photos + arbitrary files in
                // the same flow.
                pickFilesLauncher.launch(arrayOf("*/*"))
            },
            onRemoveAttachment = viewModel::onRemovePendingAttachment,
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

@Composable
private fun Header(
    groupIdHex: String,
    refreshing: Boolean,
    callActive: Boolean,
    onNavigateBack: () -> Unit,
    onRefresh: () -> Unit,
    onStartCall: () -> Unit,
    onStartVideoCall: () -> Unit,
    onShowMembers: () -> Unit,
) {
    Column {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = 4.dp, end = 12.dp, top = 4.dp, bottom = 4.dp),
        ) {
            TextButton(onClick = onNavigateBack) { Text("← Back") }
            Spacer(Modifier.width(4.dp))
            Text(
                text = "Group ${shortHex(groupIdHex)}",
                style = MaterialTheme.typography.titleMedium,
                fontFamily = FontFamily.Monospace,
                modifier = Modifier.weight(1f),
            )
            // Phase 3a10 — call buttons. Hidden when a call
            // is already active (the overlay carries the
            // Accept / Decline / Hang up affordances). Audio
            // call + video call (publishes the local camera).
            if (!callActive) {
                TextButton(onClick = onStartCall) { Text("📞 Call") }
                TextButton(onClick = onStartVideoCall) { Text("🎥 Video") }
            }
            // A4 — members sheet: roster + add / remove.
            TextButton(onClick = onShowMembers) { Text("Members") }
            if (refreshing) {
                CircularProgressIndicator(
                    modifier = Modifier
                        .height(20.dp)
                        .width(20.dp),
                    strokeWidth = 2.dp,
                )
            } else {
                TextButton(onClick = onRefresh) { Text("Refresh") }
            }
        }
        HorizontalDivider()
    }
}

// =============================================================================
// Phase 3a10 — Call overlay
// =============================================================================

@Composable
private fun CallOverlay(
    state: me.lakko.app.calls.CallSession.WebCallState,
    onAccept: () -> Unit,
    onDecline: () -> Unit,
    onHangUp: () -> Unit,
    onDismiss: () -> Unit,
) {
    val bg = when (state) {
        me.lakko.app.calls.CallSession.WebCallState.Ringing ->
            androidx.compose.ui.graphics.Color(0xFFFFA000)
        me.lakko.app.calls.CallSession.WebCallState.Connected ->
            androidx.compose.ui.graphics.Color(0xFF2E7D32)
        me.lakko.app.calls.CallSession.WebCallState.Done ->
            androidx.compose.ui.graphics.Color(0xFF616161)
        else -> androidx.compose.ui.graphics.Color(0xFF1565C0)
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .background(bg)
            .padding(horizontal = 16.dp, vertical = 10.dp),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = when (state) {
                    me.lakko.app.calls.CallSession.WebCallState.Idle -> "📞"
                    me.lakko.app.calls.CallSession.WebCallState.Outgoing ->
                        "📞 Calling…"
                    me.lakko.app.calls.CallSession.WebCallState.Ringing ->
                        "📞 Incoming call"
                    me.lakko.app.calls.CallSession.WebCallState.Connecting ->
                        "📞 Connecting…"
                    me.lakko.app.calls.CallSession.WebCallState.Connected ->
                        "📞 In call"
                    me.lakko.app.calls.CallSession.WebCallState.Ending ->
                        "📞 Ending…"
                    me.lakko.app.calls.CallSession.WebCallState.Done ->
                        "📞 Call ended"
                },
                style = MaterialTheme.typography.titleMedium,
                color = androidx.compose.ui.graphics.Color.White,
            )
            Text(
                text = "Audio · signaling-only scaffold. WebRTC pod " +
                    "integration is the 3a10-followup.",
                style = MaterialTheme.typography.bodySmall,
                color = androidx.compose.ui.graphics.Color.White.copy(alpha = 0.85f),
            )
        }
        when (state) {
            me.lakko.app.calls.CallSession.WebCallState.Ringing -> {
                TextButton(onClick = onAccept) {
                    Text("Accept", color = androidx.compose.ui.graphics.Color.White)
                }
                TextButton(onClick = onDecline) {
                    Text("Decline", color = androidx.compose.ui.graphics.Color.White)
                }
            }
            me.lakko.app.calls.CallSession.WebCallState.Done -> {
                TextButton(onClick = onDismiss) {
                    Text("Dismiss", color = androidx.compose.ui.graphics.Color.White)
                }
            }
            me.lakko.app.calls.CallSession.WebCallState.Idle -> {}
            else -> {
                TextButton(onClick = onHangUp) {
                    Text("Hang up", color = androidx.compose.ui.graphics.Color.White)
                }
            }
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
    ownLeaf: UInt?,
    /** ADR-0024 — live roster minus self; read-state inputs. */
    otherLeaves: Set<UInt>,
    readUpToByLeaf: Map<UInt, Long>,
    onPreview: (AttachmentRef) -> Unit,
    onSave: (AttachmentRef) -> Unit,
    onEdit: () -> Unit,
    onDelete: () -> Unit,
    onReact: (String) -> Unit,
) {
    when (msg.direction) {
        Direction.Outgoing -> Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.End,
        ) {
            Bubble(
                msg = msg,
                ownLeaf = ownLeaf,
                isOwn = true,
                // ADR-0024 — own bubble: compute the read tick.
                readState = ownReadStateFor(
                    messageId = msg.messageId,
                    ownLeaf = ownLeaf,
                    otherLeaves = otherLeaves,
                    readUpToByLeaf = readUpToByLeaf,
                ),
                background = MaterialTheme.colorScheme.primaryContainer,
                contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
                onPreview = onPreview,
                onSave = onSave,
                onEdit = onEdit,
                onDelete = onDelete,
                onReact = onReact,
            )
        }

        Direction.Incoming -> Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.Start,
        ) {
            Bubble(
                msg = msg,
                ownLeaf = ownLeaf,
                isOwn = false,
                // Incoming bubbles never show our read state.
                readState = OwnReadState.None,
                background = MaterialTheme.colorScheme.surfaceVariant,
                contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                onPreview = onPreview,
                onSave = onSave,
                onEdit = onEdit,
                onDelete = onDelete,
                onReact = onReact,
            )
        }

        Direction.System -> Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.Center,
        ) {
            Text(
                text = msg.text,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                fontFamily = FontFamily.Monospace,
                modifier = Modifier.padding(vertical = 4.dp),
            )
        }
    }
}

/** ADR-0023 — the per-message action menu's emoji picker set. */
private val REACTION_EMOJIS: List<String> =
    listOf("👍", "❤️", "😂", "😮", "😢", "🙏")

/**
 * ADR-0024 — the typing-indicator line just above the input bar.
 * Renders nothing when [text] is `null` (nobody typing / the
 * indicator auto-expired). The text itself ("<name> is typing…" /
 * "typing…" / "Several people are typing…") is resolved in the
 * ViewModel.
 */
@Composable
private fun TypingIndicator(text: String?) {
    if (text == null) return
    Text(
        text = text,
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        fontStyle = androidx.compose.ui.text.font.FontStyle.Italic,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 2.dp),
    )
}

@Composable
private fun Bubble(
    msg: ChatMessage,
    ownLeaf: UInt?,
    isOwn: Boolean,
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
) {
    // ADR-0023 — every affordance is gated on the message having a
    // real DS messageId (0 ⇒ a not-yet-acked outgoing row that no
    // op can target). Edit / Delete are own-message-only; React is
    // offered on any message with an id.
    val hasId = msg.messageId != 0L
    var menuOpen by remember { mutableStateOf(false) }
    var pickingReaction by remember { mutableStateOf(false) }

    Column(
        horizontalAlignment = if (isOwn) Alignment.End else Alignment.Start,
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Box {
            Surface(
                shape = RoundedCornerShape(12.dp),
                color = background,
                contentColor = contentColor,
                modifier = Modifier
                    .widthIn(max = 280.dp)
                    .then(
                        if (hasId && !msg.deleted) {
                            Modifier.combinedClickable(
                                onClick = {},
                                onLongClick = {
                                    pickingReaction = false
                                    menuOpen = true
                                },
                            )
                        } else {
                            Modifier
                        },
                    ),
            ) {
                Column(
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
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
                        if (msg.text.isNotEmpty()) {
                            Text(
                                text = msg.text,
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        }
                        if (msg.attachments.isNotEmpty()) {
                            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                for (ref in msg.attachments) {
                                    AttachmentChipRow(
                                        ref = ref,
                                        onPreview = { onPreview(ref) },
                                        onSave = { onSave(ref) },
                                    )
                                }
                            }
                        }
                        // ADR-0023 — subtle "(edited)" marker.
                        if (msg.edited) {
                            Text(
                                text = "(edited)",
                                style = MaterialTheme.typography.labelSmall,
                                color = contentColor.copy(alpha = 0.6f),
                            )
                        }
                    }
                }
            }

            // ADR-0023 — long-press action menu anchored to the bubble.
            DropdownMenu(
                expanded = menuOpen,
                onDismissRequest = {
                    menuOpen = false
                    pickingReaction = false
                },
            ) {
                if (pickingReaction) {
                    // Emoji picker — a single row of the six core
                    // reactions. Tapping one sends the toggle op.
                    Row(
                        modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp),
                        horizontalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        for (emoji in REACTION_EMOJIS) {
                            TextButton(onClick = {
                                onReact(emoji)
                                menuOpen = false
                                pickingReaction = false
                            }) { Text(emoji) }
                        }
                    }
                } else {
                    DropdownMenuItem(
                        text = { Text("React") },
                        onClick = { pickingReaction = true },
                    )
                    // Edit / Delete only on our own (outgoing) messages.
                    if (isOwn) {
                        DropdownMenuItem(
                            text = { Text("Edit") },
                            onClick = {
                                menuOpen = false
                                onEdit()
                            },
                        )
                        DropdownMenuItem(
                            text = { Text("Delete") },
                            onClick = {
                                menuOpen = false
                                onDelete()
                            },
                        )
                    }
                }
            }
        }

        // ADR-0023 — reaction chips with counts under the bubble.
        // Tapping a chip we contributed toggles it off.
        if (!msg.deleted && msg.reactions.isNotEmpty()) {
            ReactionChips(
                reactions = msg.reactions,
                ownLeaf = ownLeaf,
                onToggle = { emoji -> if (hasId) onReact(emoji) },
            )
        }

        // ADR-0024 — read-state line under an OWN bubble: a single
        // check for "Sent", a filled double check for "Read"
        // (every other member's watermark has reached this id).
        if (isOwn && readState != OwnReadState.None) {
            val (glyph, label) = when (readState) {
                OwnReadState.Read -> "✓✓" to "Read"
                OwnReadState.Sent -> "✓" to "Sent"
                OwnReadState.None -> "" to ""
            }
            val tint = if (readState == OwnReadState.Read) {
                MaterialTheme.colorScheme.primary
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            }
            Text(
                text = "$glyph $label",
                style = MaterialTheme.typography.labelSmall,
                color = tint,
                modifier = Modifier.padding(end = 4.dp),
            )
        }
    }
}

/** ADR-0023 — aggregate reactions into `emoji ×count` chips. A
 *  chip the local user contributed is tinted + tappable to
 *  toggle off. */
@Composable
private fun ReactionChips(
    reactions: List<me.lakko.app.chat.Reaction>,
    ownLeaf: UInt?,
    onToggle: (String) -> Unit,
) {
    // Aggregate by emoji preserving first-seen order; track
    // whether our own leaf is in each bucket.
    val order = ArrayList<String>()
    val counts = LinkedHashMap<String, Int>()
    val mine = HashSet<String>()
    for (r in reactions) {
        if (!counts.containsKey(r.emoji)) order.add(r.emoji)
        counts[r.emoji] = (counts[r.emoji] ?: 0) + 1
        if (ownLeaf != null && r.reactorLeaf == ownLeaf) mine.add(r.emoji)
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
                modifier = Modifier.combinedClickable(
                    onClick = { onToggle(emoji) },
                    onLongClick = {},
                ),
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
            text = "This conversation hasn't been hydrated on this device yet. " +
                "Tap Refresh — a peer's commit will rebuild it.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

// =============================================================================
// Composer
// =============================================================================

@Composable
private fun Composer(
    draft: String,
    sending: Boolean,
    sendError: String?,
    pendingAttachments: List<AttachmentRef>,
    uploading: Boolean,
    uploadStage: UploadProgress?,
    onDraftChange: (String) -> Unit,
    onSendClicked: () -> Unit,
    /** ADR-0024 — fired with the composer's focus state. On
     *  focus-loss the ViewModel publishes `typing:false` once. */
    onFocusChanged: (Boolean) -> Unit,
    onAttachClicked: () -> Unit,
    onRemoveAttachment: (Int) -> Unit,
) {
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
        // 1.19c4 — pending-attachments tray. Horizontal scroll
        // of chips above the text field; each chip shows
        // `name · size` with a × to drop it. Plus an
        // "Encrypting…" placeholder chip while [uploading] is
        // true.
        if (pendingAttachments.isNotEmpty() || uploading) {
            PendingAttachmentsTray(
                attachments = pendingAttachments,
                uploading = uploading,
                uploadStage = uploadStage,
                onRemove = onRemoveAttachment,
            )
        }
        Row(
            verticalAlignment = Alignment.Bottom,
            modifier = Modifier
                .fillMaxWidth()
                .padding(12.dp),
        ) {
            // 1.19c4 — Attach button. Drives the OS-level
            // ACTION_OPEN_DOCUMENT picker via the launcher
            // wired in [ChatScreen]; the contract surfaces both
            // photos and arbitrary files behind a single tap.
            TextButton(
                onClick = onAttachClicked,
                enabled = !sending && !uploading,
            ) {
                Text("📎")
            }
            OutlinedTextField(
                value = draft,
                onValueChange = onDraftChange,
                placeholder = { Text("Type a message…") },
                singleLine = false,
                enabled = !sending,
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.Sentences,
                ),
                modifier = Modifier
                    .weight(1f)
                    // ADR-0024 — surface focus to the ViewModel so a
                    // blur clears any outstanding typing signal.
                    .onFocusChanged { onFocusChanged(it.isFocused) },
            )
            Spacer(Modifier.width(8.dp))
            Button(
                onClick = onSendClicked,
                enabled = !sending && !uploading
                    && (draft.isNotBlank() || pendingAttachments.isNotEmpty()),
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
                    Text("Send")
                }
            }
        }
    }
}

@Composable
private fun PendingAttachmentsTray(
    attachments: List<AttachmentRef>,
    uploading: Boolean,
    uploadStage: UploadProgress?,
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
            AssistChip(
                onClick = { onRemove(index) },
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
                trailingIcon = { Text("×") },
                colors = AssistChipDefaults.assistChipColors(),
            )
        }
        if (uploading) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                CircularProgressIndicator(
                    modifier = Modifier
                        .height(16.dp)
                        .width(16.dp),
                    strokeWidth = 2.dp,
                )
                Spacer(Modifier.width(6.dp))
                Text(
                    // Polish 1.x — surface the active pick's
                    // file name + stage. Falls back to the
                    // generic "Encrypting…" copy when the stage
                    // hasn't landed yet (brief race window
                    // between flipping `uploading` and the first
                    // `uploadStage` mutation).
                    text = uploadStage?.let { stage ->
                        when (stage.stage) {
                            UploadStage.Encrypting ->
                                "Encrypting ${stage.fileName}…"
                            UploadStage.Uploading ->
                                "Uploading ${stage.fileName}…"
                        }
                    } ?: "Encrypting…",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/** Back-off delay between failed long-poll iterations. 5s
 *  matches the canonical chat-app retry cadence — see the
 *  parallel constant in `ContactsScreen`. */
private const val POLL_RETRY_BACKOFF_MS: Long = 5_000L

/** Short hex preview — same format as
 *  [me.lakko.app.ui.contacts.ContactsScreen]'s `shortHex`. */
private fun shortHex(hex: String): String {
    if (hex.length <= 12) return hex
    return "${hex.substring(0, 6)}…${hex.substring(hex.length - 4)}"
}

// =============================================================================
// 1.19c4 — picker / share helpers
// =============================================================================

/** Bytes + metadata pulled from a picker URI; the screen
 *  layer hands this to the view model. */
private data class PickedPayload(
    val bytes: ByteArray,
    val mime: String?,
    val name: String?,
)

/** Resolve a `content://` URI from `ACTION_OPEN_DOCUMENT` to
 *  raw bytes + MIME + display name. MIME comes from
 *  `ContentResolver.getType(uri)` (the picker contract
 *  guarantees this matches the actual file type); display
 *  name comes from the `OpenableColumns.DISPLAY_NAME` cursor.
 *  Returns `null` on any IO failure — the caller drops the
 *  pick silently. Must run off the main thread. */
private fun readPickedUri(context: Context, uri: Uri): PickedPayload? {
    val resolver = context.contentResolver
    val mime = resolver.getType(uri)
    var displayName: String? = null
    runCatching {
        resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (idx >= 0) displayName = cursor.getString(idx)
                }
            }
    }
    val bytes = runCatching {
        resolver.openInputStream(uri)?.use { it.readBytes() }
    }.getOrNull() ?: return null
    return PickedPayload(
        bytes = bytes,
        mime = mime?.takeUnless { it.isEmpty() },
        name = displayName?.takeUnless { it.isEmpty() },
    )
}

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
    val baseName = ref.name
        ?: "attachment-${AttachmentBridge.hex(ref.blobId).take(8)}"
    runCatching {
        val cacheDir = java.io.File(context.cacheDir, "shared-attachments").apply {
            mkdirs()
        }
        val outFile = java.io.File(cacheDir, baseName).apply {
            writeBytes(bytes)
        }
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

@Composable
private fun MembersSheet(viewModel: ChatViewModel) {
    val members by viewModel.members.collectAsStateWithLifecycle()
    val input by viewModel.addMemberInput.collectAsStateWithLifecycle()
    val busy by viewModel.membershipBusy.collectAsStateWithLifecycle()
    val error by viewModel.membershipError.collectAsStateWithLifecycle()
    val canManage by viewModel.canManageMembers.collectAsStateWithLifecycle()
    val isOwner by viewModel.isOwner.collectAsStateWithLifecycle()
    val roleTable by viewModel.roleTable.collectAsStateWithLifecycle()
    val pendingJoins by viewModel.pendingJoins.collectAsStateWithLifecycle()
    val joinAttributions by viewModel.joinAttributions.collectAsStateWithLifecycle()
    val inviteLink by viewModel.inviteLink.collectAsStateWithLifecycle()
    val clipboard = LocalClipboardManager.current
    val ownLeaf = viewModel.ownLeafIndex()

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
                Spacer(Modifier.height(12.dp))
                // ADR-0022 — only owner/admin (or an open/legacy group) sees the
                // add affordance; a plain member's add would be refused by peers.
                if (canManage) {
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
                if (canManage) {
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
                error?.let { message ->
                    Spacer(Modifier.height(6.dp))
                    Text(
                        text = message,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
                Spacer(Modifier.height(12.dp))
                // ADR-0027 (G-3) — pending external-join requests. Owner/admin
                // only (same gate as add/remove); hidden when there are none.
                // Approve drives the role-gated MLS approve + posts the commit &
                // Welcome rendezvous; Reject drops the retained proposal.
                if (canManage && pendingJoins.isNotEmpty()) {
                    Text(
                        text = "Join requests (${pendingJoins.size})",
                        style = MaterialTheme.typography.titleSmall,
                    )
                    Spacer(Modifier.height(6.dp))
                    pendingJoins.forEach { ref ->
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(vertical = 2.dp),
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    text = ref.take(8)
                                        .joinToString("") { "%02x".format(it.toInt() and 0xff) },
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
                                        text = "via ${attr.issuerFingerprint.take(11)}" +
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
                LazyColumn(modifier = Modifier.heightIn(max = 360.dp)) {
                    items(members) { member ->
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(vertical = 4.dp),
                        ) {
                            Text(
                                text = viewModel.memberLabel(member),
                                modifier = Modifier.weight(1f),
                            )
                            // ADR-0022 — owner-only promote / demote toggle;
                            // never the owner's own row.
                            val isMemberOwner = roleTable?.owner
                                ?.contentEquals(member.signaturePublicKey) ?: false
                            if (isOwner && !isMemberOwner) {
                                val isAdmin = roleTable?.admins
                                    ?.any { it.contentEquals(member.signaturePublicKey) } ?: false
                                TextButton(
                                    onClick = { viewModel.onToggleAdmin(member, !isAdmin) },
                                    enabled = !busy,
                                ) { Text(if (isAdmin) "Remove admin" else "Make admin") }
                            }
                            // ADR-0022 — Remove only when we may manage members
                            // (owner / admin / open group) and not our own leaf.
                            if (canManage && ownLeaf != member.leafIndex) {
                                TextButton(
                                    onClick = { viewModel.onRemoveMember(member.leafIndex) },
                                    enabled = !busy,
                                ) { Text("Remove") }
                            }
                        }
                    }
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
