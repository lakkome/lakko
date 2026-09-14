// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * Pins where a cold start lands for every combination of stored state
 * [decideSessionRestore] distinguishes.
 *
 * The property that matters: the ONLY way to land a session is with every
 * input present, readable and unexpired — and a session always carries
 * populated secrets. Anything else lands signed out, and a read that THROWS
 * is treated exactly like an absent value (fail closed), never as success.
 */
class SessionRestoreTest {

    private val now = 1_800_000_000_000L

    /** 16 raw bytes, standard base64 with padding — the SessionStore shape. */
    private val accountBytes = ByteArray(16) { (it + 1).toByte() }
    private val accountB64 = base64Encode(accountBytes)

    private class FakeSource(
        var marker: PersistedSession?,
        var deviceKey: Boolean = true,
        var token: PersistedSessionToken? = null,
        var seed: ByteArray? = null,
        var exportKey: ByteArray? = null,
        var throwOn: Set<String> = emptySet(),
    ) : SessionRestoreSource {
        val discarded = mutableListOf<String>()

        private fun maybeThrow(what: String) {
            if (what in throwOn) throw java.security.GeneralSecurityException("boom: $what")
        }

        override fun loadMarker(): PersistedSession? {
            maybeThrow("marker")
            return marker
        }

        override fun hasDeviceKey(accountIdB64: String): Boolean {
            maybeThrow("deviceKey")
            return deviceKey
        }

        override fun loadSessionToken(accountIdB64: String): PersistedSessionToken? {
            maybeThrow("token")
            return token
        }

        override fun loadAccountIdentitySeed(accountIdB64: String): ByteArray? {
            maybeThrow("seed")
            return seed?.copyOf()
        }

        override fun loadExportKey(accountIdB64: String): ByteArray? {
            maybeThrow("exportKey")
            return exportKey?.copyOf()
        }

        override fun discardSessionToken(accountIdB64: String) {
            discarded += accountIdB64
        }
    }

    private fun complete(): FakeSource = FakeSource(
        marker = PersistedSession(accountIdB64 = accountB64, username = "alice"),
        deviceKey = true,
        token = PersistedSessionToken(token = "v1.tok.en.sig", expiresAtMs = now + 60_000),
        seed = ByteArray(64) { 7 },
        exportKey = ByteArray(32) { 3 },
    )

    private fun decide(source: FakeSource): SessionRestoreDecision =
        decideSessionRestore(source, now) { _, _ -> }

    private fun assertSignedOut(
        decision: SessionRestoreDecision,
        reason: SignedOutReason,
    ) {
        when (decision) {
            is SessionRestoreDecision.SignedOut -> {
                assertEquals(reason, decision.reason)
                assertEquals("alice", decision.cachedUsername)
            }
            else -> fail("expected SignedOut($reason), got $decision")
        }
    }

    // -----------------------------------------------------------------

    @Test
    fun `everything present restores a session with populated secrets`() {
        val decision = decide(complete())
        if (decision !is SessionRestoreDecision.Restored) fail("expected Restored, got $decision")
        decision as SessionRestoreDecision.Restored
        assertEquals(SessionAccount(accountIdB64 = accountB64, username = "alice"), decision.account)
        assertArrayEquals(accountBytes, decision.secrets.accountId)
        assertArrayEquals(ByteArray(64) { 7 }, decision.secrets.accountIdentitySeed)
        assertArrayEquals(ByteArray(32) { 3 }, decision.secrets.exportKey)
        assertEquals("v1.tok.en.sig", decision.secrets.sessionToken)
        assertEquals(now + 60_000, decision.secrets.sessionExpiresAtMs)
    }

    @Test
    fun `no marker is a fresh device`() {
        val source = complete().apply { marker = null }
        assertSame(SessionRestoreDecision.NoAccount, decide(source))
    }

    @Test
    fun `an unreadable marker is a fresh device, not a crash`() {
        val source = complete().apply { throwOn = setOf("marker") }
        assertSame(SessionRestoreDecision.NoAccount, decide(source))
    }

    @Test
    fun `no device key lands signed out`() {
        val source = complete().apply { deviceKey = false }
        assertSignedOut(decide(source), SignedOutReason.NO_DEVICE_KEY)
    }

    @Test
    fun `an undecryptable device key lands signed out`() {
        val source = complete().apply { throwOn = setOf("deviceKey") }
        assertSignedOut(decide(source), SignedOutReason.NO_DEVICE_KEY)
    }

    @Test
    fun `a malformed account id lands signed out`() {
        val short = complete().apply {
            marker = PersistedSession(accountIdB64 = base64Encode(ByteArray(8)), username = "alice")
        }
        assertSignedOut(decide(short), SignedOutReason.MALFORMED_ACCOUNT_ID)
        val garbage = complete().apply {
            marker = PersistedSession(accountIdB64 = "***not base64***", username = "alice")
        }
        assertSignedOut(decide(garbage), SignedOutReason.MALFORMED_ACCOUNT_ID)
    }

    @Test
    fun `no persisted token lands signed out even with the seed present`() {
        // The pre-fix install shape: marker + device key + seed, no token.
        val source = complete().apply { token = null }
        assertSignedOut(decide(source), SignedOutReason.NO_SESSION_TOKEN)
    }

    @Test
    fun `an undecryptable token lands signed out`() {
        val source = complete().apply { throwOn = setOf("token") }
        assertSignedOut(decide(source), SignedOutReason.NO_SESSION_TOKEN)
    }

    @Test
    fun `a blank token lands signed out`() {
        val source = complete().apply { token = PersistedSessionToken("   ", now + 60_000) }
        assertSignedOut(decide(source), SignedOutReason.NO_SESSION_TOKEN)
    }

    @Test
    fun `an expired token lands signed out and is discarded`() {
        val source = complete().apply { token = PersistedSessionToken("t", now - 1) }
        assertSignedOut(decide(source), SignedOutReason.SESSION_TOKEN_EXPIRED)
        assertEquals(listOf(accountB64), source.discarded)
    }

    @Test
    fun `a token expiring exactly now is already expired`() {
        val source = complete().apply { token = PersistedSessionToken("t", now) }
        assertSignedOut(decide(source), SignedOutReason.SESSION_TOKEN_EXPIRED)
    }

    @Test
    fun `no identity seed lands signed out and drops the orphaned token`() {
        // The NeedsRecoveryPhrase-then-killed shape: auth succeeded, the
        // phrase was never supplied. Must NOT become a session.
        val source = complete().apply { seed = null }
        assertSignedOut(decide(source), SignedOutReason.NO_IDENTITY_SEED)
        assertEquals(listOf(accountB64), source.discarded)
    }

    @Test
    fun `an undecryptable identity seed lands signed out`() {
        val source = complete().apply { throwOn = setOf("seed") }
        assertSignedOut(decide(source), SignedOutReason.NO_IDENTITY_SEED)
    }

    @Test
    fun `a wrong-length identity seed lands signed out`() {
        val source = complete().apply { seed = ByteArray(32) { 7 } }
        assertSignedOut(decide(source), SignedOutReason.NO_IDENTITY_SEED)
    }

    @Test
    fun `the export key is optional`() {
        for (source in listOf(
            complete().apply { exportKey = null },
            complete().apply { throwOn = setOf("exportKey") },
            complete().apply { exportKey = ByteArray(5) },
        )) {
            val decision = decide(source)
            assertTrue("expected Restored, got $decision", decision is SessionRestoreDecision.Restored)
            assertEquals(0, (decision as SessionRestoreDecision.Restored).secrets.exportKey.size)
        }
    }

    @Test
    fun `a restorable session never discards its token`() {
        val source = complete()
        decide(source)
        assertTrue(source.discarded.isEmpty())
    }
}
