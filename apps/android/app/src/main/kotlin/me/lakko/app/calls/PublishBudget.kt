// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.webrtc.RtpParameters

/**
 * How much this device's camera senders may send, as the SFU says
 * (ADR-0068, amendment of 2026-10-11): the twin of the web
 * `publish-budget.ts`.
 *
 * The SFU estimates this device's uplink and tells it, over Lakko's own
 * signaling, the ceiling for its main and its middle camera sender: a
 * `publish-budget` frame with both figures in kbit/s, the whole state each
 * time, the last frame wins. The ceiling is applied as
 * `encodings[0].maxBitrateBps` with `setParameters`, the same lever on every
 * client.
 *
 * The server decides, the client executes. There is no allocation rule here:
 * how the estimate is shared between the senders, the floors and the maxima
 * are the SFU's (`services/sfu/src/publish_budget.rs`), so the three clients
 * cannot drift. This file holds
 *
 *   * the reading of the frame ([readPublishBudget]);
 *   * the one mapping ([senderCeiling]): the main sender's ceiling is the
 *     frame's main figure, the middle sender's its middle figure, in bits a
 *     second; the light sender is never touched (it keeps the ceiling it was
 *     created with); and before the first frame nothing is written, so each
 *     sender keeps what it was created with (the main one no ceiling, the
 *     middle one [CAMERA_MIDDLE_MAX_BITRATE_BPS], the light one
 *     [CAMERA_LIGHT_MAX_BITRATE_BPS]);
 *   * its application to one sender ([SenderCeiling]), on the first
 *     encoding's `maxBitrateBps` and nothing else.
 *
 * A ceiling is applied to a sender whether or not it is switched on
 * (`SenderActivity.kt`): the SFU gives a figure for a sender nobody watches
 * too, and it is what that sender starts under.
 *
 * Audio is never limited here.
 *
 * Pure apart from what the [CeilingSender] it is handed does: no Android, no
 * peer connection, no timers. `RtpParameters.Encoding` is a plain Java class.
 */

/** The SFU's last `publish-budget` frame, kilobits a second. */
internal data class PublishBudget(val mainKbps: Int, val middleKbps: Int)

/** The highest ceiling the frame may carry, kbit/s
 *  (`lakko_sfu_proto::MAX_PUBLISH_BUDGET_KBPS`). Checked by the shared
 *  parser; repeated here only to refuse a frame that carries nonsense. */
internal const val MAX_PUBLISH_BUDGET_KBPS: Int = 20_000

private fun validKbps(value: Long): Boolean = value in 1L..MAX_PUBLISH_BUDGET_KBPS.toLong()

/**
 * The two ceilings of one inbound `publish-budget` frame, as the shared
 * parser handed them over, or `null` when either is zero or above the
 * protocol's bound. Never throws.
 */
internal fun readPublishBudget(mainKbps: Long, middleKbps: Long): PublishBudget? =
    if (validKbps(mainKbps) && validKbps(middleKbps)) {
        PublishBudget(mainKbps.toInt(), middleKbps.toInt())
    } else {
        null
    }

private val PUBLISH_BUDGET_KIND = Regex("\"kind\"\\s*:\\s*\"publish-budget\"")

/**
 * Whether a text frame the shared parser refused says it is a
 * `publish-budget` one. Only decides whether the refusal is worth a warning;
 * nothing is read from such a frame.
 */
internal fun claimsPublishBudget(raw: String): Boolean = PUBLISH_BUDGET_KIND.containsMatchIn(raw)

/** The one warning a malformed `publish-budget` frame gets. */
internal const val PUBLISH_BUDGET_MALFORMED_WARNING: String =
    "sfu signaling: ignored a malformed publish-budget frame"

/** Every sender the frame names, in the order the diagnostics name them. */
internal val BUDGETED_SENDER_KINDS: List<CameraSenderKind> =
    listOf(CameraSenderKind.MAIN, CameraSenderKind.MIDDLE)

/**
 * The ceiling to set on [kind]'s encoding, bits a second, or `null` when
 * nothing is to be written: the light sender always, and every sender before
 * the first frame (it keeps what it was created with).
 */
internal fun senderCeiling(kind: CameraSenderKind, budget: PublishBudget?): Int? = when {
    budget == null || kind == CameraSenderKind.LIGHT -> null
    kind == CameraSenderKind.MAIN -> budget.mainKbps * 1000
    else -> budget.middleKbps * 1000
}

/**
 * The one line for a `publish-budget` frame that changed the budget: `call
 * publish budget main=.. middle=..`, kbit/s. `null` when the frame repeats
 * the last one.
 */
internal fun describePublishBudgetChange(before: PublishBudget?, budget: PublishBudget): String? =
    if (before == budget) null else "call publish budget main=${budget.mainKbps} middle=${budget.middleKbps}"

/** What a `send video` line says of one sender's ceiling: what its encoding
 *  holds now, bits a second; `null` when it holds none or the sender could
 *  not be read. */
internal data class SenderCeilingNote(val maxBitrateBps: Int?)

/**
 * ` ceiling=N`, the tail a `send video` line gains: the bitrate ceiling the
 * sender's encoding holds now, in kbit/s (rounded down), or `-` when it holds
 * none or the sender could not be read. No note: say nothing.
 */
internal fun describeSenderCeiling(note: SenderCeilingNote?): String = when {
    note == null -> ""
    note.maxBitrateBps == null -> " ceiling=-"
    else -> " ceiling=${note.maxBitrateBps / 1000}"
}

/** The ceiling the first of [encodings] holds, bits a second; `null` when it
 *  holds none or there is no encoding. */
internal fun encodingCeiling(encodings: List<RtpParameters.Encoding>): Int? =
    encodings.firstOrNull()?.maxBitrateBps

/** What one application did. */
internal enum class CeilingOutcome {
    /** The encoding holds the ceiling (already, or after it was set). */
    APPLIED,

    /** The sender has no encoding to set the ceiling on. */
    NO_ENCODING,

    /** The platform refused. */
    REFUSED,
}

/**
 * The change half of one read-change-set of the ceiling: put [maxBitrateBps]
 * on the first of the [encodings] the sender just reported, changing nothing
 * else of it, and hand the parameters back through [apply]
 * (`RtpSender.setParameters`) unless the encoding already holds it. Never
 * throws.
 *
 * The caller reads the parameters and calls this inside the sender's serial
 * write path (`SenderParameters.kt`).
 */
internal fun applyEncodingCeiling(
    encodings: List<RtpParameters.Encoding>,
    maxBitrateBps: Int,
    apply: () -> Boolean,
): CeilingOutcome {
    val first = encodings.firstOrNull() ?: return CeilingOutcome.NO_ENCODING
    if (first.maxBitrateBps == maxBitrateBps) return CeilingOutcome.APPLIED
    first.maxBitrateBps = maxBitrateBps
    val accepted = try {
        apply()
    } catch (cause: RuntimeException) {
        false
    }
    return if (accepted) CeilingOutcome.APPLIED else CeilingOutcome.REFUSED
}

/** The parts of a sender [SenderCeiling] uses. */
internal interface CeilingSender {
    /**
     * One whole read-change-set of the first encoding's ceiling, on the
     * sender's serial write path, from parameters read inside it
     * ([applyEncodingCeiling]). May throw.
     */
    fun setEncodingCeiling(maxBitrateBps: Int): CeilingOutcome
}

/**
 * Keeps one sender's first encoding under the ceiling last asked.
 *
 *   * **Serialised.** The write is the sender's
 *     ([CeilingSender.setEncodingCeiling]): one read-change-set on the one
 *     write path the `active` flag (`SenderActivity.kt`) and the sender's
 *     set-up use too, so none overlaps another and none puts back what
 *     another just changed.
 *   * **The last word wins.** [want] applies what it is given, at once.
 *   * **Re-applied.** Every write reads the encoding as the platform holds it
 *     now, so asking again (every counters tick does) puts back a ceiling a
 *     renegotiation, a replaced track or a new sender ([bind]) lost.
 *   * **Refused.** A refused write is asked again at every later [want], and
 *     said once per sender.
 *   * **Before the first word** (`want(null)`), nothing is written.
 *
 * libwebrtc's Java `setParameters` answers before it returns, so unlike the
 * web twin there is no pending write to coalesce: a [want] has applied its
 * word, or been refused, when it returns.
 *
 * @param name names the sender in the one warning: `main` or `middle`.
 */
internal class SenderCeiling(
    private val name: String,
    private val warn: (message: String) -> Unit = {},
) {
    private var sender: CeilingSender? = null
    private var wanted: Int? = null
    private var warned = false

    /**
     * The sender this keeps. A different one (first publish, mid-call
     * camera-on, a copy added again) may say its warning. `null`: there is
     * none.
     */
    fun bind(sender: CeilingSender?) {
        if (this.sender === sender) return
        this.sender = sender
        warned = false
    }

    /** Whether [bind] was given a sender. */
    val bound: Boolean get() = sender != null

    /** What was last asked, bits a second; `null` before anything was. */
    val lastWanted: Int? get() = wanted

    /**
     * Ask for the encoding's ceiling, bits a second; `null`: nothing is asked
     * (the sender keeps what it holds). Safe to call at any time and as often
     * as wanted: it costs one read of the sender's parameters when the
     * encoding already says so. Returns what the write did, `null` when
     * nothing was written.
     */
    fun want(maxBitrateBps: Int?): CeilingOutcome? {
        wanted = maxBitrateBps
        val target = sender ?: return null
        if (maxBitrateBps == null) return null
        val outcome = try {
            target.setEncodingCeiling(maxBitrateBps)
        } catch (cause: RuntimeException) {
            CeilingOutcome.REFUSED
        }
        if (outcome == CeilingOutcome.REFUSED && !warned) {
            warned = true
            warn(
                "camera sender ceiling: the $name camera sender's bitrate ceiling could not be set, asking again",
            )
        }
        return outcome
    }

    /** Forget the sender and what was asked: the call is over. */
    fun reset() {
        sender = null
        wanted = null
        warned = false
    }
}
