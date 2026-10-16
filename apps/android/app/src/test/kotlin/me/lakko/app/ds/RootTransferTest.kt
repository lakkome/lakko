// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0074 section 3.5 "DS" as amended by ADR-0078 phase 6: a rotation hands
 * a journal from the old root to the new root with a PLAIN `root` transfer,
 * and a re-founding resets with a plain `reset`; neither carries any field
 * beside the statement (the key-reset attestation no longer names a journal,
 * and the DS hears no attested reset).
 */
class RootTransferTest {
    private val journal = ByteArray(32) { 9 }

    private fun fields(statement: DsStatement): JsonObject =
        Json.parseToJsonElement(statement.bodyJson) as JsonObject

    private class Recorder {
        val sent = mutableListOf<DsStatement>()
        var answers: ArrayDeque<Throwable?> = ArrayDeque()

        suspend fun put(make: () -> DsStatement) {
            sent += make()
            answers.removeFirstOrNull()?.let { throw it }
        }
    }

    private fun refused(status: Int) = DsHttpError(status, "/ds/group/x", "{}")

    @Test
    fun `a re-founding resets with the statement alone`() = runTest {
        val root = RecordingStatementSigner("root")
        val wire = Recorder().apply { answers = ArrayDeque(listOf(refused(409), null)) }
        val outcome = claimOrReset(journal, 1uL, ByteArray(32), root, refounded = true, put = wire::put)
        assertEquals(JournalClaimOutcome.RESET, outcome)
        assertEquals(listOf("claim@1", "reset@1"), root.calls)
        assertEquals(setOf("by"), fields(wire.sent[1]).keys)
    }

    @Test
    fun `the old root hands the journal to the new root`() = runTest {
        val old = RecordingStatementSigner("old")
        val new = RecordingStatementSigner("new")
        val wire = Recorder()
        assertTrue(transferRootOrConfirm(journal, old, new, wire::put))
        // Signed by the OLD root, naming the new root's key, nothing beside it.
        assertEquals(listOf("root"), old.calls)
        assertTrue(new.publicKey.contentEquals(old.lastNewRoot))
        assertTrue(new.calls.isEmpty())
        assertEquals(setOf("by"), fields(wire.sent.single()).keys)
    }

    @Test
    fun `a transfer that was already made is confirmed by the new root's probe`() = runTest {
        for (status in listOf(401, 409)) {
            val old = RecordingStatementSigner("old")
            val new = RecordingStatementSigner("new")
            // The transfer is refused; the new root's probe answers 204.
            val wire = Recorder().apply { answers = ArrayDeque(listOf(refused(status), null)) }
            assertTrue(transferRootOrConfirm(journal, old, new, wire::put))
            assertEquals(listOf("root"), new.calls)
            assertTrue(new.publicKey.contentEquals(new.lastNewRoot))
        }
    }

    @Test
    fun `a journal under neither root is not reported as moved`() = runTest {
        val old = RecordingStatementSigner("old")
        val new = RecordingStatementSigner("new")
        val wire = Recorder().apply { answers = ArrayDeque(listOf(refused(401), refused(401))) }
        assertFalse(transferRootOrConfirm(journal, old, new, wire::put))
    }

    @Test
    fun `any other refusal of the transfer propagates`() = runTest {
        val old = RecordingStatementSigner("old")
        val new = RecordingStatementSigner("new")
        val wire = Recorder().apply { answers = ArrayDeque(listOf(refused(400))) }
        try {
            transferRootOrConfirm(journal, old, new, wire::put)
            fail("a refused transfer must not read as moved")
        } catch (e: DsHttpError) {
            assertEquals(400, e.status)
        }
    }

    @Test
    fun `an unchanged root sends nothing`() = runTest {
        val key = ByteArray(32) { 4 }
        val wire = Recorder()
        assertTrue(
            transferRootOrConfirm(
                journal,
                RecordingStatementSigner("a", key),
                RecordingStatementSigner("b", key.copyOf()),
                wire::put,
            ),
        )
        assertTrue(wire.sent.isEmpty())
    }
}
