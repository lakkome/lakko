// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.lakko_ffi_uniffi.speakingFloorOnsetMs
import uniffi.lakko_ffi_uniffi.speakingLevel
import uniffi.lakko_ffi_uniffi.speakingLevelMax
import uniffi.lakko_ffi_uniffi.speakingRingOnMs

/**
 * The shared speaking rule asked THROUGH the boundary, on a device
 * ([SharedSpeakingRules]): the half the host suite cannot reach, because a
 * host JVM has no native library and `SpeakingMonitorTest` drives a fake.
 *
 * The rule's behaviour is asserted where it is written
 * (`crates/lakko-ui-rules/src/speaking.rs`). What is checked here is that this
 * client's seam reaches that rule and not another: a voice lights a ring, the
 * own person rings and never holds the floor, a level above full scale is
 * read, and a silenced holder gives the floor up at once.
 */
@RunWith(AndroidJUnit4::class)
class SpeakingRuleInstrumentedTest {

    private val loud = speakingLevelMax() * 4
    private val order = listOf("local", "peer-a")

    /** Feed [levels] to everyone in them every 100 ms over `[from, to]`. */
    private fun SpeakingRule.feed(levels: Map<String, Double>, from: ULong, to: ULong) {
        var now = from
        while (now <= to) {
            for ((person, level) in levels) sample(person, level, now)
            decideFloor(order, now)
            now += 100uL
        }
    }

    @Test
    fun aVoiceLightsARingAndTakesTheFloorButNeverThisDevicesOwn() {
        val rule = SharedSpeakingRules.create()
        try {
            rule.setLocal("local")
            assertEquals(speakingLevel(), rule.threshold("peer-a"), 0.0)
            rule.feed(mapOf("local" to loud, "peer-a" to 0.0), 0uL, speakingRingOnMs())
            assertEquals(listOf("local"), rule.decideFloor(order, speakingRingOnMs()).speakingIds)
            // The own person speaks for three onsets and holds nothing.
            val long = 3uL * speakingFloorOnsetMs()
            rule.feed(mapOf("local" to loud, "peer-a" to 0.0), speakingRingOnMs() + 100uL, long)
            assertNull(rule.decideFloor(order, long).activeSpeakerId)
            // Someone else, at a level above full scale, does.
            val later = long + speakingFloorOnsetMs() + 200uL
            rule.feed(mapOf("local" to loud, "peer-a" to 7.0), long + 100uL, later)
            val snapshot = rule.decideFloor(order, later)
            assertEquals(listOf("local", "peer-a"), snapshot.speakingIds)
            assertEquals("peer-a", snapshot.activeSpeakerId)
            assertTrue("peer-a" in snapshot.lastSpokeAtMs)
            // Muted: the floor is given up at once.
            rule.silence("peer-a")
            val after = rule.decideFloor(order, later + 100uL)
            assertNull(after.activeSpeakerId)
            assertEquals(listOf("local"), after.speakingIds)
        } finally {
            rule.close()
        }
    }
}
