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

    /** Still going out (section 4.1.4.1): a picture uploading, drawn at 60%
     *  with a spinner, or the text sent with it, which waits for it. */
    SENDING,
}

/** A picture's pixel size, as its message carries it. */
public data class CallChatPictureSize(val width: Int, val height: Int)

/** What a picture bubble can draw (section 4.1.4.1). The bytes live in
 *  memory only and go with the call's messages. */
public sealed interface CallChatPictureState {
    /** Being downloaded and decrypted. */
    public data object Loading : CallChatPictureState

    /** The decrypted picture (JPEG), in memory. */
    public class Ready(public val bytes: ByteArray) : CallChatPictureState

    /** Could not be downloaded or opened: `Picture not available.` */
    public data object Unavailable : CallChatPictureState
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
    /** A picture message (section 4.1.4.1): its pixel size; [text] is empty.
     *  What the bubble draws is in [CallChatModel.pictures]. */
    val picture: CallChatPictureSize? = null,
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
    /** A picture: the line reads `Anna: Picture`. */
    val isPicture: Boolean = false,
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

    /** What each picture message draws, by message id (section 4.1.4.1). */
    private val _pictures = MutableStateFlow<Map<String, CallChatPictureState>>(emptyMap())
    public val pictures: StateFlow<Map<String, CallChatPictureState>> = _pictures.asStateFlow()

    private var serial = 0L

    /** This device wrote message [seq]. */
    public fun addOwn(seq: ULong, text: String, sent: Boolean) {
        addOwnMessage(seq, text, if (sent) CallChatDelivery.SENT else CallChatDelivery.NOT_SENT, picture = null)
    }

    /** This device wrote message [seq], which waits for the picture sent
     *  before it (section 4.1.4.1: the picture first, the text after). */
    public fun addOwnPending(seq: ULong, text: String) {
        addOwnMessage(seq, text, CallChatDelivery.SENDING, picture = null)
    }

    /** This device sent picture [seq] ([jpeg], already prepared): shown at
     *  once, at 60% with a spinner until it is sent. */
    public fun addOwnPicture(seq: ULong, size: CallChatPictureSize, jpeg: ByteArray) {
        if (addOwnMessage(seq, "", CallChatDelivery.SENDING, size)) {
            _pictures.value = _pictures.value + (ownId(seq) to CallChatPictureState.Ready(jpeg))
        }
    }

    private fun addOwnMessage(
        seq: ULong,
        text: String,
        delivery: CallChatDelivery,
        picture: CallChatPictureSize?,
    ): Boolean {
        val id = ownId(seq)
        if (_messages.value.any { it.id == id }) return false
        _messages.value = _messages.value + CallChatMessage(
            id = id,
            own = true,
            peerId = null,
            seq = seq,
            text = text,
            atMs = nowMs(),
            senderName = "",
            delivery = delivery,
            picture = picture,
        )
        return true
    }

    /** Message [seq] of this device's was sent again ([sent]) or not. */
    public fun setOwnDelivery(seq: ULong, sent: Boolean) {
        setOwnDeliveryState(seq, if (sent) CallChatDelivery.SENT else CallChatDelivery.NOT_SENT)
    }

    /** Message [seq] of this device's is going out again (a picture whose
     *  upload is tried again). */
    public fun setOwnSending(seq: ULong) {
        setOwnDeliveryState(seq, CallChatDelivery.SENDING)
    }

    private fun setOwnDeliveryState(seq: ULong, delivery: CallChatDelivery) {
        val id = ownId(seq)
        _messages.value = _messages.value.map {
            if (it.id == id) it.copy(delivery = delivery) else it
        }
    }

    /** The delivery of this device's message [seq], `null` when there is
     *  none. */
    public fun ownDelivery(seq: ULong): CallChatDelivery? =
        _messages.value.firstOrNull { it.id == ownId(seq) }?.delivery

    /**
     * A verified picture message from [peerId] arrived: shown at once as a
     * box of its shape with a spinner, counted and previewed as `Picture`.
     * Returns the message id, or `null` when it was already shown.
     */
    public fun addReceivedPicture(peerId: String, seq: ULong, size: CallChatPictureSize, senderName: String): String? {
        val id = "$peerId:$seq"
        if (_messages.value.any { it.id == id }) return null
        _pictures.value = _pictures.value + (id to CallChatPictureState.Loading)
        addReceivedMessage(id, peerId, seq, "", senderName, size)
        return id
    }

    /** What picture message [id] draws now. Nothing for a message that is
     *  not there (the call ended). */
    public fun setPicture(id: String, state: CallChatPictureState) {
        if (_messages.value.none { it.id == id }) return
        _pictures.value = _pictures.value + (id to state)
    }

    /** A verified message from [peerId] arrived. Never counted or previewed
     *  while the panel is open: then it is announced instead. */
    public fun addReceived(peerId: String, seq: ULong, text: String, senderName: String) {
        val id = "$peerId:$seq"
        if (_messages.value.any { it.id == id }) return
        addReceivedMessage(id, peerId, seq, text, senderName, picture = null)
    }

    private fun addReceivedMessage(
        id: String,
        peerId: String,
        seq: ULong,
        text: String,
        senderName: String,
        picture: CallChatPictureSize?,
    ) {
        _messages.value = _messages.value + CallChatMessage(
            id = id,
            own = false,
            peerId = peerId,
            seq = seq,
            text = text,
            atMs = nowMs(),
            senderName = senderName,
            picture = picture,
        )
        serial += 1
        if (_panelOpen.value) {
            _announcement.value = CallChatAnnouncement(serial, id)
        } else {
            _unread.value = _unread.value + 1
            _preview.value = CallChatPreview(serial, id, peerId, senderName, text, isPicture = picture != null)
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
        _pictures.value = emptyMap()
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

    /** Send message [seq] of this device's again: the same bytes, or for a
     *  picture whose upload never finished, the upload tried again. */
    public fun resend(seq: ULong)

    /** Whether [sendPicture] does anything: the composer takes a pasted
     *  picture only then. */
    public val takesPictures: Boolean get() = false

    /** Send [picture] (section 4.1.4.1), then [text] when it is not `null`
     *  (it passed [CallChatTextRule.check]) as a second message. */
    public fun sendPicture(picture: PreparedCallChatPicture, text: String?) {}
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

/** The composer after an emoji from the picker: the text, the caret, and
 *  whether the too-long line shows. */
public data class CallChatEmojiInsert(
    val text: String,
    val caret: Int,
    val inserted: Boolean,
    val tooLong: Boolean,
)

/**
 * An emoji from the picker (calls.md 4.1.4, the emoji button): it replaces
 * the selection, or goes in at the caret, and the caret moves past it. An
 * emoji that would take the text past the limit is not put in: the text and
 * the caret stay, and the too-long line shows. The picker never sends.
 */
public fun callChatInsertEmoji(
    text: String,
    selectionStart: Int,
    selectionEnd: Int,
    emoji: String,
    rule: CallChatTextRule,
): CallChatEmojiInsert {
    val start = minOf(selectionStart, selectionEnd).coerceIn(0, text.length)
    val end = maxOf(selectionStart, selectionEnd).coerceIn(0, text.length)
    val candidate = text.replaceRange(start, end, emoji)
    val bytes = candidate.toByteArray(Charsets.UTF_8).size
    if (emoji.isEmpty() || bytes > rule.maxBytes || rule.clamp(candidate) != candidate) {
        return CallChatEmojiInsert(text, caret = end, inserted = false, tooLong = emoji.isNotEmpty())
    }
    return CallChatEmojiInsert(
        text = candidate,
        caret = start + emoji.length,
        inserted = true,
        tooLong = bytes >= rule.maxBytes,
    )
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

// ---------------------------------------------------------------------------
// Pictures pasted into the call chat (`docs/ux/calls.md` section 4.1.4.1)
// ---------------------------------------------------------------------------

/** A picture ready to send: re-encoded as JPEG on this device (no metadata),
 *  its longest side at most the conversation's limit. In memory only. */
public class PreparedCallChatPicture(
    public val jpeg: ByteArray,
    public val width: Int,
    public val height: Int,
) {
    public val size: CallChatPictureSize get() = CallChatPictureSize(width, height)
}

/** The line under the field after a paste the tray did not take. */
public enum class CallChatTrayNotice {
    /** `Only pictures can be sent in a call.` */
    PICTURE_ONLY,

    /** `This picture is too large to send in a call.` */
    TOO_LARGE,
}

/** What a paste came to, once read and prepared. */
public sealed interface CallChatPaste {
    public class Picture(public val picture: PreparedCallChatPicture) : CallChatPaste

    /** It did not decode as a picture. */
    public data object NotAPicture : CallChatPaste

    /** Larger than the limit after preparing. */
    public data object TooLarge : CallChatPaste
}

/** The tray above the field: one picture at most, and the line under the
 *  field after a paste it did not take. */
public data class CallChatTray(
    val picture: PreparedCallChatPicture? = null,
    val notice: CallChatTrayNotice? = null,
)

/** The tray after a paste: a picture replaces the one there; anything else
 *  leaves the tray as it was and says why under the field. */
public fun callChatTrayAfterPaste(tray: CallChatTray, paste: CallChatPaste): CallChatTray = when (paste) {
    is CallChatPaste.Picture -> CallChatTray(picture = paste.picture, notice = null)
    CallChatPaste.NotAPicture -> tray.copy(notice = CallChatTrayNotice.PICTURE_ONLY)
    CallChatPaste.TooLarge -> tray.copy(notice = CallChatTrayNotice.TOO_LARGE)
}

/** The tray after its cross. */
public fun callChatTrayRemoved(tray: CallChatTray): CallChatTray = CallChatTray()

/**
 * Prepare a pasted [bytes] the way a conversation photo is: [reencode]
 * decodes it, brings its longest side down to [maxLongestSide] and writes a
 * JPEG with no metadata; [boundsOf] reads the JPEG's pixel size. Pure over
 * the two functions, so the rule is tested without Android.
 */
public fun prepareCallChatPicture(
    bytes: ByteArray,
    maxLongestSide: Int,
    maxBytes: Int,
    reencode: (ByteArray, Int) -> ByteArray?,
    boundsOf: (ByteArray) -> CallChatPictureSize?,
): CallChatPaste {
    if (bytes.isEmpty()) return CallChatPaste.NotAPicture
    val jpeg = reencode(bytes, maxLongestSide) ?: return CallChatPaste.NotAPicture
    val size = boundsOf(jpeg) ?: return CallChatPaste.NotAPicture
    if (size.width !in 1..CALL_CHAT_MAX_PICTURE_SIDE || size.height !in 1..CALL_CHAT_MAX_PICTURE_SIDE) {
        return CallChatPaste.NotAPicture
    }
    if (jpeg.size > maxBytes) return CallChatPaste.TooLarge
    return CallChatPaste.Picture(PreparedCallChatPicture(jpeg, size.width, size.height))
}

/** `lakko_calls::call_chat::MAX_CALL_CHAT_PICTURE_SIDE`. */
public const val CALL_CHAT_MAX_PICTURE_SIDE: Int = 8192

/** `lakko_calls::call_chat::MAX_CALL_CHAT_PICTURE_BYTES`, for a core that
 *  cannot be read. */
public const val CALL_CHAT_DEFAULT_MAX_PICTURE_BYTES: Int = 10 * 1024 * 1024

/** What one press of Send sends, in order. */
public data class CallChatSendPlan(
    /** The tray's picture goes first, as a message of its own. */
    val picture: PreparedCallChatPicture?,
    /** Then the text, when there is text the rule accepts. */
    val text: String?,
)

/**
 * What Send sends: the tray's picture first, then the field's text as a
 * second message. `null` when Send is disabled: no picture and no text the
 * rule accepts. Text the rule refuses is not sent and stays in the field.
 */
public fun callChatSendPlan(text: String, picture: PreparedCallChatPicture?, rule: CallChatTextRule): CallChatSendPlan? {
    val sendable = text.takeIf { rule.check(it) == null }
    if (picture == null && sendable == null) return null
    return CallChatSendPlan(picture, sendable)
}

/** Send is enabled: a picture in the tray, or text the rule accepts. */
public fun callChatCanSendWith(text: String, picture: PreparedCallChatPicture?, rule: CallChatTextRule): Boolean =
    callChatSendPlan(text, picture, rule) != null

/** A picture bubble's box, in dp. */
public data class CallChatBox(val width: Int, val height: Int)

/** Section 4.1.4.1: the picture fitted into 240 x 240. */
public const val CALL_CHAT_PICTURE_MAX_DP: Int = 240

/** Section 4.1.4.1: at least 80 on the short side. */
public const val CALL_CHAT_PICTURE_MIN_SHORT_DP: Int = 80

/**
 * The bubble of a [width] x [height] picture (its pixels taken as dp): fitted
 * into 240 x 240 keeping its shape and never enlarged, then its short side
 * brought up to at least 80 keeping its shape while the long side fits, else
 * the long side stays at 240 and the picture is cropped to the box. A size
 * that cannot be read is a 240 square.
 */
public fun callChatPictureBox(width: Int, height: Int): CallChatBox {
    if (width <= 0 || height <= 0) return CallChatBox(CALL_CHAT_PICTURE_MAX_DP, CALL_CHAT_PICTURE_MAX_DP)
    val max = CALL_CHAT_PICTURE_MAX_DP.toDouble()
    val min = CALL_CHAT_PICTURE_MIN_SHORT_DP.toDouble()
    var scale = minOf(1.0, max / width, max / height)
    val short = minOf(width, height) * scale
    if (short < min) scale = minOf(min / minOf(width, height), max / maxOf(width, height)).coerceAtLeast(scale)
    val w = Math.round(width * scale).toInt().coerceIn(CALL_CHAT_PICTURE_MIN_SHORT_DP, CALL_CHAT_PICTURE_MAX_DP)
    val h = Math.round(height * scale).toInt().coerceIn(CALL_CHAT_PICTURE_MIN_SHORT_DP, CALL_CHAT_PICTURE_MAX_DP)
    return CallChatBox(w, h)
}

/** The tray's thumbnail: 64 high, its width by the picture's shape, at most
 *  three times its height (a wider picture is cropped). */
public fun callChatTrayThumbWidth(width: Int, height: Int): Int {
    if (width <= 0 || height <= 0) return CALL_CHAT_TRAY_HEIGHT_DP
    val w = Math.round(CALL_CHAT_TRAY_HEIGHT_DP.toDouble() * width / height).toInt()
    return w.coerceIn(CALL_CHAT_TRAY_HEIGHT_DP / 2, CALL_CHAT_TRAY_HEIGHT_DP * 3)
}

/** Section 4.1.4.1: the tray's thumbnail is 64 high. */
public const val CALL_CHAT_TRAY_HEIGHT_DP: Int = 64
