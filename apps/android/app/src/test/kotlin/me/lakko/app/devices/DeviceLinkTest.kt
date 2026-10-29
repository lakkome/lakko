// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import kotlinx.coroutines.async
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.yield
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0049 section 2 (the device link, amended 2026-09-28) —
 * `docs/ux/identity-recovery.md` section 4 on both devices, over a fake
 * server that enforces one exchange per request, and fake SPAKE2 halves that
 * agree exactly when the typed code equals the chosen one.
 */
class DeviceLinkTest {

    private val handle = ByteArray(32) { 4 }
    private val payload = byteArrayOf(10, 20, 30)

    // ------------------------------------------------------------ fakes

    /** A one-request server (the relay). */
    private class Server : DeviceLinkApi {
        var requestId: ByteArray? = null
        var pakeB: ByteArray? = null
        var pakeA: ByteArray? = null
        var sealed: ByteArray? = null
        var confirmA: ByteArray? = null
        var outcome: String? = null
        var confirmB: ByteArray? = null
        var declined = false
        var answeredElsewhere = false

        /** Whether a fresh-auth proof is live on the answering device: the
         *  two gated calls refuse without one, as the real client does
         *  (nothing sent) and as the server does (`403 fresh-auth-required`). */
        var proofLive = true
        val log = mutableListOf<String>()
        private var n = 0

        override suspend fun createRequest(handle: ByteArray): ByteArray {
            n += 1
            requestId = byteArrayOf(n.toByte())
            pakeB = null; pakeA = null; sealed = null; confirmA = null; outcome = null; confirmB = null; declined = false
            log += "create"
            return requestId!!
        }

        override suspend fun status(requestId: ByteArray): DeviceRequestStatus =
            DeviceRequestStatus(!declined, pakeB, outcome, confirmB)

        override suspend fun postPayload(requestId: ByteArray, pakeA: ByteArray, sealed: ByteArray, confirmA: ByteArray) {
            check(this.pakeA == null) { "one payload per request" }
            this.pakeA = pakeA; this.sealed = sealed; this.confirmA = confirmA
            log += "payload"
        }

        override suspend fun list(): PendingDeviceRequest? {
            val id = requestId ?: return null
            if (declined) return null
            return PendingDeviceRequest(id, pakeB, pakeA, sealed, confirmA, outcome)
        }

        override suspend fun answer(requestId: ByteArray, pakeB: ByteArray): DeviceLinkAnswer {
            if (!proofLive) throw me.lakko.app.auth.FreshAuthRequired()
            if (answeredElsewhere || this.pakeB != null) return DeviceLinkAnswer.ALREADY_ANSWERED
            this.pakeB = pakeB
            log += "answer"
            return DeviceLinkAnswer.ANSWERED
        }

        override suspend fun postResult(requestId: ByteArray, accepted: Boolean, confirmB: ByteArray?) {
            if (!proofLive) throw me.lakko.app.auth.FreshAuthRequired()
            outcome = if (accepted) OUTCOME_ACCEPTED else OUTCOME_WRONG_CODE
            this.confirmB = confirmB
            log += "result:$outcome"
        }

        override suspend fun decline(requestId: ByteArray) {
            declined = true
            log += "decline"
        }
    }

    /** The "key" both halves derive is the code the side holds. */
    private class Initiator : DeviceLinkInitiatorPort {
        var typed: String? = null
        var verified = false

        override fun finish(pakeB: ByteArray, typedCode: String, pairingPayload: ByteArray): SealedLink {
            if (typedCode.length != 6 || !typedCode.all(Char::isDigit)) throw DeviceLinkInvalidCode()
            typed = typedCode
            return SealedLink(pakeA = byteArrayOf(1), sealedPayload = typedCode.toByteArray() + pairingPayload, confirmA = typedCode.toByteArray())
        }

        override fun verifyConfirmation(confirmB: ByteArray) {
            if (!confirmB.contentEquals(("B" + typed).toByteArray())) throw DeviceLinkWrongCode()
            verified = true
        }
    }

    private class Responder(override val code: String) : DeviceLinkResponderPort {
        override val pakeMessage: ByteArray = byteArrayOf(2)

        override fun accept(pakeA: ByteArray, sealedPayload: ByteArray, confirmA: ByteArray): AcceptedLink {
            if (!confirmA.contentEquals(code.toByteArray())) throw DeviceLinkWrongCode()
            return AcceptedLink(sealedPayload.copyOfRange(6, sealedPayload.size), ("B$code").toByteArray())
        }
    }

    private class Limit(var times: List<Long> = emptyList(), var now: Long = 1_000_000L) {
        val limit = DeviceLinkAnswerLimit(load = { times }, save = { times = it }, nowMs = { now })
    }

    private fun oldDevice(
        server: Server,
        code: String = "471903",
        paired: MutableList<ByteArray> = mutableListOf(),
        limit: DeviceLinkAnswerLimit = Limit().limit,
        open: (ByteArray) -> ByteArray = { it },
    ) = OldDeviceLink(
        api = server,
        responderFor = { Responder(code) },
        formatCode = { "${it.substring(0, 2)} ${it.substring(2, 4)} ${it.substring(4, 6)}" },
        openPayload = open,
        pair = { paired += it },
        limit = limit,
    )

    private fun newDevice(server: Server, initiator: Initiator = Initiator()) = NewDeviceLink(
        api = server,
        initiatorFor = { initiator },
        pairingPayload = payload,
        pollIntervalMs = 1L,
    )

    // ------------------------------------------------------------ the whole flow

    @Test
    fun `the right code pairs, and the new device trusts it only after the confirmation verifies`() = runTest {
        val server = Server()
        val initiator = Initiator()
        val new = newDevice(server, initiator)
        val paired = mutableListOf<ByteArray>()
        val old = oldDevice(server, paired = paired)

        new.start(handle)
        assertEquals(OldDeviceLinkState.AskConsent, old.poll())
        assertEquals(OldDeviceLinkState.ShowCode("47 19 03"), old.confirm())
        assertEquals(NewDeviceLinkState.EnterCode, new.awaitAnswer())

        // The new device posts; the old device opens, answers, pairs.
        val submit = async { new.submitCode("471903") }
        yield()
        while (server.pakeA == null) yield()
        val after = old.poll()
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_ADDED_COPY), after)
        assertEquals(NewDeviceLinkState.Confirmed, submit.await())
        assertTrue(initiator.verified)
        assertArrayEquals(payload, paired.single())
        assertEquals(listOf("create", "answer", "payload", "result:accepted"), server.log)
    }

    @Test
    fun `the code is never shown before the answer landed`() = runTest {
        val server = Server().apply { answeredElsewhere = true }
        val old = oldDevice(server)
        server.createRequest(handle)
        assertEquals(OldDeviceLinkState.AskConsent, old.poll())
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_HANDLED_ELSEWHERE_COPY), old.confirm())
    }

    @Test
    fun `a wrong code pairs nothing and both sides say so`() = runTest {
        val server = Server()
        val new = newDevice(server)
        val paired = mutableListOf<ByteArray>()
        val old = oldDevice(server, paired = paired)
        new.start(handle)
        old.poll()
        old.confirm()
        new.awaitAnswer()
        val submit = async { new.submitCode("000000") }
        while (server.pakeA == null) yield()
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_OLD_WRONG_CODE_COPY), old.poll())
        assertEquals(NewDeviceLinkState.WrongCode, submit.await())
        assertTrue(paired.isEmpty())
        assertEquals(OUTCOME_WRONG_CODE, server.outcome)
    }

    @Test
    fun `an accepted outcome whose confirmation does not verify is a wrong code`() = runTest {
        // A relay that answers `accepted` without the code cannot produce the
        // confirmation: the new device must not trust the Welcome.
        val server = Server()
        val initiator = Initiator()
        val new = newDevice(server, initiator)
        new.start(handle)
        server.pakeB = byteArrayOf(9)
        new.awaitAnswer()
        val submit = async { new.submitCode("123456") }
        while (server.pakeA == null) yield()
        server.outcome = OUTCOME_ACCEPTED
        server.confirmB = "Bforged".toByteArray()
        assertEquals(NewDeviceLinkState.WrongCode, submit.await())
        assertFalse(initiator.verified)
    }

    @Test
    fun `a code that is not six digits spends nothing`() = runTest {
        val server = Server()
        val new = newDevice(server)
        new.start(handle)
        server.pakeB = byteArrayOf(9)
        new.awaitAnswer()
        assertThrows(DeviceLinkInvalidCode::class.java) {
            kotlinx.coroutines.runBlocking { new.submitCode("12a45") }
        }
        assertNull(server.pakeA)
    }

    @Test
    fun `a payload that names another account is a wrong code and pairs nothing`() = runTest {
        val server = Server()
        val new = newDevice(server)
        val paired = mutableListOf<ByteArray>()
        val old = oldDevice(server, paired = paired, open = { throw IllegalStateException("login_handle mismatch") })
        new.start(handle)
        old.poll()
        old.confirm()
        new.awaitAnswer()
        val submit = async { new.submitCode("471903") }
        while (server.pakeA == null) yield()
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_OLD_WRONG_CODE_COPY), old.poll())
        assertEquals(NewDeviceLinkState.WrongCode, submit.await())
        assertTrue(paired.isEmpty())
    }

    @Test
    fun `it is not me declines the request and the new device reads declined`() = runTest {
        val server = Server()
        val new = newDevice(server)
        val old = oldDevice(server)
        new.start(handle)
        old.poll()
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_OLD_DECLINED_COPY), old.decline())
        assertTrue(server.declined)
        assertEquals(NewDeviceLinkState.Declined, new.awaitAnswer())
    }

    @Test
    fun `a request older than ten minutes expires`() = runTest {
        var now = 0L
        val server = Server()
        val new = NewDeviceLink(server, { Initiator() }, payload, nowMs = { now }, pollIntervalMs = 1L)
        new.start(handle)
        now = DEVICE_LINK_REQUEST_TTL_MS
        assertEquals(NewDeviceLinkState.Expired, new.awaitAnswer())
    }

    // ------------------------------------------------------------ the answer limit

    @Test
    fun `an old device answers at most five requests a day`() = runTest {
        val state = Limit()
        val server = Server()
        repeat(DeviceLinkAnswerLimit.MAX_ANSWERS) {
            val old = oldDevice(server, limit = state.limit)
            server.createRequest(handle)
            assertEquals(OldDeviceLinkState.AskConsent, old.poll())
            assertTrue(old.confirm() is OldDeviceLinkState.ShowCode)
        }
        val old = oldDevice(server, limit = state.limit)
        server.createRequest(handle)
        assertEquals(OldDeviceLinkState.AskConsent, old.poll())
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_LIMIT_COPY), old.confirm())
        assertTrue("over the limit the request is declined", server.declined)
        assertNull(server.pakeB)
    }

    @Test
    fun `nothing is sent for a request before the person says it is them`() = runTest {
        val server = Server()
        var responders = 0
        val old = OldDeviceLink(
            api = server,
            responderFor = {
                responders += 1
                Responder("471903")
            },
            formatCode = { it },
            openPayload = { it },
            pair = {},
            limit = Limit().limit,
        )
        server.createRequest(handle)
        repeat(3) { assertEquals(OldDeviceLinkState.AskConsent, old.poll()) }
        assertEquals(0, responders)
        assertNull(server.pakeB)
        assertEquals(listOf("create"), server.log)
        old.confirm()
        assertEquals(1, responders)
        assertEquals(listOf("create", "answer"), server.log)
    }

    @Test
    fun `dismissing the question sends nothing and does not ask again`() = runTest {
        val server = Server()
        val old = oldDevice(server)
        server.createRequest(handle)
        assertEquals(OldDeviceLinkState.AskConsent, old.poll())
        old.dismiss()
        assertEquals(OldDeviceLinkState.Idle, old.state)
        assertEquals(OldDeviceLinkState.Idle, old.poll())
        assertNull(server.pakeB)
        assertFalse(server.declined)
        assertEquals(listOf("create"), server.log)
    }

    @Test
    fun `it is not me on the question declines without answering`() = runTest {
        val server = Server()
        val old = oldDevice(server)
        server.createRequest(handle)
        old.poll()
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_OLD_DECLINED_COPY), old.decline())
        assertTrue(server.declined)
        assertNull(server.pakeB)
    }

    @Test
    fun `the daily budget counts only answers after yes`() = runTest {
        val state = Limit()
        val server = Server()
        repeat(DeviceLinkAnswerLimit.MAX_ANSWERS * 2) {
            val old = oldDevice(server, limit = state.limit)
            server.createRequest(handle)
            old.poll()
            old.dismiss()
        }
        assertTrue(state.times.isEmpty())
        val old = oldDevice(server, limit = state.limit)
        server.createRequest(handle)
        old.poll()
        assertTrue(old.confirm() is OldDeviceLinkState.ShowCode)
        assertEquals(1, state.times.size)
    }

    @Test
    fun `the limit window slides`() {
        val state = Limit()
        repeat(DeviceLinkAnswerLimit.MAX_ANSWERS) { assertNotNull(state.limit.tryConsume()) }
        assertNull(state.limit.tryConsume())
        state.now += DeviceLinkAnswerLimit.WINDOW_MS
        assertNotNull(state.limit.tryConsume())
    }

    // ------------------------------------------------- answers given back

    @Test
    fun `a right code whose pairing fails on this side gives the answer back`() = runTest {
        val state = Limit()
        val server = Server()
        val new = newDevice(server)
        val old = OldDeviceLink(
            api = server,
            responderFor = { Responder("471903") },
            formatCode = { it },
            openPayload = { it },
            pair = { throw IllegalStateException("commit refused") },
            limit = state.limit,
        )
        new.start(handle)
        old.poll()
        old.confirm()
        assertEquals(1, state.times.size)
        new.awaitAnswer()
        val submit = async { new.submitCode("471903") }
        while (server.pakeA == null) yield()
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_ADD_FAILED_COPY), old.poll())
        assertTrue("our own failure is not a guess", state.times.isEmpty())
        submit.cancel()
    }

    @Test
    fun `a right code whose result does not land gives the answer back`() = runTest {
        val state = Limit()
        val inner = Server()
        val failing = object : DeviceLinkApi by inner {
            override suspend fun postResult(requestId: ByteArray, accepted: Boolean, confirmB: ByteArray?) {
                throw java.io.IOException("offline")
            }
        }
        val new = newDevice(inner)
        val old = OldDeviceLink(
            api = failing,
            responderFor = { Responder("471903") },
            formatCode = { it },
            openPayload = { it },
            pair = { error("must not pair") },
            limit = state.limit,
        )
        new.start(handle)
        old.poll()
        old.confirm()
        new.awaitAnswer()
        val submit = async { new.submitCode("471903") }
        while (inner.pakeA == null) yield()
        assertTrue(runCatching { old.poll() }.isFailure)
        assertTrue(state.times.isEmpty())
        submit.cancel()
    }

    @Test
    fun `a wrong code keeps the answer counted`() = runTest {
        val state = Limit()
        val server = Server()
        val new = newDevice(server)
        val old = oldDevice(server, limit = state.limit)
        new.start(handle)
        old.poll()
        old.confirm()
        new.awaitAnswer()
        val submit = async { new.submitCode("000000") }
        while (server.pakeA == null) yield()
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_OLD_WRONG_CODE_COPY), old.poll())
        submit.await()
        assertEquals(1, state.times.size)
    }

    @Test
    fun `an answer that shows no code gives it back`() = runTest {
        val state = Limit()
        val server = Server().apply { answeredElsewhere = true }
        val old = oldDevice(server, limit = state.limit)
        server.createRequest(handle)
        old.poll()
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_HANDLED_ELSEWHERE_COPY), old.confirm())
        assertTrue(state.times.isEmpty())
    }

    @Test
    fun `answer times survive their encoding`() {
        assertEquals(listOf(1L, 2L, 3L), decodeAnswerTimes(encodeAnswerTimes(listOf(1L, 2L, 3L))))
        assertEquals(listOf(5L), decodeAnswerTimes("x,5,"))
        assertTrue(decodeAnswerTimes(null).isEmpty())
    }

    // ------------------------------------------- the fresh-auth proof (UX 17.4)

    @Test
    fun `without a proof the answer is not sent and the request stays pending and asked`() = runTest {
        val server = Server().apply { proofLive = false }
        val old = oldDevice(server)
        server.createRequest(handle)
        assertEquals(OldDeviceLinkState.AskConsent, old.poll())
        // "Yes" with no live proof (it ran out between the block and the call).
        assertEquals(OldDeviceLinkState.AskConsent, old.confirm())
        assertNull(server.pakeB)
        assertFalse(server.declined)
        assertEquals(listOf("create"), server.log)
        // The next poll still asks; nothing was answered behind the person.
        assertEquals(OldDeviceLinkState.AskConsent, old.poll())
        assertNull(server.pakeB)
        // With the proof, the same request is answered.
        server.proofLive = true
        assertEquals(OldDeviceLinkState.ShowCode("47 19 03"), old.confirm())
    }

    @Test
    fun `a proof that ran out before the result keeps the request and resumes after the block`() = runTest {
        val server = Server()
        val new = newDevice(server)
        val paired = mutableListOf<ByteArray>()
        val old = oldDevice(server, paired = paired)
        new.start(handle)
        old.poll()
        old.confirm()
        new.awaitAnswer()
        val submit = async { new.submitCode("471903") }
        while (server.pakeA == null) yield()
        // The ten minutes ran out between the answer and the result.
        server.proofLive = false
        assertEquals(OldDeviceLinkState.NeedsProof, old.poll())
        assertNull(server.outcome)
        assertTrue(paired.isEmpty())
        // Polling while the block is up changes nothing and loses nothing.
        assertEquals(OldDeviceLinkState.NeedsProof, old.poll())
        // Still refused: the block stays.
        assertEquals(OldDeviceLinkState.NeedsProof, old.resume())
        // The person proved it again: the result is posted and the pairing runs.
        server.proofLive = true
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_ADDED_COPY), old.resume())
        assertEquals(OUTCOME_ACCEPTED, server.outcome)
        assertArrayEquals(payload, paired.single())
        assertEquals(NewDeviceLinkState.Confirmed, submit.await())
    }

    @Test
    fun `a proof that ran out before the hand-over posts the result once and retries only the pairing`() = runTest {
        val server = Server()
        val new = newDevice(server)
        var pairCalls = 0
        var grantLive = false
        val old = OldDeviceLink(
            api = server,
            responderFor = { Responder("471903") },
            formatCode = { it },
            openPayload = { it },
            pair = {
                pairCalls += 1
                // The device grant (`/device/authorize/start`) needs the proof.
                if (!grantLive) throw me.lakko.app.auth.FreshAuthRequired()
            },
            limit = Limit().limit,
        )
        new.start(handle)
        old.poll()
        old.confirm()
        new.awaitAnswer()
        val submit = async { new.submitCode("471903") }
        while (server.pakeA == null) yield()
        assertEquals(OldDeviceLinkState.NeedsProof, old.poll())
        assertEquals(OUTCOME_ACCEPTED, server.outcome)
        assertEquals(1, pairCalls)
        grantLive = true
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_ADDED_COPY), old.resume())
        assertEquals(2, pairCalls)
        // The result was posted exactly once.
        assertEquals(1, server.log.count { it.startsWith("result:") })
        submit.await()
    }

    @Test
    fun `it is not me at the mid-flow block deletes a request whose result was never posted`() = runTest {
        val server = Server()
        val new = newDevice(server)
        val old = oldDevice(server)
        new.start(handle)
        old.poll()
        old.confirm()
        new.awaitAnswer()
        val submit = async { new.submitCode("471903") }
        while (server.pakeA == null) yield()
        server.proofLive = false
        assertEquals(OldDeviceLinkState.NeedsProof, old.poll())
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_OLD_DECLINED_COPY), old.decline())
        assertTrue(server.declined)
        assertNull(server.outcome)
        submit.await()
    }

    @Test
    fun `a request that expires while the person types a proof says so`() = runTest {
        val server = Server()
        val old = oldDevice(server)
        server.createRequest(handle)
        assertEquals(OldDeviceLinkState.AskConsent, old.poll())
        old.noteEngaged()
        // Expired (or deleted) on the server.
        server.requestId = null
        assertEquals(OldDeviceLinkState.Notice(DEVICE_LINK_REQUEST_EXPIRED_COPY), old.poll())
        assertNull(server.pakeB)
    }

    @Test
    fun `a request that expires before the person touched the block closes silently`() = runTest {
        val server = Server()
        val old = oldDevice(server)
        server.createRequest(handle)
        assertEquals(OldDeviceLinkState.AskConsent, old.poll())
        server.requestId = null
        assertEquals(OldDeviceLinkState.Idle, old.poll())
    }
}
