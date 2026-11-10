// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * The Calls-list row of a call with no conversation — `docs/ux/navigation.md`
 * section 5.1.1.
 *
 * A call started with `New call`, and a call this device joined from a call
 * link, belong to no conversation, so nothing writes the transcript row the
 * conversation calls get ([me.lakko.app.chat.ChatRepository.recordCallRow]).
 * [LinkCallHost] runs both, and keeps one of these per start:
 *
 *  - [live] when the call reaches the call screen past the join step (the
 *    owner at founding, a joiner once its Welcome landed);
 *  - [finish] when this device leaves the call or the call ends for it,
 *    whichever comes first. It hands back the row at most once, and only for a
 *    call that went live: waiting to be admitted and giving up is not a call
 *    this person was in.
 *
 * Pure and Android-free, so the rule is tested on the host JVM. Not
 * thread-safe: the host uses it from the main thread.
 */
public class LinkCallHistoryRecorder(
    /** Lowercase hex of the call's 16-byte `call_id`. */
    private val callIdHex: String,
    /** This device started the call (`New call`, or its own link's owner). */
    private val outgoing: Boolean,
    /** The call was started as a video call. */
    video: Boolean,
) {
    private var video: Boolean = video
    private var liveAtMs: Long? = null
    private var finished = false

    /** The call reached the call screen, at [nowMs]. Later calls change nothing. */
    public fun live(nowMs: Long) {
        if (liveAtMs == null && !finished) liveAtMs = nowMs
    }

    /** This device's camera was on during the call: the row records video. */
    public fun sawVideo() {
        video = true
    }

    /** Whether the call went live. */
    public val wentLive: Boolean
        get() = liveAtMs != null

    /**
     * The row to file, at [nowMs]: once per call, and only when it went live.
     * The time is when this device joined; the duration is how long it was in
     * the call, or none when a clock that moved backwards makes it unknowable.
     */
    public fun finish(nowMs: Long): CallHistoryEntry? {
        if (finished) return null
        finished = true
        val at = liveAtMs ?: return null
        return noConversationCallEntry(
            callIdHex = callIdHex,
            outgoing = outgoing,
            video = video,
            startedAtMs = at,
            durationSecs = callDurationSecs(at, nowMs),
        )
    }
}

/**
 * Whether a finished link or `New call` call is filed in the Calls list.
 * Never for a guest account: a guest never lands in the app, so it has no
 * Calls list to show it in (section 5.1.1).
 */
public fun linkCallWritesHistory(guestAccount: Boolean): Boolean = !guestAccount
