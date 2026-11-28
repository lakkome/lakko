// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The PRF extension over the WebAuthn JSON the Credential Manager carries:
 * how it is requested, how its output is read, and that the output never
 * reaches what is posted to the server. Pure JSON, so it runs on the host.
 */
class PasskeyPrfTest {

    private fun json(text: String): JsonElement = Json.parseToJsonElement(text)

    private val b64url = java.util.Base64.getUrlEncoder().withoutPadding()

    // ---- the salt ----------------------------------------------------------

    @Test
    fun `the salt is the fixed string every client evaluates`() {
        assertArrayEquals("lakko/passkey-vault/prf/v1".toByteArray(Charsets.UTF_8), PASSKEY_PRF_SALT)
        assertEquals(32, PASSKEY_PRF_OUTPUT_LENGTH)
    }

    @Test
    fun `the reset seal's salt is the SHA-256 of its label, as the core computes it`() {
        // ADR-0101 section 10 (`lakko-attest::reset_seal::passkey_prf_salt`).
        // Every client must evaluate the same salt, or a passkey that keeps
        // the chats on one of them would not on another.
        assertEquals(
            "556394c21f7893b053ac2e2fb837eaa3f781e39bddfd9d30f9710786227360fc",
            RESET_SEAL_PRF_SALT.joinToString("") { "%02x".format(it) },
        )
        assertFalse("distinct from the passkey vault's", RESET_SEAL_PRF_SALT.contentEquals(PASSKEY_PRF_SALT))
    }

    @Test
    fun `a second salt is asked in the same gesture, as eval second`() {
        val eval = withPrfExtension(json("""{"challenge":"abc"}"""), secondSalt = RESET_SEAL_PRF_SALT)
            .jsonObject["extensions"]!!.jsonObject["prf"]!!.jsonObject["eval"]!!.jsonObject
        assertEquals(b64url.encodeToString(PASSKEY_PRF_SALT), eval["first"]!!.jsonPrimitive.content)
        assertEquals(b64url.encodeToString(RESET_SEAL_PRF_SALT), eval["second"]!!.jsonPrimitive.content)
        // One output asked: no `second` key at all.
        val one = withPrfExtension(json("""{"challenge":"abc"}"""))
            .jsonObject["extensions"]!!.jsonObject["prf"]!!.jsonObject["eval"]!!.jsonObject
        assertEquals(setOf("first"), one.keys)
        // The reset's own ceremony evaluates the seal salt as its first.
        val reset = withPrfExtension(json("""{"challenge":"abc"}"""), salt = RESET_SEAL_PRF_SALT)
            .jsonObject["extensions"]!!.jsonObject["prf"]!!.jsonObject["eval"]!!.jsonObject
        assertEquals(b64url.encodeToString(RESET_SEAL_PRF_SALT), reset["first"]!!.jsonPrimitive.content)
    }

    @Test
    fun `the second output is read beside the first, and a passkey that gives none keeps no chats`() {
        val first = ByteArray(32) { 1 }
        val second = ByteArray(32) { 2 }
        val both = prfResultOf(
            response("""{"results":{"first":"${b64url.encodeToString(first)}","second":"${b64url.encodeToString(second)}"}}"""),
        )
        assertArrayEquals(first, both.output)
        assertArrayEquals(second, both.second)
        // The provider answered only the first (or, below API 34, nothing):
        // the passkey signs in and confirms a reset, and seals none.
        assertNull(prfResultOf(response("""{"results":{"first":"${b64url.encodeToString(first)}"}}""")).second)
        assertNull(prfResultOf(response(null)).second)
        // Another length is no output.
        assertNull(
            prfResultOf(
                response("""{"results":{"first":"${b64url.encodeToString(first)}","second":"${b64url.encodeToString(ByteArray(16))}"}}"""),
            ).second,
        )
    }

    // ---- request ---------------------------------------------------------------
    // ---- request ---------------------------------------------------------------

    @Test
    fun `the request gains prf eval first as base64url and keeps the rest`() {
        val options = json(
            """{"challenge":"abc","rpId":"lakko.me","extensions":{"uvm":true},"userVerification":"preferred"}""",
        )
        val out = withPrfExtension(options).jsonObject
        assertEquals("abc", out["challenge"]!!.jsonPrimitive.content)
        assertEquals("preferred", out["userVerification"]!!.jsonPrimitive.content)
        val extensions = out["extensions"]!!.jsonObject
        assertEquals("true", extensions["uvm"]!!.jsonPrimitive.content)
        val first = extensions["prf"]!!.jsonObject["eval"]!!.jsonObject["first"]!!.jsonPrimitive
        assertTrue(first.isString)
        assertEquals(b64url.encodeToString(PASSKEY_PRF_SALT), first.content)
        assertFalse("base64url, no padding", first.content.contains('='))
    }

    @Test
    fun `a request without extensions gets an extensions object`() {
        val out = withPrfExtension(json("""{"challenge":"abc"}""")).jsonObject
        assertTrue("prf" in out["extensions"]!!.jsonObject)
    }

    @Test
    fun `one credential can be the only one offered`() {
        val out = onlyCredential(
            json("""{"challenge":"abc","allowCredentials":[{"type":"public-key","id":"old"}]}"""),
            "Y3JlZC1pZA",
        ).jsonObject
        val allow = out["allowCredentials"]!!.jsonArray
        assertEquals(1, allow.size)
        assertEquals("Y3JlZC1pZA", allow[0].jsonObject["id"]!!.jsonPrimitive.content)
        assertEquals("public-key", allow[0].jsonObject["type"]!!.jsonPrimitive.content)
    }

    // ---- response -----------------------------------------------------------------

    private fun response(prf: String?): JsonElement = json(
        """{"id":"Y3JlZC1pZA","rawId":"Y3JlZC1pZA","type":"public-key","response":{"signature":"sig"},""" +
            """"clientExtensionResults":{"credProps":{"rk":true}${if (prf == null) "" else ""","prf":$prf"""}}}""",
    )

    @Test
    fun `a thirty two byte first result is the prf output`() {
        val secret = ByteArray(32) { (it + 1).toByte() }
        val result = prfResultOf(response("""{"results":{"first":"${b64url.encodeToString(secret)}"}}"""))
        assertArrayEquals(secret, result.output)
        assertNull(result.enabled)
    }

    @Test
    fun `a padded result decodes too`() {
        val secret = ByteArray(32) { 9 }
        val padded = java.util.Base64.getUrlEncoder().encodeToString(secret)
        assertTrue(padded.endsWith("="))
        assertArrayEquals(secret, prfResultOf(response("""{"results":{"first":"$padded"}}""")).output)
    }

    @Test
    fun `no prf entry, another length or another shape means the passkey signs in only`() {
        assertNull(prfResultOf(response(null)).output)
        assertNull(prfResultOf(response("""{"enabled":true}""")).output)
        assertNull(prfResultOf(response("""{"results":{}}""")).output)
        val short = b64url.encodeToString(ByteArray(31))
        assertNull(prfResultOf(response("""{"results":{"first":"$short"}}""")).output)
        val long = b64url.encodeToString(ByteArray(64))
        assertNull(prfResultOf(response("""{"results":{"first":"$long"}}""")).output)
        assertNull(prfResultOf(response("""{"results":{"first":"not base64url !!"}}""")).output)
        assertNull(prfResultOf(response("""{"results":{"first":{"0":1}}}""")).output)
        assertNull(prfResultOf(response(""""yes"""")).output)
        assertNull(prfResultOf(json("[]")).output)
        assertNull(prfResultOf(json("""{"clientExtensionResults":7}""")).output)
    }

    @Test
    fun `a registration reports whether the new credential supports prf`() {
        assertEquals(true, prfResultOf(response("""{"enabled":true}""")).enabled)
        assertEquals(false, prfResultOf(response("""{"enabled":false}""")).enabled)
        assertNull(prfResultOf(response("""{"enabled":"true"}""")).enabled)
        assertNull(prfResultOf(response(null)).enabled)
    }

    @Test
    fun `the prf entry is removed from what the server receives and nothing else is`() {
        val secret = b64url.encodeToString(ByteArray(32) { 5 })
        val full = response("""{"enabled":true,"results":{"first":"$secret"}}""")
        val safe = serverSafeCredential(full)
        val text = Json.encodeToString(JsonElement.serializer(), safe)
        assertFalse("the secret must not be in the posted JSON", text.contains(secret))
        val results = safe.jsonObject["clientExtensionResults"]!!.jsonObject
        assertFalse("prf" in results)
        assertTrue("credProps" in results)
        assertEquals("sig", safe.jsonObject["response"]!!.jsonObject["signature"]!!.jsonPrimitive.content)
        assertEquals("Y3JlZC1pZA", safe.jsonObject["rawId"]!!.jsonPrimitive.content)
    }

    @Test
    fun `a response without a prf entry is posted unchanged`() {
        val plain = response(null)
        assertEquals(plain, serverSafeCredential(plain))
        val bare = json("""{"id":"x"}""")
        assertEquals(bare, serverSafeCredential(bare))
        assertEquals(JsonObject(emptyMap()), serverSafeCredential(JsonObject(emptyMap())))
    }

    @Test
    fun `the credential id is the raw id of the response`() {
        assertArrayEquals("cred-id".toByteArray(), credentialIdOf(response(null)))
        assertArrayEquals("cred-id".toByteArray(), credentialIdOf(json("""{"id":"Y3JlZC1pZA"}""")))
        assertNull(credentialIdOf(json("""{"type":"public-key"}""")))
        assertNull(credentialIdOf(json("[]")))
    }

    // ---- the rules around a passkey that restores keys ----------------------------

    @Test
    fun `a passkey restores keys exactly when the server lists its credential id`() {
        assertEquals(PasskeyKeysLabel.RestoresKeys, passkeyKeysLabel("a", listOf("a", "b")))
        assertEquals(PasskeyKeysLabel.SignInOnly, passkeyKeysLabel("c", listOf("a", "b")))
        assertEquals(PasskeyKeysLabel.SignInOnly, passkeyKeysLabel("a", emptyList()))
        // Not loaded, or the server keeps no keys: what such a passkey can do.
        assertEquals(PasskeyKeysLabel.SignInOnly, passkeyKeysLabel("a", null))
    }

    @Test
    fun `the keys outcome of a cold passkey sign-in`() {
        fun outcome(prf: Boolean, blob: Boolean, opened: Boolean, matched: Boolean) =
            passkeyKeysOutcome(prfOutput = prf, blobStored = blob, opened = opened, matched = matched)
        assertEquals(PasskeyKeysOutcome.NoKeys, outcome(prf = false, blob = false, opened = false, matched = false))
        assertEquals(PasskeyKeysOutcome.NoKeys, outcome(prf = true, blob = false, opened = false, matched = false))
        assertEquals(PasskeyKeysOutcome.NoKeys, outcome(prf = false, blob = true, opened = false, matched = false))
        assertEquals(PasskeyKeysOutcome.Unusable, outcome(prf = true, blob = true, opened = false, matched = false))
        assertEquals(PasskeyKeysOutcome.Unusable, outcome(prf = true, blob = true, opened = true, matched = false))
        assertEquals(PasskeyKeysOutcome.Restored, outcome(prf = true, blob = true, opened = true, matched = true))
    }

    @Test
    fun `the restore prompt hides the passkey row only when no passkey carries keys`() {
        assertTrue(restoreOffersPasskey(null))
        assertTrue(restoreOffersPasskey(2))
        assertFalse(restoreOffersPasskey(0))
    }
}
