// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.Strings
import me.lakko.app.l10n.joinList

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
 * Pure and Android-free, so the wording is unit-tested on the host JVM. The
 * words come from the catalog in [Strings] (ADR-0061); the running time is a
 * clock value (`12:04`) in every language.
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
    strings: Strings = Localization.strings.value,
): String = when {
    // Section 4.1's state table: "`Call ended` and the duration". The duration
    // is the one the line was last given, so a call that never connected ends
    // with no number rather than a zero.
    ended -> elapsedSecs?.let(::formatCallDuration)?.let {
        strings.format("common.fact_pair", mapOf("first" to strings["call.status.ended"], "second" to it))
    } ?: strings["call.status.ended"]
    reconnecting -> strings["call.status.reconnecting"]
    connected -> elapsedSecs?.let(::formatCallDuration) ?: strings["call.status.connected"]
    connecting -> strings["common.connecting"]
    calling -> strings["call.status.calling"]
    else -> strings["call.status.ringing"]
}

/**
 * The moment a call's running time counts from.
 *
 * ## The defect this closes
 *
 * The status line used to take its origin from `System.currentTimeMillis()` at
 * the moment THE SCREEN observed the call connect. That is indistinguishable
 * from the call's own connect for a screen that is up for the whole call, and
 * wrong the moment the call can be put away and brought back
 * (`docs/ux/calls.md` section 4.5): expanding destroys and rebuilds the screen,
 * so a twelve-minute call came back reading `0:00`. Measured on an emulator: the
 * collapsed bar read `0:10`, and four seconds after expanding, the call screen
 * announced "in call, 5 seconds".
 *
 * Section 4.5 is explicit that collapsing changes nothing about the call — "the
 * media, the keys and the timer are untouched, only the picture is put away" —
 * so a screen that restarts its clock is telling the person something false
 * about their own call.
 *
 * ## The rule
 *
 * The CALL owns the moment and every view of it reads that moment. A view never
 * contributes one, so no view can report its own age as the call's.
 *
 * @param callConnectedSinceMs the moment the CALL's media first flowed, held by
 *   whichever host owns the call and therefore surviving every view of it.
 * @param viewStateConnectedSinceMs the same moment as observed by a
 *   [me.lakko.app.calls.CallUiState] the VIEW owns. For a call driven through a
 *   host-owned state these are one value; for the 1x1 call, whose screen creates
 *   its own state, this one is latched when the screen first saw the connect and
 *   is therefore the mount moment in disguise. It is the fallback and never the
 *   preference, which is the whole content of this function; it exists at all so
 *   a screen with no call behind it (the mock design route) still has a clock.
 */
public fun callClockOrigin(
    callConnectedSinceMs: Long?,
    viewStateConnectedSinceMs: Long?,
): Long? = callConnectedSinceMs ?: viewStateConnectedSinceMs

/**
 * How long a call has been running, in whole seconds, or `null` when it has not
 * connected.
 *
 * Takes the origin and the current time and does nothing else — in particular it
 * reads no clock of its own, which is what makes it impossible for a caller to
 * accidentally measure its own lifetime instead of the call's.
 *
 * A clock that moved backwards mid-call reads `0` rather than a negative number:
 * [formatCallDuration] would turn a negative into no duration at all, and a call
 * that is plainly running should not lose its clock because the host's time was
 * corrected.
 */
public fun callElapsedSecs(connectedSinceMs: Long?, nowMs: Long): Long? {
    if (connectedSinceMs == null) return null
    return ((nowMs - connectedSinceMs) / 1_000L).coerceAtLeast(0L)
}

/**
 * What TalkBack reads for the whole line (section 8: the running time is
 * announced as a duration, not digit by digit).
 */
public fun callStatusSpokenLabel(
    name: String?,
    status: String,
    elapsedSecs: Long?,
    strings: Strings = Localization.strings.value,
): String {
    val who = name?.trim()?.takeIf { it.isNotEmpty() } ?: strings["chat.call.voice"]
    val spokenStatus = when {
        elapsedSecs == null -> status
        else -> spokenCallDuration(elapsedSecs, strings)
            ?.let { strings.format("call.status.in_call_spoken", mapOf("duration" to it)) }
            ?: status
    }
    return joinList(listOf(who, spokenStatus), strings)
}

/**
 * "3 minutes 20 seconds" — the readable form of a running time, in the
 * interface language: each unit is its own plural message
 * (`common.duration.hours` / `minutes` / `seconds`), joined pair by pair
 * through `common.duration.pair`, so no unit word and no word order is written
 * here. The Kotlin twin of the web `durationWords`.
 */
internal fun spokenCallDuration(durationSecs: Long, strings: Strings = Localization.strings.value): String? {
    if (durationSecs < 0L) return null
    val hours = durationSecs / 3600L
    val minutes = (durationSecs % 3600L) / 60L
    val seconds = durationSecs % 60L
    val parts = buildList {
        if (hours > 0L) add(strings.format("common.duration.hours", counts = mapOf("count" to hours.toULong())))
        if (minutes > 0L) add(strings.format("common.duration.minutes", counts = mapOf("count" to minutes.toULong())))
        if (seconds > 0L || isEmpty()) {
            add(strings.format("common.duration.seconds", counts = mapOf("count" to seconds.toULong())))
        }
    }
    return parts.reduce { acc, part -> strings.format("common.duration.pair", mapOf("first" to acc, "second" to part)) }
}
