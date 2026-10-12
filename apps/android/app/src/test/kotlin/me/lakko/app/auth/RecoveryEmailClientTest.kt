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
 * ADR-0067 - the wire shapes of the recovery-email routes that need no
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
            HttpStatusCode.OK to """{"available":true,"enabled":true,"vault":false,"phrase_saved":true,"totp":true}""",
        ).use { c ->
            assertTrue(c.recoveryEmailAvailable())
            assertEquals(
                RecoveryEmailStatus(available = true, enabled = true, vault = false, phraseSaved = true, totp = true),
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
    fun `enrol start sends the grant, the handle as base64url and the language, and reads the salt`() = runTest {
        val seen = mutableListOf<Captured>()
        val salt = ByteArray(EMAIL_VAULT_SALT_LEN) { (0xFB + it % 5).toByte() }
        val start = client(
            seen,
            HttpStatusCode.OK to
                """{"challenge_id":"chal","vault_salt":"${base64Encode(salt)}","resend_after_secs":45,"expires_in_secs":600}""",
        ).use { it.recoveryEmailEnrolStart("tok", "grant", handle, "name@example.com", "ru") }
        assertEquals("chal", start.challengeId)
        assertTrue(salt.contentEquals(start.vaultSalt))
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
    fun `a salt of the wrong length is refused`() = runTest {
        try {
            client(
                mutableListOf(),
                HttpStatusCode.OK to """{"challenge_id":"c","vault_salt":"AAAA","resend_after_secs":1,"expires_in_secs":1}""",
            ).use { it.recoveryEmailEnrolStart("tok", "grant", handle, "a@b", null) }
            fail("a 3-byte salt must not be accepted")
        } catch (err: AuthHttpError) {
            assertEquals("/recovery-email/enrol/start", err.endpoint)
        }
    }

    @Test
    fun `enrol finish carries the vault only when one was sealed`() = runTest {
        val seen = mutableListOf<Captured>()
        val sealed = ByteArray(EMAIL_VAULT_SEALED_LEN) { it.toByte() }
        client(
            seen,
            HttpStatusCode.OK to """{"enabled":true,"vault":true}""",
            HttpStatusCode.OK to """{"enabled":true,"vault":false}""",
        ).use { c ->
            assertTrue(c.recoveryEmailEnrolFinish("tok", "chal", "123456", sealed))
            assertFalse(c.recoveryEmailEnrolFinish("tok", "chal", "123456", null))
        }
        assertEquals(base64Encode(sealed), seen[0].body!!["sealed_entropy"]!!.jsonPrimitive.content)
        assertEquals("123456", seen[0].body!!["code"]!!.jsonPrimitive.content)
        // Phrase mode: the field is absent, not null.
        assertFalse(seen[1].body!!.containsKey("sealed_entropy"))
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
    fun `open answers the vault or the password mode`() = runTest {
        val salt = ByteArray(EMAIL_VAULT_SALT_LEN) { 3 }
        val sealed = ByteArray(EMAIL_VAULT_SEALED_LEN) { 4 }
        client(
            mutableListOf(),
            HttpStatusCode.OK to
                """{"mode":"vault","vault_salt":"${base64Encode(salt)}","sealed_entropy":"${base64Encode(sealed)}"}""",
            HttpStatusCode.OK to """{"mode":"password"}""",
        ).use { c ->
            val vault = c.emailRecoverOpen("tick") as EmailRecoverOpen.Vault
            assertTrue(salt.contentEquals(vault.vaultSalt))
            assertTrue(sealed.contentEquals(vault.sealedEntropy))
            assertEquals(EmailRecoverOpen.Password, c.emailRecoverOpen("tick"))
        }
    }

    @Test
    fun `a malformed vault or an unknown mode is refused`() = runTest {
        for (body in listOf(
            """{"mode":"vault","vault_salt":"AAAA","sealed_entropy":"AAAA"}""",
            """{"mode":"vault"}""",
            """{"mode":"surprise"}""",
        )) {
            try {
                client(mutableListOf(), HttpStatusCode.OK to body).use { it.emailRecoverOpen("tick") }
                fail("accepted $body")
            } catch (err: AuthHttpError) {
                assertEquals("/recovery-email/recover/open", err.endpoint)
            }
        }
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
        assertFalse(status.enabled || status.vault || status.phraseSaved || status.totp)
        assertTrue(Json.parseToJsonElement("""{"a":true}""").jsonObject["a"]!!.jsonPrimitive.boolean)
    }
}
