// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.donate

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpMethod
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import java.security.MessageDigest
import java.util.Base64
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** docs/ux/donations.md section 4.1, against a fake signature check: the
 *  host JVM has no native library. */
class DonationsClientTest {
    private val key = ByteArray(32) { 7 }
    private val qrPng = byteArrayOf(0x89.toByte(), 0x50, 0x4e, 0x47, 1, 2, 3)
    private val qrSha = MessageDigest.getInstance("SHA-256").digest(qrPng)

    /** A statement is the UTF-8 of a token the fake knows. */
    private fun st(token: String): String = Base64.getEncoder().encodeToString(token.toByteArray())

    private inner class FakeVerifier : DonationVerifier {
        val keysSeen = mutableListOf<ByteArray>()

        override fun verifyMethod(statement: ByteArray, key: ByteArray): VerifiedDonationStatement {
            keysSeen += key
            return when (String(statement)) {
                "sbp" -> VerifiedDonationStatement("sbp", DonateKind.SBP, DonateIcon.NONE, "https://pay.example/sbp", null, null, null, qrSha, true)
                "usdt" -> VerifiedDonationStatement("usdt", DonateKind.CRYPTO, DonateIcon.TON, null, "USDT", "TRON (TRC-20)", "TXYZ", null, false)
                "bmc-http" -> VerifiedDonationStatement("bmc", DonateKind.BMC, DonateIcon.NONE, "http://insecure.example", null, null, null, null, false)
                "revolut" -> VerifiedDonationStatement("revolut", DonateKind.REVOLUT, DonateIcon.NONE, "https://revolut.example", null, null, null, null, false)
                else -> throw IllegalStateException("bad-signature: test")
            }
        }

        override fun verifyPhone(statement: ByteArray, key: ByteArray, id: String): String {
            if (String(statement) != "phone-sbp" || id != "sbp") throw IllegalStateException("id-mismatch: test")
            return "+7 900 000 00 00"
        }
    }

    private fun client(verifier: DonationVerifier, key: ByteArray? = this.key, handler: MockEngine): DonationsClient =
        DonationsClient(baseUrl = "https://x.test/discovery", engine = handler, key = key, verifier = verifier)

    private fun json(body: String, status: HttpStatusCode = HttpStatusCode.OK) =
        MockEngine { respond(body, status, headersOf(HttpHeaders.ContentType, "application/json")) }

    @Test
    fun `verified methods are kept in the server order`() = runTest {
        val body = """{"methods":[{"id":"usdt","statement_b64":"${st("usdt")}"},{"id":"sbp","statement_b64":"${st("sbp")}"}]}"""
        val v = FakeVerifier()
        val methods = client(v, handler = json(body)).methods()
        assertEquals(listOf("usdt", "sbp"), methods.map { it.id })
        assertEquals(DonateCrypto("USDT", "TRON (TRC-20)", "TXYZ", DonateIcon.TON), methods[0].crypto)
        assertEquals("https://pay.example/sbp", methods[1].link)
        assertTrue(methods[1].hasPhone)
        assertArrayEquals(key, v.keysSeen.first())
    }

    @Test
    fun `a tampered statement, a mismatched id, a repeat and an unusable method are dropped`() = runTest {
        val body = """{"methods":[
            {"id":"sbp","statement_b64":"${st("tampered")}"},
            {"id":"bmc","statement_b64":"${st("revolut")}"},
            {"id":"revolut","statement_b64":"${st("revolut")}"},
            {"id":"revolut","statement_b64":"${st("revolut")}"},
            {"id":"bmc","statement_b64":"${st("bmc-http")}"},
            {"id":"sbp","statement_b64":"not base64!!"}
        ]}"""
        val methods = client(FakeVerifier(), handler = json(body)).methods()
        assertEquals(listOf("revolut"), methods.map { it.id })
    }

    @Test
    fun `no pinned key shows nothing and asks the server nothing`() = runTest {
        var requests = 0
        val engine = MockEngine {
            requests += 1
            respond("{}", HttpStatusCode.OK)
        }
        val c = client(FakeVerifier(), key = null, handler = engine)
        assertTrue(c.methods().isEmpty())
        assertEquals(0, requests)
    }

    @Test
    fun `a server failure of the list is the error state, not an empty list`() = runTest {
        val failure = runCatching { client(FakeVerifier(), handler = json("", HttpStatusCode.InternalServerError)).methods() }
        assertTrue(failure.exceptionOrNull() is DonationsUnavailable)
    }

    @Test
    fun `the QR is returned only when its bytes hash to the signed digest`() = runTest {
        val good = MockEngine { respond(qrPng, HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "image/png")) }
        assertArrayEquals(qrPng, client(FakeVerifier(), handler = good).qr("sbp", qrSha))

        val other = qrPng.copyOf().also { it[it.size - 1] = 9 }
        val bad = MockEngine { respond(other, HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "image/png")) }
        assertNull(client(FakeVerifier(), handler = bad).qr("sbp", qrSha))

        val missing = MockEngine { respond("", HttpStatusCode.NotFound) }
        assertNull(client(FakeVerifier(), handler = missing).qr("sbp", qrSha))
    }

    @Test
    fun `a QR larger than the bound is refused even when it hashes right`() {
        assertEquals(512 * 1024, MAX_QR_BYTES)
        val atBound = ByteArray(MAX_QR_BYTES) { 1 }
        assertTrue(qrMatches(atBound, MessageDigest.getInstance("SHA-256").digest(atBound)))
        val big = ByteArray(MAX_QR_BYTES + 1)
        val sha = MessageDigest.getInstance("SHA-256").digest(big)
        assertFalse(qrMatches(big, sha))
        assertTrue(qrMatches(qrPng, qrSha))
    }

    @Test
    fun `the phone is posted and verified against the method id`() = runTest {
        var method: HttpMethod? = null
        var path: String? = null
        val engine = MockEngine { req ->
            method = req.method
            path = req.url.encodedPath
            respond("""{"statement_b64":"${st("phone-sbp")}"}""", HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        assertEquals("+7 900 000 00 00", client(FakeVerifier(), handler = engine).phone("sbp"))
        assertEquals(HttpMethod.Post, method)
        assertEquals("/discovery/discovery/donate/sbp/phone", path)
    }

    @Test
    fun `a phone statement that does not verify throws`() = runTest {
        val engine = json("""{"statement_b64":"${st("phone-other")}"}""")
        val failure = runCatching { client(FakeVerifier(), handler = engine).phone("sbp") }
        assertTrue(failure.exceptionOrNull() is IllegalStateException)
    }

    @Test
    fun `the pinned key parses only as 32 bytes of base64url`() {
        val raw = Base64.getUrlEncoder().withoutPadding().encodeToString(key)
        assertEquals(43, raw.length)
        assertArrayEquals(key, parseDonationsKey(raw))
        assertNull(parseDonationsKey(""))
        assertNull(parseDonationsKey("   "))
        assertNull(parseDonationsKey(null))
        assertNull(parseDonationsKey(Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(31))))
        assertNull(parseDonationsKey("not*base64"))
    }
}
