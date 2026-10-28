// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import me.lakko.app.l10n.EnglishForTests
import java.util.Base64
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0048 §3 — the username verification state machine: verified only when
 * discovery resolves the claim to the very key the profile is bound to; a
 * definitive answer is cached per `(key, username)`; a transient one is not; a
 * changed claim verifies afresh; concurrent callers share one lookup.
 */
class UsernameVerifierTest {
    init {
        // ADR-0061: the words under test are the English catalog's.
        EnglishForTests.install()
    }


    private val passesRule: (String) -> Boolean = { it.matches(Regex("[a-z0-9_]{3,32}")) }

    private fun verifier(
        store: UsernameVerificationStore = UsernameVerificationStore(InMemoryProfilePersistence()),
        calls: AtomicInteger = AtomicInteger(),
        resolve: suspend (String) -> UsernameResolution,
    ) = UsernameVerifier(store, { name -> calls.incrementAndGet(); resolve(name) }, passesRule)

    @Test
    fun `a claim resolving to the bound key is verified and cached`() = runTest {
        val store = UsernameVerificationStore(InMemoryProfilePersistence())
        val calls = AtomicInteger()
        val v = verifier(store, calls) { UsernameResolution.Found("KEY_A") }
        assertEquals(VerifyOutcome.VERIFIED, v.verify("KEY_A", "alice"))
        assertEquals(UsernameVerification.VERIFIED, store.get("KEY_A", "alice"))
        // Cached: a second verify spends no lookup.
        assertEquals(VerifyOutcome.VERIFIED, v.verify("KEY_A", "alice"))
        assertEquals(1, calls.get())
    }

    @Test
    fun `a claim resolving to a different key fails and is never retried`() = runTest {
        val store = UsernameVerificationStore(InMemoryProfilePersistence())
        val calls = AtomicInteger()
        val v = verifier(store, calls) { UsernameResolution.Found("KEY_PAVEL") }
        assertEquals(VerifyOutcome.FAILED, v.verify("KEY_MALLORY", "pavel"))
        assertEquals(UsernameVerification.FAILED, store.get("KEY_MALLORY", "pavel"))
        assertEquals(VerifyOutcome.FAILED, v.verify("KEY_MALLORY", "pavel"))
        assertEquals(1, calls.get())
    }

    @Test
    fun `an unregistered username fails`() = runTest {
        val v = verifier { UsernameResolution.NotRegistered }
        assertEquals(VerifyOutcome.FAILED, v.verify("KEY_A", "ghost"))
    }

    @Test
    fun `an unavailable lookup caches nothing and retries next time`() = runTest {
        val store = UsernameVerificationStore(InMemoryProfilePersistence())
        val calls = AtomicInteger()
        var online = false
        val v = verifier(store, calls) {
            if (online) UsernameResolution.Found("KEY_A") else UsernameResolution.Unavailable(null)
        }
        assertEquals(VerifyOutcome.UNAVAILABLE, v.verify("KEY_A", "alice"))
        assertNull(store.get("KEY_A", "alice"))
        online = true
        assertEquals(VerifyOutcome.VERIFIED, v.verify("KEY_A", "alice"))
        assertEquals(2, calls.get())
    }

    @Test
    fun `a changed claim is verified afresh`() = runTest {
        val store = UsernameVerificationStore(InMemoryProfilePersistence())
        val calls = AtomicInteger()
        val v = verifier(store, calls) { name ->
            if (name == "alice") UsernameResolution.Found("KEY_A") else UsernameResolution.Found("KEY_OTHER")
        }
        assertEquals(VerifyOutcome.VERIFIED, v.verify("KEY_A", "alice"))
        assertEquals(VerifyOutcome.FAILED, v.verify("KEY_A", "alice2"))
        assertEquals(2, calls.get())
        assertEquals(UsernameVerification.VERIFIED, store.get("KEY_A", "alice"))
        assertEquals(UsernameVerification.FAILED, store.get("KEY_A", "alice2"))
    }

    @Test
    fun `a claim failing the username rule fails without a lookup`() = runTest {
        val calls = AtomicInteger()
        val v = verifier(calls = calls) { UsernameResolution.Found("KEY_A") }
        assertEquals(VerifyOutcome.FAILED, v.verify("KEY_A", "pаvel"))
        assertEquals(0, calls.get())
    }

    @Test
    fun `concurrent verifications of one pair share a single lookup`() = runTest {
        val calls = AtomicInteger()
        val gate = CompletableDeferred<Unit>()
        val v = verifier(calls = calls) {
            gate.await()
            UsernameResolution.Found("KEY_A")
        }
        val first = async { v.verify("KEY_A", "alice") }
        val second = async { v.verify("KEY_A", "alice") }
        // Both callers are now suspended inside `verify` (neither can finish
        // before the gate opens, so the cache cannot be what dedupes them).
        testScheduler.advanceUntilIdle()
        assertEquals(1, calls.get())
        gate.complete(Unit)
        assertEquals(VerifyOutcome.VERIFIED, first.await())
        assertEquals(VerifyOutcome.VERIFIED, second.await())
        assertEquals(1, calls.get())
    }

    @Test
    fun `the directory shows a username only after verification lands`() {
        val keyBytes = ByteArray(32) { 7 }
        val keyB64 = Base64.getEncoder().encodeToString(keyBytes)
        val directory = ProfileDirectory(
            persistence = InMemoryProfilePersistence(),
            scope = CoroutineScope(Dispatchers.Unconfined),
            isNewer = { a, b -> a > b },
            passesRule = passesRule,
        )
        directory.onProfileReceived(keyB64, PeerProfile(null, "alice", 1uL))
        // No resolver attached yet: the claim stays hidden and the label is the
        // neutral placeholder (the fingerprint is never a name).
        assertEquals("Member", directory.resolve(keyB64).primary)
        assertEquals("0707 0707", directory.resolve(keyB64).fingerprint)
        directory.attachUsernameResolver { UsernameResolution.Found(keyB64) }
        assertEquals("@alice", directory.resolve(keyB64).primary)
    }

    @Test
    fun `the retry waits start at seconds and grow to five minutes`() {
        assertEquals(15_000L, usernameRetryDelayMs(1))
        assertEquals(30_000L, usernameRetryDelayMs(2))
        assertEquals(300_000L, usernameRetryDelayMs(50))
    }

    @Test
    fun `a check that could not complete shows no line and is tried again by itself`() = runTest {
        val keyB64 = Base64.getEncoder().encodeToString(ByteArray(32) { 7 })
        val directory = ProfileDirectory(
            persistence = InMemoryProfilePersistence(),
            scope = backgroundScope,
            isNewer = { a, b -> a > b },
            passesRule = passesRule,
        )
        val calls = AtomicInteger(0)
        directory.attachUsernameResolver {
            if (calls.getAndIncrement() == 0) UsernameResolution.Unavailable(RuntimeException("429"))
            else UsernameResolution.Found(keyB64)
        }
        directory.onProfileReceived(keyB64, PeerProfile(null, "alice", 1uL))
        testScheduler.runCurrent()
        assertEquals(1, calls.get())
        assertEquals(false, directory.resolve(keyB64).usernameNotVerified)
        assertEquals("Member", directory.resolve(keyB64).primary)

        testScheduler.advanceTimeBy(usernameRetryDelayMs(1) + 1)
        testScheduler.runCurrent()
        assertEquals(2, calls.get())
        assertEquals("@alice", directory.resolve(keyB64).primary)
    }
}
