// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0065 (amended 2026-10-04): the password-fresh reset grant - the
 *  in-memory holder, the wire fields, and the two refusals. */
class ResetGrantTest {

    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    @Test
    fun `a grant is live for its account until its expiry`() {
        val holder = ResetGrantHolder()
        holder.put("acct-a", "GRANT", expiresAtMs = 10_000L, nowMs = 1_000L)
        assertEquals("GRANT", holder.live("acct-a", 1_000L)?.grant)
        assertEquals("GRANT", holder.live("acct-a", 9_999L)?.grant)
        assertTrue(holder.hasLive("acct-a", 9_999L))
    }

    @Test
    fun `an expired grant is not live and is dropped by the read that finds it so`() {
        val holder = ResetGrantHolder()
        holder.put("acct-a", "GRANT", expiresAtMs = 10_000L, nowMs = 1_000L)
        // Unusable AT the expiry, not only after it.
        assertNull(holder.live("acct-a", 10_000L))
        // Dropped: an earlier clock does not bring it back.
        assertNull(holder.live("acct-a", 1_000L))
    }

    @Test
    fun `a grant is bound to the account it was issued for`() {
        val holder = ResetGrantHolder()
        holder.put("acct-a", "GRANT", expiresAtMs = 10_000L, nowMs = 1_000L)
        assertNull(holder.live("acct-b", 1_000L))
        assertFalse(holder.hasLive("acct-b", 1_000L))
        // Asking for another account does not spend the holder's grant.
        assertEquals("GRANT", holder.live("acct-a", 1_000L)?.grant)
    }

    @Test
    fun `a used grant is gone - single use`() {
        val holder = ResetGrantHolder()
        holder.put("acct-a", "GRANT", expiresAtMs = 10_000L, nowMs = 1_000L)
        holder.clear()
        assertNull(holder.live("acct-a", 1_000L))
    }

    @Test
    fun `a sign-in that brought no grant leaves none behind`() {
        val holder = ResetGrantHolder()
        holder.put("acct-a", "GRANT", expiresAtMs = 10_000L, nowMs = 1_000L)
        // The next password login answered without a grant: the older one
        // belongs to an older sign-in and is not kept.
        holder.put("acct-a", null, null, nowMs = 2_000L)
        assertNull(holder.live("acct-a", 2_000L))
        holder.put("acct-a", "", 10_000L, nowMs = 2_000L)
        assertNull(holder.live("acct-a", 2_000L))
        holder.put("acct-a", "LATE", expiresAtMs = 2_000L, nowMs = 2_000L)
        assertNull(holder.live("acct-a", 2_000L))
    }

    @Test
    fun `the newest password login's grant replaces the older one`() {
        val holder = ResetGrantHolder()
        holder.put("acct-a", "OLD", expiresAtMs = 10_000L, nowMs = 1_000L)
        holder.put("acct-b", "NEW", expiresAtMs = 20_000L, nowMs = 2_000L)
        assertNull(holder.live("acct-a", 2_000L))
        assertEquals("NEW", holder.live("acct-b", 2_000L)?.grant)
    }

    @Test
    fun `the grant never reaches a log line through toString`() {
        val grant = ResetGrant("acct-a", "SECRET-GRANT", 10_000L)
        assertFalse(grant.toString().contains("SECRET-GRANT"))
        val result = LoginResult(
            accountId = ByteArray(16),
            sessionKey = ByteArray(4),
            exportKey = ByteArray(4),
            session = SessionPair("v2.a.1.2.t", 1L, "r1.a.1.2.t", 2L, 1L),
            resetGrant = "SECRET-GRANT",
            resetGrantExpiresAtMs = 10_000L,
        )
        assertFalse(result.toString().contains("SECRET-GRANT"))
    }

    @Test
    fun `the password login's finish carries the grant and every other finish parses without one`() {
        val base = """"ok":true,"account_id":"ACC","session_token":"v2.A.1.2.T","session_expires_at_ms":2,""" +
            """"refresh_token":"r1.A.1.3.T","refresh_expires_at_ms":3,"key_generation":1"""
        val withGrant = json.decodeFromString(
            LoginFinishResponse.serializer(),
            """{$base,"reset_grant":"g1.OPAQUE","reset_grant_expires_at_ms":600000}""",
        )
        assertEquals("g1.OPAQUE", withGrant.reset_grant)
        assertEquals(600_000L, withGrant.reset_grant_expires_at_ms)
        val without = json.decodeFromString(LoginFinishResponse.serializer(), "{$base}")
        assertNull(without.reset_grant)
        assertNull(without.reset_grant_expires_at_ms)
    }

    @Test
    fun `both reset calls carry the grant in the body`() {
        val start = json.parseToJsonElement(
            json.encodeToString(KeyResetStartRequest.serializer(), KeyResetStartRequest("GRANT", "REQ")),
        ).jsonObject
        assertEquals("GRANT", start["reset_grant"]?.jsonPrimitive?.content)
        assertEquals("REQ", start["recovery_request"]?.jsonPrimitive?.content)
        val finish = json.parseToJsonElement(
            json.encodeToString(
                KeyResetFinishRequest.serializer(),
                KeyResetFinishRequest("GRANT", "UPLOAD", "PUBKEY"),
            ),
        ).jsonObject
        assertEquals("GRANT", finish["reset_grant"]?.jsonPrimitive?.content)
        assertEquals("UPLOAD", finish["recovery_upload"]?.jsonPrimitive?.content)
        assertEquals("PUBKEY", finish["new_signature_pubkey_b64"]?.jsonPrimitive?.content)
    }

    @Test
    fun `only a 403 password-required is the reset's password refusal`() {
        val path = "/account/keys/reset/start"
        assertTrue(isResetPasswordRequired(AuthHttpError(403, path, "{}", code = "password-required")))
        assertFalse(isResetPasswordRequired(AuthHttpError(403, path, "{}", code = "something-else")))
        assertFalse(isResetPasswordRequired(AuthHttpError(403, path, "")))
        assertFalse(isResetPasswordRequired(AuthHttpError(401, path, "{}", code = "password-required")))
        assertFalse(isResetPasswordRequired(AuthNetworkError(path, java.io.IOException("down"))))
    }

    @Test
    fun `a wrong password is the login's 401 or the local OPAQUE failure, never a limit`() {
        assertTrue(isWrongPasswordFailure(AuthHttpError(401, "/login/finish", ""), localOpaqueFailure = false))
        assertTrue(isWrongPasswordFailure(RuntimeException("opaque"), localOpaqueFailure = true))
        assertFalse(
            isWrongPasswordFailure(
                AuthHttpError(429, "/login/start", "{}", code = "too-many-attempts", retryAfterSecs = 120),
                localOpaqueFailure = false,
            ),
        )
        assertFalse(isWrongPasswordFailure(AuthHttpError(503, "/login/start", ""), localOpaqueFailure = false))
        assertFalse(
            isWrongPasswordFailure(AuthNetworkError("/login/start", java.io.IOException("down")), localOpaqueFailure = false),
        )
    }

    @Test
    fun `the reset sign-in uses the known username and never a typed one over it`() {
        assertEquals("alice", resetSignInName("alice", null))
        assertEquals("alice", resetSignInName("alice", "mallory"))
        assertEquals("alice", resetSignInName(" alice ", ""))
    }

    @Test
    fun `an unknown username falls back to the typed one, trimmed`() {
        assertEquals("bob", resetSignInName("", " bob "))
        assertEquals("bob", resetSignInName("  ", "bob"))
    }

    @Test
    fun `no known and no typed username leaves nothing to sign in with`() {
        assertEquals("", resetSignInName("", null))
        assertEquals("", resetSignInName("", "   "))
    }
}
