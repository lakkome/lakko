// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0074 — the recorded-identity-key rule (section 0.1) and the owed
 * logout (section 2.3), over a map in place of the device's storage.
 */
class SessionDutiesTest {

    private class MapStorage : MarkerStorage {
        val map = mutableMapOf<String, String>()
        override fun get(key: String): String? = map[key]
        override fun set(key: String, value: String) {
            map[key] = value
        }
        override fun remove(key: String) {
            map.remove(key)
        }
    }

    // ---- the recorded identity key ------------------------------------------

    @Test
    fun `the mint is called until it succeeded once for the key this device holds`() {
        assertTrue(genesisMintNeeded(recordedPubkeyB64 = null, currentPubkeyB64 = "K1"))
        assertFalse(genesisMintNeeded(recordedPubkeyB64 = "K1", currentPubkeyB64 = "K1"))
    }

    @Test
    fun `after a key reset the mint is called again, for the new key`() {
        assertTrue(genesisMintNeeded(recordedPubkeyB64 = "K1", currentPubkeyB64 = "K2"))
    }

    @Test
    fun `only a 2xx records the key`() {
        assertEquals(GenesisMintOutcome.Recorded, genesisMintOutcome(null))
        assertEquals(
            GenesisMintOutcome.Mismatch,
            genesisMintOutcome(AuthHttpError(409, "/auth/genesis/attest", "", code = "identity-key-mismatch")),
        )
        for (err in listOf(
            AuthHttpError(409, "/auth/genesis/attest", ""),
            AuthHttpError(400, "/auth/genesis/attest", ""),
            AuthHttpError(401, "/auth/genesis/attest", ""),
            AuthHttpError(503, "/auth/genesis/attest", ""),
            AuthNetworkError("/auth/genesis/attest", java.io.IOException("offline")),
        )) {
            assertEquals(err.toString(), GenesisMintOutcome.Retry, genesisMintOutcome(err))
        }
    }

    @Test
    fun `the mark is per account and names the key it was made for`() {
        val store = GenesisRecordedStore(MapStorage())
        assertNull(store.recordedPubkeyB64("A"))
        store.markRecorded("A", "K1")
        assertEquals("K1", store.recordedPubkeyB64("A"))
        assertNull(store.recordedPubkeyB64("B"))
        // A landing on the same key asks nothing more; a reset's new key does.
        assertFalse(genesisMintNeeded(store.recordedPubkeyB64("A"), "K1"))
        assertTrue(genesisMintNeeded(store.recordedPubkeyB64("A"), "K2"))
        store.markRecorded("A", "K2")
        assertEquals("K2", store.recordedPubkeyB64("A"))
        store.clear("A")
        assertNull(store.recordedPubkeyB64("A"))
    }

    // ---- the owed logout ----------------------------------------------------

    private fun owed(accessUntil: Long, refreshUntil: Long) = LogoutOwed("access", accessUntil, "refresh", refreshUntil)

    @Test
    fun `the retry uses the access token while it lives, else refreshes once, else drops`() {
        val now = 1_000L
        assertEquals(LogoutRetryStep.Logout, logoutRetryStep(owed(now + 1, now + 100), now))
        assertEquals(LogoutRetryStep.RefreshThenLogout, logoutRetryStep(owed(now, now + 100), now))
        assertEquals(LogoutRetryStep.RefreshThenLogout, logoutRetryStep(owed(now - 50, now + 1), now))
        // Both ran out: the session is dead at the server by itself.
        assertEquals(LogoutRetryStep.Drop, logoutRetryStep(owed(now - 50, now), now))
    }

    @Test
    fun `a logout made offline is kept for exactly one retry`() {
        val offline = AuthNetworkError("/auth/session/logout", java.io.IOException("offline"))
        assertTrue(keepLogoutOwed(offline, isRetry = false))
        assertFalse(keepLogoutOwed(offline, isRetry = true))
    }

    @Test
    fun `an answer from the server always settles the logout`() {
        for (status in listOf(401, 403, 429, 500, 503)) {
            val err = AuthHttpError(status, "/auth/session/logout", "")
            assertFalse(keepLogoutOwed(err, isRetry = false))
            assertFalse(keepLogoutOwed(err, isRetry = true))
        }
        assertFalse(keepLogoutOwed(IllegalStateException("x"), isRetry = false))
    }

    @Test
    fun `the owed logouts are kept by account and listed`() {
        val storage = MapStorage()
        val store = LogoutOwedStore(storage)
        assertTrue(store.accounts().isEmpty())
        store.save("A", owed(1, 2))
        store.save("B+/=", owed(3, 4))
        assertEquals(setOf("A", "B+/="), store.accounts())
        assertEquals(3L, store.load("B+/=")!!.accessExpiresAtMs)
        assertEquals("refresh", store.load("A")!!.refreshToken)
        // A new sign-in of A clears its owed logout: a late logout would
        // refuse the session that just landed.
        store.clear("A")
        assertNull(store.load("A"))
        assertEquals(setOf("B+/="), store.accounts())
        store.clear("B+/=")
        assertTrue(storage.map.isEmpty())
    }

    @Test
    fun `a malformed row reads as nothing owed, and an owed logout never prints its credentials`() {
        val storage = MapStorage()
        val store = LogoutOwedStore(storage)
        store.save("A", owed(1, 2))
        storage.map.keys.filter { it.endsWith(":A") }.forEach { storage.map[it] = "{not json" }
        assertNull(store.load("A"))
        val text = LogoutOwed("secret-access", 1, "secret-refresh", 2).toString()
        assertFalse(text.contains("secret"))
    }

    // ---- removed vs logged out vs reset: three screens, three deletions -----

    @Test
    fun `a removal and a reset wipe by themselves, a logout wipes nothing`() {
        // Removed: the removed screen, everything deleted with no tap.
        assertEquals(KeyResetSignOut.Kind.Removed, signOutKindFor(sessionRefusalOf(401, "device-revoked")!!))
        assertEquals(KeyResetSignOut.Phase.Removing, signOutPhaseFor(KeyResetSignOut.Kind.Removed))
        // Reset elsewhere: the signed-out screen, also by itself.
        assertEquals(KeyResetSignOut.Kind.Reset, signOutKindFor(sessionRefusalOf(401, "key-generation-superseded")!!))
        assertEquals(KeyResetSignOut.Phase.Removing, signOutPhaseFor(KeyResetSignOut.Kind.Reset))
        // Logged out at the server (`refresh-invalid`, or a plain 401): no
        // full-screen state and no deletion.
        assertNull(signOutKindFor(sessionRefusalOf(401, "refresh-invalid")!!))
        assertNull(signOutKindFor(sessionRefusalOf(401, null)!!))
        // Only the same-generation mismatch waits for the tap.
        assertEquals(KeyResetSignOut.Phase.AwaitingTap, signOutPhaseFor(KeyResetSignOut.Kind.Mismatch))
    }
}
