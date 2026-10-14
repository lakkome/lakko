// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.anon

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpMethod
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.OutgoingContent
import io.ktor.http.content.TextContent
import io.ktor.http.headersOf
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0054 S3 - the anonymous-token pool, host-side: the native core and the
 * issuer sit behind [AnonTokenCore] / [AnonTokenIssuerApi], so no native
 * library is loaded. What is pinned: a take triggers a background refill below
 * two, batches of eight up to thirty-two, a dead token is dropped, tokens are
 * per account (a logout keeps them on disk, a switch loads the other account's
 * own, the wipe removes one account's only), a batch is verified against the key from
 * the GET route and discarded when it does not verify, a used-up quota and a
 * disabled issuer surface as their own failures, and a disagreeing epoch is
 * rebuilt once.
 */
class AnonTokenPoolTest {

    // ------------------------------------------------------------ fakes

    /** Epoch = unixSecs / window; a token is live in its epoch and the next. */
    private class FakeCore(private val goodKey: String = "GOOD") : AnonTokenCore {
        val begun = mutableListOf<Triple<AnonTokenContext, Long, Int>>()
        val keysUsed = mutableListOf<String>()
        private var serial = 0

        override val headerName: String = "x-lakko-anon-token"
        override val defaultWindowSecs: Long = 100
        override fun epochAt(unixSecs: Long, windowSecs: Long): Long = unixSecs / windowSecs
        override fun isLive(tokenEpoch: Long, currentEpoch: Long): Boolean =
            tokenEpoch == currentEpoch || tokenEpoch + 1 == currentEpoch

        override fun begin(context: AnonTokenContext, epoch: Long, count: Int): AnonIssuance {
            begun += Triple(context, epoch, count)
            return object : AnonIssuance {
                override fun issueBodyJson(): String =
                    """{"context":"${context.wire}","epoch":$epoch,"count":$count}"""

                override fun finalize(
                    evaluations: List<String>,
                    proof: String,
                    publicKey: String,
                ): List<PooledAnonToken> {
                    keysUsed += publicKey
                    if (publicKey != goodKey) throw AnonFinalizeFailed("proof-verification")
                    return evaluations.map { PooledAnonToken(context, epoch, "t${serial++}") }
                }

                override fun close() = Unit
            }
        }
    }

    /** Issues [count]-many evaluations for the body's epoch, or the scripted outcome. */
    private class FakeApi(var key: String? = "GOOD") : AnonTokenIssuerApi {
        val sessions = mutableListOf<AnonSession>()
        val scripted = ArrayDeque<AnonIssueOutcome>()
        var gate: CompletableDeferred<Unit>? = null

        override suspend fun issue(session: AnonSession, bodyJson: String): AnonIssueOutcome {
            sessions += session
            gate?.await()
            scripted.removeFirstOrNull()?.let { return it }
            val epoch = Regex("\"epoch\":(\\d+)").find(bodyJson)!!.groupValues[1].toLong()
            val count = Regex("\"count\":(\\d+)").find(bodyJson)!!.groupValues[1].toInt()
            return AnonIssueOutcome.Issued(epoch, List(count) { "e$it" }, "proof")
        }

        override suspend fun publicKey(context: AnonTokenContext, epoch: Long): String? = key
    }

    private val alice = AnonSession("alice", "bearer-a")
    private val bob = AnonSession("bob", "bearer-b")

    private fun TestScope.pool(
        core: FakeCore,
        api: FakeApi,
        session: () -> AnonSession?,
        now: () -> Long = { 1_000L },
        store: AnonTokenStore = NoAnonTokenStore,
    ) = AnonTokenPool(
        core = core,
        api = api,
        session = session,
        scope = this,
        nowSecs = now,
        waitMs = 5_000,
        store = store,
    )

    /** What a store on disk keeps, without the disk: one pool per account. */
    private class MemoryStore : AnonTokenStore {
        val pools = HashMap<String, PersistedAnonPool>()
        val deletes = mutableListOf<String>()

        /** Alice's pool, the account most tests use. */
        var persisted: PersistedAnonPool?
            get() = pools["alice"]
            set(value) {
                if (value == null) pools.remove("alice") else pools[value.owner] = value
            }

        override fun load(accountKey: String): PersistedAnonPool? = pools[accountKey]
        override fun save(pool: PersistedAnonPool) {
            pools[pool.owner] = pool
        }
        override fun delete(accountKey: String) {
            deletes += accountKey
            pools.remove(accountKey)
        }
    }

    // ------------------------------------------------------------ batch sizes (S5)

    @Test
    fun eachContextDrawsItsOwnBatchSize() = runTest {
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { alice })
        for (context in AnonTokenContext.entries) pool.take(context)
        advanceUntilIdle()
        val firstBatch = core.begun.groupBy { it.first }.mapValues { it.value.first().third }
        assertEquals(
            mapOf(
                AnonTokenContext.DISCOVERY_LOOKUP to 8,
                AnonTokenContext.PUSH_REGISTER to 4,
                AnonTokenContext.INVITE_CONSUME to 2,
                AnonTokenContext.DS_KNOCK to 2,
                AnonTokenContext.DISCOVERY_REGISTER to 1,
                // ADR-0059 S2 contexts (`anonBatchPolicyFor`).
                AnonTokenContext.CHANNEL_SUBSCRIBE to 2,
                AnonTokenContext.DIRECTORY_SEARCH to 1,
                // ADR-0064 / ADR-0065 contexts; chosen, not measured.
                AnonTokenContext.VAULT_CLAIM to 4,
                AnonTokenContext.BLOB_UPLOAD to 2,
                AnonTokenContext.BLOB_READ to 2,
            ),
            firstBatch,
        )
    }

    @Test
    fun theContextListIsTheCoresTenNamesInWireOrder() {
        // The source of truth is `TokenContext::ALL` / `TokenContext::name` in
        // `crates/lakko-anon-token/src/context.rs` (wire bytes 1..=10), which
        // `anonTokenContexts()` in `crates/lakko-ffi-uniffi/src/anon_token.rs`
        // returns in that order. The host JVM cannot load the native library,
        // so the names are pinned here as literals: a context added to or
        // reordered in the core must be mirrored in `AnonTokenContext` and
        // in this list.
        assertEquals(
            listOf(
                "discovery-lookup",
                "discovery-register",
                "invite-consume",
                "ds-knock",
                "push-register",
                "channel-subscribe",
                "directory-search",
                "vault-claim",
                "blob-upload",
                "blob-read",
            ),
            AnonTokenContext.entries.map { it.wire },
        )
    }

    @Test
    fun theStorageContextsDrawSmallBatchesAndRefillWhenEmpty() {
        assertEquals(AnonBatchPolicy(batch = 4, refillBelow = 1), anonBatchPolicyFor(AnonTokenContext.VAULT_CLAIM))
        assertEquals(AnonBatchPolicy(batch = 2, refillBelow = 1), anonBatchPolicyFor(AnonTokenContext.BLOB_UPLOAD))
        assertEquals(AnonBatchPolicy(batch = 2, refillBelow = 1), anonBatchPolicyFor(AnonTokenContext.BLOB_READ))
    }

    @Test
    fun aRareContextRefillsOnlyWhenEmpty() = runTest {
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { alice })
        pool.take(AnonTokenContext.PUSH_REGISTER) // batch of 4, three left
        advanceUntilIdle()
        repeat(2) { pool.take(AnonTokenContext.PUSH_REGISTER) }
        advanceUntilIdle()
        assertEquals("one left: no refill yet", 1, core.begun.size)
        pool.take(AnonTokenContext.PUSH_REGISTER) // now empty
        advanceUntilIdle()
        assertEquals(2, core.begun.size)
        assertEquals(4, pool.held(AnonTokenContext.PUSH_REGISTER))
    }

    @Test
    fun aLookupStillRefillsBelowTwo() {
        assertEquals(AnonBatchPolicy(batch = 8, refillBelow = 2), anonBatchPolicyFor(AnonTokenContext.DISCOVERY_LOOKUP))
        for (context in AnonTokenContext.entries - AnonTokenContext.DISCOVERY_LOOKUP) {
            assertEquals(context.wire, 1, anonBatchPolicyFor(context).refillBelow)
        }
    }

    // ------------------------------------------------------------ persistence (S5)

    @Test
    fun thePoolSurvivesARelaunch() = runTest {
        val store = MemoryStore()
        val core = FakeCore()
        val api = FakeApi()
        val first = pool(core, api, { alice }, store = store)
        val spent = first.take(AnonTokenContext.DISCOVERY_LOOKUP) // seven left
        assertEquals(7, store.persisted!!.tokens.size)
        assertEquals("alice", store.persisted!!.owner)
        assertTrue(store.persisted!!.tokens.none { it.headerValue == spent.headerValue })

        // A new process: a fresh pool over the same store.
        val relaunched = pool(core, api, { alice }, store = store)
        val next = relaunched.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertEquals("t1", next.headerValue)
        assertEquals("served from disk: no new issuance", 1, api.sessions.size)
        assertEquals(6, store.persisted!!.tokens.size)
    }

    @Test
    fun deadEpochTokensAreDroppedOnLoad() = runTest {
        val store = MemoryStore()
        store.persisted = PersistedAnonPool(
            owner = "alice",
            tokens = listOf(
                PooledAnonToken(AnonTokenContext.DS_KNOCK, 8, "dead"),
                PooledAnonToken(AnonTokenContext.DS_KNOCK, 9, "live-previous"),
                PooledAnonToken(AnonTokenContext.DS_KNOCK, 10, "live-current"),
            ),
        )
        val api = FakeApi()
        val pool = pool(FakeCore(), api, { alice }, store = store) // epoch 10
        assertEquals("live-previous", pool.take(AnonTokenContext.DS_KNOCK).headerValue)
        assertTrue(store.persisted!!.tokens.none { it.headerValue == "dead" })
        assertEquals(listOf("live-current"), store.persisted!!.tokens.map { it.headerValue })
        assertTrue(api.sessions.isEmpty())
    }

    @Test
    fun logoutKeepsTheAccountsTokensForItsNextSignIn() = runTest {
        val store = MemoryStore()
        var who: AnonSession? = alice
        val api = FakeApi()
        val pool = pool(FakeCore(), api, { who }, store = store)
        pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertEquals(7, store.persisted!!.tokens.size)
        // Log out: the memory goes, the disk does not.
        pool.releaseSession()
        who = null
        assertEquals(0, pool.held(AnonTokenContext.DISCOVERY_LOOKUP))
        assertEquals(7, store.persisted!!.tokens.size)
        assertTrue(store.deletes.isEmpty())
        // The same account signs in again: its tokens load, no new issuance.
        who = alice
        assertEquals("t1", pool.take(AnonTokenContext.DISCOVERY_LOOKUP).headerValue)
        assertEquals(1, api.sessions.size)
    }

    @Test
    fun anotherAccountNeverLoadsThePersistedTokensAndLeavesThemOnDisk() = runTest {
        val store = MemoryStore()
        pool(FakeCore(), FakeApi(), { alice }, store = store).take(AnonTokenContext.DISCOVERY_LOOKUP)
        val aliceTokens = store.persisted!!.tokens
        val api = FakeApi()
        val core = FakeCore()
        val asBob = pool(core, api, { bob }, store = store)
        asBob.take(AnonTokenContext.DISCOVERY_LOOKUP)
        // Bob's token came from an issuance to Bob, not from Alice's pool.
        assertEquals(listOf(bob), api.sessions)
        assertEquals("bob", store.pools["bob"]!!.owner)
        // Alice's stay on disk, untouched.
        assertEquals(aliceTokens, store.persisted!!.tokens)
        assertTrue(store.deletes.isEmpty())
    }

    @Test
    fun twoAccountsTokensCoexistAndASwitchBackLoadsTheFirstAccountsOwn() = runTest {
        val store = MemoryStore()
        var who = alice
        val core = FakeCore()
        val api = FakeApi()
        val pool = pool(core, api, { who }, store = store)
        val a1 = pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        who = bob
        val b1 = pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertEquals(7, store.pools["alice"]!!.tokens.size)
        assertEquals(7, store.pools["bob"]!!.tokens.size)
        val bobHeld = store.pools["bob"]!!.tokens.map { it.headerValue }.toSet()
        // Back to Alice: served from Alice's own pool, never one of Bob's.
        who = alice
        val a2 = pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertEquals(2, core.begun.size)
        assertTrue(a2.headerValue !in bobHeld && a2.headerValue != b1.headerValue)
        assertTrue(a2.headerValue != a1.headerValue)
        assertEquals(6, store.pools["alice"]!!.tokens.size)
        assertEquals(7, store.pools["bob"]!!.tokens.size)
    }

    @Test
    fun theWipeRemovesOnlyTheWipedAccountsTokens() = runTest {
        val store = MemoryStore()
        var who = alice
        val api = FakeApi()
        val pool = pool(FakeCore(), api, { who }, store = store)
        pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        who = bob
        pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        // Bob is held in memory; wiping Alice leaves Bob's memory and disk.
        pool.forgetAccount("alice")
        assertEquals(null, store.pools["alice"])
        assertEquals(7, store.pools["bob"]!!.tokens.size)
        assertEquals(7, pool.held(AnonTokenContext.DISCOVERY_LOOKUP))
        // Wiping Bob (held) drops his memory and his file.
        pool.forgetAccount("bob")
        assertEquals(0, pool.held(AnonTokenContext.DISCOVERY_LOOKUP))
        assertEquals(null, store.pools["bob"])
        assertEquals(listOf("alice", "bob"), store.deletes)
    }

    @Test
    fun aRefillInFlightAtTheWipeDoesNotWriteTheFileBack() = runTest {
        val store = MemoryStore()
        val api = FakeApi()
        val pool = pool(FakeCore(), api, { alice }, store = store)
        pool.take(AnonTokenContext.DISCOVERY_LOOKUP) // seven left, below none
        // Drain to below the refill threshold with the issuer held, so a
        // refill is in flight when the wipe runs.
        api.gate = CompletableDeferred()
        repeat(6) { pool.take(AnonTokenContext.DISCOVERY_LOOKUP) }
        runCurrent()
        assertEquals("the refill is out, waiting on the issuer", 2, api.sessions.size)
        pool.forgetAccount("alice")
        api.gate!!.complete(Unit)
        advanceUntilIdle()
        assertNull("the wipe's delete stays", store.pools["alice"])
    }

    @Test
    fun aTakeThatReadTheSessionBeforeTheWipeNeitherAdoptsNorRefills() = runTest {
        // forgetDevice wipes while the live secrets still name the account,
        // and clears them only afterwards: a take in that window.
        val store = MemoryStore()
        val core = FakeCore()
        val api = FakeApi()
        val pool = pool(core, api, { alice }, store = store)
        pool.forgetAccount("alice")
        val err = runCatching { pool.take(AnonTokenContext.DISCOVERY_LOOKUP) }.exceptionOrNull()
        advanceUntilIdle()
        assertTrue(err is AnonTokenUnavailable)
        assertTrue("nothing issued for a forgotten account", core.begun.isEmpty())
        assertNull(store.pools["alice"])
    }

    @Test
    fun aForgottenAccountsPoolIsNeitherSpentNorRewrittenByADetachedTake() = runTest {
        val store = MemoryStore()
        val api = FakeApi()
        val pool = pool(FakeCore(), api, { null }, store = store)
        pool.forgetAccount("alice")
        // A file that reappeared by some other route is not read back.
        store.pools["alice"] = PersistedAnonPool("alice", listOf(PooledAnonToken(AnonTokenContext.PUSH_REGISTER, 10, "old")))
        val t = pool.takeDetached(alice, AnonTokenContext.PUSH_REGISTER)
        assertTrue(t.headerValue != "old")
        assertEquals("left as it was, never rewritten", "old", store.pools["alice"]!!.tokens.single().headerValue)
    }

    @Test
    fun theAccountsNextLandingPersistsItsPoolAgain() = runTest {
        val store = MemoryStore()
        val pool = pool(FakeCore(), FakeApi(), { alice }, store = store)
        pool.forgetAccount("alice")
        pool.accountLanded("alice")
        pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertEquals(7, store.pools["alice"]!!.tokens.size)
    }

    @Test
    fun aWipeRecordedBeforeThePoolWasBuiltStillHolds() = runTest {
        val store = MemoryStore()
        val core = FakeCore()
        val pool = AnonTokenPool(
            core = core,
            api = FakeApi(),
            session = { alice },
            scope = this,
            nowSecs = { 1_000L },
            store = store,
            forgottenAccounts = setOf("alice"),
        )
        assertTrue(runCatching { pool.take(AnonTokenContext.DISCOVERY_LOOKUP) }.exceptionOrNull() is AnonTokenUnavailable)
        assertNull(store.pools["alice"])
        // Bob is not affected.
        val bobPool = AnonTokenPool(core, FakeApi(), { bob }, this, nowSecs = { 1_000L }, store = store, forgottenAccounts = setOf("alice"))
        bobPool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertEquals(7, store.pools["bob"]!!.tokens.size)
    }

    @Test
    fun aSignOutIsNotAForgetTheUnregisterStillSavesTheReducedPool() = runTest {
        val store = MemoryStore()
        var who: AnonSession? = alice
        val pool = pool(FakeCore(), FakeApi(), { who }, store = store)
        pool.take(AnonTokenContext.PUSH_REGISTER)
        assertEquals(3, store.persisted!!.tokens.size)
        pool.releaseSession()
        who = null
        pool.takeDetached(alice, AnonTokenContext.PUSH_REGISTER)
        assertEquals(2, store.persisted!!.tokens.size)
    }

    @Test
    fun aDetachedTakeAfterLogoutDrawsFromThatAccountsDiskPool() = runTest {
        val store = MemoryStore()
        var who: AnonSession? = alice
        val api = FakeApi()
        val pool = pool(FakeCore(), api, { who }, store = store)
        pool.take(AnonTokenContext.PUSH_REGISTER) // four drawn, three left
        assertEquals(3, store.persisted!!.tokens.size)
        pool.releaseSession()
        who = null
        val t = pool.takeDetached(alice, AnonTokenContext.PUSH_REGISTER)
        assertEquals(AnonTokenContext.PUSH_REGISTER, t.context)
        assertEquals("served from disk: no new issuance", 1, api.sessions.size)
        assertEquals(2, store.persisted!!.tokens.size)
        // Bob's detached take never draws from Alice's pool.
        pool.takeDetached(bob, AnonTokenContext.PUSH_REGISTER)
        assertEquals(bob, api.sessions.last())
        assertEquals(2, store.persisted!!.tokens.size)
    }

    // ------------------------------------------------------------ take + refill

    @Test
    fun anEmptyPoolWaitsForOneBatchOfEightThenServesFromIt() = runTest {
        val core = FakeCore()
        val api = FakeApi()
        val pool = pool(core, api, { alice })
        val first = pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertEquals("t0", first.headerValue)
        assertEquals(listOf(Triple(AnonTokenContext.DISCOVERY_LOOKUP, 10L, 8)), core.begun)
        // Seven left: the next six come from the pool with no new issuance.
        repeat(6) { pool.take(AnonTokenContext.DISCOVERY_LOOKUP) }
        assertEquals(1, core.begun.size)
        assertEquals(1, pool.held(AnonTokenContext.DISCOVERY_LOOKUP))
        // Below two: this take refills in the background.
        pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        advanceUntilIdle()
        assertEquals(2, core.begun.size)
        assertEquals(8, pool.held(AnonTokenContext.DISCOVERY_LOOKUP))
        assertEquals(listOf(alice, alice), api.sessions)
    }

    @Test
    fun aContextNeverHoldsMoreThanThirtyTwo() = runTest {
        val core = FakeCore()
        val api = FakeApi()
        val pool = AnonTokenPool(
            core,
            api,
            { alice },
            this,
            nowSecs = { 1_000L },
            policy = { AnonBatchPolicy(batch = 8, refillBelow = 100) },
        )
        // A threshold above the cap: every take asks for a refill, the cap stops it.
        repeat(6) {
            pool.take(AnonTokenContext.PUSH_REGISTER)
            advanceUntilIdle()
        }
        assertTrue(pool.held(AnonTokenContext.PUSH_REGISTER) <= AnonTokenPool.MAX_HELD)
        assertTrue(core.begun.all { it.third in 1..8 })
    }

    @Test
    fun contextsAreSeparate() = runTest {
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { alice })
        pool.take(AnonTokenContext.DS_KNOCK)
        pool.take(AnonTokenContext.INVITE_CONSUME)
        assertEquals(
            listOf(AnonTokenContext.DS_KNOCK, AnonTokenContext.INVITE_CONSUME),
            core.begun.map { it.first },
        )
    }

    @Test
    fun aTokenPastItsLiveWindowIsDropped() = runTest {
        var now = 1_000L
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { alice }, now = { now })
        pool.take(AnonTokenContext.DISCOVERY_LOOKUP) // epoch 10, seven left
        now = 1_250L // epoch 12: the epoch-10 tokens are dead
        val t = pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertEquals(12L, t.epoch)
        assertEquals(12L, core.begun.last().second)
    }

    // ------------------------------------------------------------ release

    @Test
    fun logoutReleasesTheMemory() = runTest {
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { alice })
        pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertEquals(7, pool.held(AnonTokenContext.DISCOVERY_LOOKUP))
        pool.releaseSession()
        assertEquals(0, pool.held(AnonTokenContext.DISCOVERY_LOOKUP))
    }

    @Test
    fun anAccountSwitchNeverHandsOverTheOtherAccountsTokens() = runTest {
        var who = alice
        val core = FakeCore()
        val api = FakeApi()
        val pool = pool(core, api, { who })
        val a = pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        who = bob
        val b = pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertTrue(a.headerValue != b.headerValue)
        assertEquals(bob, api.sessions.last())
        assertEquals(2, core.begun.size)
    }

    @Test
    fun noSessionMeansNoTokenAndNoIssuance() = runTest {
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { null })
        val err = runCatching { pool.take(AnonTokenContext.PUSH_REGISTER) }.exceptionOrNull()
        assertTrue(err is AnonTokenUnavailable)
        assertTrue(core.begun.isEmpty())
    }

    @Test
    fun aDetachedTakeUsesTheGivenSessionAndPoolsNothing() = runTest {
        val core = FakeCore()
        val api = FakeApi()
        val pool = pool(core, api, { null })
        val t = pool.takeDetached(alice, AnonTokenContext.PUSH_REGISTER)
        assertEquals("t0", t.headerValue)
        assertEquals(listOf(alice), api.sessions)
        assertEquals(1, core.begun.single().third)
        assertEquals(0, pool.held(AnonTokenContext.PUSH_REGISTER))
    }

    // ------------------------------------------------------------ issuer outcomes

    @Test
    fun aBatchIsVerifiedAgainstTheKeyRouteAndDiscardedWhenItDoesNotVerify() = runTest {
        val core = FakeCore(goodKey = "GOOD")
        val api = FakeApi(key = "SOMEONE-ELSES")
        val pool = pool(core, api, { alice })
        val err = runCatching { pool.take(AnonTokenContext.DISCOVERY_LOOKUP) }.exceptionOrNull()
        assertTrue(err is AnonTokenUnavailable)
        assertEquals(listOf("SOMEONE-ELSES"), core.keysUsed)
        assertEquals("nothing of the batch is kept", 0, pool.held(AnonTokenContext.DISCOVERY_LOOKUP))
    }

    @Test
    fun aUsedUpQuotaSurfacesAsSuchAndIsNotAskedAgainBeforeTheWindowRollsOver() = runTest {
        var now = 1_000L
        val core = FakeCore()
        val api = FakeApi()
        api.scripted += AnonIssueOutcome.QuotaExhausted(retryAfterSecs = 50)
        val pool = pool(core, api, { alice }, now = { now })
        val err = runCatching { pool.take(AnonTokenContext.DISCOVERY_LOOKUP) }.exceptionOrNull()
        assertTrue(err is AnonTokenQuotaExhausted)
        assertEquals(50L, (err as AnonTokenQuotaExhausted).retryAfterSecs)
        val again = runCatching { pool.take(AnonTokenContext.DISCOVERY_LOOKUP) }.exceptionOrNull()
        assertTrue(again is AnonTokenQuotaExhausted)
        assertEquals("the issuer is not asked again", 1, api.sessions.size)
        now = 1_060L
        pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertEquals(2, api.sessions.size)
    }

    @Test
    fun aDisabledIssuerIsItsOwnFailure() = runTest {
        val api = FakeApi()
        api.scripted += AnonIssueOutcome.Disabled
        val err = runCatching {
            pool(FakeCore(), api, { alice }).take(AnonTokenContext.DS_KNOCK)
        }.exceptionOrNull()
        assertTrue(err is AnonTokenDisabled)
    }

    @Test
    fun aDisagreeingEpochIsRebuiltOnceForTheIssuersEpoch() = runTest {
        val core = FakeCore()
        val api = FakeApi()
        api.scripted += AnonIssueOutcome.EpochNotIssuable(currentEpoch = 11)
        val pool = pool(core, api, { alice })
        val t = pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertEquals(11L, t.epoch)
        assertEquals(listOf(10L, 11L), core.begun.map { it.second })
        // The correction sticks: the next batch is built for 11 straight away.
        repeat(7) { pool.take(AnonTokenContext.DISCOVERY_LOOKUP) }
        advanceUntilIdle()
        assertEquals(11L, core.begun.last().second)
    }

    @Test
    fun aTakeThatOutwaitsTheRefillFailsAsUnavailable() = runTest {
        val api = FakeApi()
        api.gate = CompletableDeferred()
        val pool = AnonTokenPool(FakeCore(), api, { alice }, this, nowSecs = { 1_000L }, waitMs = 1_000)
        val err = runCatching { pool.take(AnonTokenContext.DISCOVERY_LOOKUP) }.exceptionOrNull()
        assertTrue(err is AnonTokenUnavailable)
        api.gate!!.complete(Unit)
        advanceUntilIdle()
        // The refill still lands for the next action.
        assertEquals(8, pool.held(AnonTokenContext.DISCOVERY_LOOKUP))
    }

    // ------------------------------------------------------------ retry once

    @Test
    fun aRetryableRefusalIsRetriedOnceWithAnotherToken() = runTest {
        val source = FakeAnonTokenSource()
        val used = mutableListOf<String>()
        val result = source.spendWithRetry(
            AnonTokenContext.DISCOVERY_LOOKUP,
            send = { t -> used += t.headerValue; if (used.size == 1) 401 else 200 },
            refusalCode = { status -> if (status == 401) "anon-token-spent" else null },
        )
        assertEquals(200, result)
        assertEquals(listOf("tok-1", "tok-2"), used)
    }

    @Test
    fun onlyOnceAndNeverForTokenRequired() = runTest {
        for (code in ANON_TOKEN_RETRYABLE_REFUSALS) {
            val source = FakeAnonTokenSource()
            var calls = 0
            source.spendWithRetry(
                AnonTokenContext.DS_KNOCK,
                send = { calls++; 401 },
                refusalCode = { code },
            )
            assertEquals(code, 2, calls)
        }
        val source = FakeAnonTokenSource()
        var calls = 0
        source.spendWithRetry(
            AnonTokenContext.DS_KNOCK,
            send = { calls++; 401 },
            refusalCode = { "anon-token-required" },
        )
        assertEquals(1, calls)
    }

    // ------------------------------------------------------------ the issuer over HTTP

    private data class Seen(val method: HttpMethod, val path: String, val authorization: String?, val body: String?)

    private fun issuer(seen: MutableList<Seen>, issueReply: Pair<HttpStatusCode, String>): KtorAnonTokenIssuer {
        val engine = MockEngine { request ->
            val body = when (val b = request.body) {
                is TextContent -> b.text
                is OutgoingContent.ByteArrayContent -> b.bytes().toString(Charsets.UTF_8)
                else -> null
            }
            seen += Seen(request.method, request.url.encodedPath, request.headers[HttpHeaders.Authorization], body)
            val (status, text) = if (request.url.encodedPath.startsWith("/auth/auth/tokens/key/")) {
                HttpStatusCode.OK to """{"context":"discovery-lookup","epoch":10,"public_key":"GOOD"}"""
            } else {
                issueReply
            }
            respond(
                text,
                status,
                headersOf(HttpHeaders.ContentType to listOf("application/json"), HttpHeaders.RetryAfter to listOf("77")),
            )
        }
        return KtorAnonTokenIssuer(baseUrl = "http://test.invalid/auth", engine = engine)
    }

    @Test
    fun theIssuerKeyComesFromTheGetRouteNeverFromTheIssuanceAnswer() = runBlocking {
        val seen = mutableListOf<Seen>()
        val api = issuer(
            seen,
            HttpStatusCode.OK to
                """{"context":"discovery-lookup","epoch":10,"evaluations":["a","b"],"proof":"p",""" +
                """"public_key":"EVIL","remaining":90,"epoch_ends_at":1100}""",
        )
        val core = FakeCore(goodKey = "GOOD")
        // The MockEngine answers on a real dispatcher, so this runs in real
        // time rather than the test scheduler's virtual time.
        val pool = AnonTokenPool(core, api, { alice }, this, nowSecs = { 1_000L })
        pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        assertEquals(listOf("GOOD"), core.keysUsed)
        val issue = seen.first { it.path == "/auth/auth/tokens" }
        assertEquals(HttpMethod.Post, issue.method)
        assertEquals("Bearer bearer-a", issue.authorization)
        val key = seen.first { it.path.startsWith("/auth/auth/tokens/key/") }
        assertEquals(HttpMethod.Get, key.method)
        assertEquals("/auth/auth/tokens/key/discovery-lookup/10", key.path)
        assertNull("the key route is anonymous", key.authorization)
    }

    @Test
    fun theKeyIsCachedForItsMaxAge() = runTest {
        val seen = mutableListOf<Seen>()
        var now = 0L
        val engine = MockEngine { request ->
            seen += Seen(request.method, request.url.encodedPath, null, null)
            respond(
                """{"context":"ds-knock","epoch":3,"public_key":"K"}""",
                HttpStatusCode.OK,
                headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        val api = KtorAnonTokenIssuer(baseUrl = "http://x/auth", engine = engine, nowMs = { now })
        assertEquals("K", api.publicKey(AnonTokenContext.DS_KNOCK, 3))
        now = 299_000L
        assertEquals("K", api.publicKey(AnonTokenContext.DS_KNOCK, 3))
        assertEquals(1, seen.size)
        now = 301_000L
        api.publicKey(AnonTokenContext.DS_KNOCK, 3)
        assertEquals(2, seen.size)
    }

    @Test
    fun theIssuerReadsItsRefusals() = runTest {
        suspend fun outcome(reply: Pair<HttpStatusCode, String>) =
            issuer(mutableListOf(), reply).issue(alice, "{}")
        assertEquals(
            AnonIssueOutcome.QuotaExhausted(3600),
            outcome(
                HttpStatusCode.TooManyRequests to
                    """{"error":"anon-token-quota-exhausted","remaining":0,"retry_after_secs":3600}""",
            ),
        )
        assertEquals(
            AnonIssueOutcome.QuotaExhausted(77),
            outcome(HttpStatusCode.TooManyRequests to """{"error":"anon-token-quota-exhausted"}"""),
        )
        assertEquals(
            AnonIssueOutcome.EpochNotIssuable(12),
            outcome(HttpStatusCode.BadRequest to """{"error":"epoch-not-issuable","current_epoch":12}"""),
        )
        assertEquals(
            AnonIssueOutcome.Disabled,
            outcome(HttpStatusCode.ServiceUnavailable to """{"error":"anon-tokens-disabled"}"""),
        )
        assertTrue(outcome(HttpStatusCode.InternalServerError to """{"error":"internal"}""") is AnonIssueOutcome.Failed)
    }

    @Test
    fun copyForAUsedUpAllowanceIsTheSpecs() {
        val quota = AnonTokenQuotaExhausted(AnonTokenContext.DISCOVERY_LOOKUP, 10)
        assertEquals(
            "You have looked up a lot of people today. Try again tomorrow.",
            anonTokenFailureCopy(RuntimeException("wrapped", quota), LOOKUP_ALLOWANCE_USED_COPY),
        )
        assertEquals(
            "You have used today's allowance for this. Try again tomorrow.",
            anonTokenFailureCopy(quota, ACTION_ALLOWANCE_USED_COPY),
        )
        assertEquals(
            ANON_TOKEN_NETWORK_COPY,
            anonTokenFailureCopy(AnonTokenDisabled(AnonTokenContext.DS_KNOCK), ACTION_ALLOWANCE_USED_COPY),
        )
        assertNull(anonTokenFailureCopy(RuntimeException("x"), ACTION_ALLOWANCE_USED_COPY))
    }
}
