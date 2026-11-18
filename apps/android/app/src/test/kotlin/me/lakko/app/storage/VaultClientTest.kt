// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.storage

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.engine.mock.toByteArray
import io.ktor.http.HttpStatusCode
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0064 section 1 — the vault head wire as the storage service speaks it:
 * `version (u64 BE) || sealed`, the storage proof over the exact body, 404
 * for a claimed vault with no head, 401 for one nobody claimed, 409 on a
 * version another write took.
 */
class VaultClientTest {

    private val signer = object : VaultRequestSigner {
        val signed = mutableListOf<Triple<String, String, ByteArray>>()
        override val headerName: String = "x-lakko-storage-proof"
        override fun sign(method: String, path: String, body: ByteArray): String {
            signed += Triple(method, path, body)
            return "v1.proof"
        }
    }

    private val id = "ab".repeat(32)

    @Test
    fun `the version prefix is eight bytes big-endian`() {
        val joined = VaultClient.joinVersioned(0x0102030405060708L, byteArrayOf(9, 9))
        assertArrayEquals(byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8, 9, 9), joined)
        val (version, sealed) = VaultClient.splitVersioned(joined)!!
        assertEquals(0x0102030405060708L, version)
        assertArrayEquals(byteArrayOf(9, 9), sealed)
        assertNull(VaultClient.splitVersioned(ByteArray(8)))
        assertNull(VaultClient.splitVersioned(ByteArray(9)))
    }

    @Test
    fun `a head write signs the exact body it sends, on the service path`() = runTest {
        var sentBody: ByteArray? = null
        var sentUrl = ""
        var sentProof: String? = null
        val engine = MockEngine { request ->
            sentUrl = request.url.toString()
            sentProof = request.headers["x-lakko-storage-proof"]
            sentBody = request.body.toByteArray()
            respond("", HttpStatusCode.Created)
        }
        val client = VaultClient(baseUrl = { "https://t.test/native/storage" }, engine = engine)
        val outcome = client.writeHead(id, 3, byteArrayOf(7, 7, 7), signer)
        assertEquals(VaultClient.HeadWrite.WRITTEN, outcome)
        assertEquals("https://t.test/native/storage/vault/$id/head", sentUrl)
        assertEquals("v1.proof", sentProof)
        assertArrayEquals(VaultClient.joinVersioned(3, byteArrayOf(7, 7, 7)), sentBody)
        val (method, path, body) = signer.signed.single()
        assertEquals("PUT", method)
        assertEquals("/vault/$id/head", path)
        assertArrayEquals(sentBody, body)
    }

    @Test
    fun `head answers map to found, missing, refused and conflict`() = runTest {
        val statuses = ArrayDeque(listOf(200, 404, 401, 409))
        val engine = MockEngine { _ ->
            when (val status = statuses.removeFirst()) {
                200 -> respond(VaultClient.joinVersioned(5, byteArrayOf(1, 2)), HttpStatusCode.OK)
                409 -> respond("{\"error\":\"vault-version-conflict\"}", HttpStatusCode.Conflict)
                else -> respond("{}", HttpStatusCode.fromValue(status))
            }
        }
        val client = VaultClient(baseUrl = { "https://t.test" }, engine = engine)
        val found = client.readHead(id, signer)
        assertTrue(found is VaultClient.HeadRead.Found)
        assertEquals(5L, (found as VaultClient.HeadRead.Found).version)
        assertArrayEquals(byteArrayOf(1, 2), found.sealed)
        assertEquals(VaultClient.HeadRead.Missing, client.readHead(id, signer))
        assertEquals(VaultClient.HeadRead.Refused, client.readHead(id, signer))
        assertEquals(VaultClient.HeadWrite.CONFLICT, client.writeHead(id, 6, byteArrayOf(1), signer))
        assertEquals("GET", signer.signed[0].first)
        assertEquals(0, signer.signed[0].third.size)
    }

    @Test
    fun `a vault delete is a DELETE signed over an empty body`() = runTest {
        var method = ""
        var url = ""
        var proof: String? = null
        val statuses = ArrayDeque(listOf(204, 401, 500))
        val engine = MockEngine { request ->
            method = request.method.value
            url = request.url.toString()
            proof = request.headers["x-lakko-storage-proof"]
            respond("{}", HttpStatusCode.fromValue(statuses.removeFirst()))
        }
        val client = VaultClient(baseUrl = { "https://t.test/native/storage" }, engine = engine)
        assertTrue(client.deleteVault(id, signer))
        assertEquals("DELETE", method)
        assertEquals("https://t.test/native/storage/vault/$id", url)
        assertEquals("v1.proof", proof)
        val (signedMethod, path, body) = signer.signed.single()
        assertEquals("DELETE", signedMethod)
        assertEquals("/vault/$id", path)
        assertEquals(0, body.size)
        // Never claimed: nothing to delete, not an error.
        assertEquals(false, client.deleteVault(id, signer))
        try {
            client.deleteVault(id, signer)
            org.junit.Assert.fail("a 500 must surface")
        } catch (_: VaultHttpError) {
        }
    }

    @Test
    fun `the claim body names the write key in base64url`() {
        val key = ByteArray(32) { 0xfb.toByte() }
        assertEquals(
            "{\"write_pk\":\"${java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(key)}\"}",
            VaultClient.claimBody(key),
        )
        assertTrue(!VaultClient.claimBody(key).contains("="))
    }
}
