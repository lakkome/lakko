// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * Which of this device's camera senders encode and send now (ADR-0070,
 * "senders nobody watches"): the twin of the web `sender-activity.ts`.
 *
 * The SFU tells a publisher which of its own camera senders somebody is being
 * sent: a `video-needed` frame with the whole state each time, the last frame
 * wins. The light sender is always needed and is not named. This file holds
 *
 *   * the one decision ([senderActive]): a sender's encoding is active
 *     exactly when the camera is on, the sender exists, the people-count rule
 *     allows it (the middle sender only), and the SFU's last word for it is
 *     not `false`. No word yet means needed. The light sender ignores the
 *     SFU's word;
 *   * its application to one sender ([SenderActivity]), on the encoding's
 *     `active` flag and nothing else.
 *
 * Why the encoding's `active` flag and neither of the two other ways to
 * silence a sender: a disabled track is still encoded and sent (black
 * frames), so nothing is saved; and taking the track off the sender changes
 * what the media line negotiates at the next offer. The flag stops the
 * encoder and the packets and leaves the transceiver, the media line, the
 * SSRC and the SFrame encryptor, its key and its counter as they are: no
 * renegotiation.
 *
 * Pure apart from what the [ActivitySender] it is handed does: no Android,
 * no libwebrtc, no timers.
 */

/** The SFU's last `video-needed` frame. */
internal data class VideoNeeded(val main: Boolean, val middle: Boolean)

/**
 * The SFU's last word for one sender: `true` or `false` for the main and the
 * middle sender once a frame arrived, `null` before any frame and always for
 * the light sender, which the frame does not name.
 */
internal fun neededWord(kind: CameraSenderKind, needed: VideoNeeded?): Boolean? = when {
    needed == null || kind == CameraSenderKind.LIGHT -> null
    kind == CameraSenderKind.MAIN -> needed.main
    else -> needed.middle
}

/** What [senderActive] decides from. */
internal data class SenderActivityInput(
    /** The person's camera is on. */
    val cameraOn: Boolean,
    /** This device has that sender. */
    val exists: Boolean,
    /** The people-count rule (`middle_sender_wanted`) says the middle sender
     *  is sent. Read for the middle sender only. */
    val middleWanted: Boolean,
    /** The SFU's last `video-needed` frame; `null` before the first. */
    val needed: VideoNeeded?,
)

/**
 * Whether [kind]'s encoding is active: every condition has to hold.
 *
 *   * the camera is on;
 *   * the sender exists;
 *   * for the middle sender, the people-count rule;
 *   * the SFU's last word for it is not `false` (no word yet: needed). The
 *     light sender has no word and is never switched off by the SFU.
 */
internal fun senderActive(kind: CameraSenderKind, input: SenderActivityInput): Boolean {
    if (!input.cameraOn || !input.exists) return false
    if (kind == CameraSenderKind.MIDDLE && !input.middleWanted) return false
    return neededWord(kind, input.needed) != false
}

/** [senderActive] for every sender. */
internal fun sendersActive(
    cameraOn: Boolean,
    middleWanted: Boolean,
    needed: VideoNeeded?,
    exists: (CameraSenderKind) -> Boolean,
): Map<CameraSenderKind, Boolean> = CAMERA_SENDER_KINDS.associateWith { kind ->
    senderActive(kind, SenderActivityInput(cameraOn, exists(kind), middleWanted, needed))
}

/** A sender's `needed=` field in the diagnostics: `yes`, `no`, or `-` when
 *  the SFU has said nothing about it (the light sender always). */
internal fun neededField(word: Boolean?): String = when (word) {
    null -> "-"
    true -> "yes"
    false -> "no"
}

/** What the diagnostics say of one sender. */
internal data class SenderActivityNote(
    /** The SFU's last word; `null` when it has said nothing. */
    val needed: Boolean?,
    /** Whether the sender's encoding is asked to be active now. */
    val active: Boolean,
)

/** ` needed=yes|no|- active=yes|no`, the tail a `send video` line gains. */
internal fun describeSenderActivity(note: SenderActivityNote): String =
    " needed=${neededField(note.needed)} active=${if (note.active) "yes" else "no"}"

/**
 * The one line for a `video-needed` frame that changed which sender is
 * active: `call video needed main=.. middle=..` with what is active now.
 * `null` when the frame changed nothing that is active.
 */
internal fun describeVideoNeededChange(
    needed: VideoNeeded,
    before: Map<CameraSenderKind, Boolean>,
    after: Map<CameraSenderKind, Boolean>,
): String? {
    if (CAMERA_SENDER_KINDS.all { before[it] == after[it] }) return null
    fun yn(b: Boolean?): String = if (b == true) "yes" else "no"
    return "call video needed main=${yn(needed.main)} middle=${yn(needed.middle)}" +
        " active main=${yn(after[CameraSenderKind.MAIN])}" +
        " middle=${yn(after[CameraSenderKind.MIDDLE])}" +
        " light=${yn(after[CameraSenderKind.LIGHT])}"
}

/** The parts of a sender [SenderActivity] uses. */
internal interface ActivitySender {
    /** Whether the sender's first encoding is active now, as the platform
     *  holds it; `null` when the sender has no encoding. May throw. */
    fun encodingActive(): Boolean?

    /** Set the first encoding's `active` flag; `false` when the platform
     *  refuses. May throw. */
    fun setEncodingActive(active: Boolean): Boolean
}

/** What one application did. */
internal enum class ActivityOutcome {
    /** The encoding says what was wanted (already, or after it was set). */
    APPLIED,

    /** The sender has no encoding to set the flag on. */
    NO_ENCODING,

    /** The platform refused. */
    REFUSED,
}

/** One read-modify-set of the first encoding's `active` flag. Never throws. */
internal fun applyEncodingActive(sender: ActivitySender, active: Boolean): ActivityOutcome =
    try {
        when (sender.encodingActive()) {
            null -> ActivityOutcome.NO_ENCODING
            active -> ActivityOutcome.APPLIED
            else -> if (sender.setEncodingActive(active)) ActivityOutcome.APPLIED else ActivityOutcome.REFUSED
        }
    } catch (cause: RuntimeException) {
        ActivityOutcome.REFUSED
    }

/**
 * Keeps one sender's first encoding active or not, as was last asked.
 *
 *   * **Re-applied.** Every [want] reads the encoding as the platform holds
 *     it now, so asking again after a renegotiation, a replaced track or a
 *     new sender ([bind]) puts back a flag the platform reset.
 *   * **When in doubt, active.** A refused switch-off leaves the sender
 *     sending and is not asked again until something else is wanted. A
 *     refused switch-on is asked again at every later [want], because a
 *     sender somebody watches must not stay off. Either is said once per
 *     sender.
 *
 * libwebrtc's Java `setParameters` answers before it returns, so unlike the
 * web twin there is no pending write to wait for. The set itself is one
 * read-change-set on the sender's serial write path (`SenderParameters.kt`),
 * shared with the bitrate ceiling (`PublishBudget.kt`) and the sender's
 * set-up, so none of them puts back what another just changed.
 *
 * @param name names the sender in the one warning: `main`, `middle` or `light`.
 */
internal class SenderActivity(
    private val name: String,
    private val warn: (message: String) -> Unit = {},
) {
    private var sender: ActivitySender? = null
    private var wanted: Boolean? = null

    /** A switch-off the platform refused on this sender: not asked again
     *  while switch-off stays what is wanted. */
    private var offRefused = false
    private var warned = false

    /**
     * The sender this keeps. A different one (first publish, mid-call
     * camera-on, a copy added again) starts from nothing: no refusal is
     * remembered and its warning may be said. `null`: there is none.
     */
    fun bind(sender: ActivitySender?) {
        if (this.sender === sender) return
        this.sender = sender
        offRefused = false
        warned = false
    }

    /** Whether [bind] was given a sender. */
    val bound: Boolean get() = sender != null

    /** What was last asked; `null` before anything was. */
    val lastWanted: Boolean? get() = wanted

    /**
     * Ask for the encoding to be active or not. Safe to call at any time and
     * as often as wanted: it costs one read of the sender's parameters when
     * the encoding already says so.
     */
    fun want(active: Boolean): ActivityOutcome? {
        if (wanted != active) offRefused = false
        wanted = active
        val target = sender ?: return null
        if (!active && offRefused) return null
        val outcome = applyEncodingActive(target, active)
        if (outcome != ActivityOutcome.REFUSED) return outcome
        if (!active) offRefused = true
        if (!warned) {
            warned = true
            warn(
                "camera sender activity: the $name camera sender could not be " +
                    (if (active) "started, asking again" else "stopped, it goes on sending"),
            )
        }
        return outcome
    }

    /** Forget the sender and what was asked: the call is over. */
    fun reset() {
        sender = null
        wanted = null
        offRefused = false
        warned = false
    }
}
