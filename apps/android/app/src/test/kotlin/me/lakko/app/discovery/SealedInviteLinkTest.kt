// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.OutgoingContent
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Sealed invite links (ADR-0028 / ADR-0009, 2026-09-28): the fragment secret,
 * the damaged state on every kind, the two URL forms, the upload bodies.
 */
class SealedInviteLinkTest {

    private val L = TEST_INVITE_LINK_LENGTHS
    private val secret = ByteArray(12) { (it + 1).toByte() }
    private val secretText = base64UrlEncodeNoPad(secret)

    @Test
    fun `the secret reads back out of a 16-character fragment`() {
        assertEquals(16, secretText.length)
        assertArrayEquals(secret, inviteLinkSecretOf(secretText, L))
    }

    @Test
    fun `a missing or malformed fragment is damaged`() {
        for (fragment in listOf(null, "", "  ", secretText.dropLast(1), secretText + "A", "not*base64url!!!")) {
            assertNull("'$fragment'", inviteLinkSecretOf(fragment, L))
            assertThrows(InviteLinkDamaged::class.java) { requireInviteLinkSecret(fragment, L) }
        }
    }

    @Test
    fun `a sealed link that does not open under its secret is damaged`() {
        val error = assertThrows(InviteLinkDamaged::class.java) {
            openSealedInviteLink(byteArrayOf(1, 2, 3), secret) { _, _ -> throw IllegalStateException("aead") }
        }
        assertTrue(isInviteLinkDamaged(RuntimeException("wrapped", error)))
        assertEquals("opened", openSealedInviteLink(byteArrayOf(1), secret) { _, key -> assertArrayEquals(secret, key); "opened" })
    }

    @Test
    fun `the pointer link is the id plus the secret`() {
        val id = ByteArray(6) { 5 }
        val url = composeSealedInviteUrl("https://lakko.me", "/g/", id, secret, L)
        assertEquals("https://lakko.me/g/${base64UrlEncodeNoPad(id)}#$secretText", url)
        assertEquals(8 + 16 + "https://lakko.me/g/#".length, url.length)
    }

    /** There is no link without a ledger id (the inline form was removed on
     *  2026-10-19). */
    @Test
    fun `a ledger id of another width never produces a link`() {
        assertThrows(IllegalArgumentException::class.java) {
            composeSealedInviteUrl("https://lakko.me", "/c/", ByteArray(150), secret, L)
        }
    }

    @Test
    fun `a wrong-width secret never produces a link`() {
        assertThrows(IllegalArgumentException::class.java) {
            appendInviteLinkSecret("https://lakko.me/i/x", ByteArray(32), L)
        }
    }

    @Test
    fun `the link argument splits back into slug and fragment`() {
        val arg = InviteLinkArgument("abc", secretText)
        assertEquals("abc#$secretText", arg.encode())
        assertEquals(arg, InviteLinkArgument.decode(arg.encode()))
        assertEquals(InviteLinkArgument("abc", null), InviteLinkArgument.decode("abc#"))
        assertEquals("abc", InviteLinkArgument("abc", "").encode())
    }

    @Test
    fun `the group and call uploads carry the sealed link and the clear fields`() = runTest {
        val bodies = mutableListOf<Pair<String, String>>()
        val engine = MockEngine { request ->
            val body = (request.body as OutgoingContent.ByteArrayContent).bytes().toString(Charsets.UTF_8)
            bodies += request.url.encodedPath to body
            respond(
                content = """{"blob_id":"${base64UrlEncodeNoPad(ByteArray(6) { 1 })}"}""",
                status = HttpStatusCode.OK,
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        val upload = SealedInviteUpload(
            sealed = ByteArray(120) { 7 },
            revocationPubkey = ByteArray(32) { 2 },
            expiryMs = 1234L,
            maxUses = 4294967295L,
        )
        DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { L }).use { client ->
            client.setSessionToken("tok")
            assertArrayEquals(ByteArray(6) { 1 }, client.uploadGroupInvite(upload))
            client.uploadCallInvite(upload)
        }
        for ((path, body) in bodies) {
            val json = Json.parseToJsonElement(body).jsonObject
            assertEquals(setOf("sealed", "revocation_pubkey", "expiry_ms", "max_uses"), json.keys)
            assertEquals("1234", json["expiry_ms"]!!.jsonPrimitive.content)
            assertEquals("4294967295", json["max_uses"]!!.jsonPrimitive.content)
            assertTrue(path, path.endsWith("/discovery/group-invite") || path.endsWith("/discovery/call-invite"))
        }
        assertEquals(2, bodies.size)
    }

    @Test
    fun `a fetch hands back the sealed bytes`() = runTest {
        val sealed = ByteArray(40) { 3 }
        val engine = MockEngine {
            respond(
                content = """{"sealed":"${java.util.Base64.getEncoder().encodeToString(sealed)}"}""",
                status = HttpStatusCode.OK,
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { L }).use { client ->
            assertArrayEquals(sealed, client.fetchGroupInvite(ByteArray(6)))
        }
    }
}
