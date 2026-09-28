// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

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
import me.lakko.app.auth.PowProofPayload
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for [DiscoveryClient]'s **low-level HTTP
 * layer** — the six endpoint wrappers that map `@Serializable`
 * data classes to JSON requests / responses against a Ktor
 * [MockEngine].
 *
 * Mirror of [me.lakko.app.auth.AuthClientTest] (1.15d2). Same
 * scope and constraints — the high-level `registerUsername` /
 * `lookup` / `uploadIdentityBundle` flows drive UniFFI-bound
 * [uniffi.lakko_ffi_uniffi.OprfBlinder] and `powSolve` calls
 * which load `liblakko_ffi_uniffi.so` via JNA, so they don't
 * run on the host JVM. Validation gates inside those high-level
 * methods get coverage via cheap arg-shape tests (no FFI reached).
 *
 * The four interesting properties we lock down here:
 *
 *   1. **JSON wire shape stays in lock-step with the server.**
 *      Request bodies serialize with snake_case field names
 *      (`account_id`, `oprf_hash`, etc.) verbatim; responses
 *      parse back into the typed shapes.
 *   2. **404 surfaces as `null`** on `/discovery/lookup` and
 *      `GET /discovery/identity/{...}` — the typed not-found
 *      path the high-level orchestrators rely on.
 *   3. **Non-2xx (other than 404 on those endpoints) throws
 *      [DiscoveryHttpError]** with the status + truncated body.
 *   4. **Transport failures throw [DiscoveryNetworkError]** —
 *      distinct from HTTP errors so the UI can suggest "check
 *      connection".
 */
class DiscoveryClientTest {

    private data class CapturedRequest(
        val method: HttpMethod,
        val url: String,
        val body: JsonObject?,
    )

    private fun client(
        responses: List<Pair<HttpStatusCode, String>>,
        captured: MutableList<CapturedRequest> = mutableListOf(),
    ): DiscoveryClient {
        var index = 0
        val engine = MockEngine { request ->
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
        return DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS })
    }

    // ===================================================================
    // /pow/challenge
    // ===================================================================

    @Test
    fun `powChallenge parses response`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(
                HttpStatusCode.OK to
                    """{"seed":"AAAAAAAAAAAAAAAAAAAAAA==","effort_bits":4,"expires_in":120}""",
            ),
            captured = captured,
        ).use { client ->
            val resp = client.powChallenge()
            assertEquals("AAAAAAAAAAAAAAAAAAAAAA==", resp.seed)
            assertEquals(4, resp.effort_bits)
            assertEquals(120, resp.expires_in)
        }
        assertEquals(1, captured.size)
        assertTrue(captured[0].url.endsWith("/pow/challenge"))
        assertNull(captured[0].body)  // POST with no body
    }

    // ===================================================================
    // /discovery/oprf
    // ===================================================================

    @Test
    fun `oprfEvaluate serializes request and parses response`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(HttpStatusCode.OK to """{"evaluated":"EV_B64"}"""),
            captured = captured,
        ).use { client ->
            val resp = client.oprfEvaluate(
                OprfRequest(
                    pow = PowProofPayload(seed = "S", nonce = "N", solution = "X"),
                    blinded = "BLINDED_B64",
                ),
            )
            assertEquals("EV_B64", resp.evaluated)
        }
        val body = captured[0].body
        assertNotNull(body)
        assertEquals("BLINDED_B64", body!!["blinded"]!!.jsonPrimitive.content)
        // Nested pow object came through with snake_case fields.
        val pow = body["pow"]!!.jsonObject
        assertEquals("S", pow["seed"]!!.jsonPrimitive.content)
        assertEquals("N", pow["nonce"]!!.jsonPrimitive.content)
        assertEquals("X", pow["solution"]!!.jsonPrimitive.content)
    }

    // ===================================================================
    // /discovery/register
    // ===================================================================

    @Test
    fun `register sends snake_case account_id + oprf_hash`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(HttpStatusCode.OK to """{"ok":true}"""),
            captured = captured,
        ).use { client ->
            val resp = client.register(
                RegisterRequest(
                    pow = PowProofPayload("S", "N", "X"),
                    account_id = "ACC_B64",
                    oprf_hash = "HASH_B64",
                ),
            )
            assertTrue(resp.ok)
        }
        val body = captured[0].body!!
        assertEquals("ACC_B64", body["account_id"]!!.jsonPrimitive.content)
        assertEquals("HASH_B64", body["oprf_hash"]!!.jsonPrimitive.content)
    }

    @Test
    fun `register 409 surfaces as DiscoveryHttpError`() {
        val err = assertThrows(DiscoveryHttpError::class.java) {
            runTest {
                client(
                    responses = listOf(HttpStatusCode.Conflict to "username taken"),
                ).use { client ->
                    client.register(
                        RegisterRequest(
                            pow = PowProofPayload("S", "N", "X"),
                            account_id = "ACC",
                            oprf_hash = "HASH",
                        ),
                    )
                }
            }
        }
        assertEquals(409, err.status)
        assertEquals("/discovery/register", err.endpoint)
        assertEquals("username taken", err.body)
    }

    // ===================================================================
    // /discovery/lookup — 200 + 404 paths
    // ===================================================================

    @Test
    fun `lookupRaw parses 200 response`() = runTest {
        client(
            responses = listOf(HttpStatusCode.OK to """{"account_id":"ACC_B64"}"""),
        ).use { client ->
            val resp = client.lookupRaw(
                LookupRequest(
                    pow = PowProofPayload("S", "N", "X"),
                    oprf_hash = "HASH",
                ),
            )
            assertNotNull(resp)
            assertEquals("ACC_B64", resp!!.account_id)
        }
    }

    @Test
    fun `lookupRaw folds 404 into null`() = runTest {
        client(
            responses = listOf(HttpStatusCode.NotFound to """{"error":"not found"}"""),
        ).use { client ->
            val resp = client.lookupRaw(
                LookupRequest(
                    pow = PowProofPayload("S", "N", "X"),
                    oprf_hash = "HASH",
                ),
            )
            assertNull(resp)
        }
    }

    @Test
    fun `lookupRaw 500 throws DiscoveryHttpError`() {
        val err = assertThrows(DiscoveryHttpError::class.java) {
            runTest {
                client(
                    responses = listOf(HttpStatusCode.InternalServerError to "boom"),
                ).use { client ->
                    client.lookupRaw(
                        LookupRequest(
                            pow = PowProofPayload("S", "N", "X"),
                            oprf_hash = "HASH",
                        ),
                    )
                }
            }
        }
        assertEquals(500, err.status)
    }

    // ===================================================================
    // /discovery/identity — upload + fetch (200 + 404)
    // ===================================================================

    @Test
    fun `identityUpload sends both pubkeys snake_cased`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(HttpStatusCode.OK to """{"ok":true}"""),
            captured = captured,
        ).use { client ->
            val resp = client.identityUpload(
                IdentityUploadRequest(
                    pow = PowProofPayload("S", "N", "X"),
                    account_id = "ACC_B64",
                    signature_pubkey = "SIG_B64",
                    x25519_pubkey = "X25_B64",
                ),
            )
            assertTrue(resp.ok)
        }
        val body = captured[0].body!!
        assertEquals("SIG_B64", body["signature_pubkey"]!!.jsonPrimitive.content)
        assertEquals("X25_B64", body["x25519_pubkey"]!!.jsonPrimitive.content)
    }

    @Test
    fun `identityFetch returns the bundle on 200`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(
                HttpStatusCode.OK to
                    """{"signature_pubkey":"SIG","x25519_pubkey":"X25"}""",
            ),
            captured = captured,
        ).use { client ->
            val resp = client.identityFetch(ByteArray(16))
            assertNotNull(resp)
            assertEquals("SIG", resp!!.signature_pubkey)
            assertEquals("X25", resp.x25519_pubkey)
        }
        assertEquals(HttpMethod.Get, captured[0].method)
        // Path is base64url-no-pad, no `+` / `/` / `=`.
        val url = captured[0].url
        assertTrue(url.contains("/discovery/identity/"))
        val slug = url.substringAfter("/discovery/identity/")
        assertEquals(-1, slug.indexOf('='))
        assertEquals(-1, slug.indexOf('+'))
        assertEquals(-1, slug.indexOf('/'))
    }

    @Test
    fun `identityFetch folds 404 into null`() = runTest {
        client(
            responses = listOf(HttpStatusCode.NotFound to ""),
        ).use { client ->
            val resp = client.identityFetch(ByteArray(16))
            assertNull(resp)
        }
    }

    @Test
    fun `identityFetch rejects wrong-length accountId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.identityFetch(ByteArray(8))
                }
            }
        }
    }

    // ===================================================================
    // Transport failure
    // ===================================================================

    @Test
    fun `network failure throws DiscoveryNetworkError`() {
        val engine = MockEngine { throw IOException("simulated DNS failure") }
        val err = assertThrows(DiscoveryNetworkError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.powChallenge()
                }
            }
        }
        assertEquals("/pow/challenge", err.endpoint)
        assertTrue(err.message!!.contains("simulated DNS failure"))
    }

    // ===================================================================
    // High-level arg-validation (no FFI reached)
    // ===================================================================

    @Test
    fun `uploadIdentityBundle rejects wrong-length accountId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.uploadIdentityBundle(
                        accountId = ByteArray(8),
                        signaturePubkey = ByteArray(32),
                        x25519Pubkey = ByteArray(32),
                    )
                }
            }
        }
    }

    @Test
    fun `uploadIdentityBundle rejects wrong-length pubkey before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.uploadIdentityBundle(
                        accountId = ByteArray(16),
                        signaturePubkey = ByteArray(31),
                        x25519Pubkey = ByteArray(32),
                    )
                }
            }
        }
    }

    @Test
    fun `registerUsername rejects wrong-length accountId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.registerUsername(ByteArray(8), "alice")
                }
            }
        }
    }

    // ===================================================================
    // /discovery/keypackage — 1.15f1
    // ===================================================================

    @Test
    fun `keypackageUpload sends snake_case keypackage field`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(HttpStatusCode.OK to """{"ok":true}"""),
            captured = captured,
        ).use { client ->
            val resp = client.keypackageUpload(
                KeyPackageUploadRequest(
                    pow = me.lakko.app.auth.PowProofPayload("S", "N", "X"),
                    account_id = "ACC_B64",
                    device_id = "DEV_B64",
                    keypackage = "KP_B64",
                ),
            )
            assertTrue(resp.ok)
        }
        val body = captured[0].body!!
        assertEquals("ACC_B64", body["account_id"]!!.jsonPrimitive.content)
        assertEquals("DEV_B64", body["device_id"]!!.jsonPrimitive.content)
        assertEquals("KP_B64", body["keypackage"]!!.jsonPrimitive.content)
    }

    @Test
    fun `keypackageFetch returns the row on 200`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(
                HttpStatusCode.OK to
                    """{"keypackages":[{"device_id":"DEV_B64","keypackage":"KP_BYTES_B64"}]}""",
            ),
            captured = captured,
        ).use { client ->
            val resp = client.keypackageFetch(ByteArray(16))
            assertNotNull(resp)
            assertEquals(1, resp!!.keypackages.size)
            assertEquals("DEV_B64", resp.keypackages[0].device_id)
            assertEquals("KP_BYTES_B64", resp.keypackages[0].keypackage)
        }
        assertEquals(HttpMethod.Get, captured[0].method)
        // Path is base64url-no-pad — same shape as identityFetch.
        val url = captured[0].url
        assertTrue(url.contains("/discovery/keypackage/"))
        val slug = url
            .substringAfter("/discovery/keypackage/")
        assertEquals(-1, slug.indexOf('='))
        assertEquals(-1, slug.indexOf('+'))
        assertEquals(-1, slug.indexOf('/'))
    }

    @Test
    fun `keypackageFetch folds 404 into null`() = runTest {
        client(
            responses = listOf(HttpStatusCode.NotFound to ""),
        ).use { client ->
            val resp = client.keypackageFetch(ByteArray(16))
            assertNull(resp)
        }
    }

    @Test
    fun `keypackageFetch rejects wrong-length accountId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.keypackageFetch(ByteArray(8))
                }
            }
        }
    }

    @Test
    fun `uploadKeyPackage rejects wrong-length accountId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.uploadKeyPackage(
                        accountId = ByteArray(8),
                        keyPackageBytes = ByteArray(32),
                        deviceId = ByteArray(32),
                    )
                }
            }
        }
    }

    @Test
    fun `uploadKeyPackage rejects empty bytes before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.uploadKeyPackage(
                        accountId = ByteArray(16),
                        keyPackageBytes = ByteArray(0),
                        deviceId = ByteArray(32),
                    )
                }
            }
        }
    }

    @Test
    fun `uploadKeyPackage rejects oversized bytes before network`() {
        val engine = MockEngine { error("must not reach the network") }
        val err = assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.uploadKeyPackage(
                        accountId = ByteArray(16),
                        keyPackageBytes = ByteArray(2049), // 1 over the 2048 ceiling
                        deviceId = ByteArray(32),
                    )
                }
            }
        }
        assertTrue(err.message!!.contains("2048"))
    }

    @Test
    fun `uploadKeyPackage rejects wrong-length deviceId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.uploadKeyPackage(
                        accountId = ByteArray(16),
                        keyPackageBytes = ByteArray(32),
                        deviceId = ByteArray(16),
                    )
                }
            }
        }
    }

    @Test
    fun `deleteKeyPackage sends DELETE to the device path`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(HttpStatusCode.NoContent to ""),
            captured = captured,
        ).use { client ->
            client.deleteKeyPackage(ByteArray(32))
        }
        assertEquals(HttpMethod.Delete, captured[0].method)
        assertTrue(captured[0].url.contains("/discovery/keypackage/device/"))
    }

    // ===================================================================
    // /discovery/invite — 1.15e9
    // ===================================================================

    @Test
    fun `inviteUpload sends capability + parses blob_id`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(HttpStatusCode.OK to """{"blob_id":"BLOB_B64URL"}"""),
            captured = captured,
        ).use { client ->
            val resp = client.inviteUpload(
                InviteUploadRequest(
                    pow = me.lakko.app.auth.PowProofPayload("S", "N", "X"),
                    capability = "CAP_B64",
                ),
            )
            assertEquals("BLOB_B64URL", resp.blob_id)
        }
        val body = captured[0].body!!
        assertEquals("CAP_B64", body["capability"]!!.jsonPrimitive.content)
        assertTrue(captured[0].url.endsWith("/discovery/invite"))
    }

    @Test
    fun `inviteFetch decodes 200 + folds 404 into null`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(
                HttpStatusCode.OK to """{"capability":"CAP_B64"}""",
                HttpStatusCode.NotFound to "",
            ),
            captured = captured,
        ).use { client ->
            val ok = client.inviteFetch(ByteArray(6))
            assertNotNull(ok)
            assertEquals("CAP_B64", ok!!.capability)

            val gone = client.inviteFetch(ByteArray(6))
            assertNull(gone)
        }
        // Both calls used base64url-no-pad slugs.
        for (req in captured) {
            assertEquals(HttpMethod.Get, req.method)
            val slug = req.url.substringAfter("/discovery/invite/")
            assertEquals(-1, slug.indexOf('='))
            assertEquals(-1, slug.indexOf('+'))
            assertEquals(-1, slug.indexOf('/'))
        }
    }

    @Test
    fun `inviteFetch rejects wrong-length blobId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.inviteFetch(ByteArray(16))
                }
            }
        }
    }

    @Test
    fun `inviteConsume serializes joiner_tag + posts to slug path`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(
                HttpStatusCode.OK to
                    """{"used_count":3,"max_uses":10,"already_consumed":false}""",
            ),
            captured = captured,
        ).use { client ->
            val resp = client.inviteConsume(
                blobIdSlug = "BLOB_SLUG",
                req = InviteConsumeRequest(
                    pow = me.lakko.app.auth.PowProofPayload("S", "N", "X"),
                    joiner_tag = "TAG_B64",
                ),
            )
            assertEquals(3, resp.used_count)
            assertEquals(10L, resp.max_uses)
            assertEquals(false, resp.already_consumed)
        }
        val body = captured[0].body!!
        assertEquals("TAG_B64", body["joiner_tag"]!!.jsonPrimitive.content)
        assertTrue(captured[0].url.endsWith("/discovery/invite/BLOB_SLUG/consume"))
    }

    @Test
    fun `inviteRevoke 204 returns + sends snake_case body`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        client(
            responses = listOf(HttpStatusCode.NoContent to ""),
            captured = captured,
        ).use { client ->
            client.inviteRevoke(
                blobIdSlug = "BLOB_SLUG",
                req = InviteRevokeRequest(
                    signed_at_ms = 1_700_000_000_000L,
                    signature = "SIG_B64",
                ),
            )
        }
        val req = captured[0]
        assertEquals(HttpMethod.Delete, req.method)
        assertTrue(req.url.endsWith("/discovery/invite/BLOB_SLUG"))
        val body = req.body!!
        assertEquals(1_700_000_000_000L, body["signed_at_ms"]!!.jsonPrimitive.content.toLong())
        assertEquals("SIG_B64", body["signature"]!!.jsonPrimitive.content)
    }

    @Test
    fun `inviteRevoke 404 throws DiscoveryHttpError`() {
        val err = assertThrows(DiscoveryHttpError::class.java) {
            runTest {
                client(
                    responses = listOf(HttpStatusCode.NotFound to ""),
                ).use { client ->
                    client.inviteRevoke(
                        blobIdSlug = "SLUG",
                        req = InviteRevokeRequest(signed_at_ms = 0L, signature = "X"),
                    )
                }
            }
        }
        assertEquals(404, err.status)
    }

    // ===================================================================
    // High-level invite arg-validation gates (no FFI reached)
    // ===================================================================

    @Test
    fun `uploadInvite rejects empty bytes before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.uploadInvite(ByteArray(0))
                }
            }
        }
    }

    @Test
    fun `uploadInvite rejects oversized bytes before network`() {
        val engine = MockEngine { error("must not reach the network") }
        val err = assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.uploadInvite(ByteArray(2049))
                }
            }
        }
        assertTrue(err.message!!.contains("2048"))
    }

    @Test
    fun `fetchInvite rejects wrong-length blobId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.fetchInvite(ByteArray(16))
                }
            }
        }
    }

    @Test
    fun `consumeInvite rejects wrong-length blobId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.consumeInvite(
                        blobId = ByteArray(16),
                        capabilityBytes = byteArrayOf(1, 2, 3),
                        joinerAccountId = ByteArray(16),
                    )
                }
            }
        }
    }

    @Test
    fun `consumeInvite rejects wrong-length joinerAccountId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.consumeInvite(
                        blobId = ByteArray(6),
                        capabilityBytes = byteArrayOf(1),
                        joinerAccountId = ByteArray(8),
                    )
                }
            }
        }
    }

    @Test
    fun `revokeInvite rejects wrong-length blobId before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.revokeInvite(
                        blobId = ByteArray(16),
                        signingSeed = ByteArray(32),
                    )
                }
            }
        }
    }

    @Test
    fun `revokeInvite rejects wrong-length signingSeed before network`() {
        val engine = MockEngine { error("must not reach the network") }
        assertThrows(DiscoveryClientArgError::class.java) {
            runTest {
                DiscoveryClient(baseUrl = "http://test.invalid", engine = engine, inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS }).use { client ->
                    client.revokeInvite(
                        blobId = ByteArray(6),
                        signingSeed = ByteArray(16),
                    )
                }
            }
        }
    }

    // ===================================================================
    // Short links (ADR-0028 amendment of 2026-09-27)
    // ===================================================================

    @Test
    fun `inviteFetch puts the 8-character id in the path`() = runTest {
        val captured = mutableListOf<CapturedRequest>()
        val id = ByteArray(6) { (it + 1).toByte() }
        client(
            responses = listOf(HttpStatusCode.OK to """{"capability":"CAP_B64"}"""),
            captured = captured,
        ).use { client -> client.inviteFetch(id) }
        val slug = captured[0].url.substringAfter("/discovery/invite/")
        assertEquals(me.lakko.app.auth.base64UrlEncodeNoPad(id), slug)
        assertEquals(8, slug.length)
    }

    @Test
    fun `the old 32-byte id is refused before network on every invite route`() {
        val engine = MockEngine { error("must not reach the network") }
        val old = ByteArray(32)
        for (call in listOf<suspend (DiscoveryClient) -> Unit>(
            { it.fetchInvite(old) },
            { it.fetchCallInvite(old) },
            { it.fetchGroupInvite(old) },
            { it.consumeGroupInvite(old, byteArrayOf(1), ByteArray(16)) },
            { it.revokeGroupInvite(old, ByteArray(32)) },
            { it.revokeCallInvite(old, ByteArray(32)) },
        )) {
            assertThrows(DiscoveryClientArgError::class.java) {
                runTest {
                    DiscoveryClient(
                        baseUrl = "http://test.invalid",
                        engine = engine,
                        inviteLinkLengths = { TEST_INVITE_LINK_LENGTHS },
                    ).use { call(it) }
                }
            }
        }
    }

    @Test
    fun `a 429 on group-invite fetch surfaces as rate limited, never as not found`() {
        val err = assertThrows(DiscoveryHttpError::class.java) {
            runTest {
                client(responses = listOf(HttpStatusCode.TooManyRequests to "")).use { client ->
                    client.fetchGroupInvite(ByteArray(6))
                }
            }
        }
        assertEquals(429, err.status)
        assertTrue(isDiscoveryRateLimited(err))
    }

    @Test
    fun `a 429 on call-invite and contact-invite fetch is rate limited too`() {
        for (call in listOf<suspend (DiscoveryClient) -> Unit>(
            { it.fetchCallInvite(ByteArray(6)) },
            { it.fetchInvite(ByteArray(6)) },
        )) {
            val err = assertThrows(DiscoveryHttpError::class.java) {
                runTest {
                    client(responses = listOf(HttpStatusCode.TooManyRequests to "")).use { call(it) }
                }
            }
            assertTrue(isDiscoveryRateLimited(err))
        }
    }
}
