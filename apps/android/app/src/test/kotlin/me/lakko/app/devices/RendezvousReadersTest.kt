// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import java.util.Base64
import kotlinx.coroutines.runBlocking
import me.lakko.app.devices.sync.RendezvousAnswer
import me.lakko.app.devices.sync.rendezvousAnswer
import me.lakko.app.ds.DsClient
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Security follow-up of 2026-11-21: anyone who learned a rendezvous id can
 * write to it, so every reader passes over a row that does not open or apply
 * and keeps reading, instead of ending the flow on it or taking it as the
 * answer. The DS is a MockEngine serving the rows past `after` the way the
 * server does; no native library.
 */
class RendezvousReadersTest {

    /** Serves [rows] (row `i` has id `i + 1`) past the `after` cursor, at
     *  most `limit` per page. Records each `after` asked. */
    private class Rendezvous(val rows: List<ByteArray>) {
        val afters = ArrayList<Long>()

        fun client(): DsClient {
            val engine = MockEngine { request ->
                val after = request.url.parameters["after"]?.toLong() ?: 0L
                val limit = request.url.parameters["limit"]?.toInt() ?: 100
                synchronized(afters) { afters += after }
                val rest = rows.withIndex().filter { it.index + 1L > after }
                val page = rest.take(limit)
                val messages = page.joinToString(",") {
                    """{"id":${it.index + 1},"payload_b64":"${Base64.getEncoder().encodeToString(it.value)}","created_at":"t"}"""
                }
                respond(
                    content = """{"messages":[$messages],"has_more":${rest.size > limit}}""",
                    status = HttpStatusCode.OK,
                    headers = headersOf(HttpHeaders.ContentType, "application/json"),
                )
            }
            return DsClient(baseUrl = "http://test.invalid", engine = engine, anonTokens = me.lakko.app.anon.FakeAnonTokenSource())
        }
    }

    private val rendezvousId = ByteArray(32) { 7 }
    private val junk = byteArrayOf(0x6a, 0x75, 0x6e, 0x6b)

    // ------------------------------------------------------------ pairing Welcome

    @Test
    fun `the Welcome poll passes over rows that are not a Welcome for this device`() = runBlocking {
        val welcome = byteArrayOf(0x57, 1)
        val server = Rendezvous(listOf(junk, junk, welcome))
        val outcome = WelcomeCourier.pollForWelcome(
            rendezvousPseudonym = rendezvousId,
            timeoutMillis = 5_000,
            ds = server.client(),
            accepts = { it.contentEquals(welcome) },
        )
        assertTrue(outcome is WelcomeCourier.WelcomeOutcome.Received)
        outcome as WelcomeCourier.WelcomeOutcome.Received
        assertArrayEquals(welcome, outcome.welcomeBytes)
        assertEquals("the cursor is the Welcome's own row", 3L, outcome.welcomeCursor)
        assertEquals(listOf(0L, 1L, 2L), server.afters)
    }

    @Test
    fun `a Welcome probe that throws counts as no`() = runBlocking {
        val welcome = byteArrayOf(0x57, 2)
        val server = Rendezvous(listOf(junk, welcome))
        val outcome = WelcomeCourier.pollForWelcome(
            rendezvousPseudonym = rendezvousId,
            timeoutMillis = 5_000,
            ds = server.client(),
            accepts = { if (it.contentEquals(junk)) throw IllegalStateException("not a Welcome") else true },
        )
        assertEquals(2L, (outcome as WelcomeCourier.WelcomeOutcome.Received).welcomeCursor)
    }

    @Test
    fun `only junk on the rendezvous times out instead of handing junk to the join`() = runBlocking {
        val server = Rendezvous(listOf(junk, junk))
        val outcome = WelcomeCourier.pollForWelcome(
            rendezvousPseudonym = rendezvousId,
            timeoutMillis = 150,
            ds = server.client(),
            accepts = { false },
        )
        assertSame(WelcomeCourier.WelcomeOutcome.Timeout, outcome)
    }

    // ------------------------------------------------------------ recovery payload

    /** A row is a recovery row when its first byte is 3 (current) or 2
     *  (another version, refused). */
    private class Codec : RecoveryRow.Codec {
        override fun exporterLabel(): String = "lakko/device-recovery/v3"
        override fun exporterKeyLen(): UInt = 32u
        override fun compose(
            grant: ByteArray,
            phraseEntropy: ByteArray,
            predecessors: List<StateBundle.OwnPredecessorSeed>,
        ): ByteArray = grant + phraseEntropy
        override fun decode(plaintext: ByteArray): RecoveryRow.Parsed =
            RecoveryRow.Parsed(plaintext.copyOfRange(0, 32), plaintext.copyOfRange(32, 48), emptyList())
        override fun wrap(sealed: ByteArray): ByteArray = byteArrayOf(3) + sealed
        override fun unwrap(row: ByteArray): ByteArray? = when (row.firstOrNull()) {
            3.toByte() -> row.copyOfRange(1, row.size)
            2.toByte() -> throw RecoveryRow.Refused()
            else -> null
        }
    }

    @Test
    fun `the recovery poll passes over a refused row and a row that does not open`() = runBlocking {
        val good = byteArrayOf(0x47)
        val server = Rendezvous(
            listOf(
                byteArrayOf(2, 9), // another version: before, this ended the flow
                byteArrayOf(3, 0x4a), // a recovery row that does not open
                byteArrayOf(0x11), // not a recovery row at all
                byteArrayOf(3) + good,
            ),
        )
        val outcome = RecoveryRow.poll(
            rendezvousPseudonym = rendezvousId,
            afterCursor = 0L,
            timeoutMillis = 5_000,
            ds = server.client(),
            codec = Codec(),
            accepts = { it.contentEquals(good) },
        )
        assertTrue(outcome is RecoveryRow.Outcome.Received)
        assertArrayEquals(good, (outcome as RecoveryRow.Outcome.Received).sealedBlob)
    }

    @Test
    fun `a refused row with nothing after it is reported as refused once the time ran out`() = runBlocking {
        val server = Rendezvous(listOf(byteArrayOf(2, 9)))
        val outcome = RecoveryRow.poll(
            rendezvousPseudonym = rendezvousId,
            afterCursor = 0L,
            timeoutMillis = 150,
            ds = server.client(),
            codec = Codec(),
        )
        assertSame(RecoveryRow.Outcome.Refused, outcome)
        val quiet = RecoveryRow.poll(
            rendezvousPseudonym = rendezvousId,
            afterCursor = 0L,
            timeoutMillis = 150,
            ds = Rendezvous(listOf(byteArrayOf(3, 0x4a))).client(),
            codec = Codec(),
            accepts = { false },
        )
        assertSame("a row that does not open is not a refusal", RecoveryRow.Outcome.Timeout, quiet)
    }

    // ------------------------------------------------------------ state bundle

    @Test
    fun `the state poll passes over a state row that does not open`() = runBlocking {
        val good = byteArrayOf(0x53, 0x53)
        val server = Rendezvous(listOf(StateBundle.wrapRow(byteArrayOf(0x4a)), junk, StateBundle.wrapRow(good)))
        val sealed = StateBundle.poll(
            rendezvousPseudonym = rendezvousId,
            afterCursor = 0L,
            ds = server.client(),
            timeoutMillis = 5_000,
        ) { it.contentEquals(good) }
        assertArrayEquals(good, sealed)
        val none = StateBundle.poll(
            rendezvousPseudonym = rendezvousId,
            afterCursor = 0L,
            ds = Rendezvous(listOf(StateBundle.wrapRow(byteArrayOf(0x4a)))).client(),
            timeoutMillis = 150,
        ) { false }
        assertNull(none)
    }

    // ------------------------------------------------------------ rejoin rendezvous

    @Test
    fun `a rejoin read passes over a row that is neither a presence note nor a Welcome it can open`() {
        val present = byteArrayOf(1)
        val welcome = byteArrayOf(0x57)
        val isPresent: (ByteArray) -> Boolean = { it.contentEquals(present) }
        val isWelcome: (ByteArray) -> Boolean = { it.contentEquals(welcome) }
        assertNull(rendezvousAnswer(listOf(junk), isPresent, isWelcome))
        assertEquals(RendezvousAnswer.Present, rendezvousAnswer(listOf(junk, present), isPresent, isWelcome))
        val got = rendezvousAnswer(listOf(junk, present, junk, welcome), isPresent, isWelcome)
        assertArrayEquals(welcome, (got as RendezvousAnswer.Welcome).bytes)
        val throwing: (ByteArray) -> Boolean = { if (it.contentEquals(junk)) error("not a Welcome") else true }
        assertNull(rendezvousAnswer(listOf(junk), isPresent, throwing))
    }
}
