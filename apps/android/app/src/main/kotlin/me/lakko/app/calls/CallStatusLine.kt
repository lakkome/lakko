// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * The call screen's status line — `docs/ux/calls.md` section 4, item 1: the
 * other person's name, and under it the call state in one word or the running
 * time.
 *
 * This line did not exist. The top of the call screen was the safety number's
 * strip of digits and a coloured dot, so the screen never said who the call was
 * with or how long it had been going; the state appeared only as a floating pill
 * over the stage. Section 4 puts both on this line and the lock chip beside it,
 * which is what gave the digits somewhere else to live (section 5).
 *
 * Pure and Android-free, so the wording is unit-tested on the host JVM.
 */

/**
 * The second line: a state word, or the running time once media is flowing.
 *
 * `Connected` is the only state that reports a time, and it reports the one it
 * was given — an [elapsedSecs] of `null` (the screen has not yet seen media
 * start) reads `Connected` rather than `0:00`, which would be a clock the user
 * could watch being wrong.
 *
 * [calling] is the outgoing call waiting to be answered, and it is separate from
 * [connecting] because the state table in section 4.1 separates them: `Calling`
 * is "we have dialled and nobody has picked up", where only End is active, and
 * `Connecting` is "they picked up and the media is being negotiated". Both read
 * `Connecting...` before section 3.1 made dialling immediate, which meant the
 * caller of a ringing phone was told the call was already being set up.
 */
@Suppress("LongParameterList")
public fun callStatusText(
    calling: Boolean,
    connecting: Boolean,
    connected: Boolean,
    reconnecting: Boolean,
    ended: Boolean,
    elapsedSecs: Long?,
): String = when {
    // Section 4.1's state table: "`Call ended` and the duration". The duration
    // is the one the line was last given, so a call that never connected ends
    // with no number rather than a zero.
    ended -> elapsedSecs?.let { "Call ended, ${formatCallDuration(it)}" } ?: "Call ended"
    reconnecting -> "Reconnecting..."
    connected -> elapsedSecs?.let(::formatCallDuration) ?: "Connected"
    connecting -> "Connecting..."
    calling -> "Calling..."
    else -> "Ringing..."
}

/**
 * What TalkBack reads for the whole line (section 8: the running time is
 * announced as a duration, not digit by digit).
 */
public fun callStatusSpokenLabel(name: String?, status: String, elapsedSecs: Long?): String {
    val who = name?.trim()?.takeIf { it.isNotEmpty() } ?: "Call"
    val spokenStatus = when {
        elapsedSecs == null -> status
        else -> "in call, " + (spokenCallDuration(elapsedSecs) ?: status)
    }
    return "$who, $spokenStatus"
}

/** "3 minutes 20 seconds" — the readable form of a running time. */
internal fun spokenCallDuration(durationSecs: Long): String? {
    if (durationSecs < 0L) return null
    val hours = durationSecs / 3600L
    val minutes = (durationSecs % 3600L) / 60L
    val seconds = durationSecs % 60L
    val parts = buildList {
        if (hours > 0L) add(spokenUnit(hours, "hour"))
        if (minutes > 0L) add(spokenUnit(minutes, "minute"))
        if (seconds > 0L || isEmpty()) add(spokenUnit(seconds, "second"))
    }
    return parts.joinToString(" ")
}

private fun spokenUnit(value: Long, noun: String): String =
    if (value == 1L) "$value $noun" else "$value ${noun}s"
