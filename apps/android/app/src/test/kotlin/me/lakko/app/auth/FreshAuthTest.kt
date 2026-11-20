// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import me.lakko.app.webauthn.AuthFinishResponse
import me.lakko.app.webauthn.RegisterFinishRequest
import me.lakko.app.webauthn.RegisterStartRequest
import me.lakko.app.webauthn.WebauthnHttpError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** UX section 17: the fresh-auth proof - the in-memory holder, the wire
 *  fields, the one refusal, and the password-change gate. */
class FreshAuthTest {

    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    // ---------------------------------------------------------------- holder

    @Test
    fun `a proof is live for its account until its expiry`() {
        val holder = FreshAuthHolder()
        holder.put("acct-a", "GRANT", expiresAtMs = 10_000L, nowMs = 1_000L)
        assertEquals("GRANT", holder.live("acct-a", 1_000L)?.grant)
        assertEquals("GRANT", holder.live("acct-a", 9_999L)?.grant)
        assertTrue(holder.hasLive("acct-a", 9_999L))
    }

    @Test
    fun `one proof serves any number of actions within its lifetime`() {
        val holder = FreshAuthHolder()
        holder.put("acct-a", "GRANT", expiresAtMs = 10_000L, nowMs = 1_000L)
        // Reading it (to send on a gated route) does not spend it.
        repeat(5) { assertEquals("GRANT", holder.live("acct-a", 2_000L)?.grant) }
    }

    @Test
    fun `an expired proof is not live and is dropped by the read that finds it so`() {
        val holder = FreshAuthHolder()
        holder.put("acct-a", "GRANT", expiresAtMs = 10_000L, nowMs = 1_000L)
        // Unusable AT the expiry, not only after it.
        assertNull(holder.live("acct-a", 10_000L))
        // Dropped: an earlier clock does not bring it back.
        assertNull(holder.live("acct-a", 1_000L))
    }

    @Test
    fun `a proof is bound to the account it was issued for`() {
        val holder = FreshAuthHolder()
        holder.put("acct-a", "GRANT", expiresAtMs = 10_000L, nowMs = 1_000L)
        assertNull(holder.live("acct-b", 1_000L))
        assertFalse(holder.hasLive("acct-b", 1_000L))
        // Asking for another account does not spend the holder's proof.
        assertEquals("GRANT", holder.live("acct-a", 1_000L)?.grant)
    }

    @Test
    fun `clear drops the proof - sign-out, wipe, a key reset, a server refusal`() {
        val holder = FreshAuthHolder()
        holder.put("acct-a", "GRANT", expiresAtMs = 10_000L, nowMs = 1_000L)
        holder.clear()
        assertNull(holder.live("acct-a", 1_000L))
    }

    @Test
    fun `a password or passkey sign-in that brought no proof leaves none behind`() {
        val holder = FreshAuthHolder()
        holder.put("acct-a", "GRANT", expiresAtMs = 10_000L, nowMs = 1_000L)
        holder.put("acct-a", null, null, nowMs = 2_000L)
        assertNull(holder.live("acct-a", 2_000L))
        holder.put("acct-a", "", 10_000L, nowMs = 2_000L)
        assertNull(holder.live("acct-a", 2_000L))
        holder.put("acct-a", "LATE", expiresAtMs = 2_000L, nowMs = 2_000L)
        assertNull(holder.live("acct-a", 2_000L))
    }

    @Test
    fun `the newest sign-in's proof replaces the older one - an account change drops it`() {
        val holder = FreshAuthHolder()
        holder.put("acct-a", "OLD", expiresAtMs = 10_000L, nowMs = 1_000L)
        holder.put("acct-b", "NEW", expiresAtMs = 20_000L, nowMs = 2_000L)
        assertNull(holder.live("acct-a", 2_000L))
        assertEquals("NEW", holder.live("acct-b", 2_000L)?.grant)
    }

    @Test
    fun `a sign-in that mints no proof keeps the same account's and drops another's`() {
        val holder = FreshAuthHolder()
        holder.put("acct-a", "GRANT", expiresAtMs = 10_000L, nowMs = 1_000L)
        // An authenticator code for the same account does not revoke it.
        holder.onSignInWithoutProof("acct-a")
        assertEquals("GRANT", holder.live("acct-a", 2_000L)?.grant)
        // The same route for another account: the held proof goes.
        holder.onSignInWithoutProof("acct-b")
        assertNull(holder.live("acct-a", 2_000L))
        assertNull(holder.live("acct-b", 2_000L))
    }

    @Test
    fun `the grant never reaches a log line through toString`() {
        assertFalse(FreshAuthProof("acct-a", "SECRET-GRANT", 10_000L).toString().contains("SECRET-GRANT"))
        val pair = SessionPair("v2.a.1.2.t", 1L, "r1.a.1.2.t", 2L, 1L)
        val login = LoginResult(
            accountId = ByteArray(16),
            sessionKey = ByteArray(4),
            exportKey = ByteArray(4),
            session = pair,
            freshGrant = "SECRET-GRANT",
            freshGrantExpiresAtMs = 10_000L,
        )
        assertFalse(login.toString().contains("SECRET-GRANT"))
        val signup = SignupResult(
            accountId = ByteArray(16),
            exportKey = ByteArray(4),
            session = pair,
            freshGrant = "SECRET-GRANT",
            freshGrantExpiresAtMs = 10_000L,
        )
        assertFalse(signup.toString().contains("SECRET-GRANT"))
        assertFalse(RegisterStartRequest("alice", "phone", "SECRET-GRANT").toString().contains("SECRET-GRANT"))
    }

    // ------------------------------------------------------------------ wire

    private val pairJson = """"session_token":"v2.A.1.2.T","session_expires_at_ms":2,""" +
        """"refresh_token":"r1.A.1.3.T","refresh_expires_at_ms":3,"key_generation":1"""

    @Test
    fun `the password login's finish carries the proof and every other finish parses without one`() {
        val base = """"ok":true,"account_id":"ACC",$pairJson"""
        val withGrant = json.decodeFromString(
            LoginFinishResponse.serializer(),
            """{$base,"fresh_grant":"g1.OPAQUE","fresh_grant_expires_at_ms":600000}""",
        )
        assertEquals("g1.OPAQUE", withGrant.fresh_grant)
        assertEquals(600_000L, withGrant.fresh_grant_expires_at_ms)
        val without = json.decodeFromString(LoginFinishResponse.serializer(), "{$base}")
        assertNull(without.fresh_grant)
        assertNull(without.fresh_grant_expires_at_ms)
    }

    @Test
    fun `the old field names are gone - a reset_grant is not read as a proof`() {
        val old = json.decodeFromString(
            LoginFinishResponse.serializer(),
            """{"ok":true,"account_id":"ACC",$pairJson,"reset_grant":"g","reset_grant_expires_at_ms":600000}""",
        )
        assertNull(old.fresh_grant)
        assertNull(old.fresh_grant_expires_at_ms)
    }

    @Test
    fun `the signup finish and the passkey sign-in finish carry the proof`() {
        val signup = json.decodeFromString(
            SignupFinishResponse.serializer(),
            """{"ok":true,$pairJson,"fresh_grant":"g1.S","fresh_grant_expires_at_ms":7}""",
        )
        assertEquals("g1.S", signup.fresh_grant)
        assertEquals(7L, signup.fresh_grant_expires_at_ms)
        assertNull(json.decodeFromString(SignupFinishResponse.serializer(), """{"ok":true,$pairJson}""").fresh_grant)

        val passkey = json.decodeFromString(
            AuthFinishResponse.serializer(),
            """{"account_id_b64":"ACC",$pairJson,"fresh_grant":"g1.P","fresh_grant_expires_at_ms":9}""",
        )
        assertEquals("g1.P", passkey.freshGrant)
        assertEquals(9L, passkey.freshGrantExpiresAtMs)
        assertNull(
            json.decodeFromString(AuthFinishResponse.serializer(), """{"account_id_b64":"ACC",$pairJson}""").freshGrant,
        )
    }

    @Test
    fun `both reset calls carry the proof in the body`() {
        val start = json.parseToJsonElement(
            json.encodeToString(KeyResetStartRequest.serializer(), KeyResetStartRequest("GRANT", "REQ")),
        ).jsonObject
        assertEquals("GRANT", start["fresh_grant"]?.jsonPrimitive?.content)
        assertEquals("REQ", start["recovery_request"]?.jsonPrimitive?.content)
        assertNull(start["reset_grant"])
        val finish = json.parseToJsonElement(
            json.encodeToString(
                KeyResetFinishRequest.serializer(),
                KeyResetFinishRequest("GRANT", "UPLOAD", "PUBKEY", "PROOF"),
            ),
        ).jsonObject
        assertEquals("GRANT", finish["fresh_grant"]?.jsonPrimitive?.content)
        assertEquals("UPLOAD", finish["recovery_upload"]?.jsonPrimitive?.content)
        assertEquals("PUBKEY", finish["new_signature_pubkey_b64"]?.jsonPrimitive?.content)
        assertEquals("PROOF", finish["identity_proof_b64"]?.jsonPrimitive?.content)
    }

    @Test
    fun `the add-device calls carry the proof in the body`() {
        val authorize = json.parseToJsonElement(
            json.encodeToString(DeviceAuthorizeStartRequest.serializer(), DeviceAuthorizeStartRequest("GRANT")),
        ).jsonObject
        assertEquals("GRANT", authorize["fresh_grant"]?.jsonPrimitive?.content)
        val pake = json.parseToJsonElement(
            json.encodeToString(DeviceRequestAnswerBody.serializer(), DeviceRequestAnswerBody("PAKE", "GRANT")),
        ).jsonObject
        assertEquals("PAKE", pake["pake_b_b64"]?.jsonPrimitive?.content)
        assertEquals("GRANT", pake["fresh_grant"]?.jsonPrimitive?.content)
        val result = json.parseToJsonElement(
            json.encodeToString(
                DeviceRequestResultBody.serializer(),
                DeviceRequestResultBody(outcome = "accepted", confirm_b_b64 = "C", fresh_grant = "GRANT"),
            ),
        ).jsonObject
        assertEquals("accepted", result["outcome"]?.jsonPrimitive?.content)
        assertEquals("GRANT", result["fresh_grant"]?.jsonPrimitive?.content)
    }

    @Test
    fun `both passkey registration calls carry the proof in the body`() {
        val start = json.parseToJsonElement(
            json.encodeToString(RegisterStartRequest.serializer(), RegisterStartRequest("alice", "phone", "GRANT")),
        ).jsonObject
        assertEquals("GRANT", start["fresh_grant"]?.jsonPrimitive?.content)
        val finish = json.parseToJsonElement(
            json.encodeToString(
                RegisterFinishRequest.serializer(),
                RegisterFinishRequest("state", "phone", json.parseToJsonElement("{}"), "GRANT"),
            ),
        ).jsonObject
        assertEquals("GRANT", finish["fresh_grant"]?.jsonPrimitive?.content)
        assertEquals("state", finish["state_id"]?.jsonPrimitive?.content)
    }

    // ---------------------------------------------------------- the refusal

    @Test
    fun `only a 403 fresh-auth-required is the proof refusal`() {
        val path = "/account/keys/reset/finish"
        assertTrue(isFreshAuthRequired(AuthHttpError(403, path, "{}", code = "fresh-auth-required")))
        // The old code is not it (flag day).
        assertFalse(isFreshAuthRequired(AuthHttpError(403, path, "{}", code = "password-required")))
        assertFalse(isFreshAuthRequired(AuthHttpError(403, path, "{}", code = "something-else")))
        assertFalse(isFreshAuthRequired(AuthHttpError(403, path, "")))
        assertFalse(isFreshAuthRequired(AuthHttpError(401, path, "{}", code = "fresh-auth-required")))
        assertFalse(isFreshAuthRequired(AuthNetworkError(path, java.io.IOException("down"))))
        // The client-side refusal to send without a proof reads the same.
        assertTrue(isFreshAuthRequired(FreshAuthRequired()))
    }

    @Test
    fun `the passkey client's 403 is told from the action grant's by its body`() {
        // DELETE /account: a passkey account's missing ACTION grant is a 403
        // too, with another body; only this one re-opens the proof block.
        assertTrue(isFreshAuthRequired(WebauthnHttpError(403, "/account", """{"error":"fresh-auth-required"}""")))
        assertTrue(isFreshAuthRequired(WebauthnHttpError(403, "/account", """{ "error" : "fresh-auth-required" }""")))
        assertFalse(isFreshAuthRequired(WebauthnHttpError(403, "/account", """{"error":"action-grant-required"}""")))
        assertFalse(isFreshAuthRequired(WebauthnHttpError(403, "/account", "")))
        assertFalse(isFreshAuthRequired(WebauthnHttpError(403, "/account", "fresh-auth-required")))
        assertFalse(isFreshAuthRequired(WebauthnHttpError(401, "/account", """{"error":"fresh-auth-required"}""")))
    }

    @Test
    fun `an error body's code is read without trusting the body`() {
        assertEquals("too-many-attempts", errorCodeOfBody("""{"error":"too-many-attempts","retry_after_secs":9}"""))
        assertNull(errorCodeOfBody(""))
        assertNull(errorCodeOfBody("<html>502</html>"))
        assertNull(errorCodeOfBody("""{"error":42}"""))
        assertNull(errorCodeOfBody("""{"error":"Has Spaces"}"""))
    }

    @Test
    fun `a 401 or a local OPAQUE failure is a wrong password - a limit and an outage are not`() {
        assertTrue(isWrongPasswordFailure(AuthHttpError(401, "/login/finish", ""), localOpaqueFailure = false))
        assertTrue(isWrongPasswordFailure(RuntimeException("opaque"), localOpaqueFailure = true))
        assertFalse(
            isWrongPasswordFailure(
                AuthHttpError(429, "/login/start", "{}", code = "too-many-attempts", retryAfterSecs = 60),
                localOpaqueFailure = false,
            ),
        )
        assertFalse(isWrongPasswordFailure(AuthHttpError(503, "/login/start", ""), localOpaqueFailure = false))
        assertFalse(
            isWrongPasswordFailure(AuthNetworkError("/login/start", java.io.IOException("down")), localOpaqueFailure = false),
        )
    }

    // ------------------------------------------------------ the sign-in name

    @Test
    fun `the proof signs in by the username this device knows, never a typed one`() {
        assertEquals("alice", proofSignInName("alice", null))
        assertEquals("alice", proofSignInName("alice", "mallory"))
        assertEquals("alice", proofSignInName(" alice ", ""))
    }

    @Test
    fun `with no username on the device the typed name is used`() {
        assertEquals("bob", proofSignInName("", " bob "))
        assertEquals("bob", proofSignInName("  ", "bob"))
    }

    @Test
    fun `neither a known nor a typed name leaves no name to sign in with`() {
        assertEquals("", proofSignInName("", null))
        assertEquals("", proofSignInName("", "   "))
    }

    // ---------------------------------------------- the password-change gate

    @Test
    fun `a bare session may not change the password`() {
        assertFalse(passwordChangeAllowed(proofLive = false, wordsCheckedAtMs = null, nowMs = 1_000L))
    }

    @Test
    fun `a live proof opens the password change`() {
        assertTrue(passwordChangeAllowed(proofLive = true, wordsCheckedAtMs = null, nowMs = 1_000L))
    }

    @Test
    fun `the local three-word check opens it for ten minutes and no longer`() {
        val checked = 1_000_000L
        assertTrue(passwordChangeAllowed(false, checked, checked))
        assertTrue(passwordChangeAllowed(false, checked, checked + FRESH_AUTH_WINDOW_MS - 1))
        assertFalse(passwordChangeAllowed(false, checked, checked + FRESH_AUTH_WINDOW_MS))
        assertEquals(600_000L, FRESH_AUTH_WINDOW_MS)
    }

    @Test
    fun `a clock set back never extends the three-word check`() {
        assertFalse(wordsCheckLive(checkedAtMs = 1_000_000L, nowMs = 999_999L))
        assertFalse(wordsCheckLive(checkedAtMs = null, nowMs = 1L))
    }
}
