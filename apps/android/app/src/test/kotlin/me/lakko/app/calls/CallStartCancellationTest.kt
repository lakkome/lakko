// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The two steps a call's start waits on before it opens the microphone and
 * the camera, when the call is left meanwhile.
 *
 * Neither needs a network here: the TURN client has no session token (it
 * refuses before it connects) and the signaling client is closed before it is
 * asked to connect.
 */
class CallStartCancellationTest {

    /**
     * A one-to-one call that ended while the TURN credential was being
     * fetched cancels its start. The fetch used to answer that cancellation
     * with "no TURN", so the cancelled start ran on and opened a peer
     * connection and the camera for a call that was over.
     */
    @Test
    fun `a cancelled start is not answered with no TURN`() = runBlocking {
        val client = TurnCredentialClient.forCall(ByteArray(16)) { error("a cancelled start mints nothing") }
        var answered = false
        val start = launch(start = CoroutineStart.UNDISPATCHED) {
            // The call ended: its scope is cancelled while the fetch is due.
            cancel()
            client.fetchOrNull()
            answered = true
        }
        start.join()
        assertTrue(start.isCancelled)
        assertFalse("the start went on after the call ended", answered)
    }

    /**
     * A signaling client closed while its socket was opening must not become
     * a session afterwards. Closed before the connect is the same refusal,
     * reached without a server.
     */
    @Test
    fun `a closed signaling client never connects`() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        val client = SfuSignalingClient(
            baseUrl = "http://127.0.0.1:1",
            jwt = "jwt",
            scope = scope,
            callbacks = object : SfuSignalingCallbacks {},
        )
        client.close()
        val refused = try {
            client.connect()
            false
        } catch (_: IllegalStateException) {
            true
        }
        scope.cancel()
        assertTrue(refused)
        assertFalse(client.isOpen)
    }
}
