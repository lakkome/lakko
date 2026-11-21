// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.content.ReceiveContentListener
import androidx.compose.foundation.content.TransferableContent
import androidx.compose.foundation.content.consume
import androidx.compose.foundation.content.contentReceiver
import androidx.compose.foundation.text.input.InputTransformation
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.clearText
import androidx.compose.foundation.text.input.rememberTextFieldState
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.runtime.produceState
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import android.graphics.BitmapFactory
import me.lakko.app.calls.CALL_CHAT_PICTURE_MAX_DP
import me.lakko.app.calls.CALL_CHAT_TRAY_HEIGHT_DP
import me.lakko.app.calls.CallChatControl
import me.lakko.app.calls.CallChatPaste
import me.lakko.app.calls.CallChatPictureSize
import me.lakko.app.calls.CallChatPictureState
import me.lakko.app.calls.CallChatTray
import me.lakko.app.calls.CallChatTrayNotice
import me.lakko.app.calls.PreparedCallChatPicture
import me.lakko.app.calls.callChatCanSendWith
import me.lakko.app.calls.callChatPictureBox
import me.lakko.app.calls.callChatSendPlan
import me.lakko.app.calls.callChatTrayAfterPaste
import me.lakko.app.calls.callChatTrayRemoved
import me.lakko.app.calls.callChatTrayThumbWidth
import me.lakko.app.calls.prepareCallChatPaste
import me.lakko.app.chat.AttachmentBridge
import me.lakko.app.ui.components.rememberFileSaver
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.ZoneId
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.calls.CallChatDelivery
import me.lakko.app.calls.CallChatHandle
import me.lakko.app.calls.CallChatMessage
import me.lakko.app.calls.CallChatModel
import me.lakko.app.calls.CallChatPreview
import me.lakko.app.calls.CallChatTextRule
import me.lakko.app.calls.CallControl
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.NativeCallChatTextRule
import me.lakko.app.calls.callChatInput
import me.lakko.app.calls.callChatInsertEmoji
import me.lakko.app.calls.callChatSenderName
import me.lakko.app.calls.callChatShowsSender
import me.lakko.app.calls.remoteParticipantId
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.media.EmojiAssets
import me.lakko.app.media.EmojiDataset
import me.lakko.app.media.MediaPrefs
import me.lakko.app.ui.chat.EmojiPickerSheet
import me.lakko.app.ui.chat.PickerMode
import me.lakko.app.ui.chat.copyMessageText
import me.lakko.app.ui.chat.formatClockTime
import me.lakko.app.ui.components.MessageLinkText
import me.lakko.app.ui.components.submitOnHardwareEnter
import me.lakko.app.ui.theme.LakkoDark
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/**
 * The chat of a group call — `docs/ux/calls.md` section 4.1.4. What is shown
 * and when is decided in `CallChatModel.kt`; this file only draws it.
 *
 * Two shapes: under 900 dp a panel over the whole call screen with Close and
 * the mute button in its header (the full panel hides the control row); at
 * 900 dp and wider a 360 dp column at the end side, beside the tiles, with
 * the control row still visible and no mute button.
 */

/** The name over a bubble, the one the sender's tile shows. */
@Composable
private fun senderNameOf(message: CallChatMessage, participants: List<CallUiState.Participant>): String {
    val tile = message.peerId?.let { peer ->
        participants.firstOrNull { it.id == remoteParticipantId(peer) }?.displayName
    }
    return callChatSenderName(message, tile).ifEmpty { t("call.participant.unnamed") }
}

@Composable
@Suppress("LongMethod", "LongParameterList")
internal fun CallChatPanel(
    handle: CallChatHandle,
    participants: List<CallUiState.Participant>,
    /** The end-side column of a wide screen; else the full panel. */
    column: Boolean,
    /** The microphone control, drawn as the call screen draws it; `null` on
     *  the column, which leaves the control row visible. */
    micControl: CallControl?,
    onToggleMic: () -> Unit,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
    rule: CallChatTextRule = NativeCallChatTextRule,
) {
    val model = handle.model
    val messages by model.messages.collectAsState()
    val pictures by model.pictures.collectAsState()
    val announcement by model.announcement.collectAsState()
    val strings = LocalStrings.current
    val panelName = t("call.chat.a11y")
    var copyTarget by remember { mutableStateOf<CallChatMessage?>(null) }
    // A picture opened full size over the call (section 4.1.4.1).
    var viewing by remember { mutableStateOf<ByteArray?>(null) }

    Box(modifier = modifier) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(if (column) LakkoDark.surface else LakkoDark.background)
            .windowInsetsPadding(
                if (column) {
                    WindowInsets.safeDrawing.only(WindowInsetsSides.Vertical + WindowInsetsSides.End)
                } else {
                    WindowInsets.safeDrawing
                },
            )
            .imePadding()
            .semantics { paneTitle = panelName },
    ) {
        // 1. The header.
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = 16.dp, end = 4.dp, top = 4.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = t("call.control.chat"),
                color = LakkoDark.text,
                fontSize = 17.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            if (micControl != null) {
                ChatMuteButton(micControl, onToggleMic)
            }
            val closeLabel = t("call.chat.close_a11y")
            IconButton(onClick = onClose, modifier = Modifier.semantics { contentDescription = closeLabel }) {
                LkIcon(
                    icon = if (column) R.drawable.ic_lk_xmark else R.drawable.ic_lk_chevron_down,
                    contentDescription = null,
                    tint = LakkoDark.text,
                )
            }
        }

        // 2. The messages.
        BoxWithConstraints(modifier = Modifier.weight(1f).fillMaxWidth()) {
            val bubbleMax = maxWidth * 0.65f
            if (messages.isEmpty()) {
                Column(
                    modifier = Modifier.align(Alignment.Center).padding(horizontal = 24.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    ChatNotice(strings["call.chat.notice_people"], strings["call.chat.notice_ends"], 15)
                }
            } else {
                ChatMessageList(
                    messages = messages,
                    pictures = pictures,
                    participants = participants,
                    bubbleMax = bubbleMax,
                    onLongPress = { message ->
                        // A picture has its sheet only once it can be saved.
                        if (message.picture == null || pictures[message.id] is CallChatPictureState.Ready) {
                            copyTarget = message
                        }
                    },
                    onOpenPicture = { viewing = it },
                    onSendAgain = { handle.control.resend(it.seq) },
                )
            }
        }

        // 3. The composer.
        ChatComposer(rule = rule, control = handle.control)
    }

    // A message that arrives while the panel is open is read out politely:
    // `Anna: text`.
    var spoken by remember { mutableStateOf("") }
    LaunchedEffect(announcement?.serial) {
        val a = announcement ?: return@LaunchedEffect
        val message = messages.firstOrNull { it.id == a.messageId } ?: return@LaunchedEffect
        val tile = message.peerId?.let { peer ->
            participants.firstOrNull { it.id == remoteParticipantId(peer) }?.displayName
        }
        val name = callChatSenderName(message, tile).ifEmpty { strings["call.participant.unnamed"] }
        val body = if (message.picture != null) strings["call.chat.picture"] else message.text
        spoken = strings.format("call.chat.message_a11y", mapOf("name" to name, "text" to body))
    }
    Box(
        modifier = Modifier
            .size(1.dp)
            .semantics {
                liveRegion = LiveRegionMode.Polite
                contentDescription = spoken
            },
    )

    // Long-press: the action sheet with one row, `Copy`, or `Save picture`
    // for a picture.
    copyTarget?.let { target ->
        val bytes = (pictures[target.id] as? CallChatPictureState.Ready)?.bytes
        if (target.picture != null && bytes != null) {
            ChatSavePictureSheet(bytes = bytes, onDismiss = { copyTarget = null })
        } else {
            ChatCopySheet(text = target.text, onDismiss = { copyTarget = null })
        }
    }
    viewing?.let { bytes ->
        CallChatPictureViewer(bytes = bytes, onClose = { viewing = null })
    }
    }
}

/** The two notice lines: centred in an empty chat, a small note at the top
 *  of the list once there are messages. */
@Composable
private fun ChatNotice(first: String, second: String, sizeSp: Int) {
    Text(first, color = LakkoDark.textMuted, fontSize = sizeSp.sp, textAlign = androidx.compose.ui.text.style.TextAlign.Center)
    Text(second, color = LakkoDark.textMuted, fontSize = sizeSp.sp, textAlign = androidx.compose.ui.text.style.TextAlign.Center)
}

@Composable
@Suppress("LongMethod")
private fun ChatMessageList(
    messages: List<CallChatMessage>,
    pictures: Map<String, CallChatPictureState>,
    participants: List<CallUiState.Participant>,
    bubbleMax: androidx.compose.ui.unit.Dp,
    onLongPress: (CallChatMessage) -> Unit,
    onOpenPicture: (ByteArray) -> Unit,
    onSendAgain: (CallChatMessage) -> Unit,
) {
    val strings = LocalStrings.current
    val listState = rememberLazyListState()
    val scope = rememberCoroutineScope()
    // The list holds the note first, then the messages.
    val lastIndex = messages.size
    val atBottom by remember { derivedStateOf { !listState.canScrollForward } }
    var newBelow by remember { mutableStateOf(false) }
    var seen by remember { mutableStateOf(-1) }

    LaunchedEffect(messages.size) {
        val before = seen
        seen = messages.size
        if (before < 0) {
            // The panel opened: at the newest.
            listState.scrollToItem(lastIndex)
            return@LaunchedEffect
        }
        if (messages.size <= before) return@LaunchedEffect
        val newest = messages.last()
        // At the bottom, or the person's own message: follow it. Scrolled
        // up: the list stays and the pill says there is more below.
        val wasAtBottom = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index?.let { it >= lastIndex - 1 } ?: true
        if (newest.own || wasAtBottom) {
            listState.animateScrollToItem(lastIndex)
            newBelow = false
        } else {
            newBelow = true
        }
    }
    LaunchedEffect(atBottom) { if (atBottom) newBelow = false }

    val zone = remember { ZoneId.systemDefault() }
    val context = LocalContext.current
    val locale = remember(strings.language) { Locale.forLanguageTag(strings.language) }
    val use24h = remember(context) { me.lakko.app.l10n.DeviceFormats.is24Hour(context) }

    Box(modifier = Modifier.fillMaxSize()) {
        LazyColumn(
            state = listState,
            modifier = Modifier.fillMaxSize(),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            item(key = "notice") {
                Column(
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 8.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    ChatNotice(strings["call.chat.notice_people"], strings["call.chat.notice_ends"], 12)
                }
            }
            itemsIndexed(messages, key = { _, m -> m.id }) { index, message ->
                val showSender = callChatShowsSender(messages, index)
                ChatBubble(
                    message = message,
                    picture = pictures[message.id],
                    senderName = if (showSender) senderNameOf(message, participants) else null,
                    time = formatClockTime(message.atMs, zone, use24h, locale),
                    maxWidth = bubbleMax,
                    onLongPress = { onLongPress(message) },
                    onOpenPicture = onOpenPicture,
                    onSendAgain = { onSendAgain(message) },
                )
            }
        }
        if (newBelow && !atBottom) {
            // While scrolled up, a new message does not move the list; this
            // pill takes the person to it.
            Box(
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(bottom = 8.dp)
                    .clip(RoundedCornerShape(50))
                    .background(LakkoDark.accent)
                    .clickable(role = Role.Button) {
                        newBelow = false
                        scope.launch { listState.animateScrollToItem(lastIndex) }
                    }
                    .padding(horizontal = 14.dp, vertical = 6.dp),
            ) {
                Text(t("call.chat.new_messages"), color = LakkoDark.accentFg, fontSize = 13.sp, fontWeight = FontWeight.Medium)
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
@Suppress("LongParameterList")
private fun ChatBubble(
    message: CallChatMessage,
    picture: CallChatPictureState?,
    senderName: String?,
    time: String,
    maxWidth: androidx.compose.ui.unit.Dp,
    onLongPress: () -> Unit,
    onOpenPicture: (ByteArray) -> Unit,
    onSendAgain: () -> Unit,
) {
    val own = message.own
    val notSent = own && message.delivery == CallChatDelivery.NOT_SENT
    val sending = own && message.delivery == CallChatDelivery.SENDING
    val fg = if (own) LakkoDark.accentFg else LakkoDark.text
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp),
        horizontalAlignment = if (own) Alignment.End else Alignment.Start,
    ) {
        if (senderName != null) {
            Text(
                text = senderName,
                color = LakkoDark.textMuted,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(start = 4.dp, top = 6.dp, bottom = 2.dp),
            )
        }
        val pictureSize = message.picture
        if (pictureSize != null) {
            PictureBubble(
                size = pictureSize,
                state = picture ?: CallChatPictureState.Loading,
                time = time,
                dimmed = notSent || sending,
                uploading = sending,
                onLongPress = onLongPress,
                onOpen = onOpenPicture,
            )
        } else {
            Column(
                modifier = Modifier
                    .widthIn(max = maxWidth)
                    .alpha(if (notSent || sending) 0.6f else 1f)
                    .clip(RoundedCornerShape(18.dp))
                    .background(if (own) LakkoDark.accent else LakkoDark.sunken)
                    .combinedClickable(onClick = {}, onLongClick = onLongPress)
                    .padding(horizontal = 14.dp, vertical = 10.dp),
            ) {
                MessageLinkText(
                    text = message.text,
                    style = TextStyle(fontSize = 15.sp, color = fg),
                    linkColor = if (own) fg else LakkoDark.accent,
                    color = fg,
                    onLongPress = onLongPress,
                )
                Text(
                    text = time,
                    color = if (own) fg.copy(alpha = 0.7f) else LakkoDark.textMuted,
                    fontSize = 11.sp,
                    modifier = Modifier.align(Alignment.End).padding(top = 2.dp),
                )
            }
        }
        if (notSent) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(t("call.chat.not_sent"), color = LakkoDark.danger, fontSize = 13.sp)
                TextButton(onClick = onSendAgain) {
                    Text(t("call.chat.send_again"), color = LakkoDark.textMuted, fontSize = 13.sp)
                }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
@Suppress("LongMethod")
private fun ChatComposer(rule: CallChatTextRule, control: CallChatControl) {
    // A state-based field: only it takes a pasted picture (the content
    // receiver), and the emoji picker puts an emoji at its caret (calls.md
    // 4.1.4, the emoji button).
    val field = rememberTextFieldState()
    var tooLong by remember { mutableStateOf(false) }
    var picking by remember { mutableStateOf(false) }
    // The tray above the field: one pasted picture at most (section 4.1.4.1).
    var tray by remember { mutableStateOf(CallChatTray()) }
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val takesPictures = control.takesPictures
    val text = field.text.toString()
    val canSend = callChatCanSendWith(text, tray.picture, rule)
    val send = {
        val plan = callChatSendPlan(field.text.toString(), tray.picture, rule)
        if (plan != null) {
            val picture = plan.picture
            if (picture != null) {
                control.sendPicture(picture, plan.text)
            } else {
                plan.text?.let(control::send)
            }
            // Text the rule refused stays in the field; white space goes.
            if (plan.text != null || field.text.isBlank()) field.clearText()
            tray = CallChatTray()
            tooLong = false
        }
    }
    val clamp = remember(rule) {
        InputTransformation {
            val raw = asCharSequence().toString()
            val input = callChatInput(raw, rule)
            if (input.text != raw) replace(0, length, input.text)
            tooLong = input.tooLong
        }
    }
    val receiver = remember(context) {
        ReceiveContentListener { content ->
            takePastedPicture(context, content) { uri ->
                scope.launch {
                    val paste = readPastedPicture(context, uri)
                    tray = callChatTrayAfterPaste(tray, paste)
                }
            }
        }
    }
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(LakkoDark.surface)
            .padding(horizontal = 12.dp, vertical = 8.dp),
    ) {
        tray.picture?.let { picture ->
            ComposerPictureTray(picture = picture, onRemove = { tray = callChatTrayRemoved(tray) })
            Spacer(Modifier.height(8.dp))
        }
        Row(verticalAlignment = Alignment.Bottom) {
            val placeholder = t("call.chat.placeholder")
            Box(
                modifier = Modifier
                    .weight(1f)
                    .heightIn(min = 40.dp)
                    .clip(RoundedCornerShape(20.dp))
                    .background(LakkoDark.sunken)
                    .padding(horizontal = 14.dp, vertical = 10.dp),
                contentAlignment = Alignment.CenterStart,
            ) {
                if (text.isEmpty()) {
                    Text(placeholder, color = LakkoDark.textMuted, fontSize = 15.sp, maxLines = 1)
                }
                BasicTextField(
                    state = field,
                    inputTransformation = clamp,
                    textStyle = TextStyle(fontSize = 15.sp, color = LakkoDark.text),
                    cursorBrush = SolidColor(LakkoDark.accent),
                    lineLimits = TextFieldLineLimits.MultiLine(maxHeightInLines = 5),
                    modifier = Modifier
                        .fillMaxWidth()
                        .semantics { contentDescription = placeholder }
                        .then(if (takesPictures) Modifier.contentReceiver(receiver) else Modifier)
                        .submitOnHardwareEnter { send() },
                )
            }
            // The emoji button: the conversation composer's smiling face,
            // between the field and Send; it opens that composer's picker,
            // Emoji tab only (calls.md 4.1.4).
            Spacer(Modifier.width(4.dp))
            val emojiLabel = t("call.chat.emoji")
            Box(
                modifier = Modifier
                    .size(44.dp)
                    .clip(CircleShape)
                    .clickable { picking = true }
                    .semantics {
                        contentDescription = emojiLabel
                        role = Role.Button
                    },
                contentAlignment = Alignment.Center,
            ) {
                LkIcon(
                    icon = R.drawable.ic_lk_face_smiling,
                    contentDescription = null,
                    tint = LakkoDark.textMuted,
                    size = LkIconSize.Row,
                )
            }
            val sendLabel = t("call.chat.send_a11y")
            // The compact composer's send: the bare arrow-in-circle glyph,
            // accent when sendable and muted when not, as on iOS and web.
            Box(
                modifier = Modifier
                    .size(44.dp)
                    .clip(CircleShape)
                    .clickable(enabled = canSend, onClick = send)
                    .semantics {
                        contentDescription = sendLabel
                        role = Role.Button
                        if (!canSend) disabled()
                    },
                contentAlignment = Alignment.Center,
            ) {
                LkIcon(
                    icon = R.drawable.ic_lk_arrow_up_circle_fill,
                    contentDescription = null,
                    tint = if (canSend) LakkoDark.accent else LakkoDark.textMuted,
                    size = 36.dp,
                )
            }
        }
        val notice = when (tray.notice) {
            CallChatTrayNotice.PICTURE_ONLY -> t("call.chat.picture_only")
            CallChatTrayNotice.TOO_LARGE -> t("call.chat.picture_too_large")
            null -> if (tooLong) t("call.chat.too_long") else null
        }
        if (notice != null) {
            Text(
                text = notice,
                color = LakkoDark.textMuted,
                fontSize = 13.sp,
                modifier = Modifier.padding(start = 14.dp, top = 4.dp),
            )
        }
    }
    if (picking) {
        CallChatEmojiPicker(
            onEmoji = { emoji ->
                val current = field.selection
                val insert = callChatInsertEmoji(
                    text = field.text.toString(),
                    selectionStart = current.start,
                    selectionEnd = current.end,
                    emoji = emoji,
                    rule = rule,
                )
                if (insert.inserted) {
                    field.edit {
                        replace(0, length, insert.text)
                        selection = TextRange(insert.caret)
                    }
                }
                tooLong = insert.tooLong
            },
            onDismiss = { picking = false },
        )
    }
}

/**
 * Take a pasted or keyboard-inserted picture out of [content] (section
 * 4.1.4.1): the first item that carries a URI and is not text. Whether it is a
 * picture is decided by decoding it, so a file that is not one still ends in
 * `Only pictures can be sent in a call.` Text goes back to the field.
 */
@OptIn(ExperimentalFoundationApi::class)
private fun takePastedPicture(
    context: android.content.Context,
    content: TransferableContent,
    onPicture: (android.net.Uri) -> Unit,
): TransferableContent? {
    var taken: android.net.Uri? = null
    val rest = content.consume { item ->
        val uri = item.uri ?: return@consume false
        val type = runCatching { context.contentResolver.getType(uri) }.getOrNull()?.lowercase()
        if (type != null && type.startsWith("text/")) return@consume false
        // One picture at a time: a second item of the same paste is dropped.
        if (taken == null) taken = uri
        true
    }
    taken?.let(onPicture)
    return rest
}

/** Read a pasted picture right away (the read grant holds only now) and
 *  prepare it as a conversation photo is prepared. */
private suspend fun readPastedPicture(context: android.content.Context, uri: android.net.Uri): CallChatPaste {
    val read = withContext(Dispatchers.IO) {
        me.lakko.app.media.readPickedUri(context, uri, AttachmentBridge.MAX_ATTACHMENT_BYTES)
    }
    return when (read) {
        is me.lakko.app.media.PickedRead.Ok -> prepareCallChatPaste(read.payload.bytes)
        is me.lakko.app.media.PickedRead.Failed ->
            if (read.error is me.lakko.app.chat.AttachmentBridgeException.TooLarge) {
                CallChatPaste.TooLarge
            } else {
                CallChatPaste.NotAPicture
            }
    }
}

/** The tray above the field: the picture 64 high with a cross to remove it. */
@Composable
private fun ComposerPictureTray(picture: PreparedCallChatPicture, onRemove: () -> Unit) {
    val density = LocalDensity.current
    val thumbPx = with(density) { (CALL_CHAT_TRAY_HEIGHT_DP * 3).dp.roundToPx() }
    val bitmap by produceState<ImageBitmap?>(null, picture) {
        value = withContext(Dispatchers.Default) { decodeCallChatBitmap(picture.jpeg, thumbPx) }
    }
    val width = callChatTrayThumbWidth(picture.width, picture.height)
    val removeLabel = t("call.chat.picture_remove")
    Box(
        modifier = Modifier
            .size(width = width.dp, height = CALL_CHAT_TRAY_HEIGHT_DP.dp)
            .clip(RoundedCornerShape(10.dp))
            .background(LakkoDark.sunken),
    ) {
        bitmap?.let {
            Image(
                bitmap = it,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
            )
        }
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .align(Alignment.TopEnd)
                .padding(2.dp)
                .size(20.dp)
                .clip(CircleShape)
                .background(Color.Black.copy(alpha = 0.55f))
                .clickable(role = Role.Button, onClickLabel = removeLabel, onClick = onRemove)
                .semantics { contentDescription = removeLabel },
        ) {
            LkIcon(R.drawable.ic_lk_xmark, contentDescription = null, tint = Color.White, size = 12.dp)
        }
    }
}

/**
 * A picture in the chat (section 4.1.4.1): fitted into 240 x 240, at least
 * 80 on the short side; a box of its shape with a spinner while it loads,
 * `Picture not available.` when it cannot be downloaded or opened. An own
 * picture is at 60% while it uploads (with a spinner) and when it was not
 * sent. A tap opens it full size.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
@Suppress("LongParameterList")
private fun PictureBubble(
    size: CallChatPictureSize,
    state: CallChatPictureState,
    time: String,
    dimmed: Boolean,
    uploading: Boolean,
    onLongPress: () -> Unit,
    onOpen: (ByteArray) -> Unit,
) {
    val box = callChatPictureBox(size.width, size.height)
    val density = LocalDensity.current
    val boxPx = with(density) { (CALL_CHAT_PICTURE_MAX_DP * 2).dp.roundToPx() }
    val ready = state as? CallChatPictureState.Ready
    // `null` while decoding, then the picture, or a failure that reads as
    // not available.
    val decoded by produceState<DecodedPicture?>(null, ready) {
        value = if (ready == null) {
            null
        } else {
            DecodedPicture(withContext(Dispatchers.Default) { decodeCallChatBitmap(ready.bytes, boxPx) })
        }
    }
    val bitmap = decoded?.bitmap
    val unavailable = state is CallChatPictureState.Unavailable || (decoded != null && bitmap == null)
    val spoken = t("call.chat.picture")
    Box(
        modifier = Modifier
            .size(width = box.width.dp, height = box.height.dp)
            .alpha(if (dimmed) 0.6f else 1f)
            .clip(RoundedCornerShape(18.dp))
            .background(LakkoDark.sunken)
            .combinedClickable(
                onClick = { if (bitmap != null && ready != null) onOpen(ready.bytes) },
                onLongClick = onLongPress,
            )
            .semantics { contentDescription = spoken },
        contentAlignment = Alignment.Center,
    ) {
        when {
            bitmap != null -> Image(
                bitmap = bitmap,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
            )
            unavailable -> Text(
                text = t("call.chat.picture_unavailable"),
                color = LakkoDark.textMuted,
                fontSize = 13.sp,
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                modifier = Modifier.padding(12.dp),
            )
            else -> CircularProgressIndicator(
                modifier = Modifier.size(24.dp),
                strokeWidth = 2.dp,
                color = LakkoDark.textMuted,
            )
        }
        if (uploading && bitmap != null) {
            CircularProgressIndicator(
                modifier = Modifier.size(24.dp),
                strokeWidth = 2.dp,
                color = Color.White,
            )
        }
        if (bitmap != null) {
            Text(
                text = time,
                color = Color.White,
                fontSize = 11.sp,
                modifier = Modifier
                    .align(Alignment.BottomEnd)
                    .padding(6.dp)
                    .clip(RoundedCornerShape(8.dp))
                    .background(Color.Black.copy(alpha = 0.45f))
                    .padding(horizontal = 6.dp, vertical = 1.dp),
            )
        }
    }
}

/** A decode that finished: [bitmap] is `null` when the bytes are not a
 *  picture. */
private class DecodedPicture(val bitmap: ImageBitmap?)

/** Decode [bytes] with its longest side about [maxSidePx] at most (a power
 *  of two below), never the full picture into memory for a thumbnail.
 *  `null` when it is not a picture. */
private fun decodeCallChatBitmap(bytes: ByteArray, maxSidePx: Int): ImageBitmap? = runCatching {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return@runCatching null
    var sample = 1
    while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= maxSidePx) sample *= 2
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, BitmapFactory.Options().apply { inSampleSize = sample })
        ?.asImageBitmap()
}.getOrNull()

/**
 * A picture full size over the call screen, on black (section 4.1.4.1):
 * closed by a tap, the back gesture or the close button. The call runs on
 * behind it.
 */
@Composable
private fun CallChatPictureViewer(bytes: ByteArray, onClose: () -> Unit) {
    val maxPx = with(LocalDensity.current) { 2048.dp.roundToPx() }.coerceAtMost(4096)
    val bitmap by produceState<ImageBitmap?>(null, bytes) {
        value = withContext(Dispatchers.Default) { decodeCallChatBitmap(bytes, maxPx) }
    }
    val spoken = t("call.chat.picture")
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(Color.Black)
                .clickable(onClick = onClose)
                .semantics { contentDescription = spoken },
            contentAlignment = Alignment.Center,
        ) {
            bitmap?.let {
                Image(
                    bitmap = it,
                    contentDescription = null,
                    contentScale = ContentScale.Fit,
                    modifier = Modifier.fillMaxSize(),
                )
            } ?: CircularProgressIndicator(color = Color.White)
            IconButton(
                onClick = onClose,
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .windowInsetsPadding(WindowInsets.safeDrawing)
                    .padding(8.dp),
            ) {
                LkIcon(R.drawable.ic_lk_xmark, contentDescription = t("common.close"), tint = Color.White)
            }
        }
    }
}

/** The action sheet of a picture: one row, `Save picture`, through the save
 *  the conversation's photo viewer uses (the system's save picker). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChatSavePictureSheet(bytes: ByteArray, onDismiss: () -> Unit) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    val saver = rememberFileSaver()
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState, containerColor = LakkoDark.surface) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(52.dp)
                .clickable(role = Role.Button) {
                    saver.save(name = CALL_CHAT_PICTURE_FILE_NAME, mime = "image/jpeg", bytes = bytes)
                    onDismiss()
                }
                .padding(horizontal = 20.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            LkIcon(R.drawable.ic_lk_download, contentDescription = null, tint = LakkoDark.text, size = LkIconSize.Row)
            Spacer(Modifier.width(16.dp))
            Text(t("call.chat.picture_save"), color = LakkoDark.text, fontSize = 15.sp)
        }
        Spacer(Modifier.height(16.dp))
    }
}

/** The name a saved picture is suggested under. */
private const val CALL_CHAT_PICTURE_FILE_NAME = "picture.jpg"

/** The conversation composer's emoji picker as a bottom sheet over the call
 *  chat panel: Emoji tab only (the call chat carries text), the shared Recent
 *  list, and the sheet stays open after a tap; a swipe down closes it. */
@Composable
private fun CallChatEmojiPicker(onEmoji: (String) -> Unit, onDismiss: () -> Unit) {
    val context = LocalContext.current
    var dataset by remember { mutableStateOf<EmojiDataset?>(null) }
    LaunchedEffect(Unit) {
        if (dataset == null) {
            dataset = withContext(Dispatchers.IO) { EmojiAssets.load(context) }
        }
    }
    EmojiPickerSheet(
        mode = PickerMode.COMPOSER,
        dataset = dataset,
        prefs = MediaPrefs.get(context),
        gifModel = null,
        loadGifPreview = { null },
        onEmoji = onEmoji,
        onGif = {},
        onDismiss = onDismiss,
    )
}

/** The round mute button of the collapsed bar, drawn as the call screen
 *  draws it (section 4.1.4: the full panel hides the control row). */
@Composable
private fun ChatMuteButton(control: CallControl, onToggle: () -> Unit) {
    val spoken = control.spokenLabel.text()
    Box(
        modifier = Modifier
            .size(48.dp)
            .alpha(if (control.enabled) 1f else 0.4f)
            .clickable(enabled = control.enabled, onClick = onToggle)
            .semantics(mergeDescendants = true) {
                contentDescription = spoken
                role = Role.Button
                if (!control.enabled) disabled()
            },
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .size(36.dp)
                .clip(CircleShape)
                .background(control.emphasis.fill()),
            contentAlignment = Alignment.Center,
        ) {
            LkIcon(
                icon = control.icon(),
                contentDescription = null,
                tint = control.emphasis.foreground(),
                modifier = Modifier.clearAndSetSemantics { },
                size = LkIconSize.Row,
            )
        }
    }
}

/** The design system's action sheet with one row, `Copy`. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChatCopySheet(text: String, onDismiss: () -> Unit) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    val context = LocalContext.current
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState, containerColor = LakkoDark.surface) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(52.dp)
                .clickable(role = Role.Button) {
                    copyMessageText(context, text)
                    onDismiss()
                }
                .padding(horizontal = 20.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            LkIcon(R.drawable.ic_lk_copy, contentDescription = null, tint = LakkoDark.text, size = LkIconSize.Row)
            Spacer(Modifier.width(16.dp))
            Text(t("common.copy"), color = LakkoDark.text, fontSize = 15.sp)
        }
        Spacer(Modifier.height(16.dp))
    }
}

/**
 * The one-line preview above the control bar while the panel is closed: the
 * sender's name in 13/600 and the start of the text. Taken down after 4
 * seconds; a second message replaces it and restarts the 4 seconds. Tapping
 * it opens the panel.
 */
@Composable
internal fun CallChatPreviewLine(
    preview: CallChatPreview,
    model: CallChatModel,
    participants: List<CallUiState.Participant>,
    modifier: Modifier = Modifier,
) {
    LaunchedEffect(preview.serial) {
        delay(CallChatModel.PREVIEW_MS)
        model.expirePreview(preview.serial)
    }
    val tile = participants.firstOrNull { it.id == remoteParticipantId(preview.peerId) }?.displayName
    val name = tile?.trim()?.takeIf { it.isNotEmpty() }
        ?: preview.senderName.trim().ifEmpty { t("call.participant.unnamed") }
    Row(
        modifier = modifier
            .widthIn(max = 480.dp)
            .clip(RoundedCornerShape(14.dp))
            .background(LakkoDark.background.copy(alpha = 0.78f))
            .clickable(role = Role.Button) { model.openPanel() }
            .padding(horizontal = 14.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = name,
            color = LakkoDark.text,
            fontSize = 13.sp,
            fontWeight = FontWeight.SemiBold,
            maxLines = 1,
        )
        Spacer(Modifier.width(8.dp))
        Text(
            text = if (preview.isPicture) t("call.chat.picture") else preview.text.replace('\n', ' '),
            color = LakkoDark.text,
            fontSize = 13.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}
