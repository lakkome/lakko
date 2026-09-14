// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

/**
 * UX spec §2 timeline bubble — "Playback continues to the next consecutive
 * unplayed voice message from the same sender."
 *
 * Port of `apps/web/src/media/playback-queue.ts`. "Consecutive" is read
 * literally: from the message that just finished, walk forward through the
 * timeline while each row is a voice message by the same sender. The first
 * unplayed one in that run plays next. Any other row — a text message, a voice
 * message from someone else, a video note — ends the run, so auto-continue
 * never jumps over the conversation to an unrelated recording. An unknown
 * sender never matches, including another unknown one: two unattributed rows
 * are not known to share an author.
 */
public data class VoiceQueueItem(
    /** Stable key the player addresses the item by. */
    val key: String,
    val isVoice: Boolean,
    /** Author identity (signature key), `null` when unresolved. */
    val senderKey: String?,
    val played: Boolean,
)

public fun nextVoiceToPlay(items: List<VoiceQueueItem>, finishedKey: String): String? {
    val index = items.indexOfFirst { it.key == finishedKey }
    if (index == -1) return null
    val sender = items[index].senderKey ?: return null
    for (i in index + 1 until items.size) {
        val item = items[i]
        if (!item.isVoice || item.senderKey != sender) return null
        if (!item.played) return item.key
    }
    return null
}

/** §2 — the speed toggle cycles 1x -> 1.5x -> 2x -> 1x. */
public val PLAYBACK_RATES: List<Float> = listOf(1f, 1.5f, 2f)

public fun nextPlaybackRate(current: Float): Float {
    val i = PLAYBACK_RATES.indexOf(current)
    return PLAYBACK_RATES[(i + 1) % PLAYBACK_RATES.size]
}

public fun rateLabel(rate: Float): String =
    if (rate == rate.toInt().toFloat()) "${rate.toInt()}x" else "${rate}x"

/**
 * The one-at-a-time rule (§2: "Only one voice message plays at a time;
 * starting another pauses the first"), as plain bookkeeping so it is testable
 * without a `MediaPlayer`. A player [claim]s the slot before it starts and is
 * handed the previous holder's pause callback to run; [release] frees the slot
 * only when the caller still holds it.
 */
public class ExclusivePlayback {
    private var holder: Pair<String, () -> Unit>? = null

    /** Take the slot for [key]; pauses whoever held it (if it was not [key]). */
    @Synchronized
    public fun claim(key: String, pause: () -> Unit) {
        val previous = holder
        holder = key to pause
        if (previous != null && previous.first != key) previous.second()
    }

    @Synchronized
    public fun release(key: String) {
        if (holder?.first == key) holder = null
    }

    @Synchronized
    public fun current(): String? = holder?.first

    /** Pause and forget the holder — the chat closed. */
    @Synchronized
    public fun stopAll() {
        val previous = holder
        holder = null
        previous?.second?.invoke()
    }
}
