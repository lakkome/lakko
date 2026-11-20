// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/**
 * ADR-0056 security pass: a join-by-name restore verifies the transferred seed
 * against the published identity, then checks at discovery that the typed name
 * resolves to the redeemed account, and only then writes anything - the
 * pairing's held state and this device's own. Host JVM: the gates are fakes.
 */
class DeviceRestoreNameCheckTest {

    private class Run(
        val lookupResult: () -> ByteArray?,
        val seedVerifies: Boolean = true,
    ) {
        val calls = mutableListOf<String>()
        val seed = ByteArray(64) { 7 }

        suspend fun go(account: ByteArray) = runCheckedDeviceRestore(
            username = "alice",
            accountIdBytes = account,
            phraseSeed = seed,
            verifySeed = {
                calls += "verify"
                if (!seedVerifies) throw PhraseMismatch()
            },
            lookup = { name ->
                calls += "lookup:$name"
                lookupResult()
            },
            persistJoin = { calls += "persistJoin" },
            land = { calls += "land" },
            log = {},
        )
    }

    private val account = ByteArray(16) { 3 }

    @Test
    fun theEarlierKeysAreStoredBeforeTheJoinStateAndTheLanding() = runTest {
        // G-56 (contract 3.3 step 4): the restore wraps the pairing's held
        // writes so the account's own earlier keys go to disk first.
        val calls = mutableListOf<String>()
        runCheckedDeviceRestore(
            username = "alice",
            accountIdBytes = account,
            phraseSeed = ByteArray(64) { 7 },
            verifySeed = { calls += "verify" },
            lookup = { account },
            persistJoin = predecessorsFirst(
                storePredecessors = { calls += "predecessors" },
                persistJoin = { calls += "persistJoin" },
            ),
            land = { calls += "land" },
            log = {},
        )
        assertEquals(listOf("verify", "predecessors", "persistJoin", "land"), calls)
    }

    @Test
    fun aSeedThatFailsVerificationPersistsNothingAndIsNotLookedUp() {
        val run = Run(lookupResult = { account }, seedVerifies = false)
        assertThrows(PhraseMismatch::class.java) { runBlocking { run.go(account) } }
        assertEquals(listOf("verify"), run.calls)
        assertTrue(run.seed.all { it == 0.toByte() })
    }

    @Test
    fun aNameResolvingToAnotherAccountIsRefusedWithTheCopyAndNothingPersisted() {
        val run = Run(lookupResult = { ByteArray(16) { 9 } })
        val err = assertThrows(TypedNameOtherAccount::class.java) { runBlocking { run.go(account) } }
        assertEquals(
            "The name you typed belongs to a different account. Nothing was changed on this device.",
            err.message,
        )
        assertEquals(listOf("verify", "lookup:alice"), run.calls)
        assertTrue(run.seed.all { it == 0.toByte() })
    }

    @Test
    fun aNameDiscoveryDoesNotKnowIsRefusedTheSameWay() {
        val run = Run(lookupResult = { null })
        assertThrows(TypedNameOtherAccount::class.java) { runBlocking { run.go(account) } }
        assertTrue(run.calls.none { it == "persistJoin" || it == "land" })
        assertTrue(run.seed.all { it == 0.toByte() })
    }

    @Test
    fun aLookupThatCannotRunFailsClosed() {
        val failures: List<Throwable> = listOf(
            IOException("no network"),
            me.lakko.app.anon.AnonTokenDisabled(me.lakko.app.anon.AnonTokenContext.DISCOVERY_LOOKUP),
            me.lakko.app.anon.AnonTokenQuotaExhausted(me.lakko.app.anon.AnonTokenContext.DISCOVERY_LOOKUP, 60L),
        )
        for (failure in failures) {
            val run = Run(lookupResult = { throw failure })
            val err = assertThrows(TypedNameCheckUnavailable::class.java) { runBlocking { run.go(account) } }
            assertEquals(TYPED_NAME_CHECK_UNAVAILABLE_COPY, err.message)
            assertTrue(run.calls.none { it == "persistJoin" || it == "land" })
            assertTrue(run.seed.all { it == 0.toByte() })
        }
    }

    @Test
    fun theHappyPathPersistsAfterBothChecksThenLands() = runTest {
        val run = Run(lookupResult = { account.copyOf() })
        run.go(account)
        assertEquals(listOf("verify", "lookup:alice", "persistJoin", "land"), run.calls)
        assertTrue("the seed is handed to the landing, not zeroed", run.seed.any { it != 0.toByte() })
    }
}
