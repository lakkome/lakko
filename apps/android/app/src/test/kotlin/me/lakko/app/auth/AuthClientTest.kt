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
        val authorization: String? = null,
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
            // The `OutgoingContent.ByteArrayContent` branch is the catch-all
            // for a body Ktor's `defaultTransformers` wrapped (a raw String /
            // ByteArray `setBody` lands in an ANONYMOUS subclass of the
            // ABSTRACT type, which the concrete `TextContent` check above
            // does NOT match). `TextContent` stays first so a
            // ContentNegotiation-serialized body is read losslessly from
            // `.text` rather than round-tripped through a charset.
            val rawBody: String? = when (val outgoing = request.body) {
                is TextContent -> outgoing.text
                is OutgoingContent.ByteArrayContent -> outgoing.bytes().toString(Charsets.UTF_8)
                is OutgoingContent.NoContent -> null
                else -> error("unexpected OutgoingContent: ${outgoing::class}")
            }
            val parsedBody = rawBody?.let { Json.parseToJsonElement(it).jsonObject }
            captured.add(
                CapturedRequest(
                    method = request.method,
                    url = request.url.toString(),
                    body = parsedBody,
                    authorization = request.headers[HttpHeaders.Authorization],
                ),
            )
            val (status, body) = responses[index]
            index += 1
            respond(
                content = body,
                status = status,
                headers = headersOf(
                    HttpHeaders.ContentType to listOf("application/json"),
                    HttpHeaders.RetryAfter to listOf("120"),
                ),
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
            // Phase 1.5b: server now emits session_token +
            // session_expires_at_ms on every */finish endpoint.
            responses = listOf(
                HttpStatusCode.OK to
                    """{"ok":true,"session_token":"v1.AAAA.1.BBBB","session_expires_at_ms":2000000000000}""",
            ),
        ).use { client ->
            val resp = client.signupFinish(
                SignupFinishRequest(
                    account_id = "ACC",
                    password_upload = "UPLOAD",
                ),
            )
            assertTrue(resp.ok)
            assertEquals("v1.AAAA.1.BBBB", resp.session_token)
            assertEquals(2_000_000_000_000L, resp.session_expires_at_ms)
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
                HttpStatusCode.OK to
                    """{"ok":true,"account_id":"ACC","session_token":"v1.AAAA.1.BBBB","session_expires_at_ms":2000000000000}""",
            ),
        ).use { client ->
            val start = client.loginStart(
                LoginStartRequest(
                    handle = "HANDLE",
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
                            handle = "HANDLE",
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
                    client.login(handle = ByteArray(32), password = ByteArray(0))
                }
            }
        }
    }

    @Test
    fun `login rejects a wrong-length handle before any network call`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(AuthClientArgError::class.java) {
            runTest {
                AuthClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.login(handle = ByteArray(16), password = "pw".toByteArray())
                }
            }
        }
    }

    @Test
    fun `recover rejects a wrong-length handle before any network call`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(AuthClientArgError::class.java) {
            runTest {
                AuthClient(baseUrl = "http://test.invalid", engine = engine).use { client ->
                    client.recover(
                        handle = ByteArray(16),
                        mnemonicSeed = ByteArray(64),
                        newPassword = "pw".toByteArray(),
                    )
                }
            }
        }
    }

    // ---------------------------------------------------------------
    // ADR-0054 S4 — no proof of work outside signup start
    // ---------------------------------------------------------------

    @Test
    fun `login start carries no pow field`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(HttpStatusCode.OK to """{"session_id":"S","response":"R"}"""),
            captured = captured,
        ).use { client ->
            client.loginStart(LoginStartRequest(handle = "HANDLE", request = "REQ"))
        }
        assertEquals(1, captured.size)
        assertTrue(captured[0].url.endsWith("/login/start"))
        assertFalse(captured[0].body!!.containsKey("pow"))
    }

    @Test
    fun `totp recover and contact recovery bodies carry no pow and hit no challenge`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(
                HttpStatusCode.OK to
                    """{"account_id_b64":"AAAAAAAAAAAAAAAAAAAAAA","session_token":"t","session_expires_at_ms":1}""",
                HttpStatusCode.OK to """{"sent":true}""",
                HttpStatusCode.OK to
                    """{"account_id_b64":"AAAAAAAAAAAAAAAAAAAAAA","session_token":"t","session_expires_at_ms":1}""",
                HttpStatusCode.OK to """{"sent":true}""",
                HttpStatusCode.OK to
                    """{"account_id_b64":"AAAAAAAAAAAAAAAAAAAAAA","session_token":"t","session_expires_at_ms":1}""",
            ),
            captured = captured,
        ).use { client ->
            client.totpRecover(ByteArray(32), "123456")
            client.recoveryContactRecoverStart(ByteArray(32), ContactKind.EMAIL, "a@b.c")
            client.recoveryContactRecoverFinish(ByteArray(32), ContactKind.EMAIL, "123456")
            client.recoveryContactEnrollStart("tok", ContactKind.EMAIL, "a@b.c")
            client.deviceAuthorizeRedeem("grant")
        }
        assertEquals(5, captured.size)
        for (req in captured) {
            assertFalse(req.url, req.url.endsWith("/pow/challenge"))
            assertFalse(req.url, req.body?.containsKey("pow") ?: false)
        }
    }

    @Test
    fun `device authorize redeem sends only the grant and takes the account id from the response`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        val accountId = ByteArray(16) { (it + 1).toByte() }
        val result = client(
            responses = listOf(
                HttpStatusCode.OK to
                    """{"account_id_b64":"${base64UrlEncodeNoPad(accountId)}","session_token":"tok","session_expires_at_ms":42}""",
            ),
            captured = captured,
        ).use { client -> client.deviceAuthorizeRedeem("R1JBTlQ") }
        assertEquals(1, captured.size)
        assertTrue(captured[0].url.endsWith("/device/authorize/redeem"))
        val body = captured[0].body!!
        assertEquals(setOf("grant_token_b64"), body.keys)
        assertEquals("R1JBTlQ", body["grant_token_b64"]!!.jsonPrimitive.content)
        assertNull(captured[0].authorization)
        assertTrue(result.accountId.contentEquals(accountId))
        assertEquals("tok", result.sessionToken)
        assertEquals(42L, result.sessionExpiresAtMs)
    }

    @Test
    fun `device authorize redeem refuses a response whose account id is not 16 bytes`() {
        assertThrows(AuthHttpError::class.java) {
            runTest {
                client(
                    responses = listOf(
                        HttpStatusCode.OK to
                            """{"account_id_b64":"AAAA","session_token":"tok","session_expires_at_ms":42}""",
                    ),
                ).use { client -> client.deviceAuthorizeRedeem("R1JBTlQ") }
            }
        }
    }

    @Test
    fun `totp enroll start and device authorize start send no body but the bearer`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(
                HttpStatusCode.OK to """{"otpauth_uri":"otpauth://x","secret_b32":"ABC"}""",
                HttpStatusCode.OK to """{"grant_token_b64":"G"}""",
            ),
            captured = captured,
        ).use { client ->
            client.totpEnrollStart("tok")
            assertEquals("G", client.deviceAuthorizeStart("tok"))
        }
        assertEquals(2, captured.size)
        assertTrue(captured[0].url.endsWith("/totp/enroll/start"))
        assertTrue(captured[1].url.endsWith("/device/authorize/start"))
        for (req in captured) {
            assertNull(req.body)
            assertEquals("Bearer tok", req.authorization)
        }
    }

    @Test
    fun `a 429 carries its code and stated wait`() {
        val err = assertThrows(AuthHttpError::class.java) {
            runTest {
                client(
                    responses = listOf(
                        HttpStatusCode.TooManyRequests to
                            """{"error":"too-many-attempts","retry_after_secs":900}""",
                    ),
                ).use { client ->
                    client.loginStart(LoginStartRequest(handle = "HANDLE", request = "REQ"))
                }
            }
        }
        assertEquals(429, err.status)
        assertEquals("too-many-attempts", err.code)
        assertEquals(900L, err.retryAfterSecs)
        assertEquals("Too many attempts. Try again in 15 minutes.", authLimitCopy(err))
    }

    @Test
    fun `a 429 without a body wait falls back to the Retry-After header`() {
        val err = assertThrows(AuthHttpError::class.java) {
            runTest {
                client(
                    responses = listOf(
                        HttpStatusCode.TooManyRequests to """{"error":"signup-rate-limited"}""",
                    ),
                ).use { client ->
                    client.powChallenge()
                }
            }
        }
        assertEquals("signup-rate-limited", err.code)
        assertEquals(120L, err.retryAfterSecs)
        assertEquals(SIGNUP_RATE_LIMITED_COPY, authLimitCopy(err))
    }

    // ---------------------------------------------------------------
    // ADR-0054 S5 — sign in and recover by name (login handle)
    // ---------------------------------------------------------------

    private val handle = ByteArray(32) { (it * 7 + 3).toByte() }
    private val handleB64 = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(handle)
    private val accountId = ByteArray(16) { (it + 1).toByte() }

    private fun assertNamesByHandle(req: CapturedRequest) {
        val body = req.body!!
        assertEquals(req.url, handleB64, body["handle"]!!.jsonPrimitive.content)
        assertEquals(43, handleB64.length)
        assertFalse(req.url, handleB64.contains('=') || handleB64.contains('+') || handleB64.contains('/'))
        for (key in body.keys) {
            assertFalse("${req.url} carries $key", key.startsWith("account_id"))
        }
    }

    @Test
    fun `every name route sends the handle and no account id`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        val accountB64Url = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(accountId)
        client(
            responses = listOf(
                HttpStatusCode.OK to """{"session_id":"S","response":"R"}""",
                HttpStatusCode.OK to """{"session_id":"S","response":"R"}""",
                HttpStatusCode.OK to
                    """{"session_id":"S","recovery_response":"R","new_password_response":"N"}""",
                HttpStatusCode.OK to
                    """{"account_id_b64":"$accountB64Url","session_token":"t","session_expires_at_ms":1}""",
                HttpStatusCode.OK to """{"sent":true}""",
                HttpStatusCode.OK to
                    """{"account_id_b64":"$accountB64Url","session_token":"t","session_expires_at_ms":1}""",
            ),
            captured = captured,
        ).use { client ->
            client.loginStart(LoginStartRequest(handle = handleB64, request = "REQ"))
            client.recoveryLoginStart(LoginStartRequest(handle = handleB64, request = "REQ"))
            client.passwordResetStart(
                PasswordResetStartRequest(
                    handle = handleB64,
                    recovery_request = "RR",
                    new_password_request = "NR",
                ),
            )
            val totp = client.totpRecover(handle, "123456")
            client.recoveryContactRecoverStart(handle, ContactKind.EMAIL, "a@b.c")
            val contact = client.recoveryContactRecoverFinish(handle, ContactKind.EMAIL, "123456")
            // The account id is learned from the response, never sent.
            org.junit.Assert.assertArrayEquals(accountId, totp.accountId)
            org.junit.Assert.assertArrayEquals(accountId, contact.accountId)
        }
        assertEquals(
            listOf(
                "/login/start",
                "/recovery/login/start",
                "/password/reset/start",
                "/totp/recover",
                "/recovery-contact/recover/start",
                "/recovery-contact/recover/finish",
            ),
            captured.map { it.url.removePrefix("http://test.invalid") },
        )
        captured.forEach(::assertNamesByHandle)
    }

    @Test
    fun `a finish naming an account id of the wrong length is refused`() {
        val err = assertThrows(AuthHttpError::class.java) {
            runTest {
                client(
                    responses = listOf(
                        HttpStatusCode.OK to """{"account_id_b64":"AAAA","session_token":"t","session_expires_at_ms":1}""",
                    ),
                ).use { client -> client.totpRecover(handle, "123456") }
            }
        }
        assertEquals("/totp/recover", err.endpoint)
    }

    @Test
    fun `binding the handle is a bearer PUT and a 409 reads as taken`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(
                HttpStatusCode.OK to """{"bound":true}""",
                HttpStatusCode.Conflict to """{"error":"handle-taken"}""",
                HttpStatusCode.OK to """{"removed":true}""",
            ),
            captured = captured,
        ).use { client ->
            assertEquals(LoginHandleBinding.Bound, client.bindLoginHandle("tok", handle))
            assertEquals(LoginHandleBinding.Taken, client.bindLoginHandle("tok", handle))
            assertTrue(client.unbindLoginHandle("tok"))
        }
        assertEquals(listOf(HttpMethod.Put, HttpMethod.Put, HttpMethod.Delete), captured.map { it.method })
        for (req in captured) {
            assertTrue(req.url.endsWith("/auth/login-handle"))
            assertEquals("Bearer tok", req.authorization)
        }
        assertEquals(handleB64, captured[0].body!!["handle"]!!.jsonPrimitive.content)
        assertNull(captured[2].body)
    }

    @Test
    fun `login by name disabled is a 503 the copy helpers recognise`() {
        val err = assertThrows(AuthHttpError::class.java) {
            runTest {
                client(
                    responses = listOf(
                        HttpStatusCode.ServiceUnavailable to """{"error":"login-by-name-disabled"}""",
                    ),
                ).use { client -> client.bindLoginHandle("tok", handle) }
            }
        }
        assertTrue(isLoginByNameDisabled(err))
        assertNull("not a limit", authLimitCopy(err))
    }
}
