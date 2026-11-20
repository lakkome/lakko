// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.TextContent
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0089 F3 — every auth call that records the account's identity key
 * carries `identity_proof_b64`. The genesis mint runs through a MockEngine;
 * reset / rotate / recall finish need the native OPAQUE state machines, so
 * their request bodies are pinned by serialization and their call sites by
 * the source (the host suite has no native core).
 */
class IdentityProofSentTest {

    private val proof = ByteArray(AuthClient.IDENTITY_PROOF_LEN) { (it + 1).toByte() }
    private val proofB64 = base64UrlEncodeNoPad(proof)
    private val json = Json { explicitNulls = false }

    private fun client(status: HttpStatusCode, body: String, sent: MutableList<JsonObject>): AuthClient {
        val engine = MockEngine { request ->
            val text = (request.body as? TextContent)?.text
            if (text != null) sent += Json.parseToJsonElement(text).jsonObject
            respond(body, status, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        return AuthClient(baseUrl = "http://test.invalid", engine = engine)
    }

    @Test
    fun `the genesis mint sends the identity proof`() = runTest {
        val sent = mutableListOf<JsonObject>()
        val pubkey = ByteArray(32) { 3 }
        client(HttpStatusCode.OK, """{"genesis_attest_b64":"AAAA"}""", sent).use {
            it.genesisAttest("token", pubkey, proof)
        }
        assertEquals(proofB64, sent.single()["identity_proof_b64"]!!.jsonPrimitive.content)
        assertEquals(base64UrlEncodeNoPad(pubkey), sent.single()["pubkey_b64"]!!.jsonPrimitive.content)
    }

    @Test
    fun `a proof of the wrong length is refused before anything is sent`() = runTest {
        val sent = mutableListOf<JsonObject>()
        client(HttpStatusCode.OK, "{}", sent).use { c ->
            try {
                c.genesisAttest("token", ByteArray(32), ByteArray(63))
                fail("expected AuthClientArgError")
            } catch (_: AuthClientArgError) {
            }
        }
        assertTrue(sent.isEmpty())
    }

    @Test
    fun `a refused proof or a taken key takes the ordinary failure path`() = runTest {
        for ((status, code) in listOf(400 to "identity-proof-invalid", 409 to "identity-key-taken")) {
            val sent = mutableListOf<JsonObject>()
            val err = client(HttpStatusCode.fromValue(status), """{"error":"$code"}""", sent).use { c ->
                try {
                    c.genesisAttest("token", ByteArray(32), proof)
                    null
                } catch (e: AuthHttpError) {
                    e
                }
            }
            assertEquals(status, err!!.status)
            assertEquals(code, err.code)
            // Neither is the "auth holds another key" outcome: retried later.
            assertEquals(GenesisMintOutcome.Retry, genesisMintOutcome(err))
        }
    }

    @Test
    fun `the reset, rotate and recall finish bodies carry the proof`() {
        val reset = json.encodeToString(
            KeyResetFinishRequest(
                fresh_grant = "g",
                recovery_upload = "u",
                new_signature_pubkey_b64 = "k",
                identity_proof_b64 = proofB64,
            ),
        )
        val rotate = json.encodeToString(
            KeyRotateFinishRequest(
                fresh_grant = "g",
                recovery_upload = "u",
                new_signature_pubkey_b64 = "k",
                identity_proof_b64 = proofB64,
                supersession_proof_b64 = "s",
            ),
        )
        val recall = json.encodeToString(
            RecallFinishRequest(
                session_id = "s",
                account_id_b64 = "a",
                anchor_seq = 1,
                login_finish = "l",
                password_upload = "p",
                recovery_upload = "r",
                new_signature_pubkey_b64 = "k",
                identity_proof_b64 = proofB64,
                statement_b64 = "st",
            ),
        )
        for (body in listOf(reset, rotate, recall)) {
            assertEquals(proofB64, Json.parseToJsonElement(body).jsonObject["identity_proof_b64"]!!.jsonPrimitive.content)
        }
    }

    @Test
    fun `the client puts the proof on each of the four finish calls`() {
        val source = java.io.File("src/main/kotlin/me/lakko/app/auth/AuthClient.kt").readText()
        for (fn in listOf("genesisAttest", "resetAccountKeys", "rotateAccountKeys", "recallAccount")) {
            val body = source.substringAfter("    public suspend fun $fn(").substringBefore("\n    }\n")
            assertTrue(fn, "requireIdentityProof(identityProof)" in body)
            assertTrue(fn, "identity_proof_b64 = " in body)
        }
    }

    @Test
    fun `every call site signs the proof by the key being recorded`() {
        val source = java.io.File("src/main/kotlin/me/lakko/app/auth/AuthRepository.kt").readText()
        // genesis: the identity whose key is sent, over the account id.
        assertEquals(2, Regex("""genesisAttest\(\w+, pubkey, proof\)""").findAll(source).count())
        assertTrue("signIdentityProof(identity, accountId)" in source)
        // reset, rotate, recall: the NEW key's seed signs.
        assertTrue("identityProof = identityProofFor(seedCopy, accountIdBytes)" in source)
        assertTrue("identityProof = identityProofFor(newSeed, secrets.accountId)" in source)
        assertTrue("identityProof = identityProofFor(seedCopy, offer.accountId)" in source)
        assertTrue("identityProof = identityProofFor(seedCopy, record.accountId)" in source)
        assertEquals(
            Regex("""it\.(resetAccountKeys|rotateAccountKeys|recallAccount)\(""").findAll(source).count(),
            Regex("""identityProof = identityProofFor\(""").findAll(source).count(),
        )
    }

    @Test
    fun `the reset refuses a proof of the wrong length before anything is sent`() {
        assertThrows(AuthClientArgError::class.java) {
            kotlinx.coroutines.runBlocking {
                client(HttpStatusCode.OK, "{}", mutableListOf()).use {
                    it.resetAccountKeys("t", "g", ByteArray(64), ByteArray(32), ByteArray(10))
                }
            }
        }
    }
}
