// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import me.lakko.app.l10n.UiText

/**
 * The chat of a group call (`docs/ux/calls.md` section 4.1.4): what the call
 * screen draws. The messages, the panel's open state, the unread count on the
 * `Chat` circle, the 4-second preview above the control bar, and the polite
 * announcement while the panel is open. [CallChatChannel] is the protocol
 * half; the call adapters connect the two.
 *
 * Pure: no Android, no native library, so it is tested on the host JVM.
 * Used from the main thread.
 */

/** Whether a message this device wrote went out. */
public enum class CallChatDelivery {
    /** Gone to the server; never confirmed (nothing comes back). */
    SENT,

    /** Could not be sent: drawn at 60% with `Not sent.` and `Send again`. */
    NOT_SENT,
}

/** One message of the call's chat. */
public data class CallChatMessage(
    /** Unique in the call: `self:<seq>` or `<peerId>:<seq>`. */
    val id: String,
    /** Written on this device. */
    val own: Boolean,
    /** The sender's SFU peer id; `null` for this device's own. */
    val peerId: String?,
    val seq: ULong,
    val text: String,
    /** When it arrived on (or was written on) this device. */
    val atMs: Long,
    /** The sender's name when it arrived, for after they left the call; the
     *  screen prefers the name their tile shows now. Empty when unknown. */
    val senderName: String,
    /** Own messages only. */
    val delivery: CallChatDelivery = CallChatDelivery.SENT,
)

/** The one-line preview of a message that arrived while the panel was closed. */
public data class CallChatPreview(
    /** Increases with every preview, so the 4 seconds restart and an old
     *  timer never takes down a newer one. */
    val serial: Long,
    val messageId: String,
    val peerId: String,
    val senderName: String,
    val text: String,
)

/** A message to announce while the panel is open (`Anna: text`). */
public data class CallChatAnnouncement(val serial: Long, val messageId: String)

/** The chat state of one call. */
public class CallChatModel(private val nowMs: () -> Long = System::currentTimeMillis) {
    private val _messages = MutableStateFlow<List<CallChatMessage>>(emptyList())
    public val messages: StateFlow<List<CallChatMessage>> = _messages.asStateFlow()

    private val _panelOpen = MutableStateFlow(false)
    public val panelOpen: StateFlow<Boolean> = _panelOpen.asStateFlow()

    private val _unread = MutableStateFlow(0)
    public val unread: StateFlow<Int> = _unread.asStateFlow()

    private val _preview = MutableStateFlow<CallChatPreview?>(null)
    public val preview: StateFlow<CallChatPreview?> = _preview.asStateFlow()

    private val _announcement = MutableStateFlow<CallChatAnnouncement?>(null)
    public val announcement: StateFlow<CallChatAnnouncement?> = _announcement.asStateFlow()

    private var serial = 0L

    /** This device wrote message [seq]. */
    public fun addOwn(seq: ULong, text: String, sent: Boolean) {
        val id = ownId(seq)
        if (_messages.value.any { it.id == id }) return
        _messages.value = _messages.value + CallChatMessage(
            id = id,
            own = true,
            peerId = null,
            seq = seq,
            text = text,
            atMs = nowMs(),
            senderName = "",
            delivery = if (sent) CallChatDelivery.SENT else CallChatDelivery.NOT_SENT,
        )
    }

    /** Message [seq] of this device's was sent again ([sent]) or not. */
    public fun setOwnDelivery(seq: ULong, sent: Boolean) {
        val id = ownId(seq)
        _messages.value = _messages.value.map {
            if (it.id == id) it.copy(delivery = if (sent) CallChatDelivery.SENT else CallChatDelivery.NOT_SENT) else it
        }
    }

    /** A verified message from [peerId] arrived. Never counted or previewed
     *  while the panel is open: then it is announced instead. */
    public fun addReceived(peerId: String, seq: ULong, text: String, senderName: String) {
        val id = "$peerId:$seq"
        if (_messages.value.any { it.id == id }) return
        _messages.value = _messages.value + CallChatMessage(
            id = id,
            own = false,
            peerId = peerId,
            seq = seq,
            text = text,
            atMs = nowMs(),
            senderName = senderName,
        )
        serial += 1
        if (_panelOpen.value) {
            _announcement.value = CallChatAnnouncement(serial, id)
        } else {
            _unread.value = _unread.value + 1
            _preview.value = CallChatPreview(serial, id, peerId, senderName, text)
        }
    }

    /** Open the panel: the count and the preview go. */
    public fun openPanel() {
        _panelOpen.value = true
        _unread.value = 0
        _preview.value = null
    }

    public fun closePanel() {
        _panelOpen.value = false
    }

    public fun togglePanel() {
        if (_panelOpen.value) closePanel() else openPanel()
    }

    /** The preview numbered [serial] has been up for its 4 seconds. A newer
     *  preview stays. */
    public fun expirePreview(serial: Long) {
        if (_preview.value?.serial == serial) _preview.value = null
    }

    /** The call is over for this device: every message goes. */
    public fun clear() {
        _messages.value = emptyList()
        _panelOpen.value = false
        _unread.value = 0
        _preview.value = null
        _announcement.value = null
    }

    private fun ownId(seq: ULong) = "self:$seq"

    public companion object {
        /** How long the preview line stays (section 4.1.4). */
        public const val PREVIEW_MS: Long = 4_000L
    }
}

/** What the call screen can do with the chat. */
public interface CallChatControl {
    /** Send [text], which passed [CallChatTextRule.check]. */
    public fun send(text: String)

    /** Send message [seq] of this device's again: the same bytes. */
    public fun resend(seq: ULong)
}

/** The chat of the call on screen, as [CallUiState] holds it. */
public class CallChatHandle(public val model: CallChatModel, public val control: CallChatControl)

/**
 * The control a call adapter hands the screen: a message is shown once the
 * session numbered it, sent or not; `Send again` updates the same bubble.
 */
public fun callChatControl(
    model: CallChatModel,
    sendOut: (text: String, done: (CallChatSendResult) -> Unit) -> Unit,
    resendOut: (seq: ULong, done: (sent: Boolean) -> Unit) -> Unit,
): CallChatControl = object : CallChatControl {
    override fun send(text: String) {
        sendOut(text) { result -> model.addOwn(result.seq, text, result.sent) }
    }

    override fun resend(seq: ULong) {
        resendOut(seq) { sent -> model.setOwnDelivery(seq, sent) }
    }
}

/** [callChatControl] over an SFU call session. */
internal fun sessionChatControl(session: SfuCallSession, model: CallChatModel): CallChatControl =
    callChatControl(model, session::sendCallChat, session::resendCallChat)

/** The count on the `Chat` circle: `1` to `9`, then `9+`; nothing at 0. */
public fun callChatBadgeText(unread: Int): String? = when {
    unread <= 0 -> null
    unread > 9 -> "9+"
    else -> unread.toString()
}

/** The `Chat` circle's spoken name: the action, and the count while closed. */
public fun callChatSpokenLabel(open: Boolean, unread: Int): UiText = when {
    open -> UiText("call.control.chat_hide_a11y")
    unread > 0 -> UiText("call.control.chat_unread_a11y", counts = mapOf("count" to unread.toULong()))
    else -> UiText("call.control.chat_show_a11y")
}

/** Section 4.1.4: the panel is an end-side column at 900 dp and wider, a
 *  full-screen panel below. */
public const val CALL_CHAT_COLUMN_MIN_WIDTH_DP: Float = 900f

/** The column's width on a wide screen. */
public const val CALL_CHAT_COLUMN_WIDTH_DP: Float = 360f

public fun callChatIsColumn(screenWidthDp: Float): Boolean = screenWidthDp >= CALL_CHAT_COLUMN_MIN_WIDTH_DP

/** Whether the preview line is drawn now: only while the panel is closed,
 *  the control bar is up, and no sheet or list is over the call. */
public fun callChatPreviewShown(
    preview: CallChatPreview?,
    panelOpen: Boolean,
    controlsVisible: Boolean,
    overlayOpen: Boolean,
): Boolean = preview != null && !panelOpen && controlsVisible && !overlayOpen

/** Whether the sender's name goes above [index]: the first of a run of
 *  bubbles from one other person. */
public fun callChatShowsSender(messages: List<CallChatMessage>, index: Int): Boolean {
    val message = messages.getOrNull(index) ?: return false
    if (message.own) return false
    val previous = messages.getOrNull(index - 1) ?: return true
    return previous.own || previous.peerId != message.peerId
}

/** The name over a bubble: the name the person's tile shows now, else the
 *  one they had when the message arrived, else empty (the screen then draws
 *  the unnamed state). */
public fun callChatSenderName(message: CallChatMessage, tileName: String?): String =
    tileName?.trim()?.takeIf { it.isNotEmpty() } ?: message.senderName.trim()

/** The text rule of the core (`lakko_calls::call_chat`): written once, used
 *  through the FFI. */
public interface CallChatTextRule {
    /** `null` when [text] may be sent, else the reason. */
    public fun check(text: String): String?

    /** [text] cut to the limit on a character boundary. */
    public fun clamp(text: String): String

    /** The limit, in UTF-8 bytes. */
    public val maxBytes: Int
}

/** The composer's text after an edit, and whether the too-long line shows. */
public data class CallChatInput(val text: String, val tooLong: Boolean)

/**
 * An edit of the composer: line ends made LF, the text cut to the limit; the
 * too-long line shows when the edit was cut or the text is at the limit, so
 * the field takes no more.
 */
public fun callChatInput(raw: String, rule: CallChatTextRule): CallChatInput {
    val normalized = raw.replace("\r\n", "\n").replace('\r', '\n')
    val clamped = rule.clamp(normalized)
    val atLimit = clamped.toByteArray(Charsets.UTF_8).size >= rule.maxBytes
    return CallChatInput(clamped, tooLong = clamped != normalized || atLimit)
}

/** The send button is enabled. */
public fun callChatCanSend(text: String, rule: CallChatTextRule): Boolean = rule.check(text) == null

/** The rule from the native core. A core that cannot be reached sends
 *  nothing (fail closed) and clamps nothing. */
public object NativeCallChatTextRule : CallChatTextRule {
    override fun check(text: String): String? = try {
        uniffi.lakko_ffi_uniffi.checkCallChatText(text)
    } catch (_: Throwable) {
        "unavailable"
    }

    override fun clamp(text: String): String = try {
        uniffi.lakko_ffi_uniffi.clampCallChatText(text)
    } catch (_: Throwable) {
        text
    }

    override val maxBytes: Int by lazy {
        try {
            uniffi.lakko_ffi_uniffi.callChatMaxTextBytes().toInt()
        } catch (_: Throwable) {
            DEFAULT_MAX_BYTES
        }
    }

    /** `lakko_calls::call_chat::MAX_TEXT_BYTES`, for a core that cannot be
     *  read. */
    private const val DEFAULT_MAX_BYTES = 800
}
