// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transport

import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.request.get
import io.ktor.http.HttpStatusCode
import java.io.IOException
import java.net.UnknownHostException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** `docs/ux/connection-state.md` - the one connection-state source. */
class ConnectionStateTest {

    @Before
    fun setUp() = ConnectionState.resetForTest()

    @After
    fun tearDown() = ConnectionState.resetForTest()

    @Test
    fun `a network failure or no route enters waiting from any phase`() {
        for (from in ConnectionPhase.entries) {
            assertEquals(ConnectionPhase.WAITING, nextConnectionPhase(from, ConnectionEvent.NETWORK_FAILURE))
            assertEquals(ConnectionPhase.WAITING, nextConnectionPhase(from, ConnectionEvent.OS_NO_ROUTE))
        }
    }

    @Test
    fun `an answer clears every phase`() {
        for (from in ConnectionPhase.entries) {
            assertEquals(ConnectionPhase.ONLINE, nextConnectionPhase(from, ConnectionEvent.ANSWERED))
        }
    }

    @Test
    fun `the route coming back moves waiting to connecting and nothing else`() {
        assertEquals(ConnectionPhase.CONNECTING, nextConnectionPhase(ConnectionPhase.WAITING, ConnectionEvent.OS_ROUTE_BACK))
        assertEquals(ConnectionPhase.ONLINE, nextConnectionPhase(ConnectionPhase.ONLINE, ConnectionEvent.OS_ROUTE_BACK))
        assertEquals(ConnectionPhase.CONNECTING, nextConnectionPhase(ConnectionPhase.CONNECTING, ConnectionEvent.OS_ROUTE_BACK))
    }

    @Test
    fun `only an I-O failure counts as a network failure`() {
        assertTrue(isNetworkFailure(IOException("reset")))
        assertTrue(isNetworkFailure(UnknownHostException("test.lakko.me")))
        assertFalse(isNetworkFailure(CancellationException("left the screen")))
        assertFalse(isNetworkFailure(IllegalStateException("decode")))
        assertFalse(isNetworkFailure(AntiCensorshipException("body")))
    }

    private fun client(answer: () -> Unit, status: HttpStatusCode = HttpStatusCode.OK): HttpClient =
        HttpClient(
            AntiCensorshipEngine(
                MockEngine { _ ->
                    answer()
                    respond("", status)
                },
            ),
        ) { expectSuccess = false }

    @Test
    fun `a request with no answer enters waiting, an HTTP error does not`() = runTest {
        val failing = client({ throw UnknownHostException("test.lakko.me") })
        runCatching { failing.get("https://test.lakko.me/native/ds/x") }
        assertEquals(ConnectionPhase.WAITING, ConnectionState.phase.value)

        client({}, HttpStatusCode.ServiceUnavailable).get("https://test.lakko.me/native/ds/x")
        assertEquals(ConnectionPhase.ONLINE, ConnectionState.phase.value)

        client({}, HttpStatusCode.Unauthorized).get("https://test.lakko.me/native/ds/x")
        assertEquals(ConnectionPhase.ONLINE, ConnectionState.phase.value)
    }

    @Test
    fun `connecting lasts until the first answer, and a failed retry goes back to waiting`() = runTest {
        ConnectionState.note(ConnectionEvent.OS_NO_ROUTE)
        ConnectionState.note(ConnectionEvent.OS_ROUTE_BACK)
        assertEquals(ConnectionPhase.CONNECTING, ConnectionState.phase.value)
        runCatching { client({ throw IOException("timeout") }).get("https://test.lakko.me/native/ds/x") }
        assertEquals(ConnectionPhase.WAITING, ConnectionState.phase.value)
        ConnectionState.note(ConnectionEvent.OS_ROUTE_BACK)
        client({}).get("https://test.lakko.me/native/ds/x")
        assertEquals(ConnectionPhase.ONLINE, ConnectionState.phase.value)
    }

    @Test
    fun `each transition is announced once`() {
        assertNull(ConnectionState.announcement.value)
        ConnectionState.note(ConnectionEvent.NETWORK_FAILURE)
        assertEquals(ConnectionPhase.WAITING, ConnectionState.announcement.value)
        ConnectionState.note(ConnectionEvent.ANSWERED)
        assertEquals(ConnectionPhase.ONLINE, ConnectionState.announcement.value)
    }

    @Test
    fun `the network coming back ends a receive loop's backoff`() = runTest {
        ConnectionState.note(ConnectionEvent.NETWORK_FAILURE)
        val sleeping = async { ConnectionState.delayUnlessNetworkReturns(60_000L) }
        yield()
        ConnectionState.note(ConnectionEvent.OS_ROUTE_BACK)
        withTimeout(1_000L) { sleeping.await() }
        assertEquals(ConnectionPhase.CONNECTING, ConnectionState.phase.value)
    }

    @Test
    fun `the header swaps to the connection words and back`() {
        assertNull(connectionHeaderKey(ConnectionPhase.ONLINE))
        assertEquals("connection.waiting", connectionHeaderKey(ConnectionPhase.WAITING))
        assertEquals("connection.connecting", connectionHeaderKey(ConnectionPhase.CONNECTING))
        assertEquals("connection.restored_a11y", connectionAnnouncementKey(ConnectionPhase.ONLINE))
    }
}
