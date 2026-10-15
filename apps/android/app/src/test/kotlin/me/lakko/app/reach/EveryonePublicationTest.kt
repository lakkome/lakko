// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiIntroductionPayload
import uniffi.lakko_ffi_uniffi.OprfResult

/**
 * ADR-0060 section 5 — the Everyone publication. The native steps are fakes:
 * an OPRF result's output and lookup key are distinct, recognisable bytes, so
 * the tests can tell which one went where.
 */
class EveryonePublicationTest {

    private class Put(val public: Boolean, val lookupKeys: List<ByteArray>, val record: ByteArray?)

    private class Recorder {
        val puts = mutableListOf<Put>()
        var composedPayload: FfiIntroductionPayload? = null
        var composedOutputs: List<ByteArray> = emptyList()
        val oprfNameCalls = mutableListOf<String>()
        val oprfUsernameCalls = mutableListOf<String>()
    }

    private fun output(tag: String) = "out:$tag".toByteArray()
    private fun lookup(tag: String) = "key:$tag".toByteArray()

    private fun ops(r: Recorder, terms: List<String>) = EveryoneOps(
        nameSearchTerms = { terms },
        oprfNameTerm = { t -> r.oprfNameCalls += t; OprfResult(output("n:$t"), lookup("n:$t")) },
        oprfUsername = { u -> r.oprfUsernameCalls += u; OprfResult(output("u:$u"), lookup("u:$u")) },
        composePublicRecord = { payload, outputs, _ ->
            r.composedPayload = payload
            r.composedOutputs = outputs
            "RECORD".toByteArray()
        },
        putVisibility = { public, keys, record -> r.puts += Put(public, keys, record) },
    )

    private val identityPk = ByteArray(32) { 7 }
    private val accountId = ByteArray(16) { 3 }
    private val certificate = ByteArray(153) { 9 }
    private val binding = ByteArray(97) { 4 }

    @Test
    fun `the public record carries the mailbox binding and without one nothing is published`() = runTest {
        // ADR-0078: a searcher addresses the open mailbox through the binding
        // the record carries; there is no identity-derived address to fall
        // back to, so without a binding nothing is sent.
        val r = Recorder()
        val outcome = publishEveryone(
            wantPublic = true,
            who = EveryoneIdentity("Anna Weber", "anna", certificate, identityPk, accountId, binding),
            ops = ops(r, listOf("ann")),
            nowMs = 1_000,
        )
        assertEquals(EveryoneOutcome.PUBLISHED, outcome)
        assertArrayEquals(binding, r.composedPayload!!.mailboxBinding)

        val none = Recorder()
        val skipped = publishEveryone(
            wantPublic = true,
            who = EveryoneIdentity("Anna Weber", "anna", certificate, identityPk, accountId, null),
            ops = ops(none, listOf("ann")),
            nowMs = 1_000,
        )
        assertEquals(EveryoneOutcome.NOTHING_TO_PUBLISH, skipped)
        assertTrue(none.puts.isEmpty())
    }

    @Test
    fun `entering Everyone sends lookup keys and a record sealed over the outputs`() = runTest {
        val r = Recorder()
        val outcome = publishEveryone(
            wantPublic = true,
            who = EveryoneIdentity("Anna Weber", "anna", certificate, identityPk, accountId, binding),
            ops = ops(r, listOf("ann", "anna", "web")),
            nowMs = 1_000,
        )
        assertEquals(EveryoneOutcome.PUBLISHED, outcome)
        assertEquals(1, r.puts.size)
        val put = r.puts.single()
        assertTrue(put.public)
        // Only the name terms' LOOKUP KEYS go to discovery, never an output
        // and never the username's key.
        assertEquals(
            listOf("key:n:ann", "key:n:anna", "key:n:web"),
            put.lookupKeys.map { String(it) },
        )
        assertArrayEquals("RECORD".toByteArray(), put.record)
        // The record is sealed over the OUTPUTS: the username first, then
        // every name term.
        assertEquals(
            listOf("out:u:anna", "out:n:ann", "out:n:anna", "out:n:web"),
            r.composedOutputs.map { String(it) },
        )
        val payload = r.composedPayload!!
        assertEquals("Anna Weber", payload.displayName)
        assertEquals("anna", payload.username)
        assertArrayEquals(certificate, payload.usernameCertificate)
        assertNull(payload.fof)
        assertArrayEquals(identityPk, payload.identityPk)
        assertArrayEquals(accountId, payload.accountId)
    }

    @Test
    fun `a username without a certificate is not published`() = runTest {
        val r = Recorder()
        publishEveryone(
            wantPublic = true,
            who = EveryoneIdentity("Anna", "anna", null, identityPk, accountId, binding),
            ops = ops(r, listOf("anna")),
            nowMs = 1,
        )
        assertTrue(r.oprfUsernameCalls.isEmpty())
        assertNull(r.composedPayload!!.username)
        assertNull(r.composedPayload!!.usernameCertificate)
        assertEquals(listOf("out:n:anna"), r.composedOutputs.map { String(it) })
    }

    @Test
    fun `a certified username alone is published with no name terms`() = runTest {
        val r = Recorder()
        val outcome = publishEveryone(
            wantPublic = true,
            who = EveryoneIdentity(null, "anna", certificate, identityPk, accountId, binding),
            ops = ops(r, emptyList()),
            nowMs = 1,
        )
        assertEquals(EveryoneOutcome.PUBLISHED, outcome)
        assertTrue(r.puts.single().lookupKeys.isEmpty())
        assertEquals(listOf("out:u:anna"), r.composedOutputs.map { String(it) })
    }

    @Test
    fun `neither a name nor a certified username publishes nothing`() = runTest {
        val r = Recorder()
        val outcome = publishEveryone(
            wantPublic = true,
            who = EveryoneIdentity(null, "anna", null, identityPk, accountId, binding),
            ops = ops(r, emptyList()),
            nowMs = 1,
        )
        assertEquals(EveryoneOutcome.NOTHING_TO_PUBLISH, outcome)
        assertTrue(r.puts.isEmpty())
        assertNull(r.composedPayload)
    }

    @Test
    fun `leaving Everyone sends public false with no terms and no record`() = runTest {
        val r = Recorder()
        val outcome = publishEveryone(
            wantPublic = false,
            who = EveryoneIdentity("Anna", "anna", certificate, identityPk, accountId, binding),
            ops = ops(r, listOf("anna")),
            nowMs = 1,
        )
        assertEquals(EveryoneOutcome.WITHDRAWN, outcome)
        val put = r.puts.single()
        assertFalse(put.public)
        assertTrue(put.lookupKeys.isEmpty())
        assertNull(put.record)
        assertTrue(r.oprfNameCalls.isEmpty())
    }

    @Test
    fun `the republish key changes with the name, the username and the certificate`() {
        val base = everyonePublishKey("Anna", "anna", null)
        assertEquals(base, everyonePublishKey("Anna", "anna", null))
        assertNotEquals(base, everyonePublishKey("Ann", "anna", null))
        assertNotEquals(base, everyonePublishKey("Anna", "anna2", null))
        // A certificate arriving later republishes.
        assertNotEquals(base, everyonePublishKey("Anna", "anna", certificate))
        assertNotEquals(
            everyonePublishKey("Anna", "anna", certificate),
            everyonePublishKey("Anna", "anna", certificate.copyOf().also { it[0] = 1 }),
        )
    }

    @Test
    fun `the action keys on the whole publication, not the name alone`() {
        val before = everyonePublishKey("Anna", "anna", null)
        val after = everyonePublishKey("Anna", "anna", certificate)
        assertEquals(EveryoneAction.NONE, everyoneAction(true, true, before, before))
        assertEquals(EveryoneAction.PUT, everyoneAction(true, true, before, after))
        assertEquals(EveryoneAction.PUT, everyoneAction(true, false, null, after))
        assertEquals(EveryoneAction.PUT, everyoneAction(true, null, null, after))
        assertEquals(EveryoneAction.PUT, everyoneAction(false, true, before, before))
        assertEquals(EveryoneAction.NONE, everyoneAction(false, false, null, before))
        assertEquals(EveryoneAction.NOTE_WITHDRAWN, everyoneAction(false, null, null, before))
    }
}
