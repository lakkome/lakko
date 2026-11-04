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
    fun `the phrase status is a bearer GET of its own`() = runTest {
        val seen = mutableListOf<Captured>()
        val status = client(seen, HttpStatusCode.OK to """{"saved":false,"rotation_needed":true}""")
            .use { it.accountPhraseStatus("tok") }
        assertEquals(AccountPhraseStatus(saved = false, rotationNeeded = true), status)
        assertEquals(HttpMethod.Get, seen.single().method)
        assertTrue(seen.single().url.endsWith("/account/phrase"))
        assertEquals("Bearer tok", seen.single().authorization)
    }

    @Test
    fun `a phrase status without rotation_needed is a failure, not a guess`() = runTest {
        val failed = try {
            client(mutableListOf(), HttpStatusCode.OK to """{"saved":false}""").use { it.accountPhraseStatus("tok") }
            false
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            // Whatever the client threw: the screen's failed state.
            true
        }
        assertTrue("an answer with no path must not decide one", failed)
    }

    @Test
    fun `marking the phrase saved sends the grant and reads the refusals`() = runTest {
        val seen = mutableListOf<Captured>()
        client(seen, HttpStatusCode.NoContent to "").use { it.markPhraseSaved("tok", "grant") }
        assertEquals(HttpMethod.Post, seen.single().method)
        assertTrue(seen.single().url.endsWith("/account/phrase/saved"))
        assertEquals("Bearer tok", seen.single().authorization)
        assertEquals("grant", seen.single().body!!["fresh_grant"]!!.jsonPrimitive.content)
        assertEquals(1, seen.single().body!!.size)

        suspend fun refusal(status: HttpStatusCode, body: String): Throwable = try {
            client(mutableListOf(), status to body).use { it.markPhraseSaved("tok", "grant") }
            error("refused")
        } catch (err: AuthHttpError) {
            err
        }
        assertEquals(
            PhraseSavedOutcome.NeedsRotation,
            phraseSavedOutcomeOf(refusal(HttpStatusCode.Conflict, """{"error":"phrase-save-needs-rotation"}""")),
        )
        assertEquals(
            PhraseSavedOutcome.ProofRequired,
            phraseSavedOutcomeOf(refusal(HttpStatusCode.Forbidden, """{"error":"fresh-auth-required"}""")),
        )
        assertEquals(PhraseSavedOutcome.Failed, phraseSavedOutcomeOf(refusal(HttpStatusCode.InternalServerError, "{}")))
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

    @Test
    fun `the re-seal is a PUT with the handle, the address, a salt and the vault, and no grant`() = runTest {
        val seen = mutableListOf<Captured>()
        val salt = ByteArray(EMAIL_VAULT_SALT_LEN) { (0xF9 + it % 7).toByte() }
        val sealed = ByteArray(EMAIL_VAULT_SEALED_LEN) { (0xFA + it % 6).toByte() }
        client(seen, HttpStatusCode.OK to """{"vault":true}""")
            .use { it.recoveryEmailVaultPut("tok", handle, "name@example.com", salt, sealed) }
        val request = seen.single()
        assertEquals(HttpMethod.Put, request.method)
        assertTrue(request.url.endsWith("/recovery-email/vault"))
        assertEquals("Bearer tok", request.authorization)
        val body = request.body!!
        assertEquals(base64UrlEncodeNoPad(handle), body["handle"]!!.jsonPrimitive.content)
        assertEquals("name@example.com", body["address"]!!.jsonPrimitive.content)
        assertEquals(base64Encode(salt), body["vault_salt"]!!.jsonPrimitive.content)
        assertEquals(base64Encode(sealed), body["sealed_entropy"]!!.jsonPrimitive.content)
        assertFalse(body.containsKey("fresh_grant"))
    }

    @Test
    fun `the re-seal's refusals keep their codes`() = runTest {
        for ((status, code, outcome) in listOf(
            Triple(HttpStatusCode.Conflict, "vault-exists", ResealOutcome.Settled),
            Triple(HttpStatusCode.Forbidden, "address-mismatch", ResealOutcome.AddressStale),
            Triple(HttpStatusCode.TooManyRequests, "too-many-attempts", ResealOutcome.RetryLater),
        )) {
            try {
                client(mutableListOf(), status to """{"error":"$code"}""").use {
                    it.recoveryEmailVaultPut("tok", handle, "a@b", ByteArray(EMAIL_VAULT_SALT_LEN), ByteArray(EMAIL_VAULT_SEALED_LEN))
                }
                fail("$code must throw")
            } catch (err: AuthHttpError) {
                assertEquals(outcome, resealOutcomeOf(err))
            }
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
                recoveryEmail = LoginHandleRecoveryEmail(address = "name@example.com", vault_salt = "c2FsdA==", sealed_entropy = "dmF1bHQ="),
            )
        }
        val body = seen.single().body!!
        assertEquals(HttpMethod.Put, seen.single().method)
        assertEquals("grant", body["fresh_grant"]!!.jsonPrimitive.content)
        assertEquals(base64UrlEncodeNoPad(previous), body["previous_handle"]!!.jsonPrimitive.content)
        val email = body["recovery_email"]!!.jsonObject
        assertEquals("name@example.com", email["address"]!!.jsonPrimitive.content)
        assertEquals("c2FsdA==", email["vault_salt"]!!.jsonPrimitive.content)
        assertEquals("dmF1bHQ=", email["sealed_entropy"]!!.jsonPrimitive.content)
    }

    @Test
    fun `with a saved phrase the recovery email travels without a vault`() = runTest {
        val seen = mutableListOf<Captured>()
        client(seen, HttpStatusCode.OK to """{"bound":true}""").use {
            it.bindLoginHandle("tok", handle, "grant", ByteArray(32) { 9 }, LoginHandleRecoveryEmail(address = "a@b"))
        }
        assertEquals(setOf("address"), seen.single().body!!["recovery_email"]!!.jsonObject.keys)
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
