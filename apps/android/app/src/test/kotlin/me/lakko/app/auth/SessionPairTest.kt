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

    @Test
    fun `the account segment is read off a v2 access token only`() {
        assertEquals("AAAAAAAAAAAAAAAAAAAAAA", accountSegmentOfAccessToken("v2.AAAAAAAAAAAAAAAAAAAAAA.3.1700000000000.tag"))
        // A refresh credential is never an access token.
        assertNull(accountSegmentOfAccessToken("r1.AAAAAAAAAAAAAAAAAAAAAA.3.1700000000000.tag"))
        // Older formats and junk.
        assertNull(accountSegmentOfAccessToken("v1.AAAA.1.tag"))
        assertNull(accountSegmentOfAccessToken("v2.AAAA.1.tag"))
        assertNull(accountSegmentOfAccessToken("v2..1.2.tag"))
        assertNull(accountSegmentOfAccessToken(""))
        assertNull(accountSegmentOfAccessToken("Bearer v2.a.1.2.t"))
    }

    @Test
    fun `the pair never prints its credentials`() {
        val pair = SessionPair("v2.a.1.2.secret-access", 2L, "r1.a.1.3.secret-refresh", 3L, 1L)
        assertFalse(pair.toString().contains("secret"))
        assertTrue(pair.toString().contains("keyGeneration=1"))
        assertEquals(pair, SessionPair("v2.a.1.2.secret-access", 2L, "r1.a.1.3.secret-refresh", 3L, 1L))
    }
}
