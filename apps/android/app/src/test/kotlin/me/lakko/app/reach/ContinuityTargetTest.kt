// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.coroutines.test.runTest
import me.lakko.app.account.AccountStoreGate
import me.lakko.app.auth.MarkerStorage
import me.lakko.app.discovery.IdentityFetchResponse
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** ADR-0101 sections 2 to 4 - the peer verifies the continuity binding under
 *  the attested key and keeps the mailbox it names. Mirror of the ADR-0101
 *  cases of `apps/web/src/reach/continuity.test.ts`; the core's verifier is
 *  faked (the host JVM loads no native library). */
class ContinuityTargetTest {

    private class MapStorage : MarkerStorage {
        val rows = HashMap<String, String>()
        override fun get(key: String): String? = rows[key]
        override fun set(key: String, value: String) {
            rows[key] = value
        }
        override fun remove(key: String) {
            rows.remove(key)
        }
    }

    @Before
    fun setUp() = AccountStoreGate.resetForTest()

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    private val enc = java.util.Base64.getEncoder()
    private val newKey = ByteArray(32) { 0x11 }
    private val mailboxPk = ByteArray(32) { 0x22 }

    /** A 105-byte binding with [expiresAtMs] at bytes 33..41 (big-endian). */
    private fun binding(expiresAtMs: Long): ByteArray {
        val b = ByteArray(CONTINUITY_BINDING_LEN)
        b[0] = 1
        for (i in 0 until 8) b[33 + i] = (expiresAtMs ushr (56 - 8 * i)).toByte()
        return b
    }

    /** The core's verifier, faked: accepts only under [newKey] and returns
     *  [mailboxPk]. */
    private val verify: (ByteArray, ByteArray, Long) -> ByteArray = { _, key, _ ->
        if (!key.contentEquals(newKey)) throw IllegalArgumentException("badSignature")
        mailboxPk
    }

    @Test
    fun `the window equals the core's`() {
        assertEquals(30L * 24 * 60 * 60 * 1000, CONTINUITY_KEY_TTL_MS)
        assertEquals(105, CONTINUITY_BINDING_LEN)
    }

    @Test
    fun `the expiry is read off the layout, and a wrong length reads none`() {
        assertEquals(1_700_000_001_000L, continuityBindingExpiryMs(binding(1_700_000_001_000L)))
        assertNull(continuityBindingExpiryMs(ByteArray(104)))
    }

    @Test
    fun `a binding the attested key signed yields its mailbox and expiry`() {
        val now = 1_700_000_000_000L
        val target = peerContinuityTarget(enc.encodeToString(binding(now + 1000)), newKey, now, verify)
        assertEquals(ContinuityTarget(enc.encodeToString(mailboxPk), now + 1000), target)
    }

    @Test
    fun `another key, an expired binding, garbage and absence are refused`() {
        val now = 1_700_000_000_000L
        val b = enc.encodeToString(binding(now + 1000))
        assertNull(peerContinuityTarget(b, ByteArray(32) { 0x33 }, now, verify))
        assertNull(peerContinuityTarget(b, newKey, now + 1000, verify))
        assertNull(peerContinuityTarget("AAAA", newKey, now, verify))
        assertNull(peerContinuityTarget("not base64!", newKey, now, verify))
        assertNull(peerContinuityTarget(null, newKey, now, verify))
        assertNull(peerContinuityTarget("", newKey, now, verify))
    }

    @Test
    fun `reachable through the continuity mailbox only with a live target and a usable key`() {
        val live = ContinuityTarget("x", 2000L)
        assertTrue(continuityReachable(live, 0L, 1000L))
        assertFalse(continuityReachable(live, 0L, 2000L))
        assertFalse(continuityReachable(null, 0L, 1000L))
        assertFalse(continuityReachable(live, null, 1000L))
        assertFalse(continuityReachable(ContinuityTarget("x", Long.MAX_VALUE), 0L, CONTINUITY_KEY_TTL_MS))
        assertTrue(continuityTargetLive(live, 1999L))
        assertFalse(continuityTargetLive(live, 2000L))
        assertFalse(continuityTargetLive(null, 0L))
    }

    @Test
    fun `kept targets are per account, dropped at expiry and forgotten on null`() {
        val store = ContinuityTargetStore(MapStorage())
        store.remember("me", "K1", ContinuityTarget("m1", 100L), nowMs = 0L)
        store.remember("me", "K2", ContinuityTarget("m2", 50L), nowMs = 0L)
        assertEquals("m1", store.targetOf("me", "K1", 10L)?.mailboxPkB64)
        assertNull(store.targetOf("other", "K1", 10L))
        assertNull(store.targetOf("me", "K2", 50L))
        store.remember("me", "K1", null, nowMs = 60L)
        assertNull(store.targetOf("me", "K1", 61L))
    }

    @Test
    fun `the refresh keeps a verified target, forgets one for another key, keeps what is held on a failure`() = runTest {
        val store = ContinuityTargetStore(MapStorage())
        val acct = enc.encodeToString(ByteArray(16) { 1 })
        val now = 1_700_000_000_000L
        fun bundle(key: ByteArray, b: String?) =
            IdentityFetchResponse(signature_pubkey = enc.encodeToString(key), x25519_pubkey = "", continuity_binding_b64 = b)
        val kept = refreshContinuityTarget(
            fetch = { bundle(newKey, enc.encodeToString(binding(now + 5000))) },
            store = store, ownAccountIdB64 = "me", accountIdB64 = acct,
            attestedKeyB64 = enc.encodeToString(newKey), nowMs = now, verify = verify,
        )
        assertEquals(now + 5000, kept?.expiresAtMs)
        // A network failure keeps what is held.
        val held = refreshContinuityTarget(
            fetch = { throw java.io.IOException("offline") },
            store = store, ownAccountIdB64 = "me", accountIdB64 = acct,
            attestedKeyB64 = enc.encodeToString(newKey), nowMs = now + 1, verify = verify,
        )
        assertEquals(kept, held)
        // The directory now names another key: forgotten.
        val gone = refreshContinuityTarget(
            fetch = { bundle(ByteArray(32) { 0x44 }, null) },
            store = store, ownAccountIdB64 = "me", accountIdB64 = acct,
            attestedKeyB64 = enc.encodeToString(newKey), nowMs = now + 2, verify = verify,
        )
        assertNull(gone)
        assertNull(store.targetOf("me", enc.encodeToString(newKey), now + 3))
    }
}
