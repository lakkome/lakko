// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import kotlinx.coroutines.async
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.yield
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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
            if (answeredElsewhere || this.pakeB != null) return DeviceLinkAnswer.ALREADY_ANSWERED
            this.pakeB = pakeB
            log += "answer"
            return DeviceLinkAnswer.ANSWERED
        }

        override suspend fun postResult(requestId: ByteArray, accepted: Boolean, confirmB: ByteArray?) {
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
        repeat(DeviceLinkAnswerLimit.MAX_ANSWERS) { assertTrue(state.limit.tryConsume()) }
        assertFalse(state.limit.tryConsume())
        state.now += DeviceLinkAnswerLimit.WINDOW_MS
        assertTrue(state.limit.tryConsume())
    }

    @Test
    fun `answer times survive their encoding`() {
        assertEquals(listOf(1L, 2L, 3L), decodeAnswerTimes(encodeAnswerTimes(listOf(1L, 2L, 3L))))
        assertEquals(listOf(5L), decodeAnswerTimes("x,5,"))
        assertTrue(decodeAnswerTimes(null).isEmpty())
    }
}
