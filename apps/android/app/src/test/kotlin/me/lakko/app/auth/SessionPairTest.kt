// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0065 section 3: the pure rules around the session pair. */
class SessionPairTest {

    @Test
    fun `a 401 means a reset, a dead refresh credential, or a plain refusal, by its code`() {
        assertEquals(SessionRefusal.KeysReset, sessionRefusalOf(401, "key-generation-superseded"))
        assertEquals(SessionRefusal.RefreshInvalid, sessionRefusalOf(401, "refresh-invalid"))
        assertEquals(SessionRefusal.Unauthorized, sessionRefusalOf(401, null))
        assertEquals(SessionRefusal.Unauthorized, sessionRefusalOf(401, "proof required"))
        // Only a 401 is a session refusal: a 403, a 429 or a 500 is not.
        assertNull(sessionRefusalOf(403, "key-generation-superseded"))
        assertNull(sessionRefusalOf(429, "too-many-attempts"))
        assertNull(sessionRefusalOf(500, null))
        assertNull(sessionRefusalOf(200, null))
    }

    @Test
    fun `the access token is refreshed when 120 seconds or less remain`() {
        val now = 1_000_000L
        assertEquals(TokenAction.Use, tokenActionAt(now + SESSION_REFRESH_AHEAD_MS + 1, now))
        assertEquals(TokenAction.Refresh, tokenActionAt(now + SESSION_REFRESH_AHEAD_MS, now))
        assertEquals(TokenAction.Refresh, tokenActionAt(now + 1, now))
        assertEquals(TokenAction.Refresh, tokenActionAt(now, now))
        assertEquals(TokenAction.Refresh, tokenActionAt(now - 60_000, now))
        assertEquals(120_000L, SESSION_REFRESH_AHEAD_MS)
    }

    private val dev = "D".repeat(43)

    @Test
    fun `the account segment is read off a v3 access token only`() {
        val token = "v3.AAAAAAAAAAAAAAAAAAAAAA.3.$dev.1690000000000.1700000000000.tag"
        assertEquals("AAAAAAAAAAAAAAAAAAAAAA", accountSegmentOfAccessToken(token))
        // A refresh credential is never an access token.
        assertNull(accountSegmentOfAccessToken("r2.AAAAAAAAAAAAAAAAAAAAAA.3.$dev.1.1700000000000.tag"))
        // The flag day: the v2 / r1 shapes are gone.
        assertNull(accountSegmentOfAccessToken("v2.AAAAAAAAAAAAAAAAAAAAAA.3.1700000000000.tag"))
        assertNull(accountSegmentOfAccessToken("v2.AAAAAAAAAAAAAAAAAAAAAA.3.$dev.1.1700000000000.tag"))
        // A fresh-auth grant (five segments), wrong counts and junk.
        assertNull(accountSegmentOfAccessToken("k1.AAAA.1.2.tag"))
        assertNull(accountSegmentOfAccessToken("v3.AAAA.1.tag"))
        assertNull(accountSegmentOfAccessToken("v3.AAAA.1.$dev.1.2.tag.extra"))
        assertNull(accountSegmentOfAccessToken("v3..1.$dev.1.2.tag"))
        assertNull(accountSegmentOfAccessToken(""))
        assertNull(accountSegmentOfAccessToken("Bearer v3.a.1.$dev.1.2.t"))
    }

    @Test
    fun `the device segment is read off a v3 access token only`() {
        assertEquals(dev, deviceSegmentOfAccessToken("v3.AAAA.3.$dev.1.2.tag"))
        assertEquals(UNBOUND_DEVICE_SEGMENT, deviceSegmentOfAccessToken("v3.AAAA.3.${"A".repeat(43)}.1.2.tag"))
        assertNull(deviceSegmentOfAccessToken("r2.AAAA.3.$dev.1.2.tag"))
        assertNull(deviceSegmentOfAccessToken("v2.AAAA.3.2.tag"))
        assertNull(deviceSegmentOfAccessToken("v3.AAAA.3..1.2.tag"))
        // The all-zero id is 32 zero bytes in base64url without padding.
        assertEquals(base64UrlEncodeNoPad(ByteArray(32)), UNBOUND_DEVICE_SEGMENT)
    }

    // ---- ADR-0066: removed, logged out, reset: three answers, three screens ----

    @Test
    fun `device-revoked, superseded and refresh-invalid are three different refusals`() {
        assertEquals(SessionRefusal.DeviceRevoked, sessionRefusalOf(401, "device-revoked"))
        assertEquals(SessionRefusal.KeysReset, sessionRefusalOf(401, "key-generation-superseded"))
        // A logout at the server: the refresh is `refresh-invalid`, a bearer
        // route a plain `401`. Neither wipes.
        assertEquals(SessionRefusal.RefreshInvalid, sessionRefusalOf(401, "refresh-invalid"))
        assertEquals(SessionRefusal.Unauthorized, sessionRefusalOf(401, null))
        // Only a `401` is a session refusal at all.
        assertNull(sessionRefusalOf(403, "device-revoked"))
        assertNull(sessionRefusalOf(200, null))
    }

    @Test
    fun `discovery's 403 device-revoked is a hint to refresh at auth, never a removal`() {
        assertTrue(deviceRevokedHintOf(403, "device-revoked"))
        assertFalse(deviceRevokedHintOf(403, "device-mismatch"))
        assertFalse(deviceRevokedHintOf(403, null))
        assertFalse(deviceRevokedHintOf(401, "device-revoked"))
        // It classifies as no session refusal: nothing is wiped on it.
        assertNull(sessionRefusalOf(403, "device-revoked"))
    }

    @Test
    fun `the pair never prints its credentials`() {
        val pair = SessionPair("v3.a.1.d.1.2.secret-access", 2L, "r2.a.1.d.1.3.secret-refresh", 3L, 1L)
        assertFalse(pair.toString().contains("secret"))
        assertTrue(pair.toString().contains("keyGeneration=1"))
        assertEquals(pair, SessionPair("v3.a.1.d.1.2.secret-access", 2L, "r2.a.1.d.1.3.secret-refresh", 3L, 1L))
    }
}
