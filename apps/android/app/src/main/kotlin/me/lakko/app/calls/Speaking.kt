// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * Who is speaking in a group call (`docs/ux/group-call-grid.md` sections 5.2
 * and 7.3) — the Kotlin twin of the web `apps/web/src/calls/speaking.ts`.
 *
 * Worked out on this device from audio it has already received, decrypted and
 * decoded, so the call server never learns it. Two outputs with two different
 * timings, because they answer two different questions:
 *
 *  - **the ring** on a tile: on after 200 ms of sound, off after 800 ms of
 *    silence. It may come on for a cough.
 *  - **the floor**: the one person the large tile follows. A person takes it
 *    after 1.5 s of continuous speech, and keeps it while they keep speaking,
 *    so a cough never moves a tile and two people talking over each other do
 *    not make the stage flip between them.
 *
 * Every number is chosen, not measured.
 *
 * Pure and clock-free: the caller supplies the level and the time, so the
 * whole rule runs in a test.
 */

/** A level at or above this counts as sound, on a `0..1` scale. */
public const val SPEAKING_LEVEL: Double = 0.02

/** Sound for this long turns the ring on. */
public const val RING_ON_MS: Long = 200L

/** Silence for this long turns it off. */
public const val RING_OFF_MS: Long = 800L

/** Continuous speech for this long takes the floor. */
public const val FLOOR_ONSET_MS: Long = 1_500L

/** What the tracker reports after a sample. */
public data class SpeakingSnapshot(
    /** Everyone whose ring is on. */
    val speakingIds: Set<String>,
    /** The one person holding the floor, or `null`. */
    val activeSpeakerId: String?,
    /** When each person last held the floor. */
    val lastSpokeAtMs: Map<String, Long>,
)

/** Follows the people of one call. Not thread-safe: one caller, one thread. */
public class SpeakingTracker {
    private class Voice(
        /** Since when the level has been above the threshold, or `null`. */
        var soundSince: Long? = null,
        /** Since when it has been below, or `null`. */
        var silenceSince: Long? = null,
        /** The ring. */
        var speaking: Boolean = false,
        /** Since when the ring has been on without a break, or `null`. */
        var speakingSince: Long? = null,
    )

    private val voices = LinkedHashMap<String, Voice>()
    private var floor: String? = null
    private var lastSpoke: Map<String, Long> = emptyMap()

    /** One level reading for one person. */
    public fun sample(id: String, level: Double, nowMs: Long) {
        val v = voices.getOrPut(id) { Voice() }
        if (level.isFinite() && level >= SPEAKING_LEVEL) {
            v.silenceSince = null
            val since = v.soundSince ?: nowMs.also { v.soundSince = it }
            if (!v.speaking && nowMs - since >= RING_ON_MS) {
                v.speaking = true
                v.speakingSince = since
            }
        } else {
            v.soundSince = null
            val since = v.silenceSince ?: nowMs.also { v.silenceSince = it }
            if (v.speaking && nowMs - since >= RING_OFF_MS) {
                v.speaking = false
                v.speakingSince = null
            }
        }
    }

    /** Forget people who are no longer in the call. */
    public fun keepOnly(ids: Set<String>) {
        voices.keys.retainAll(ids)
        if (floor != null && floor !in ids) floor = null
        lastSpoke = lastSpoke.filterKeys { it in ids }
    }

    /**
     * The state after the samples so far.
     *
     * [order] is the roster order, which breaks the tie when two people reach
     * the onset in the same reading.
     */
    public fun snapshot(order: List<String>, nowMs: Long): SpeakingSnapshot {
        val speakingIds = voices.filterValues { it.speaking }.keys.toSet()

        fun qualifies(id: String): Boolean {
            val v = voices[id] ?: return false
            val since = v.speakingSince ?: return false
            return v.speaking && nowMs - since >= FLOOR_ONSET_MS
        }
        // Whoever holds the floor keeps it while they keep speaking; otherwise it
        // goes to the first person in roster order who has earned it.
        val held = floor
        if (held == null || !qualifies(held)) {
            floor = order.firstOrNull(::qualifies)
        }
        floor?.let { lastSpoke = lastSpoke + (it to nowMs) }
        return SpeakingSnapshot(
            speakingIds = speakingIds,
            activeSpeakerId = floor,
            lastSpokeAtMs = lastSpoke,
        )
    }
}
