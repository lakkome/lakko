// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.background
import androidx.lifecycle.compose.collectAsStateWithLifecycle
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
    val callStateSnapshot = activeCall?.state?.collectAsStateWithLifecycle()?.value

    Column(modifier = modifier.fillMaxSize()) {
        Header(
            groupIdHex = state.groupIdHex,
            refreshing = state.refreshing,
            callActive = activeCall != null,
            onNavigateBack = onNavigateBack,
            onRefresh = viewModel::onRefreshClicked,
            onStartCall = { viewModel.startCall(audio = true, video = false) },
        )

        // Phase 3a10 — sticky call overlay. Renders only
        // when an active session exists.
        if (activeCall != null) {
            CallOverlay(
                state = callStateSnapshot
                    ?: me.lakko.app.calls.CallSession.WebCallState.Idle,
                onAccept = { viewModel.acceptIncomingCall() },
                onDecline = { viewModel.declineIncomingCall() },
                onHangUp = { viewModel.hangUpCall() },
                onDismiss = { viewModel.clearFinishedCall() },
            )
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
                        )
                    }
                }
            }
        }

        Composer(
            draft = state.draft,
            sending = state.sending,
            sendError = state.sendError,
            pendingAttachments = state.pendingAttachments,
            uploading = state.uploading,
            uploadStage = state.uploadStage,
            onDraftChange = viewModel::onDraftChange,
            onSendClicked = viewModel::onSendClicked,
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
            // Phase 3a10 — call button. Hidden when a call
            // is already active (the overlay carries the
            // Accept / Decline / Hang up affordances).
            if (!callActive) {
                TextButton(onClick = onStartCall) { Text("📞 Call") }
            }
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
    onPreview: (AttachmentRef) -> Unit,
    onSave: (AttachmentRef) -> Unit,
) {
    when (msg.direction) {
        Direction.Outgoing -> Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.End,
        ) {
            Bubble(
                text = msg.text,
                attachments = msg.attachments,
                background = MaterialTheme.colorScheme.primaryContainer,
                contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
                onPreview = onPreview,
                onSave = onSave,
            )
        }

        Direction.Incoming -> Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.Start,
        ) {
            Bubble(
                text = msg.text,
                attachments = msg.attachments,
                background = MaterialTheme.colorScheme.surfaceVariant,
                contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                onPreview = onPreview,
                onSave = onSave,
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

@Composable
private fun Bubble(
    text: String,
    attachments: List<AttachmentRef>,
    background: Color,
    contentColor: Color,
    onPreview: (AttachmentRef) -> Unit,
    onSave: (AttachmentRef) -> Unit,
) {
    Surface(
        shape = RoundedCornerShape(12.dp),
        color = background,
        contentColor = contentColor,
        modifier = Modifier.widthIn(max = 280.dp),
    ) {
        Column(
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            if (text.isNotEmpty()) {
                Text(
                    text = text,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            if (attachments.isNotEmpty()) {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    for (ref in attachments) {
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
                modifier = Modifier.weight(1f),
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
