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
 * guard (ADR-0057: signed by a root or registrar), and the clock-skew rule.
 */
private fun Signer(epoch: ULong): DsJournalSigner = FakeJournalSigner(epoch)

class DsJournalSignerTest {


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

    private val journal = ByteArray(32) { 9 }

    @Test
    fun `a commit registers the next epoch's key with the statement signer, proven by the current`() = runTest {
        val before = Signer(7uL)
        val after = Signer(8uL)
        val root = RecordingStatementSigner("root")
        var put: DsStatement? = null
        var proof: DsJournalSigner? = null
        commitEpochWith(journal, before, after, root, emptyList(), byteArrayOf(1)) { make, p ->
            put = make()
            proof = p
            DsCommitAnswer(id = 1, createdAt = "t", resent = false)
        }
        assertSame(before, proof)
        assertEquals(listOf("register@8"), root.calls)
        assertTrue(put!!.path.endsWith("/commit"))
    }

    @Test
    fun `registration refuses without a signer, without a one-epoch step, or without a statement signer`() {
        val calls = mutableListOf<String>()
        val root = RecordingStatementSigner("root")
        for ((pair, statements) in listOf(
            (null to Signer(1uL)) to root,
            (Signer(1uL) to null) to root,
            (Signer(1uL) to Signer(1uL)) to root,
            (Signer(1uL) to Signer(3uL)) to root,
            (Signer(1uL) to Signer(2uL)) to null,
        )) {
            assertThrows(DsRegistrationUnavailable::class.java) {
                kotlinx.coroutines.runBlocking {
                    commitEpochWith(journal, pair.first, pair.second, statements, emptyList(), byteArrayOf(1)) { _, _ ->
                        calls += "register"
                        DsCommitAnswer(id = 1, createdAt = "t", resent = false)
                    }
                }
            }
        }
        assertTrue("nothing may be registered", calls.isEmpty())
        assertTrue(root.calls.isEmpty())
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
