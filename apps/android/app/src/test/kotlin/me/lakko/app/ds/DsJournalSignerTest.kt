// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0053 — the pure client rules: which signer a journal is signed with
 * (and which one the 401 retry falls back to), the pre-publish registration
 * guard, heal on apply, and the clock-skew rule.
 */
class DsJournalSignerTest {

    private class Signer(override val epoch: ULong) : DsJournalSigner {
        override fun sign(method: String, path: String, query: String, body: ByteArray) = "p$epoch"
        override fun groupClaimBody() = "{}"
        override fun mailboxClaimBody() = "{}"
    }

    private class MovingSource(var epoch: ULong, var readable: Boolean = true) : DsSignerSource {
        var derives = 0
        override fun epoch(): ULong = epoch
        override fun derive(): DsJournalSigner {
            derives++
            return Signer(epoch)
        }
        override val epochReadable: Boolean get() = readable
    }

    // ------------------------------------------------------------ registry

    @Test
    fun `the current signer follows the epoch and keeps the one before it`() {
        val registry = DsSignerRegistry()
        val source = MovingSource(3uL)
        registry.install("g/x", source)
        assertEquals(3uL, registry.current("g/x")!!.epoch)
        assertNull(registry.previous("g/x"))

        source.epoch = 4uL
        assertEquals(4uL, registry.current("g/x")!!.epoch)
        assertEquals(3uL, registry.previous("g/x")!!.epoch)
    }

    @Test
    fun `an unchanged epoch derives nothing new`() {
        val registry = DsSignerRegistry()
        val source = MovingSource(3uL)
        registry.install("g/x", source)
        val first = registry.current("g/x")
        val second = registry.current("g/x")
        assertSame(first, second)
        assertEquals(1, source.derives)
    }

    @Test
    fun `a rollback drops a previous signer at or above the current epoch`() {
        val registry = DsSignerRegistry()
        val source = MovingSource(3uL)
        registry.install("g/x", source)
        registry.current("g/x")
        source.epoch = 4uL
        registry.current("g/x") // previous = 3
        source.epoch = 3uL // an ADR-0040 abort reset the group
        assertEquals(3uL, registry.current("g/x")!!.epoch)
        assertNull("the previous (3) is not older than the current (3)", registry.previous("g/x"))
    }

    @Test
    fun `a source without a cheap epoch read derives and compares`() {
        val registry = DsSignerRegistry()
        val source = MovingSource(1uL, readable = false)
        registry.install("g/d", source)
        val first = registry.current("g/d")
        assertSame("same epoch keeps the cached signer", first, registry.current("g/d"))
        source.epoch = 2uL
        assertEquals(2uL, registry.current("g/d")!!.epoch)
        assertEquals(1uL, registry.previous("g/d")!!.epoch)
    }

    @Test
    fun `a replaced source keeps the cached previous signer`() {
        val registry = DsSignerRegistry()
        val a = MovingSource(5uL)
        registry.install("g/x", a)
        registry.current("g/x")
        registry.install("g/x", MovingSource(6uL))
        assertEquals(6uL, registry.current("g/x")!!.epoch)
        assertEquals(5uL, registry.previous("g/x")!!.epoch)
    }

    @Test
    fun `clearing an owner leaves the others`() {
        val registry = DsSignerRegistry()
        registry.installStatic("m/a", Signer(0uL), owner = "chat")
        registry.installStatic("g/b", Signer(1uL), owner = "devicegroup")
        registry.clearOwner("chat")
        assertFalse(registry.has("m/a"))
        assertTrue(registry.has("g/b"))
    }

    @Test
    fun `an unknown journal has no signer`() {
        assertNull(DsSignerRegistry().current("g/none"))
    }

    // ------------------------------------------------------------ register before publish

    @Test
    fun `registration proves the next epoch under the current one and returns the current`() = runTest {
        val before = Signer(7uL)
        val after = Signer(8uL)
        val seen = mutableListOf<Pair<ULong, ULong>>()
        val used = registerBeforePublish(before, after) { next, current -> seen += next.epoch to current.epoch }
        assertSame(before, used)
        assertEquals(listOf(8uL to 7uL), seen)
    }

    @Test
    fun `registration refuses without a signer or without a one-epoch step`() {
        val calls = mutableListOf<String>()
        for ((before, after) in listOf(
            null to Signer(1uL),
            Signer(1uL) to null,
            Signer(1uL) to Signer(1uL),
            Signer(1uL) to Signer(3uL),
        )) {
            assertThrows(DsRegistrationUnavailable::class.java) {
                kotlinx.coroutines.runBlocking {
                    registerBeforePublish(before, after) { _, _ -> calls += "register" }
                }
            }
        }
        assertTrue("nothing may be registered", calls.isEmpty())
    }

    // ------------------------------------------------------------ heal

    @Test
    fun `heal registers only when the batch advanced the epoch`() {
        assertNull(healRegistration(Signer(3uL), Signer(3uL)))
        assertNull(healRegistration(null, Signer(4uL)))
        assertNull(healRegistration(Signer(4uL), null))
        assertNull(healRegistration(Signer(4uL), Signer(3uL)))
        val (next, current) = healRegistration(Signer(3uL), Signer(5uL))!!
        assertEquals(5uL, next.epoch)
        assertEquals(3uL, current.epoch)
    }

    // ------------------------------------------------------------ skew

    @Test
    fun `the clock is blamed only past a minute and only on a parseable date`() {
        val now = java.time.Instant.parse("2026-09-27T12:00:00Z").toEpochMilli()
        assertFalse(dsClockSkewed(null, now))
        assertFalse(dsClockSkewed("not a date", now))
        assertFalse(dsClockSkewed("Sun, 27 Sep 2026 12:00:59 GMT", now))
        assertTrue(dsClockSkewed("Sun, 27 Sep 2026 12:01:01 GMT", now))
        assertTrue(dsClockSkewed("Sun, 27 Sep 2026 11:58:00 GMT", now))
    }
}
