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
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0075 - the wire shapes of the recovery-email routes that need no
 * native core (the OPAQUE and statement routes do and are not run on the
 * host JVM). A [MockEngine] answers; nothing reaches a network.
 */
class RecoveryEmailClientTest {

    private class Captured(
        val method: HttpMethod,
        val url: String,
        val body: JsonObject?,
        val authorization: String?,
    )

    private fun client(captured: MutableList<Captured>, vararg responses: Pair<HttpStatusCode, String>): AuthClient {
        var index = 0
        val engine = MockEngine { request ->
            val raw: String? = when (val outgoing = request.body) {
                is TextContent -> outgoing.text
                is OutgoingContent.ByteArrayContent -> outgoing.bytes().toString(Charsets.UTF_8)
                else -> null
            }
            captured += Captured(
                method = request.method,
                url = request.url.toString(),
                body = raw?.takeIf { it.isNotEmpty() }?.let { Json.parseToJsonElement(it).jsonObject },
                authorization = request.headers[HttpHeaders.Authorization],
            )
            val (status, body) = responses[index++]
            respond(body, status, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        return AuthClient(baseUrl = "http://test.invalid", engine = engine)
    }

    private val handle = ByteArray(32) { (0xF0 + it % 16).toByte() }

    @Test
    fun `availability and status are plain GETs and never carry an address`() = runTest {
        val seen = mutableListOf<Captured>()
        client(
            seen,
            HttpStatusCode.OK to """{"available":true}""",
            HttpStatusCode.OK to """{"available":true,"enabled":true,"totp":true}""",
        ).use { c ->
            assertTrue(c.recoveryEmailAvailable())
            assertEquals(
                RecoveryEmailStatus(available = true, enabled = true, totp = true),
                c.recoveryEmailStatus("tok"),
            )
        }
        assertEquals(HttpMethod.Get, seen[0].method)
        assertTrue(seen[0].url.endsWith("/recovery-email/available"))
        assertNull(seen[0].authorization)
        assertEquals(HttpMethod.Get, seen[1].method)
        assertTrue(seen[1].url.endsWith("/recovery-email"))
        assertEquals("Bearer tok", seen[1].authorization)
    }

    @Test
    fun `enrol start sends the grant, the handle as base64url and the language`() = runTest {
        val seen = mutableListOf<Captured>()
        val start = client(
            seen,
            HttpStatusCode.OK to """{"challenge_id":"chal","resend_after_secs":45,"expires_in_secs":600}""",
        ).use { it.recoveryEmailEnrolStart("tok", "grant", handle, "name@example.com", "ru") }
        assertEquals("chal", start.challengeId)
        assertEquals(45L, start.resendAfterSecs)
        assertEquals(600L, start.expiresInSecs)
        val body = seen.single().body!!
        assertEquals(HttpMethod.Post, seen.single().method)
        assertTrue(seen.single().url.endsWith("/recovery-email/enrol/start"))
        assertEquals("Bearer tok", seen.single().authorization)
        assertEquals("grant", body["fresh_grant"]!!.jsonPrimitive.content)
        assertEquals(base64UrlEncodeNoPad(handle), body["handle"]!!.jsonPrimitive.content)
        assertEquals("name@example.com", body["address"]!!.jsonPrimitive.content)
        assertEquals("ru", body["lang"]!!.jsonPrimitive.content)
    }

    @Test
    fun `enrol finish carries the challenge and the code and nothing derived from the phrase`() = runTest {
        // The email vault is gone (ADR-0075, amended 2026-11-26): no copy of
        // the phrase ever leaves the device.
        val seen = mutableListOf<Captured>()
        client(seen, HttpStatusCode.OK to """{"enabled":true}""").use { c ->
            assertTrue(c.recoveryEmailEnrolFinish("tok", "chal", "123456"))
        }
        assertEquals(setOf("challenge_id", "code"), seen.single().body!!.keys)
        assertEquals("123456", seen.single().body!!["code"]!!.jsonPrimitive.content)
    }

    @Test
    fun `removing is a DELETE with the grant in its body`() = runTest {
        val seen = mutableListOf<Captured>()
        client(seen, HttpStatusCode.OK to """{"removed":true}""").use { it.recoveryEmailRemove("tok", "grant") }
        assertEquals(HttpMethod.Delete, seen.single().method)
        assertTrue(seen.single().url.endsWith("/recovery-email"))
        assertEquals("grant", seen.single().body!!["fresh_grant"]!!.jsonPrimitive.content)
    }

    @Test
    fun `a refused proof keeps its code`() = runTest {
        try {
            client(mutableListOf(), HttpStatusCode.Forbidden to """{"error":"fresh-auth-required"}""")
                .use { it.recoveryEmailRemove("tok", null) }
            fail("403 must throw")
        } catch (err: AuthHttpError) {
            assertTrue(isFreshAuthRequired(err))
        }
    }

    @Test
    fun `notices are read after the cursor`() = runTest {
        val seen = mutableListOf<Captured>()
        val page = client(
            seen,
            HttpStatusCode.OK to
                """{"notices":[{"id":8,"kind":"recovery-email-added","at_ms":99}],"next_after":8,"more":false}""",
            HttpStatusCode.OK to """{"notices":[],"next_after":0,"more":false}""",
        ).use { c ->
            // No cursor yet: by time.
            c.accountNotices("tok", after = null, sinceMs = 1234L)
            c.accountNotices("tok", 7L).also { /* second answer is the empty page */ }
        }
        assertTrue(seen[0].url.endsWith("/account/notices?since_ms=1234"))
        assertTrue(seen[1].url.endsWith("/account/notices?after=7"))
        assertTrue(page.notices.isEmpty())
    }

    @Test
    fun `the recover routes name the account by handle and carry no session`() = runTest {
        val seen = mutableListOf<Captured>()
        client(
            seen,
            HttpStatusCode.OK to """{"challenge_id":"chal","resend_after_secs":45,"expires_in_secs":600}""",
            HttpStatusCode.OK to """{"ticket":"tick","totp_required":true,"expires_in_secs":600}""",
            HttpStatusCode.OK to """{"ok":true}""",
        ).use { c ->
            assertEquals("chal", c.emailRecoverStart(handle, "name@example.com", null).challenge_id)
            val verified = c.emailRecoverVerify("chal", "123456")
            assertEquals("tick", verified.ticket)
            assertTrue(verified.totp_required)
            c.emailRecoverTotp("tick", "654321")
        }
        assertTrue(seen.all { it.authorization == null && it.method == HttpMethod.Post })
        assertEquals(base64UrlEncodeNoPad(handle), seen[0].body!!["handle"]!!.jsonPrimitive.content)
        assertFalse("no language was given", seen[0].body!!.containsKey("lang"))
        assertEquals("chal", seen[1].body!!["challenge_id"]!!.jsonPrimitive.content)
        assertEquals("tick", seen[2].body!!["ticket"]!!.jsonPrimitive.content)
    }

    @Test
    fun `a wrong code is the one coded 401`() = runTest {
        try {
            client(mutableListOf(), HttpStatusCode.Unauthorized to """{"error":"code-refused"}""")
                .use { it.emailRecoverVerify("chal", "000000") }
            fail("401 must throw")
        } catch (err: AuthHttpError) {
            assertEquals(
                EmailRecoverFailure.WrongCode(me.lakko.app.l10n.UiText("recovery_email.code.wrong")),
                emailRecoverFailureOf(EmailRecoverCall.Verify, err),
            )
        }
    }

    @Test
    fun `the status flags default to false when a field is missing`() = runTest {
        val status = client(mutableListOf(), HttpStatusCode.OK to """{"available":true}""")
            .use { it.recoveryEmailStatus("tok") }
        assertFalse(status.enabled || status.totp)
        assertTrue(Json.parseToJsonElement("""{"a":true}""").jsonObject["a"]!!.jsonPrimitive.boolean)
    }

    // ---- 2026-11-26: the last factor, the authenticator, the reset's factor --

    @Test
    fun `removing the account's last second way in is the coded 409`() = runTest {
        try {
            client(mutableListOf(), HttpStatusCode.Conflict to """{"error":"last-factor"}""")
                .use { it.recoveryEmailRemove("tok", "grant") }
            fail("409 must throw")
        } catch (err: AuthHttpError) {
            assertTrue(isLastFactor(err))
        }
    }

    @Test
    fun `setting up an authenticator carries the fresh grant on both calls`() = runTest {
        val seen = mutableListOf<Captured>()
        client(
            seen,
            HttpStatusCode.OK to """{"otpauth_uri":"otpauth://totp/x","secret_b32":"ABCDEFGH"}""",
            HttpStatusCode.OK to """{"enabled":true}""",
        ).use { c ->
            val start = c.totpEnrollStart("tok", "grant")
            assertEquals("ABCDEFGH", start.secretB32)
            c.totpEnrollFinish("tok", " 123456 ", "grant")
        }
        assertTrue(seen[0].url.endsWith("/totp/enroll/start"))
        assertEquals("grant", seen[0].body!!["fresh_grant"]!!.jsonPrimitive.content)
        assertTrue(seen[1].url.endsWith("/totp/enroll/finish"))
        assertEquals("grant", seen[1].body!!["fresh_grant"]!!.jsonPrimitive.content)
        assertEquals("123456", seen[1].body!!["code"]!!.jsonPrimitive.content)
        assertTrue(seen.all { it.authorization == "Bearer tok" })
    }

    @Test
    fun `the reset's factor routes carry the password grant and answer a reset grant`() = runTest {
        val seen = mutableListOf<Captured>()
        val answer = """{"reset_grant":"RG","reset_grant_expires_at_ms":9000}"""
        client(
            seen,
            HttpStatusCode.OK to answer,
            HttpStatusCode.OK to """{"challenge_id":"chal","resend_after_secs":45,"expires_in_secs":600}""",
            HttpStatusCode.OK to answer,
            HttpStatusCode.OK to answer,
        ).use { c ->
            val totp = c.resetFactorTotp("tok", "pw-grant", "123456")
            assertEquals("RG", totp.grant)
            assertEquals(9000L, totp.expiresAtMs)
            assertEquals("chal", c.resetFactorEmailStart("tok", "pw-grant", handle, "name@example.com", null).challenge_id)
            assertEquals("RG", c.resetFactorEmailFinish("tok", "pw-grant", "chal", "654321").grant)
            assertEquals("RG", c.resetFactorPasskey("tok", "pw-grant").grant)
        }
        assertEquals(
            listOf(
                "/account/keys/reset/factor/totp",
                "/account/keys/reset/factor/email/start",
                "/account/keys/reset/factor/email/finish",
                "/account/keys/reset/factor/passkey",
            ),
            seen.map { "/" + it.url.substringAfter("http://test.invalid/") },
        )
        assertTrue(seen.all { it.method == HttpMethod.Post && it.authorization == "Bearer tok" })
        assertTrue(seen.all { it.body!!["fresh_grant"]!!.jsonPrimitive.content == "pw-grant" })
        assertEquals("123456", seen[0].body!!["code"]!!.jsonPrimitive.content)
        assertEquals(base64UrlEncodeNoPad(handle), seen[1].body!!["handle"]!!.jsonPrimitive.content)
        assertEquals("name@example.com", seen[1].body!!["address"]!!.jsonPrimitive.content)
        assertEquals("chal", seen[2].body!!["challenge_id"]!!.jsonPrimitive.content)
        assertEquals(setOf("fresh_grant"), seen[3].body!!.keys)
        // The grant never prints.
        assertFalse(ResetFactorGrant("RG", 1L).toString().contains("RG"))
    }

    @Test
    fun `a refused factor and a missing factor grant keep their codes`() = runTest {
        try {
            client(mutableListOf(), HttpStatusCode.Unauthorized to """{"error":"factor-refused"}""")
                .use { it.resetFactorTotp("tok", "pw-grant", "000000") }
            fail("401 must throw")
        } catch (err: AuthHttpError) {
            assertEquals(
                ResetFactorFailure.WrongCode(me.lakko.app.l10n.UiText("settings.recovery.totp.error.wrong")),
                resetFactorFailureOf(ResetFactorCall.Totp, err),
            )
        }
        try {
            client(mutableListOf(), HttpStatusCode.Forbidden to """{"error":"reset-factor-required"}""")
                .use { it.ownedChannelsSecret("tok", null) }
            fail("403 must throw")
        } catch (err: AuthHttpError) {
            assertTrue(isResetFactorRequired(err))
        }
    }

    @Test
    fun `the owned-channels server half is read with the reset grant and must be 32 bytes`() = runTest {
        val seen = mutableListOf<Captured>()
        val half = ByteArray(32) { (it + 1).toByte() }
        val read = client(
            seen,
            HttpStatusCode.OK to """{"owned_channels_secret":"${base64UrlEncodeNoPad(half)}"}""",
        ).use { it.ownedChannelsSecret("tok", "RG") }
        assertTrue(half.contentEquals(read))
        assertTrue(seen.single().url.endsWith("/account/owned-channels-secret"))
        assertEquals("RG", seen.single().body!!["reset_grant"]!!.jsonPrimitive.content)
        try {
            client(mutableListOf(), HttpStatusCode.OK to """{"owned_channels_secret":"AAAA"}""")
                .use { it.ownedChannelsSecret("tok", "RG") }
            fail("a 3-byte half must not be accepted")
        } catch (err: AuthHttpError) {
            assertEquals("/account/owned-channels-secret", err.endpoint)
        }
    }

    // ---- ADR-0076: the login handle on a rename -----------------------------

    @Test
    fun `a first bind sends the handle alone`() = runTest {
        val seen = mutableListOf<Captured>()
        client(seen, HttpStatusCode.OK to """{"bound":true}""").use { it.bindLoginHandle("tok", handle) }
        val body = seen.single().body!!
        assertEquals(setOf("handle"), body.keys)
    }

    @Test
    fun `a rename's bind carries the grant, the previous handle and the recovery email`() = runTest {
        val seen = mutableListOf<Captured>()
        val previous = ByteArray(32) { 9 }
        client(seen, HttpStatusCode.OK to """{"bound":true}""").use {
            it.bindLoginHandle(
                sessionToken = "tok",
                handle = handle,
                freshGrant = "grant",
                previousHandle = previous,
                recoveryEmail = LoginHandleRecoveryEmail(address = "name@example.com"),
            )
        }
        val body = seen.single().body!!
        assertEquals(HttpMethod.Put, seen.single().method)
        assertEquals("grant", body["fresh_grant"]!!.jsonPrimitive.content)
        assertEquals(base64UrlEncodeNoPad(previous), body["previous_handle"]!!.jsonPrimitive.content)
        val email = body["recovery_email"]!!.jsonObject
        // The address and nothing else: no vault travels any more.
        assertEquals(setOf("address"), email.keys)
        assertEquals("name@example.com", email["address"]!!.jsonPrimitive.content)
    }

    @Test
    fun `a rename that would drop the last second way in is the coded 409`() = runTest {
        try {
            client(mutableListOf(), HttpStatusCode.Conflict to """{"error":"last-factor"}""")
                .use { it.bindLoginHandle("tok", handle, "grant", ByteArray(32) { 9 }, null) }
            fail("last-factor must throw")
        } catch (err: AuthHttpError) {
            assertTrue(isLastFactor(err))
        }
    }

    @Test
    fun `handle-taken is taken, handle-mismatch is its own refusal`() = runTest {
        val taken = client(mutableListOf(), HttpStatusCode.Conflict to """{"error":"handle-taken"}""")
            .use { it.bindLoginHandle("tok", handle) }
        assertEquals(LoginHandleBinding.Taken, taken)
        try {
            client(mutableListOf(), HttpStatusCode.Conflict to """{"error":"handle-mismatch"}""")
                .use { it.bindLoginHandle("tok", handle) }
            fail("handle-mismatch must throw")
        } catch (err: AuthHttpError) {
            assertEquals("handle-mismatch", err.code)
        }
    }

    @Test
    fun `the check names the account's handle state, and an unknown answer binds nothing`() = runTest {
        val seen = mutableListOf<Captured>()
        client(
            seen,
            HttpStatusCode.OK to """{"state":"same"}""",
            HttpStatusCode.OK to """{"state":"other"}""",
            HttpStatusCode.OK to """{"state":"none"}""",
            HttpStatusCode.OK to """{"state":"surprise"}""",
        ).use { c ->
            assertEquals(LoginHandleState.Same, c.checkLoginHandle("tok", handle))
            assertEquals(LoginHandleState.Other, c.checkLoginHandle("tok", handle))
            assertEquals(LoginHandleState.None, c.checkLoginHandle("tok", handle))
            assertEquals(HandleSyncAction.WaitForRegister, handleSyncAction(c.checkLoginHandle("tok", handle)))
        }
        assertTrue(seen[0].url.endsWith("/auth/login-handle/check"))
        assertEquals(HttpMethod.Post, seen[0].method)
        assertEquals("Bearer tok", seen[0].authorization)
        assertEquals(base64UrlEncodeNoPad(handle), seen[0].body!!["handle"]!!.jsonPrimitive.content)
    }
}
