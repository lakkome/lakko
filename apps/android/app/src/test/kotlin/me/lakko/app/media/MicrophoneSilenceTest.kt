// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * UX spec §2 "Hearing nothing" — the fold that decides whether the microphone
 * has gone quiet, driven here with a series of samples and no recorder, no clock
 * and no UI.
 *
 * `SilentMicrophoneUiTest` drives the real bar over a real recorder that gives
 * nothing, which is the other half: that the warning is wired to a view. These
 * are about the rule itself, and in particular about the two ways it could be
 * wrong in opposite directions — accusing a microphone that is merely slow to
 * open, or accusing a person who speaks softly.
 */
class MicrophoneSilenceTest {

    /** Feed a run of samples at [everyMs] apart, starting at [everyMs]. */
    private fun fold(vararg levels: Double, everyMs: Long = 100L): SilenceWatch {
        var watch = SilenceWatch()
        levels.forEachIndexed { i, level ->
            watch = observeLevel(watch, level, (i + 1) * everyMs)
        }
        return watch
    }

    private fun silentFor(ms: Long, everyMs: Long = 100L): SilenceWatch =
        fold(*DoubleArray((ms / everyMs).toInt()) { 0.0 }, everyMs = everyMs)

    // MARK: - The two seconds

    /** Nothing is claimed before a take has begun. */
    @Test
    fun aTakeWithNoSamplesYetIsNotAccused() {
        val watch = SilenceWatch()
        assertFalse(watch.warns)
        assertNull(watch.silentSinceMs)
    }

    /**
     * The direction that matters most: a microphone that is merely slow to open
     * reads zero for its first tick or two, and a bar that accused it on the
     * first zero sample would accuse every microphone on the platform.
     */
    @Test
    fun aMicrophoneThatHasOnlyJustBeenOpenedIsNotAccused() {
        assertFalse("one zero sample is not two seconds of them", fold(0.0).warns)
        assertFalse(silentFor(500).warns)
        assertFalse(silentFor(1_900).warns)
    }

    /** At the window, not past it: the spec's two seconds are two seconds. */
    @Test
    fun theWarningArrivesExactlyAtTwoSeconds() {
        var watch = SilenceWatch()
        watch = observeLevel(watch, 0.0, 0)
        assertFalse(watch.warns)
        watch = observeLevel(watch, 0.0, SILENCE_WARNING_MS - 1)
        assertFalse(watch.warns)
        watch = observeLevel(watch, 0.0, SILENCE_WARNING_MS)
        assertTrue(watch.warns)
    }

    @Test
    fun theWarningStaysWhileTheSilenceDoes() {
        assertTrue(silentFor(3_000).warns)
        assertTrue(silentFor(30_000).warns)
    }

    // MARK: - Clearing

    /** §2 — "clears the moment any level is measured". */
    @Test
    fun oneAudibleSampleClearsEverything() {
        var watch = silentFor(5_000)
        assertTrue(watch.warns)
        watch = observeLevel(watch, 0.01, 5_100)
        assertFalse("a warning survived hearing something", watch.warns)
        assertNull("the run of silence was not reset", watch.silentSinceMs)
    }

    /** And the clock starts again from there, rather than from the take. */
    @Test
    fun theWindowIsMeasuredFromWhenTheSilenceBegan() {
        var watch = observeLevel(SilenceWatch(), 0.5, 100)
        watch = observeLevel(watch, 0.0, 200)
        assertEquals(200L, watch.silentSinceMs)
        watch = observeLevel(watch, 0.0, 200 + SILENCE_WARNING_MS - 1)
        assertFalse("the window was measured from the take, not from the silence", watch.warns)
        watch = observeLevel(watch, 0.0, 200 + SILENCE_WARNING_MS)
        assertTrue(watch.warns)
    }

    // MARK: - Zero is the question, not "quiet"

    /**
     * The other direction it could be wrong in. The defect this rule exists to
     * catch is a microphone that gives literally nothing, so the floor has to sit
     * below anything a recorder can represent as sound — a person speaking
     * quietly, or a room with nothing but its own hum in it, must never be told
     * the microphone is dead.
     */
    @Test
    fun aQuietRoomIsNeverAccused() {
        // One least-significant bit of 16-bit PCM — the quietest non-silence
        // `MediaRecorder.getMaxAmplitude` can report.
        assertTrue(hasInputSound(levelFromMaxAmplitude(1)))
        assertFalse(silentForThenAudible(levelFromMaxAmplitude(1)).warns)
        // A whisper, and an ordinary speaking voice.
        assertFalse(silentForThenAudible(0.005).warns)
        assertFalse(silentForThenAudible(0.3).warns)
    }

    /** A dead input on either platform's meter reads as silence. */
    @Test
    fun aDeadMicrophoneIsAccusedOnEveryPlatformsScale() {
        // Android: `getMaxAmplitude` is an integer and reports exactly 0.
        assertFalse(hasInputSound(levelFromMaxAmplitude(0)))
        assertTrue(silentForThenAudible(levelFromMaxAmplitude(0)).warns)
        // iOS: -160 dBFS for digital silence, which is 1e-8 on this scale — the
        // reason the floor is not an exact-zero test.
        assertFalse(hasInputSound(1e-8))
        assertTrue(silentForThenAudible(1e-8).warns)
    }

    /**
     * The floor has room on both sides: far under one bit of real signal, far
     * over what a meter reports when it hears nothing. Pinned so a later nudge
     * toward "quiet" has to be deliberate.
     */
    @Test
    fun theFloorSitsBetweenSilenceAndTheQuietestRealSignal() {
        val oneBit = levelFromMaxAmplitude(1)
        assertTrue("the floor reaches a real signal", SILENCE_LEVEL_FLOOR < oneBit / 10)
        assertTrue("the floor is under an iOS silent reading", SILENCE_LEVEL_FLOOR > 1e-8 * 10)
    }

    /** A meter reporting nonsense has not heard anything either. */
    @Test
    fun aNonFiniteOrNegativeReadingCountsAsSilence() {
        assertFalse(hasInputSound(Double.NaN))
        assertFalse(hasInputSound(-1.0))
        assertTrue(silentForThenAudible(Double.NaN).warns)
    }

    // MARK: - The clock

    /**
     * The elapsed time is monotonic in production, but a fold that read a
     * backwards step as a huge gap would raise the warning on a take that had
     * just started. It reads as no time passed instead, which only ever delays.
     */
    @Test
    fun aBackwardsClockNeverRaisesTheWarning() {
        var watch = observeLevel(SilenceWatch(), 0.0, 5_000)
        watch = observeLevel(watch, 0.0, 10)
        assertFalse(watch.warns)
    }

    /** Two seconds is the spec's number, shared by all three clients. */
    @Test
    fun theWindowIsTheSpecsTwoSeconds() {
        assertEquals(2_000L, SILENCE_WARNING_MS)
    }

    /** A run of [SILENCE_WARNING_MS] of [level], to see whether it is accused. */
    private fun silentForThenAudible(level: Double): SilenceWatch {
        var watch = SilenceWatch()
        var t = 0L
        while (t <= SILENCE_WARNING_MS) {
            watch = observeLevel(watch, level, t)
            t += 100
        }
        return watch
    }
}
