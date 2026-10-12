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
 *    silence. It may come on for a cough. "Sound" is a level well above the
 *    person's own noise floor ([NoiseGate]), not above one fixed number.
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

/**
 * The scale every level is on: the root mean square of samples in `-1..1`
 * (so `0..1`, full scale is 1) over the time between two readings.
 *
 * `SPEAKING_LEVEL` is the threshold a person starts with, before anything is
 * known about the noise around their microphone.
 */
public const val SPEAKING_LEVEL: Double = 0.02

/**
 * Sound counts as speech when it is this many times the person's noise floor
 * (about 9.5 dB above it).
 *
 * Why the threshold follows the person instead of being one number: on a
 * real call (2026-10-11) a phone's microphone delivered speech at 0.01 to
 * 0.05 on this scale, while a laptop in a room idled at 0.01 to 0.02. No
 * single number separates the two; each person's own floor does.
 */
public const val SPEECH_OVER_NOISE: Double = 3.0

/** The threshold never goes below this, however quiet the floor (about
 *  -46 dB full scale). Chosen. */
public const val SPEAKING_LEVEL_MIN: Double = 0.005

/** Nor above this, however loud the room: a floor learned wrong must not
 *  shut a person out for good. Chosen. */
public const val SPEAKING_LEVEL_MAX: Double = 0.05

/** The floor climbs towards steady sound by this factor a second (doubling
 *  in about seven seconds), so a fan switched on stops counting as speech
 *  and a sentence does not. Chosen. */
public const val NOISE_RISE_PER_SECOND: Double = 1.1

/**
 * The floor drops to at most this fraction of itself per reading of the web
 * client, so one torn reading does not empty it. Chosen.
 *
 * The web client states the rule per reading and reads every
 * [NOISE_FALL_SAMPLE_MS]. This client reads at another interval
 * ([SPEAKING_SAMPLE_INTERVAL_MS]), so the rule is applied per unit of time:
 * over `t` milliseconds the floor drops to at most
 * `NOISE_FALL_PER_SAMPLE ^ (t / NOISE_FALL_SAMPLE_MS)` of itself. At the web
 * client's interval that is the web client's rule exactly; at 150 ms it is
 * about 0.354 a reading, and after any stretch of time both clients' floors
 * have been allowed to fall by the same factor.
 */
public const val NOISE_FALL_PER_SAMPLE: Double = 0.5

/** The reading interval [NOISE_FALL_PER_SAMPLE] is stated for: the web
 *  client's `SAMPLE_INTERVAL_MS`. */
public const val NOISE_FALL_SAMPLE_MS: Long = 100L

/** A reading below this says nothing about the room: it is a muted sender
 *  or a stream that is not flowing (digital silence), not a quiet room. */
public const val NOISE_EVIDENCE_MIN: Double = 1e-4

/** The longest time between two readings that counts, for the rise and for
 *  the fall: a long gap is not a long rise. */
private const val NOISE_ELAPSED_MAX_MS: Long = 1_000L

/**
 * One person's noise floor and the threshold that follows from it: the twin
 * of the web `NoiseGate`.
 *
 * The floor falls quickly to the quietest recent reading and rises slowly
 * towards whatever is steadily there. Pure: the caller supplies the time.
 */
public class NoiseGate {
    private var floor: Double = SPEAKING_LEVEL / SPEECH_OVER_NOISE
    private var lastAtMs: Long? = null

    /** A level at or above this counts as sound, now. */
    public val threshold: Double
        get() = (floor * SPEECH_OVER_NOISE).coerceIn(SPEAKING_LEVEL_MIN, SPEAKING_LEVEL_MAX)

    /** Fold one reading in. Digital silence and non-numbers change nothing. */
    public fun update(level: Double, nowMs: Long) {
        val last = lastAtMs
        val elapsed = if (last == null) null else (nowMs - last).coerceIn(0L, NOISE_ELAPSED_MAX_MS)
        lastAtMs = nowMs
        if (!level.isFinite() || level < NOISE_EVIDENCE_MIN) return
        if (level < floor) {
            // The first reading has no time before it: it may fall as far as
            // one reading of the web client does.
            val fallMs = elapsed ?: NOISE_FALL_SAMPLE_MS
            val keep = Math.pow(NOISE_FALL_PER_SAMPLE, fallMs.toDouble() / NOISE_FALL_SAMPLE_MS)
            floor = maxOf(level, floor * keep)
        } else {
            floor = minOf(level, floor * Math.pow(NOISE_RISE_PER_SECOND, (elapsed ?: 0L) / 1_000.0))
        }
    }
}

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
        /** The last reading with sound while the ring was on, or `null`. */
        var lastSoundAt: Long? = null,
        /** What counts as sound for this person. */
        val gate: NoiseGate = NoiseGate(),
    )

    private val voices = LinkedHashMap<String, Voice>()
    private var floor: String? = null
    private var lastSpoke: Map<String, Long> = emptyMap()

    /**
     * One level reading for one person, on the scale of [SPEAKING_LEVEL].
     * `null` is "could not be read": it counts as silence for the ring and
     * teaches the noise floor nothing.
     */
    public fun sample(id: String, level: Double?, nowMs: Long) {
        val v = voices.getOrPut(id) { Voice() }
        // Judged against the floor as it stood before this reading.
        val threshold = v.gate.threshold
        if (level != null) v.gate.update(level, nowMs)
        if (level != null && level.isFinite() && level >= threshold) {
            v.silenceSince = null
            val since = v.soundSince ?: nowMs.also { v.soundSince = it }
            if (!v.speaking && nowMs - since >= RING_ON_MS) {
                v.speaking = true
                v.speakingSince = since
            }
            if (v.speaking) v.lastSoundAt = nowMs
        } else {
            v.soundSince = null
            val since = v.silenceSince ?: nowMs.also { v.silenceSince = it }
            if (v.speaking && nowMs - since >= RING_OFF_MS) {
                v.speaking = false
                v.speakingSince = null
                v.lastSoundAt = null
            }
        }
    }

    /**
     * The person is muted: their ring goes off at once and they give up the
     * floor at the next snapshot. A muted person is never speaking, whatever a
     * level reading says.
     */
    public fun silence(id: String) {
        val v = voices[id] ?: return
        v.soundSince = null
        v.silenceSince = null
        v.speaking = false
        v.speakingSince = null
        v.lastSoundAt = null
    }

    /** What counts as sound for [id] now (for the diagnostics line). */
    public fun thresholdOf(id: String): Double = voices[id]?.gate?.threshold ?: SPEAKING_LEVEL

    /** Forget people who are no longer in the call. */
    public fun keepOnly(ids: Set<String>) {
        voices.keys.retainAll(ids)
        if (floor != null && floor !in ids) floor = null
        lastSpoke = lastSpoke.filterKeys { it in ids }
    }

    /**
     * The state after the samples so far.
     *
     * [candidates] are the people who can hold the floor, in roster order,
     * which breaks the tie when two reach the onset in the same reading. This
     * device's own person is NOT one of them: the floor exists for the large
     * tile, which never shows the own tile while anyone else is present, and a
     * floor held by the own microphone (the person talking, or the room heard
     * through it) kept everyone else from taking it, so the large tile never
     * moved to the person speaking. The ring is for everyone sampled, the own
     * tile included.
     */
    public fun snapshot(candidates: List<String>, nowMs: Long): SpeakingSnapshot {
        val speakingIds = voices.filterValues { it.speaking }.keys.toSet()

        /** The ring is on: enough to keep a floor already held. */
        fun stillSpeaking(id: String): Boolean = voices[id]?.speaking == true

        /**
         * Enough to take the floor: the speech itself has lasted the onset.
         * The time the ring stays on after the last sound does not count, or
         * a 0.8-second sound would grow into a 1.5-second one by standing
         * still.
         */
        fun earned(id: String): Boolean {
            val v = voices[id] ?: return false
            val since = v.speakingSince ?: return false
            val lastSound = v.lastSoundAt ?: return false
            return v.speaking && lastSound - since >= FLOOR_ONSET_MS
        }
        // Whoever holds the floor keeps it while they keep speaking; otherwise
        // it goes to the first candidate in roster order who has earned it.
        // When the holder stops and nobody has, the floor is empty and
        // `lastSpokeAtMs` still names them, which is what keeps the large tile
        // on them.
        val held = floor
        if (held == null || held !in candidates || !stillSpeaking(held)) {
            floor = candidates.firstOrNull(::earned)
        }
        floor?.let { lastSpoke = lastSpoke + (it to nowMs) }
        return SpeakingSnapshot(
            speakingIds = speakingIds,
            activeSpeakerId = floor,
            lastSpokeAtMs = lastSpoke,
        )
    }
}
