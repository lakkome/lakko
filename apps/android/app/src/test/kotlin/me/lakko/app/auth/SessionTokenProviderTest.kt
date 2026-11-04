// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0073 section 3.3 — the token provider's rules, over a fake refresh
 * route and a settable clock.
 */
class SessionTokenProviderTest {

    private val acct = "AAAAAAAAAAAAAAAAAAAAAA"

    /** The device segment (base64url of 32 bytes). */
    private val dev = "D".repeat(43)

    private fun pair(n: Int, expiresAtMs: Long, generation: Long = 1L) = SessionPair(
        accessToken = "v3.$acct.$generation.$dev.1.$expiresAtMs.access$n",
        accessExpiresAtMs = expiresAtMs,
        refreshToken = "r2.$acct.$generation.$dev.1.9.refresh$n",
        refreshExpiresAtMs = expiresAtMs + 1_000_000L,
        keyGeneration = generation,
    )

    private class FakeRefresher : SessionTokenProvider.SessionRefresher {
        val calls = mutableListOf<String>()
        var answer: suspend (String) -> SessionPair = { throw IllegalStateException("no answer set") }
        override suspend fun refresh(refreshToken: String): SessionPair {
            calls += refreshToken
            return answer(refreshToken)
        }
    }

    private class Installed {
        val persisted = mutableListOf<SessionPair>()
        val refusals = mutableListOf<SessionRefusal>()
    }

    private fun install(provider: SessionTokenProvider, pair: SessionPair): Installed {
        val installed = Installed()
        provider.install(pair, persist = { installed.persisted += it }, onRefusal = { installed.refusals += it })
        return installed
    }

    private var now = 1_000_000L

    private fun provider(refresher: FakeRefresher) = SessionTokenProvider(refresher, clock = { now })

    @Test
    fun `the token is handed out as it is while more than 120 seconds remain`() = runTest {
        val refresher = FakeRefresher()
        val p = provider(refresher)
        install(p, pair(1, now + 300_000L))

        assertEquals("v3.$acct.1.$dev.1.${now + 300_000L}.access1", p.current())
        assertTrue(refresher.calls.isEmpty())
    }

    @Test
    fun `a token with 120 seconds or less left is refreshed first and the new pair persisted`() = runTest {
        val refresher = FakeRefresher()
        val fresh = pair(2, now + 900_000L)
        refresher.answer = { fresh }
        val p = provider(refresher)
        val installed = install(p, pair(1, now + 120_000L))

        assertEquals(fresh.accessToken, p.current())
        assertEquals(listOf("r2.$acct.1.$dev.1.9.refresh1"), refresher.calls)
        assertEquals(listOf(fresh), installed.persisted)
        assertEquals(fresh, p.peek())
        // The next call uses the new token without another refresh.
        assertEquals(fresh.accessToken, p.current())
        assertEquals(1, refresher.calls.size)
    }

    @Test
    fun `one refresh is in flight at a time and the queued callers get its result`() = runTest {
        val refresher = FakeRefresher()
        val gate = CompletableDeferred<SessionPair>()
        refresher.answer = { gate.await() }
        val p = provider(refresher)
        install(p, pair(1, now))

        val a = async { p.current() }
        val b = async { p.current() }
        val c = async { p.afterUnauthorized("v3.$acct.1.$dev.1.$now.access1") }
        repeat(5) { yield() }
        assertEquals(1, refresher.calls.size)
        val fresh = pair(2, now + 900_000L)
        gate.complete(fresh)
        assertEquals(fresh.accessToken, a.await())
        assertEquals(fresh.accessToken, b.await())
        assertEquals(fresh.accessToken, c.await())
        assertEquals(1, refresher.calls.size)
    }

    @Test
    fun `a 401 retry refreshes once, and a caller whose token was already replaced sends nothing`() = runTest {
        val refresher = FakeRefresher()
        val fresh = pair(2, now + 900_000L)
        refresher.answer = { fresh }
        val p = provider(refresher)
        val stale = pair(1, now + 900_000L)
        install(p, stale)

        assertEquals(fresh.accessToken, p.afterUnauthorized(stale.accessToken))
        assertEquals(1, refresher.calls.size)
        // A second caller that used the same stale token gets the replacement
        // without a second refresh.
        assertEquals(fresh.accessToken, p.afterUnauthorized(stale.accessToken))
        assertEquals(1, refresher.calls.size)
        // A caller that used the FRESH token and still got a 401 refreshes again.
        val fresher = pair(3, now + 900_000L)
        refresher.answer = { fresher }
        assertEquals(fresher.accessToken, p.afterUnauthorized(fresh.accessToken))
        assertEquals(2, refresher.calls.size)
    }

    @Test
    fun `a refresh refused as superseded is reported once and keeps the stale pair`() = runTest {
        val refresher = FakeRefresher()
        refresher.answer = {
            throw AuthHttpError(401, "/auth/session/refresh", """{"error":"key-generation-superseded"}""", code = "key-generation-superseded")
        }
        val p = provider(refresher)
        val stale = pair(1, now)
        val installed = install(p, stale)

        // The stale token is what the caller gets: the device is about to wipe.
        assertEquals(stale.accessToken, p.current())
        assertEquals(listOf(SessionRefusal.KeysReset), installed.refusals)
        // Reported once; no second refresh request is made for this session.
        assertEquals(stale.accessToken, p.current())
        assertNull(p.afterUnauthorized(stale.accessToken))
        assertEquals(listOf(SessionRefusal.KeysReset), installed.refusals)
        assertEquals(1, refresher.calls.size)
        assertTrue(installed.persisted.isEmpty())
    }

    @Test
    fun `a superseded seen on a bearer route is reported once too`() {
        val p = provider(FakeRefresher())
        val installed = install(p, pair(1, now + 900_000L))

        p.reportSupersededFromBearerRoute(pair(1, now + 900_000L).accessToken)
        p.reportSupersededFromBearerRoute(pair(1, now + 900_000L).accessToken)

        assertEquals(listOf(SessionRefusal.KeysReset), installed.refusals)
        // Nothing installed: nothing reported.
        val empty = provider(FakeRefresher())
        empty.reportSupersededFromBearerRoute("any")
    }

    @Test
    fun `a superseded answer to a token already replaced reports nothing`() {
        val p = provider(FakeRefresher())
        val installed = install(p, pair(1, now + 900_000L))
        p.update(pair(2, now + 900_000L))

        p.reportSupersededFromBearerRoute(pair(1, now + 900_000L).accessToken)

        assertTrue(installed.refusals.isEmpty())
    }

    @Test
    fun `a refresh refused as invalid drops the session and reports it, nothing wiped`() = runTest {
        val refresher = FakeRefresher()
        refresher.answer = {
            throw AuthHttpError(401, "/auth/session/refresh", """{"error":"refresh-invalid"}""", code = "refresh-invalid")
        }
        val p = provider(refresher)
        val installed = install(p, pair(1, now))

        assertNull(p.current())
        assertEquals(listOf(SessionRefusal.RefreshInvalid), installed.refusals)
        assertNull(p.peek())
        assertFalse(p.manages("v3.$acct.1.$dev.1.$now.access1"))
    }

    @Test
    fun `a refresh that cannot be made keeps the current token`() = runTest {
        val refresher = FakeRefresher()
        refresher.answer = { throw AuthNetworkError("/auth/session/refresh", java.io.IOException("offline")) }
        val p = provider(refresher)
        val stale = pair(1, now)
        val installed = install(p, stale)

        assertEquals(stale.accessToken, p.current())
        assertNull(p.afterUnauthorized(stale.accessToken))
        assertTrue(installed.refusals.isEmpty())
        assertEquals(stale, p.peek())
        // A 5xx is the same: no refusal, the token stays.
        refresher.answer = { throw AuthHttpError(503, "/auth/session/refresh", "") }
        assertEquals(stale.accessToken, p.current())
        assertTrue(installed.refusals.isEmpty())
    }

    @Test
    fun `the provider manages access tokens of its account only`() {
        val p = provider(FakeRefresher())
        install(p, pair(1, now + 900_000L))

        assertTrue(p.manages("v3.$acct.1.$dev.1.5.someOlderToken"))
        assertFalse(p.manages("v3.BBBBBBBBBBBBBBBBBBBBBB.1.$dev.1.5.other"))
        assertFalse(p.manages("r2.$acct.1.$dev.1.5.refresh"))
        assertFalse(p.manages("v1.x.y.z"))
        p.clear()
        assertFalse(p.manages("v3.$acct.1.$dev.1.5.someOlderToken"))
        assertNull(p.peek())
    }

    @Test
    fun `a landing's newer pair replaces the installed one for the same account only`() {
        val p = provider(FakeRefresher())
        val installed = install(p, pair(1, now))
        val newer = pair(2, now + 900_000L, generation = 2L)

        p.update(newer)
        assertEquals(newer, p.peek())
        // Another account's pair is ignored.
        p.update(SessionPair("v3.BBBBBBBBBBBBBBBBBBBBBB.1.$dev.1.1.x", 1L, "r2.B.1.$dev.1.1.y", 2L, 1L))
        assertEquals(newer, p.peek())
        assertTrue(installed.persisted.isEmpty())
    }

    @Test
    fun `refreshNow renews whatever the expiry and hands back the pair`() = runTest {
        val refresher = FakeRefresher()
        val fresh = pair(2, now + 900_000L)
        refresher.answer = { fresh }
        val p = provider(refresher)
        install(p, pair(1, now + 900_000L))

        assertEquals(fresh, p.refreshNow())
        assertEquals(1, refresher.calls.size)
        assertNull(provider(FakeRefresher()).refreshNow())
    }

    // ---- ADR-0074: this device was removed ---------------------------------

    @Test
    fun `a refresh refused as device-revoked reports it once and keeps the pair`() = runTest {
        val refresher = FakeRefresher()
        val p = provider(refresher)
        val installed = install(p, pair(1, now + 10_000L))
        refresher.answer = { throw AuthHttpError(401, "/auth/session/refresh", "{}", code = DEVICE_REVOKED) }
        // The stale token is still handed back (requests in flight fail as they like).
        assertEquals(pair(1, now + 10_000L).accessToken, p.current())
        assertEquals(listOf(SessionRefusal.DeviceRevoked), installed.refusals)
        // Reported once; the pair is kept for the wipe that follows.
        p.current()
        p.reportDeviceRevokedFromBearerRoute()
        assertEquals(listOf(SessionRefusal.DeviceRevoked), installed.refusals)
        assertEquals(1, refresher.calls.size)
        assertEquals(pair(1, now + 10_000L), p.peek())
    }

    @Test
    fun `a bearer route's device-revoked is reported once`() = runTest {
        val p = provider(FakeRefresher())
        val installed = install(p, pair(1, now + 900_000L))
        p.reportDeviceRevokedFromBearerRoute()
        p.reportDeviceRevokedFromBearerRoute()
        assertEquals(listOf(SessionRefusal.DeviceRevoked), installed.refusals)
    }

    @Test
    fun `a logout at the server reads as refresh-invalid, never as a removal`() = runTest {
        val refresher = FakeRefresher()
        val p = provider(refresher)
        val installed = install(p, pair(1, now + 10_000L))
        refresher.answer = { throw AuthHttpError(401, "/auth/session/refresh", "{}", code = REFRESH_INVALID) }
        p.current()
        assertEquals(listOf(SessionRefusal.RefreshInvalid), installed.refusals)
        assertNull(p.peek())
    }

    @Test
    fun `adopt serves and persists the pair the bind returned, for the same account only`() = runTest {
        val p = provider(FakeRefresher())
        val installed = install(p, pair(1, now + 900_000L))
        val bound = pair(2, now + 900_000L)
        p.adopt(bound)
        assertEquals(bound.accessToken, p.current())
        assertEquals(listOf(bound), installed.persisted)
        p.adopt(SessionPair("v3.BBBBBBBBBBBBBBBBBBBBBB.1.$dev.1.1.x", 1L, "r2.B.1.$dev.1.1.y", 2L, 1L))
        assertEquals(bound.accessToken, p.current())
        assertEquals(1, installed.persisted.size)
    }
}
