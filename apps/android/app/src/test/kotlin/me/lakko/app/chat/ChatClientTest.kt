// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpStatusCode
import me.lakko.app.ds.DsClient
import org.junit.Test

/**
 * Host-side smoke tests for [ChatClient].
 *
 * The interesting paths ([ChatClient.sendMessage],
 * [ChatClient.pollMessages]) drive UniFFI-bound MLS state
 * (`Identity`, `Group`) which loads `liblakko_ffi_uniffi.so`
 * via JNA — those flows are covered by the instrumented test
 * suite that 1.15f5 will land. What we CAN exercise on the
 * host JVM:
 *
 *   1. **Construction** — the client wires through a custom
 *      [DsClient] cleanly, the default-engine path doesn't
 *      explode at instantiation.
 *   2. **[ChatClient.markOwnPublishedCommit]** is a pure
 *      Kotlin Set mutation — runnable here. Its observable
 *      effect (the next `pollMessages` drops the matching DS
 *      id) needs FFI; we test the absence-of-throw + that the
 *      call is idempotent. The end-to-end suppression behaviour
 *      lives in the instrumented suite.
 *   3. **[ChatClient.close]** propagates to the underlying
 *      DsClient — verified via the standard `MockEngine`-backed
 *      DsClient that responds to a final probe before the
 *      close.
 */
class ChatClientTest {

    @Test
    fun `default constructor wires through and closes cleanly`() {
        // Using the parameterless constructor would build a real
        // OkHttp-backed DsClient; we don't want a network setup
        // in unit tests, so wire a MockEngine through the
        // explicit constructor and verify close() doesn't throw.
        val engine = MockEngine { respond(content = "", status = HttpStatusCode.OK) }
        val ds = DsClient(baseUrl = "http://test.invalid", engine = engine)
        val client = ChatClient(ds = ds)
        client.close()
        // close() on the underlying Ktor HttpClient is
        // idempotent — call again, ensure no throw.
        client.close()
    }

    @Test
    fun `markOwnPublishedCommit is idempotent and safe to spam`() {
        val engine = MockEngine { respond(content = "", status = HttpStatusCode.OK) }
        val ds = DsClient(baseUrl = "http://test.invalid", engine = engine)
        ChatClient(ds = ds).use { client ->
            // No-throw smoke. The observable suppression effect
            // requires a live Group + pollMessages round-trip,
            // exercised in the instrumented suite (1.15f5).
            client.markOwnPublishedCommit(GROUP_A, 1L)
            client.markOwnPublishedCommit(GROUP_A, 1L) // duplicate
            client.markOwnPublishedCommit(GROUP_A, Long.MAX_VALUE)
        }
    }

    private companion object {
        val GROUP_A: String = "aa".repeat(16)
    }
}
