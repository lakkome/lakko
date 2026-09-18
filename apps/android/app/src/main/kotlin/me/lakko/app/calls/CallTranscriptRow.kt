// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * What a finished call leaves behind in the conversation — `docs/ux/calls.md`
 * section 6.
 *
 * ## The defect this closes
 *
 * A call left no trace at all. It rang, it connected, it ended, and the chat it
 * happened in looked exactly as it had before — no way to see that a call had
 * happened, when, which way it went, or that one had been missed. The spec asks
 * for one row per finished call, reading `Outgoing call, 12:04`,
 * `Incoming call, 12:04`, `Missed call` or `Declined`, tappable to call back.
 *
 * ## Local only, and why that is the right shape
 *
 * The row is written on the device that was in the call and nowhere else. It is
 * not a message: nothing is composed, nothing is published to the DS journal, no
 * wire format changes, and the other side writes its own row from what it
 * observed. The two devices therefore agree without exchanging anything — the
 * caller writes `Outgoing call` and the callee `Incoming call` for the same call
 * — which is exactly the asymmetry a shared artefact would have had to encode
 * per-recipient anyway.
 *
 * It rides the existing encrypted message log ([me.lakko.app.chat.MessageLogStore]),
 * so it survives a relaunch the way the timeline does, is wiped by
 * `forgetDevice` the way the timeline is, and needs no new store.
 *
 * ## The honesty rule
 *
 * A duration is shown only for a call that actually **connected**, and it is the
 * time media flowed. A call that rang and was never answered has no duration and
 * is never given one, per the spec's "Never pretend": it reads `Outgoing call`
 * with nothing after it, or `Missed call` from the other side.
 *
 * Pure and Android-free, so every rule here is unit-tested on the host JVM.
 */

/** Which of the four rows in section 6 a finished call writes. */
public enum class CallOutcome {
    /** This device placed the call. */
    OUTGOING,

    /** This device answered the call. */
    INCOMING,

    /** An incoming call that was never answered here: it rang out, or another
     *  call was already in progress so it was refused busy. */
    MISSED,

    /** Refused on purpose — by this device, or by the other side. */
    DECLINED,
}

/** One finished call, as the conversation records it. */
public data class CallTranscriptEntry(
    val outcome: CallOutcome,
    /** The call carried video. Picks the row's icon; never part of its words. */
    val video: Boolean,
    /**
     * How long media flowed, in whole seconds, or `null` when the call never
     * connected. Never a substitute for "no answer" — see the honesty rule.
     */
    val durationSecs: Long? = null,
)

/**
 * Decide the outcome from what the call host observed. Order matters, and each
 * step is the answer to a question the next one cannot ask:
 *
 *  - a **deliberate refusal** by either side is `Declined`, whichever way the
 *    call was going: the user chose this, and it is the one outcome that is not
 *    an accident;
 *  - an **incoming call that was never answered** is `Missed`, whether it rang
 *    out, the caller gave up first, or this device was busy — from the callee's
 *    seat those are the same fact;
 *  - everything else is simply which way the call went.
 *
 * @param incoming this device was being called.
 * @param answered the user accepted (incoming) or dialed (outgoing).
 * @param declinedHere the user pressed Decline on this device.
 * @param declinedByPeer the other side answered with a decline.
 */
public fun callOutcome(
    incoming: Boolean,
    answered: Boolean,
    declinedHere: Boolean,
    declinedByPeer: Boolean,
): CallOutcome = when {
    declinedHere || declinedByPeer -> CallOutcome.DECLINED
    incoming && !answered -> CallOutcome.MISSED
    incoming -> CallOutcome.INCOMING
    else -> CallOutcome.OUTGOING
}

/**
 * A call duration as `12:04`, or `1:02:04` past an hour.
 *
 * Seconds are shown rather than rounded away because a call can genuinely last
 * eleven seconds, and `0:11` says so where "less than a minute" would not.
 * A negative input (a clock that moved backwards mid-call) reads as no duration
 * at all rather than as a nonsense number.
 */
public fun formatCallDuration(durationSecs: Long): String? {
    if (durationSecs < 0L) return null
    val hours = durationSecs / 3600L
    val minutes = (durationSecs % 3600L) / 60L
    val seconds = durationSecs % 60L
    return if (hours > 0L) {
        "$hours:${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}"
    } else {
        "$minutes:${seconds.toString().padStart(2, '0')}"
    }
}

/**
 * The one line the row shows (`docs/ux/calls.md` section 6).
 *
 * `Missed call` and `Declined` never carry a duration — neither ever connected,
 * so there is nothing to report. A connected call appends its duration after a
 * comma, exactly as the spec writes it; one that ended before media flowed says
 * only which way it went.
 */
public fun callRowText(entry: CallTranscriptEntry): String {
    val head = when (entry.outcome) {
        CallOutcome.OUTGOING -> "Outgoing call"
        CallOutcome.INCOMING -> "Incoming call"
        CallOutcome.MISSED -> return "Missed call"
        CallOutcome.DECLINED -> return "Declined"
    }
    val duration = entry.durationSecs?.let(::formatCallDuration) ?: return head
    return "$head, $duration"
}

/**
 * What TalkBack reads for the row (section 8: nothing relies on a glyph, and a
 * duration is announced as a duration rather than digit by digit).
 */
public fun callRowSpokenLabel(entry: CallTranscriptEntry): String {
    val kind = if (entry.video) "Video call" else "Call"
    val head = when (entry.outcome) {
        CallOutcome.OUTGOING -> "$kind, outgoing"
        CallOutcome.INCOMING -> "$kind, incoming"
        CallOutcome.MISSED -> "Missed $kind".replaceFirstChar { it.titlecase() }
        CallOutcome.DECLINED -> "$kind declined"
    }
    // The same readable duration the call screen's status line announces, so a
    // screen reader hears one call described the same way live and afterwards.
    val spoken = entry.durationSecs?.let(::spokenCallDuration)
    val body = if (spoken == null) head else "$head, lasted $spoken"
    return "$body. Double tap to call back."
}

/**
 * The seconds a call lasted, from the instant it connected to the instant it
 * ended, or `null` when it never connected or the clock moved backwards.
 *
 * Wall-clock rather than a monotonic reading because both instants are taken by
 * the same host on the same device within one call; a backwards jump is rejected
 * rather than shown as a negative or a wrapped duration.
 */
public fun callDurationSecs(connectedAtMs: Long?, endedAtMs: Long): Long? {
    if (connectedAtMs == null) return null
    val elapsed = endedAtMs - connectedAtMs
    if (elapsed < 0L) return null
    return elapsed / 1000L
}
