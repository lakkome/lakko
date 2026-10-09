// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0066 "T5 as built" items 21 and 22: the DS refuses an attested `reset`
 * (after a key reset or a recall) without the root endorsement, and takes an
 * attested `root` transfer (after a rotation) with the same two fields.
 */
class KeyResetRootEvidenceTest {
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
    fun `the evidence adds the attestation AND the endorsement beside the statement`() {
        val plain = DsStatement("/ds/group/ab/reset", """{"statement_b64":"c3Q="}""")
        val body = fields(plain.withKeyResetEvidence(KeyResetRootEvidence("QVRU", "RU5E")))
        assertEquals("c3Q=", body["statement_b64"]!!.jsonPrimitive.content)
        assertEquals("QVRU", body[KEY_RESET_ATTEST_FIELD]!!.jsonPrimitive.content)
        assertEquals("RU5E", body[ROOT_ENDORSEMENT_FIELD]!!.jsonPrimitive.content)
        assertEquals("root_endorsement_b64", ROOT_ENDORSEMENT_FIELD)
        assertEquals("key_reset_attest_b64", KEY_RESET_ATTEST_FIELD)
    }

    @Test
    fun `an attested re-founding sends the endorsement of the resetting root`() = runTest {
        val root = RecordingStatementSigner("new-root")
        val wire = Recorder().apply { answers = ArrayDeque(listOf(refused(409), null)) }
        var endorsedKey: ByteArray? = null
        val outcome = claimOrReset(
            journalId = journal,
            epoch = 1uL,
            epochKey = ByteArray(32) { 1 },
            statements = root,
            refounded = true,
            keyResetEvidence = { rootKey ->
                endorsedKey = rootKey
                KeyResetRootEvidence("QVRU", "RU5E")
            },
            put = wire::put,
        )
        assertEquals(JournalClaimOutcome.RESET, outcome)
        // The claim first, plain; then the reset, with both fields.
        assertEquals(listOf("claim@1", "reset@1"), root.calls)
        assertNull(fields(wire.sent[0])[KEY_RESET_ATTEST_FIELD])
        val reset = fields(wire.sent[1])
        assertEquals("QVRU", reset[KEY_RESET_ATTEST_FIELD]!!.jsonPrimitive.content)
        assertEquals("RU5E", reset[ROOT_ENDORSEMENT_FIELD]!!.jsonPrimitive.content)
        // The endorsed key is the reset statement's signer.
        assertTrue(root.publicKey.contentEquals(endorsedKey))
    }

    @Test
    fun `an ordinary same-root re-founding carries neither field`() = runTest {
        val root = RecordingStatementSigner("root")
        val wire = Recorder().apply { answers = ArrayDeque(listOf(refused(409), null)) }
        claimOrReset(journal, 1uL, ByteArray(32), root, refounded = true, put = wire::put)
        val reset = fields(wire.sent[1])
        assertNull(reset[KEY_RESET_ATTEST_FIELD])
        assertNull(reset[ROOT_ENDORSEMENT_FIELD])
        // And a provider that has nothing to give changes nothing.
        val again = Recorder().apply { answers = ArrayDeque(listOf(refused(409), null)) }
        claimOrReset(journal, 1uL, ByteArray(32), root, refounded = true, keyResetEvidence = { null }, put = again::put)
        assertNull(fields(again.sent[1])[ROOT_ENDORSEMENT_FIELD])
    }

    // --- the attested root transfer of a rotation ---------------------------

    @Test
    fun `the old root hands the journal to the new root with the evidence`() = runTest {
        val old = RecordingStatementSigner("old")
        val new = RecordingStatementSigner("new")
        val wire = Recorder()
        val moved = transferRootOrConfirm(journal, old, new, KeyResetRootEvidence("QVRU", "RU5E"), wire::put)
        assertTrue(moved)
        // Signed by the OLD root, naming the new root's key.
        assertEquals(listOf("root"), old.calls)
        assertTrue(new.publicKey.contentEquals(old.lastNewRoot))
        assertTrue(new.calls.isEmpty())
        val body = fields(wire.sent.single())
        assertEquals("QVRU", body[KEY_RESET_ATTEST_FIELD]!!.jsonPrimitive.content)
        assertEquals("RU5E", body[ROOT_ENDORSEMENT_FIELD]!!.jsonPrimitive.content)
    }

    @Test
    fun `a transfer without an attestation is the plain hand-over`() = runTest {
        val old = RecordingStatementSigner("old")
        val new = RecordingStatementSigner("new")
        val wire = Recorder()
        assertTrue(transferRootOrConfirm(journal, old, new, evidence = null, put = wire::put))
        assertNull(fields(wire.sent.single())[KEY_RESET_ATTEST_FIELD])
    }

    @Test
    fun `a transfer that was already made is confirmed by the new root's probe`() = runTest {
        for (status in listOf(401, 409)) {
            val old = RecordingStatementSigner("old")
            val new = RecordingStatementSigner("new")
            // The transfer is refused; the new root's probe answers 204.
            val wire = Recorder().apply { answers = ArrayDeque(listOf(refused(status), null)) }
            assertTrue(transferRootOrConfirm(journal, old, new, null, wire::put))
            assertEquals(listOf("root"), new.calls)
            assertTrue(new.publicKey.contentEquals(new.lastNewRoot))
        }
    }

    @Test
    fun `a journal under neither root is not reported as moved`() = runTest {
        val old = RecordingStatementSigner("old")
        val new = RecordingStatementSigner("new")
        val wire = Recorder().apply { answers = ArrayDeque(listOf(refused(401), refused(401))) }
        assertFalse(transferRootOrConfirm(journal, old, new, null, wire::put))
    }

    @Test
    fun `any other refusal of the transfer propagates`() = runTest {
        val old = RecordingStatementSigner("old")
        val new = RecordingStatementSigner("new")
        val wire = Recorder().apply { answers = ArrayDeque(listOf(refused(400))) }
        try {
            transferRootOrConfirm(journal, old, new, KeyResetRootEvidence("a", "b"), wire::put)
            fail("a malformed endorsement must not read as moved")
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
                null,
                wire::put,
            ),
        )
        assertTrue(wire.sent.isEmpty())
    }
}
