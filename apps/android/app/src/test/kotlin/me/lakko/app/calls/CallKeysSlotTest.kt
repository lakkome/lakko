// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * [CallKeysSlot]: one set of call keys per connection, and no transform
 * built on keys at a seed other than the one asked for (G-18, ADR-0029).
 */
class CallKeysSlotTest {

    /** Stand-in for the native call keys: records the seed it is at. */
    private class FakeKeys(var seed: ByteArray) {
        var released = false
        var prunedAt: Long? = null
    }

    private class Harness(
        var createRefuses: Boolean = false,
        var rekeyRefuses: Boolean = false,
    ) {
        val created = mutableListOf<FakeKeys>()
        val rekeys = mutableListOf<Triple<ByteArray, Long, Long>>()
        var now = 1_000L
        val slot = CallKeysSlot<FakeKeys>(
            create = { seed -> if (createRefuses) null else FakeKeys(seed.copyOf()).also { created += it } },
            rekeyKeys = { keys, seed, nowMs, overlapMs ->
                if (rekeyRefuses) {
                    false
                } else {
                    rekeys += Triple(seed.copyOf(), nowMs, overlapMs)
                    keys.seed = seed.copyOf()
                    true
                }
            },
            pruneKeys = { keys, nowMs -> keys.prunedAt = nowMs },
            releaseKeys = { keys -> keys.released = true },
            nowMs = { now },
            defaultOverlapMs = 1_000L,
        )

        /** What a transform built now would be built on. */
        fun armAt(seed: ByteArray): FakeKeys? = slot.build(seed) { it }
    }

    private fun seed(b: Int) = ByteArray(32) { b.toByte() }

    @Test
    fun firstArmCreatesTheKeysAndLaterArmsShareThem() {
        val h = Harness()
        val first = h.armAt(seed(1))
        val second = h.armAt(seed(1))
        assertSame(first, second)
        assertEquals(1, h.created.size)
        assertTrue(h.rekeys.isEmpty())
    }

    @Test
    fun anArmAtANewSeedRekeysTheSharedKeysFirst() {
        val h = Harness()
        val keys = h.armAt(seed(1))!!
        h.now = 5_000L
        val again = h.armAt(seed(2))
        assertSame(keys, again)
        assertEquals(1, h.created.size)
        assertEquals(1, h.rekeys.size)
        assertTrue(keys.seed.contentEquals(seed(2)))
        assertEquals(5_000L, h.rekeys[0].second)
        assertEquals(1_000L, h.rekeys[0].third)
    }

    @Test
    fun anArmAtANewSeedUsesTheOverlapOfTheLastRekey() {
        val h = Harness()
        h.armAt(seed(1))
        assertTrue(h.slot.rekey(seed(2), 10L, 750L))
        h.armAt(seed(3))
        assertEquals(750L, h.rekeys.last().third)
    }

    @Test
    fun aRefusedRekeyOnArmBuildsNothing() {
        val h = Harness()
        val keys = h.armAt(seed(1))!!
        h.rekeyRefuses = true
        var built = false
        assertNull(h.slot.build(seed(2)) { built = true; it })
        assertFalse(built)
        assertTrue(keys.seed.contentEquals(seed(1)))
    }

    @Test
    fun aRefusedCreateBuildsNothing() {
        val h = Harness(createRefuses = true)
        assertNull(h.armAt(seed(1)))
        assertFalse(h.slot.isHeld)
    }

    @Test
    fun oneRekeyMovesTheWholeCall() {
        val h = Harness()
        val keys = h.armAt(seed(1))!!
        assertTrue(h.slot.rekey(seed(2), 10L, 1_000L))
        assertTrue(keys.seed.contentEquals(seed(2)))
        // A transform armed afterwards at the new seed needs no further rekey.
        assertSame(keys, h.armAt(seed(2)))
        assertEquals(1, h.rekeys.size)
    }

    @Test
    fun aRekeyToTheCurrentSeedChangesNothing() {
        val h = Harness()
        h.armAt(seed(1))
        assertTrue(h.slot.rekey(seed(1), 10L, 1_000L))
        assertTrue(h.rekeys.isEmpty())
    }

    @Test
    fun aRekeyBeforeAnyArmIsANoOp() {
        val h = Harness()
        assertTrue(h.slot.rekey(seed(2), 10L, 1_000L))
        assertTrue(h.rekeys.isEmpty())
        // The first arm then creates the keys at the seed it is given.
        val keys = h.armAt(seed(2))!!
        assertTrue(keys.seed.contentEquals(seed(2)))
    }

    @Test
    fun aRefusedRekeyKeepsTheCurrentSeed() {
        val h = Harness()
        val keys = h.armAt(seed(1))!!
        h.rekeyRefuses = true
        assertFalse(h.slot.rekey(seed(2), 10L, 1_000L))
        assertTrue(keys.seed.contentEquals(seed(1)))
    }

    @Test
    fun aStaleArmDoesNotRollTheCallBack() {
        val h = Harness()
        val keys = h.armAt(seed(1))!!
        assertTrue(h.slot.rekey(seed(2), 10L, 1_000L))
        // An arm that read the seed just before the rekey.
        assertSame(keys, h.armAt(seed(1)))
        assertTrue(keys.seed.contentEquals(seed(2)))
        assertEquals(1, h.rekeys.size)
    }

    @Test
    fun pruneReachesTheKeys() {
        val h = Harness()
        val keys = h.armAt(seed(1))!!
        h.slot.prune(42L)
        assertEquals(42L, keys.prunedAt)
    }

    @Test
    fun clearReleasesTheKeysOnceAndNothingIsBuiltAfter() {
        val h = Harness()
        val keys = h.armAt(seed(1))!!
        h.slot.clear()
        h.slot.clear()
        assertTrue(keys.released)
        assertFalse(h.slot.isHeld)
        assertNull(h.armAt(seed(1)))
        assertEquals(1, h.created.size)
    }
}
