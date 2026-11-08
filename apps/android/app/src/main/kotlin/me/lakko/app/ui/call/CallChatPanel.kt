// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.ExperimentalFoundationApi
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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.ExpandMore
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
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.ZoneId
import java.util.Locale
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.calls.CallChatDelivery
import me.lakko.app.calls.CallChatHandle
import me.lakko.app.calls.CallChatMessage
import me.lakko.app.calls.CallChatModel
import me.lakko.app.calls.CallChatPreview
import me.lakko.app.calls.CallChatTextRule
import me.lakko.app.calls.CallControl
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.NativeCallChatTextRule
import me.lakko.app.calls.callChatCanSend
import me.lakko.app.calls.callChatInput
import me.lakko.app.calls.callChatSenderName
import me.lakko.app.calls.callChatShowsSender
import me.lakko.app.calls.remoteParticipantId
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.chat.copyMessageText
import me.lakko.app.ui.chat.formatClockTime
import me.lakko.app.ui.components.MessageLinkText
import me.lakko.app.ui.components.submitOnHardwareEnter
import me.lakko.app.ui.theme.LakkoDark

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
    val announcement by model.announcement.collectAsState()
    val strings = LocalStrings.current
    val panelName = t("call.chat.a11y")
    var copyTarget by remember { mutableStateOf<CallChatMessage?>(null) }

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
                Icon(
                    imageVector = if (column) Icons.Filled.Close else Icons.Filled.ExpandMore,
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
                    participants = participants,
                    bubbleMax = bubbleMax,
                    onLongPress = { copyTarget = it },
                    onSendAgain = { handle.control.resend(it.seq) },
                )
            }
        }

        // 3. The composer.
        ChatComposer(rule = rule, onSend = { handle.control.send(it) })
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
        spoken = strings.format("call.chat.message_a11y", mapOf("name" to name, "text" to message.text))
    }
    Box(
        modifier = Modifier
            .size(1.dp)
            .semantics {
                liveRegion = LiveRegionMode.Polite
                contentDescription = spoken
            },
    )

    // Long-press: the action sheet with one row, `Copy`.
    copyTarget?.let { target ->
        ChatCopySheet(text = target.text, onDismiss = { copyTarget = null })
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
    participants: List<CallUiState.Participant>,
    bubbleMax: androidx.compose.ui.unit.Dp,
    onLongPress: (CallChatMessage) -> Unit,
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
                    senderName = if (showSender) senderNameOf(message, participants) else null,
                    time = formatClockTime(message.atMs, zone, use24h, locale),
                    maxWidth = bubbleMax,
                    onLongPress = { onLongPress(message) },
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
    senderName: String?,
    time: String,
    maxWidth: androidx.compose.ui.unit.Dp,
    onLongPress: () -> Unit,
    onSendAgain: () -> Unit,
) {
    val own = message.own
    val notSent = own && message.delivery == CallChatDelivery.NOT_SENT
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
        Column(
            modifier = Modifier
                .widthIn(max = maxWidth)
                .alpha(if (notSent) 0.6f else 1f)
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

@Composable
private fun ChatComposer(rule: CallChatTextRule, onSend: (String) -> Unit) {
    var text by remember { mutableStateOf("") }
    var tooLong by remember { mutableStateOf(false) }
    val canSend = callChatCanSend(text, rule)
    val send = {
        if (callChatCanSend(text, rule)) {
            onSend(text)
            text = ""
            tooLong = false
        }
    }
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(LakkoDark.surface)
            .padding(horizontal = 12.dp, vertical = 8.dp),
    ) {
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
                    value = text,
                    onValueChange = { raw ->
                        val input = callChatInput(raw, rule)
                        text = input.text
                        tooLong = input.tooLong
                    },
                    textStyle = TextStyle(fontSize = 15.sp, color = LakkoDark.text),
                    cursorBrush = SolidColor(LakkoDark.accent),
                    maxLines = 5,
                    modifier = Modifier
                        .fillMaxWidth()
                        .semantics { contentDescription = placeholder }
                        .submitOnHardwareEnter { send() },
                )
            }
            Spacer(Modifier.width(8.dp))
            val sendLabel = t("call.chat.send_a11y")
            Box(
                modifier = Modifier
                    .size(40.dp)
                    .alpha(if (canSend) 1f else 0.4f)
                    .clip(CircleShape)
                    .background(LakkoDark.accent)
                    .clickable(enabled = canSend, onClick = send)
                    .semantics {
                        contentDescription = sendLabel
                        role = Role.Button
                        if (!canSend) disabled()
                    },
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.AutoMirrored.Filled.Send,
                    contentDescription = null,
                    tint = LakkoDark.accentFg,
                    modifier = Modifier.size(20.dp),
                )
            }
        }
        if (tooLong) {
            Text(
                text = t("call.chat.too_long"),
                color = LakkoDark.textMuted,
                fontSize = 13.sp,
                modifier = Modifier.padding(start = 14.dp, top = 4.dp),
            )
        }
    }
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
            Icon(
                imageVector = control.icon(),
                contentDescription = null,
                tint = control.emphasis.foreground(),
                modifier = Modifier.size(20.dp).clearAndSetSemantics { },
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
            Icon(Icons.Filled.ContentCopy, contentDescription = null, tint = LakkoDark.text)
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
            text = preview.text.replace('\n', ' '),
            color = LakkoDark.text,
            fontSize = 13.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}
