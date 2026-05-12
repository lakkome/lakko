// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpMethod
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.OutgoingContent
import io.ktor.http.content.TextContent
import io.ktor.http.headersOf
import java.io.IOException
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for [AuthClient]'s **low-level HTTP layer**
 * — the seven endpoint wrappers that map `@Serializable` data
 * classes to JSON requests / responses against a Ktor [MockEngine].
 *
 * The **high-level** `signup` / `login` / `recover` flows drive
 * UniFFI-bound OPAQUE state machines (which load
 * `liblakko_ffi_uniffi.so` via JNA); those don't run on the host
 * JVM without an emulator, so they're covered by manual
 * smoke + the future instrumented tests in 1.15d4–6. The low-level
 * surface is HTTP + JSON only — pure Kotlin, no native deps —
 * and that's what we exhaustively unit-test here.
 *
 * Why so granular? `lakko-svc-auth`'s JSON shapes ARE the contract
 * between the Compose client and the backend. A drift in a field
 * name (snake-cased → camelCased), a missing `explicitNulls =
 * false`, or a missing `ignoreUnknownKeys = true` would cause
 * production breakage that's invisible in a UI smoke test until
 * the user hits the right path. We pin every property of the
 * codec down here.
 */
class AuthClientTest {

    /**
     * Captures every request the mock engine receives, plus its
     * decoded JSON body, so test assertions can inspect both
     * request shape and response handling in one place.
     */
    private data class CapturedRequest(
        val method: HttpMethod,
        val url: String,
        val body: JsonObject?,
    )

    /** Convenience: build an [AuthClient] backed by a [MockEngine]
     *  that returns [responses] in order. */
    private fun client(
        responses: List<Pair<HttpStatusCode, String>>,
        captured: MutableList<CapturedRequest> = mutableListOf(),
    ): AuthClient {
        var index = 0
        val engine = MockEngine { request ->
            // `setBody(@Serializable T)` via ContentNegotiation +
            // kotlinx-serialization produces a `TextContent` — the
            // JSON literal that hits the wire. `OutgoingContent.NoContent`
            // is what `client.post(url) { /* no body */ }` produces,
            // i.e. the `/pow/challenge` shape.
            val rawBody: String? = when (val outgoing = request.body) {
                is TextContent -> outgoing.text
                is OutgoingContent.NoContent -> null
                else -> error("unexpected OutgoingContent: ${outgoing::class}")
            }
            val parsedBody = rawBody?.let { Json.parseToJsonElement(it).jsonObject }
            captured.add(
                CapturedRequest(
                    method = request.method,
                    url = request.url.toString(),
                    body = parsedBody,
                ),
            )
            val (status, body) = responses[index]
            index += 1
            respond(
                content = body,
                status = status,
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        return AuthClient(baseUrl = "http://test.invalid", engine = engine)
    }

    // ---------------------------------------------------------------
    // /pow/challenge
    // ---------------------------------------------------------------

    @Test
    fun `powChallenge parses response`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(
                HttpStatusCode.OK to """{"seed":"AAAAAAAAAAAAAAAAAAAAAA==","effort_bits":8,"expires_in":300}""",
            ),
            captured = captured,
        ).use { client ->
            val resp = client.powChallenge()
            assertEquals("AAAAAAAAAAAAAAAAAAAAAA==", resp.seed)
            assertEquals(8, resp.effort_bits)
            assertEquals(300, resp.expires_in)
        }
        // No body is sent on POST /pow/challenge.
        assertEquals(1, captured.size)
        val req = captured[0]
        assertEquals(HttpMethod.Post, req.method)
        assertTrue(req.url.endsWith("/pow/challenge"))
        assertNull(req.body)
    }

    // ---------------------------------------------------------------
    // /signup/start — request shape + null-elision
    // ---------------------------------------------------------------

    @Test
    fun `signupStart serializes the request without recovery_request when null`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(
                HttpStatusCode.OK to """{"password_response":"PW_RESP"}""",
            ),
            captured = captured,
        ).use { client ->
            val resp = client.signupStart(
                SignupStartRequest(
                    pow = PowProofPayload(seed = "S", nonce = "N", solution = "X"),
                    account_id = "ACC",
                    password_request = "PW_REQ",
                    recovery_request = null,
                ),
            )
            assertEquals("PW_RESP", resp.password_response)
            assertNull(resp.recovery_response)
        }
        // The `explicitNulls = false` codec setting MUST omit
        // `recovery_request` from the wire entirely when null. If
        // the field appeared with a `null` value, the server's
        // strict serde would still accept it (Option<String>
        // deserialises null as None), but the contract is that
        // optional fields are simply absent — match the TS side
        // byte-for-byte.
        assertEquals(1, captured.size)
        val body = captured[0].body
        assertNotNull(body)
        assertFalse(body!!.containsKey("recovery_request"))
        assertEquals("ACC", body["account_id"]!!.jsonPrimitive.content)
        assertEquals("PW_REQ", body["password_request"]!!.jsonPrimitive.content)
        // The nested `pow` object came through with snake_case
        // fields, exactly matching the server's expectations.
        val pow = body["pow"]!!.jsonObject
        assertEquals("S", pow["seed"]!!.jsonPrimitive.content)
        assertEquals("N", pow["nonce"]!!.jsonPrimitive.content)
        assertEquals("X", pow["solution"]!!.jsonPrimitive.content)
    }

    @Test
    fun `signupStart sends recovery_request when present`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(
                HttpStatusCode.OK to
                    """{"password_response":"PW_RESP","recovery_response":"RC_RESP"}""",
            ),
            captured = captured,
        ).use { client ->
            val resp = client.signupStart(
                SignupStartRequest(
                    pow = PowProofPayload(seed = "S", nonce = "N", solution = "X"),
                    account_id = "ACC",
                    password_request = "PW_REQ",
                    recovery_request = "RC_REQ",
                ),
            )
            assertEquals("PW_RESP", resp.password_response)
            assertEquals("RC_RESP", resp.recovery_response)
        }
        val body = captured[0].body!!
        assertEquals("RC_REQ", body["recovery_request"]!!.jsonPrimitive.content)
    }

    // ---------------------------------------------------------------
    // /signup/finish — request shape + ok-flag
    // ---------------------------------------------------------------

    @Test
    fun `signupFinish parses ok flag`() = runTest {
        client(
            responses = listOf(HttpStatusCode.OK to """{"ok":true}"""),
        ).use { client ->
            val resp = client.signupFinish(
                SignupFinishRequest(
                    pow = PowProofPayload("S", "N", "X"),
                    account_id = "ACC",
                    password_upload = "UPLOAD",
                ),
            )
            assertTrue(resp.ok)
        }
    }

    // ---------------------------------------------------------------
    // /login/start + /login/finish round trip
    // ---------------------------------------------------------------

    @Test
    fun `loginStart and loginFinish round-trip`() = runTest {
        client(
            responses = listOf(
                HttpStatusCode.OK to """{"session_id":"01HXY","response":"CRED_RESP"}""",
                HttpStatusCode.OK to """{"ok":true,"account_id":"ACC"}""",
            ),
        ).use { client ->
            val start = client.loginStart(
                LoginStartRequest(
                    pow = PowProofPayload("S", "N", "X"),
                    account_id = "ACC",
                    request = "CRED_REQ",
                ),
            )
            assertEquals("01HXY", start.session_id)
            assertEquals("CRED_RESP", start.response)

            val finish = client.loginFinish(
                LoginFinishRequest(
                    session_id = start.session_id,
                    finalize = "FINAL",
                ),
            )
            assertTrue(finish.ok)
            assertEquals("ACC", finish.account_id)
        }
    }

    // ---------------------------------------------------------------
    // Error surfaces
    // ---------------------------------------------------------------

    @Test
    fun `non-2xx response throws AuthHttpError with status and body`() {
        val err = assertThrows(AuthHttpError::class.java) {
            runTest {
                client(
                    responses = listOf(HttpStatusCode.Unauthorized to "bad credentials"),
                ).use { client ->
                    client.loginStart(
                        LoginStartRequest(
                            pow = PowProofPayload("S", "N", "X"),
                            account_id = "ACC",
                            request = "REQ",
                        ),
                    )
                }
            }
        }
        assertEquals(401, err.status)
        assertEquals("/login/start", err.endpoint)
        assertEquals("bad credentials", err.body)
    }

    @Test
    fun `truncated body caps at 512 characters`() {
        val longBody = "x".repeat(1024)
        val err = assertThrows(AuthHttpError::class.java) {
            runTest {
                client(
                    responses = listOf(HttpStatusCode.InternalServerError to longBody),
                ).use { client ->
                    client.powChallenge()
                }
            }
        }
        // 512 chars + ellipsis = 513 chars total.
        assertEquals(513, err.body.length)
        assertTrue(err.body.endsWith("…"))
    }

    @Test
    fun `network failure throws AuthNetworkError`() {
        // MockEngine that throws inside the handler simulates a
        // transport-level error (DNS / TLS / connection refused).
        val engine = MockEngine { throw IOException("simulated DNS failure") }
        val err = assertThrows(AuthNetworkError::class.java) {
            runTest {
                AuthClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.powChallenge()
                }
            }
        }
        assertEquals("/pow/challenge", err.endpoint)
        assertTrue(err.message!!.contains("simulated DNS failure"))
    }

    // ---------------------------------------------------------------
    // Codec: unknown-keys forward-compat
    // ---------------------------------------------------------------

    @Test
    fun `unknown response fields do not crash the parser`() = runTest {
        // The server is allowed to add fields over time. The codec
        // is configured with `ignoreUnknownKeys = true` so an
        // older client doesn't break when the server starts
        // sending a new informational field.
        client(
            responses = listOf(
                HttpStatusCode.OK to
                    """{"seed":"X","effort_bits":4,"expires_in":60,"future_field":42}""",
            ),
        ).use { client ->
            val resp = client.powChallenge()
            assertEquals("X", resp.seed)
            assertEquals(4, resp.effort_bits)
            assertEquals(60, resp.expires_in)
        }
    }

    // ---------------------------------------------------------------
    // High-level arg-validation (no FFI needed for these paths)
    // ---------------------------------------------------------------

    @Test
    fun `signup rejects empty password before any network call`() {
        // No mock responses needed — the validation happens before
        // any HTTP roundtrip. If the validation were missing we'd
        // see an IndexOutOfBoundsException from the MockEngine
        // (no response queued) instead of an AuthClientArgError.
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(AuthClientArgError::class.java) {
            runTest {
                AuthClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.signup(password = ByteArray(0))
                }
            }
        }
    }

    @Test
    fun `login rejects empty password before any network call`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(AuthClientArgError::class.java) {
            runTest {
                AuthClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.login(accountId = ByteArray(16), password = ByteArray(0))
                }
            }
        }
    }

    @Test
    fun `login rejects wrong-length accountId before any network call`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(AuthClientArgError::class.java) {
            runTest {
                AuthClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.login(accountId = ByteArray(8), password = "pw".toByteArray())
                }
            }
        }
    }

    @Test
    fun `recover rejects wrong-length accountId before any network call`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(AuthClientArgError::class.java) {
            runTest {
                AuthClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.recover(
                        accountId = ByteArray(8),
                        mnemonicSeed = ByteArray(64),
                        newPassword = "pw".toByteArray(),
                    )
                }
            }
        }
    }
}
