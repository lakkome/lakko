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
import java.util.Collections
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancel
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
 * ADR-0054 S3 / ADR-0078 section 8 - the anonymous-token pool, host-side: the
 * native core and the issuer sit behind [AnonTokenCore] / [AnonTokenIssuerApi],
 * so no native library is loaded. What is pinned: a bound session tops every
 * context up to its target on a randomised schedule, and NOTHING a take does
 * starts an issuance; a take waits only for a refill the schedule runs; never
 * more than thirty-two held; a dead token is dropped; tokens are per account
 * (a logout keeps them on disk, a switch loads the other account's own, the
 * wipe removes one account's only); a batch is verified against the key from
 * the GET route and discarded when it does not verify; a used-up quota and a
 * disabled issuer surface as their own failures; a disagreeing epoch is
 * rebuilt once.
 *
 * The schedule runs in the test's background scope: the first refill of each
 * context fires at once (the random draw is 0), every later one waits until
 * the test calls [AnonTokenPool.runScheduledRefills], which is what a timer
 * tick does.
 */
class AnonTokenPoolTest {

    // ------------------------------------------------------------ fakes

    /** Epoch = unixSecs / window; a token is live in its epoch and the next. */
    private class FakeCore(private val goodKey: String = "GOOD") : AnonTokenCore {
        val begun: MutableList<Triple<AnonTokenContext, Long, Int>> = Collections.synchronizedList(mutableListOf())
        val keysUsed: MutableList<String> = Collections.synchronizedList(mutableListOf())
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
                    return synchronized(this@FakeCore) {
                        evaluations.map { PooledAnonToken(context, epoch, "${context.wire}-${serial++}") }
                    }
                }

                override fun close() = Unit
            }
        }

        fun issuesFor(context: AnonTokenContext): List<Triple<AnonTokenContext, Long, Int>> =
            synchronized(begun) { begun.filter { it.first == context } }
    }

    /** Issues [count]-many evaluations for the body's epoch, or the outcome
     *  scripted for the body's context. */
    private class FakeApi(var key: String? = "GOOD") : AnonTokenIssuerApi {
        val sessions: MutableList<AnonSession> = Collections.synchronizedList(mutableListOf())
        val scripted = HashMap<String, ArrayDeque<AnonIssueOutcome>>()
        var gate: CompletableDeferred<Unit>? = null

        fun script(context: AnonTokenContext, outcome: AnonIssueOutcome) {
            scripted.getOrPut(context.wire) { ArrayDeque() }.addLast(outcome)
        }

        override suspend fun issue(session: AnonSession, bodyJson: String): AnonIssueOutcome {
            sessions += session
            gate?.await()
            val context = Regex("\"context\":\"([a-z-]+)\"").find(bodyJson)!!.groupValues[1]
            synchronized(scripted) { scripted[context]?.removeFirstOrNull() }?.let { return it }
            val epoch = Regex("\"epoch\":(\\d+)").find(bodyJson)!!.groupValues[1].toLong()
            val count = Regex("\"count\":(\\d+)").find(bodyJson)!!.groupValues[1].toInt()
            return AnonIssueOutcome.Issued(epoch, List(count) { "e$it" }, "proof")
        }

        override suspend fun publicKey(context: AnonTokenContext, epoch: Long): String? = key
    }

    private val alice = AnonSession("alice", "bearer-a")
    private val bob = AnonSession("bob", "bearer-b")

    /** The first refill fires at once, every later one waits for the test. */
    private val parked: suspend (Long) -> Unit = { ms -> if (ms >= REFILL_INTERVAL_MIN_MS) awaitCancellation() }

    private fun TestScope.pool(
        core: FakeCore,
        api: FakeApi,
        session: () -> AnonSession?,
        now: () -> Long = { 1_000L },
        store: AnonTokenStore = NoAnonTokenStore,
        waitMs: Long = 5_000,
        target: (AnonTokenContext) -> Int = ::anonPoolTargetFor,
        forgotten: Set<String> = emptySet(),
        refillOnEmptyTake: Boolean = false,
    ) = AnonTokenPool(
        core = core,
        api = api,
        session = session,
        scope = this,
        nowSecs = now,
        waitMs = waitMs,
        target = target,
        store = store,
        forgottenAccounts = forgotten,
        scheduleScope = backgroundScope,
        sleep = parked,
        random = { 0.0 },
        refillOnEmptyTake = refillOnEmptyTake,
    )

    /** What a store on disk keeps, without the disk: one pool per account. */
    private class MemoryStore : AnonTokenStore {
        val pools = HashMap<String, PersistedAnonPool>()
        val deletes = mutableListOf<String>()

        fun held(owner: String, context: AnonTokenContext): Int =
            pools[owner]?.tokens?.count { it.context == context } ?: 0

        override fun load(accountKey: String): PersistedAnonPool? = pools[accountKey]
        override fun save(pool: PersistedAnonPool) {
            pools[pool.owner] = pool
        }
        override fun delete(accountKey: String) {
            deletes += accountKey
            pools.remove(accountKey)
        }
    }

    /** Run the schedule's background work too: `advanceUntilIdle` alone
     *  stops once only background tasks remain. */
    private fun TestScope.settle() {
        repeat(4) {
            runCurrent()
            advanceUntilIdle()
        }
    }

    private val lookup = AnonTokenContext.DISCOVERY_LOOKUP
    private val lookupTarget = anonPoolTargetFor(AnonTokenContext.DISCOVERY_LOOKUP)
    private val push = AnonTokenContext.PUSH_REGISTER

    // ------------------------------------------------------------ the schedule (ADR-0078 section 8)

    @Test
    fun aBoundSessionTopsEveryContextUpToItsTarget() = runTest {
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { alice })
        pool.bindSession()
        settle()
        val firstBatch = AnonTokenContext.entries.associateWith { core.issuesFor(it).single().third }
        assertEquals(
            mapOf(
                AnonTokenContext.DISCOVERY_LOOKUP to 32,
                AnonTokenContext.DISCOVERY_REGISTER to 2,
                AnonTokenContext.INVITE_CONSUME to 4,
                AnonTokenContext.DS_KNOCK to 8,
                AnonTokenContext.PUSH_REGISTER to 8,
                AnonTokenContext.CHANNEL_SUBSCRIBE to 4,
                AnonTokenContext.DIRECTORY_SEARCH to 4,
                AnonTokenContext.VAULT_CLAIM to 8,
                AnonTokenContext.BLOB_UPLOAD to 4,
                AnonTokenContext.BLOB_READ to 4,
                AnonTokenContext.MAILBOX_WRITE to 4,
                AnonTokenContext.CALL_JOIN to 4,
                AnonTokenContext.CIRCLE_CLAIM to 2,
            ),
            firstBatch,
        )
        for (context in AnonTokenContext.entries) {
            assertEquals(context.wire, anonPoolTargetFor(context), pool.held(context))
        }
    }

    @Test
    fun thePoolRefillsOnATimerNotOnSpend() = runTest {
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { alice })
        pool.bindSession()
        settle()
        val afterFirst = core.begun.size
        // Spend every lookup token: not one issuance follows.
        repeat(lookupTarget) { pool.take(lookup) }
        advanceUntilIdle()
        assertEquals("no spend starts an issuance", afterFirst, core.begun.size)
        val empty = runCatching { pool.take(lookup) }.exceptionOrNull()
        advanceUntilIdle()
        assertTrue("an empty context waits for the schedule", empty is AnonTokenUnavailable)
        assertEquals("not even an empty one", afterFirst, core.begun.size)
        // The timer fires: only what is missing is issued.
        pool.runScheduledRefills()
        advanceUntilIdle()
        assertEquals(afterFirst + 1, core.begun.size)
        assertEquals(Triple(lookup, 10L, lookupTarget), core.begun.last())
        assertEquals(lookupTarget, pool.held(lookup))
    }

    @Test
    fun aScheduledRefillTopsUpOnlyWhatIsMissing() = runTest {
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { alice })
        pool.bindSession()
        settle()
        repeat(3) { pool.take(push) }
        pool.runScheduledRefills()
        advanceUntilIdle()
        assertEquals(listOf(8, 3), core.issuesFor(push).map { it.third })
        assertEquals(8, pool.held(push))
    }

    @Test
    fun theScheduleIsRandomisedWithinItsWindows() = runTest {
        val slept = Collections.synchronizedList(mutableListOf<Long>())
        val pool = AnonTokenPool(
            core = FakeCore(),
            api = FakeApi(),
            session = { alice },
            scope = this,
            nowSecs = { 1_000L },
            scheduleScope = backgroundScope,
            sleep = { ms ->
                slept += ms
                if (ms >= REFILL_INTERVAL_MIN_MS) awaitCancellation()
            },
            random = { 0.5 },
        )
        pool.bindSession()
        settle()
        val contexts = AnonTokenContext.entries.size
        assertEquals(List(contexts) { INITIAL_REFILL_SPREAD_MS / 2 }, slept.filter { it < REFILL_INTERVAL_MIN_MS })
        val interval = REFILL_INTERVAL_MIN_MS + (REFILL_INTERVAL_MAX_MS - REFILL_INTERVAL_MIN_MS) / 2
        assertEquals(List(contexts) { interval }, slept.filter { it >= REFILL_INTERVAL_MIN_MS })
    }

    @Test
    fun aTakeOnAFreshSessionWaitsForItsFirstRefill() = runTest {
        val api = FakeApi()
        api.gate = CompletableDeferred()
        val pool = pool(FakeCore(), api, { alice })
        val taken = backgroundScope.async { pool.take(lookup) }
        runCurrent()
        assertTrue("waiting on the first scheduled refill", !taken.isCompleted)
        api.gate!!.complete(Unit)
        advanceUntilIdle()
        assertEquals(lookup, taken.await().context)
    }

    @Test
    fun theContextListIsTheCoresTwelveNamesInWireOrder() {
        // The source of truth is `TokenContext::ALL` / `TokenContext::name` in
        // `crates/lakko-anon-token/src/context.rs` (wire bytes 1..=12), which
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
                "mailbox-write",
                "call-join",
                "circle-claim",
            ),
            AnonTokenContext.entries.map { it.wire },
        )
    }

    // ------------------------------------------------------------ persistence (S5)

    @Test
    fun thePoolSurvivesARelaunch() = runTest {
        val store = MemoryStore()
        val core = FakeCore()
        val api = FakeApi()
        val first = pool(core, api, { alice }, store = store)
        val spent = first.take(lookup)
        advanceUntilIdle()
        assertEquals(lookupTarget - 1, store.held("alice", lookup))
        assertTrue(store.pools["alice"]!!.tokens.none { it.headerValue == spent.headerValue })
        val persisted = store.pools["alice"]!!.tokens.filter { it.context == lookup }.map { it.headerValue }.toSet()

        // A new process: a fresh pool over the same store.
        val relaunched = pool(core, api, { alice }, store = store)
        val next = relaunched.take(lookup)
        assertTrue("served from disk", next.headerValue in persisted)
    }

    @Test
    fun deadEpochTokensAreDroppedOnLoad() = runTest {
        val store = MemoryStore()
        store.pools["alice"] = PersistedAnonPool(
            owner = "alice",
            tokens = listOf(
                PooledAnonToken(AnonTokenContext.DS_KNOCK, 8, "dead"),
                PooledAnonToken(AnonTokenContext.DS_KNOCK, 9, "live-previous"),
                PooledAnonToken(AnonTokenContext.DS_KNOCK, 10, "live-current"),
            ),
        )
        val pool = pool(FakeCore(), FakeApi(), { alice }, store = store) // epoch 10
        assertEquals("live-previous", pool.take(AnonTokenContext.DS_KNOCK).headerValue)
        assertTrue(store.pools["alice"]!!.tokens.none { it.headerValue == "dead" })
        assertTrue(store.pools["alice"]!!.tokens.any { it.headerValue == "live-current" })
    }

    @Test
    fun logoutKeepsTheAccountsTokensForItsNextSignIn() = runTest {
        val store = MemoryStore()
        var who: AnonSession? = alice
        val pool = pool(FakeCore(), FakeApi(), { who }, store = store)
        pool.take(lookup)
        advanceUntilIdle()
        assertEquals(lookupTarget - 1, store.held("alice", lookup))
        // Log out: the memory goes, the disk does not.
        pool.releaseSession()
        who = null
        assertEquals(0, pool.held(lookup))
        assertEquals(lookupTarget - 1, store.held("alice", lookup))
        assertTrue(store.deletes.isEmpty())
        val persisted = store.pools["alice"]!!.tokens.map { it.headerValue }.toSet()
        // The same account signs in again: its tokens load.
        who = alice
        assertTrue(pool.take(lookup).headerValue in persisted)
    }

    @Test
    fun anotherAccountNeverLoadsThePersistedTokensAndLeavesThemOnDisk() = runTest {
        val store = MemoryStore()
        pool(FakeCore(), FakeApi(), { alice }, store = store).take(lookup)
        advanceUntilIdle()
        val aliceTokens = store.pools["alice"]!!.tokens
        val api = FakeApi()
        val asBob = pool(FakeCore(), api, { bob }, store = store)
        val token = asBob.take(lookup)
        advanceUntilIdle()
        // Bob's tokens came from issuances to Bob, not from Alice's pool.
        assertTrue(api.sessions.all { it == bob })
        assertTrue(token.headerValue !in aliceTokens.map { it.headerValue })
        assertEquals("bob", store.pools["bob"]!!.owner)
        // Alice's stay on disk, untouched.
        assertEquals(aliceTokens, store.pools["alice"]!!.tokens)
        assertTrue(store.deletes.isEmpty())
    }

    @Test
    fun theWipeRemovesOnlyTheWipedAccountsTokens() = runTest {
        val store = MemoryStore()
        var who = alice
        val pool = pool(FakeCore(), FakeApi(), { who }, store = store)
        pool.take(lookup)
        advanceUntilIdle()
        who = bob
        pool.take(lookup)
        advanceUntilIdle()
        // Bob is held in memory; wiping Alice leaves Bob's memory and disk.
        pool.forgetAccount("alice")
        assertNull(store.pools["alice"])
        assertEquals(lookupTarget - 1, store.held("bob", lookup))
        assertEquals(lookupTarget - 1, pool.held(lookup))
        // Wiping Bob (held) drops his memory and his file.
        pool.forgetAccount("bob")
        assertEquals(0, pool.held(lookup))
        assertNull(store.pools["bob"])
        assertEquals(listOf("alice", "bob"), store.deletes)
    }

    @Test
    fun aRefillInFlightAtTheWipeDoesNotWriteTheFileBack() = runTest {
        val store = MemoryStore()
        val api = FakeApi()
        api.gate = CompletableDeferred()
        val pool = pool(FakeCore(), api, { alice }, store = store)
        pool.bindSession()
        runCurrent()
        assertTrue("the first refills are out, waiting on the issuer", api.sessions.isNotEmpty())
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
        val pool = pool(core, FakeApi(), { alice }, store = store)
        pool.forgetAccount("alice")
        val err = runCatching { pool.take(lookup) }.exceptionOrNull()
        pool.bindSession()
        settle()
        assertTrue(err is AnonTokenUnavailable)
        assertTrue("nothing issued for a forgotten account", core.begun.isEmpty())
        assertNull(store.pools["alice"])
    }

    @Test
    fun aForgottenAccountsPoolIsNeitherSpentNorRewrittenByADetachedTake() = runTest {
        val store = MemoryStore()
        val pool = pool(FakeCore(), FakeApi(), { null }, store = store)
        pool.forgetAccount("alice")
        // A file that reappeared by some other route is not read back.
        store.pools["alice"] = PersistedAnonPool("alice", listOf(PooledAnonToken(push, 10, "old")))
        val t = pool.takeDetached(alice, push)
        assertTrue(t.headerValue != "old")
        assertEquals("left as it was, never rewritten", "old", store.pools["alice"]!!.tokens.single().headerValue)
    }

    @Test
    fun theAccountsNextLandingPersistsItsPoolAgain() = runTest {
        val store = MemoryStore()
        val pool = pool(FakeCore(), FakeApi(), { alice }, store = store)
        pool.forgetAccount("alice")
        pool.accountLanded("alice")
        pool.take(lookup)
        advanceUntilIdle()
        assertEquals(lookupTarget - 1, store.held("alice", lookup))
    }

    @Test
    fun aWipeRecordedBeforeThePoolWasBuiltStillHolds() = runTest {
        val store = MemoryStore()
        val pool = pool(FakeCore(), FakeApi(), { alice }, store = store, forgotten = setOf("alice"))
        assertTrue(runCatching { pool.take(lookup) }.exceptionOrNull() is AnonTokenUnavailable)
        assertNull(store.pools["alice"])
        // Bob is not affected.
        val bobPool = pool(FakeCore(), FakeApi(), { bob }, store = store, forgotten = setOf("alice"))
        bobPool.take(lookup)
        advanceUntilIdle()
        assertEquals(lookupTarget - 1, store.held("bob", lookup))
    }

    @Test
    fun aSignOutIsNotAForgetTheUnregisterStillSavesTheReducedPool() = runTest {
        val store = MemoryStore()
        var who: AnonSession? = alice
        val pool = pool(FakeCore(), FakeApi(), { who }, store = store)
        pool.take(push)
        advanceUntilIdle()
        assertEquals(7, store.held("alice", push))
        pool.releaseSession()
        who = null
        pool.takeDetached(alice, push)
        assertEquals(6, store.held("alice", push))
    }

    @Test
    fun aDetachedTakeAfterLogoutDrawsFromThatAccountsDiskPool() = runTest {
        val store = MemoryStore()
        var who: AnonSession? = alice
        val api = FakeApi()
        val pool = pool(FakeCore(), api, { who }, store = store)
        pool.take(push)
        advanceUntilIdle()
        assertEquals(7, store.held("alice", push))
        pool.releaseSession()
        who = null
        val issuedBefore = api.sessions.size
        val t = pool.takeDetached(alice, push)
        assertEquals(push, t.context)
        assertEquals("served from disk: no new issuance", issuedBefore, api.sessions.size)
        assertEquals(6, store.held("alice", push))
        // Bob's detached take never draws from Alice's pool.
        pool.takeDetached(bob, push)
        assertEquals(bob, api.sessions.last())
        assertEquals(6, store.held("alice", push))
    }

    // ------------------------------------------------------------ holding

    @Test
    fun aContextNeverHoldsMoreThanThirtyTwo() = runTest {
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { alice }, target = { 100 })
        pool.bindSession()
        settle()
        pool.runScheduledRefills()
        advanceUntilIdle()
        for (context in AnonTokenContext.entries) {
            assertTrue(pool.held(context) <= AnonTokenPool.MAX_HELD)
        }
        assertTrue(core.begun.all { it.third in 1..AnonTokenPool.MAX_HELD })
    }

    @Test
    fun aTokenPastItsLiveWindowIsDropped() = runTest {
        var now = 1_000L
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { alice }, now = { now })
        pool.take(lookup) // epoch 10
        advanceUntilIdle()
        now = 1_250L // epoch 12: the epoch-10 tokens are dead
        assertTrue(runCatching { pool.take(lookup) }.exceptionOrNull() is AnonTokenUnavailable)
        pool.runScheduledRefills()
        advanceUntilIdle()
        val t = pool.take(lookup)
        assertEquals(12L, t.epoch)
        assertEquals(12L, core.issuesFor(lookup).last().second)
    }

    // ------------------------------------------------------------ release

    @Test
    fun logoutReleasesTheMemoryAndStopsTheSchedule() = runTest {
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { alice })
        pool.bindSession()
        settle()
        assertEquals(lookupTarget, pool.held(lookup))
        pool.releaseSession()
        assertEquals(0, pool.held(lookup))
        val issued = core.begun.size
        pool.runScheduledRefills()
        advanceUntilIdle()
        assertEquals("a released pool refills nothing", issued, core.begun.size)
    }

    @Test
    fun anAccountSwitchNeverHandsOverTheOtherAccountsTokens() = runTest {
        var who = alice
        val api = FakeApi()
        val pool = pool(FakeCore(), api, { who })
        val a = pool.take(lookup)
        advanceUntilIdle()
        who = bob
        val b = pool.take(lookup)
        assertTrue(a.headerValue != b.headerValue)
        assertEquals(bob, api.sessions.last())
    }

    @Test
    fun noSessionMeansNoTokenAndNoIssuance() = runTest {
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { null })
        pool.bindSession()
        val err = runCatching { pool.take(push) }.exceptionOrNull()
        advanceUntilIdle()
        assertTrue(err is AnonTokenUnavailable)
        assertTrue(core.begun.isEmpty())
    }

    @Test
    fun aDetachedTakeUsesTheGivenSessionAndPoolsNothing() = runTest {
        val core = FakeCore()
        val api = FakeApi()
        val pool = pool(core, api, { null })
        val t = pool.takeDetached(alice, push)
        assertEquals(push, t.context)
        assertEquals(listOf(alice), api.sessions.toList())
        assertEquals(1, core.begun.single().third)
        assertEquals(0, pool.held(push))
    }

    // ------------------------------------------------------------ issuer outcomes

    @Test
    fun aBatchIsVerifiedAgainstTheKeyRouteAndDiscardedWhenItDoesNotVerify() = runTest {
        val core = FakeCore(goodKey = "GOOD")
        val api = FakeApi(key = "SOMEONE-ELSES")
        val pool = pool(core, api, { alice })
        val err = runCatching { pool.take(lookup) }.exceptionOrNull()
        advanceUntilIdle()
        assertTrue(err is AnonTokenUnavailable)
        assertTrue(core.keysUsed.isNotEmpty() && core.keysUsed.all { it == "SOMEONE-ELSES" })
        assertEquals("nothing of the batch is kept", 0, pool.held(lookup))
    }

    @Test
    fun aUsedUpQuotaSurfacesAsSuchAndIsNotAskedAgainBeforeTheWindowRollsOver() = runTest {
        var now = 1_000L
        val core = FakeCore()
        val api = FakeApi()
        api.script(lookup, AnonIssueOutcome.QuotaExhausted(retryAfterSecs = 50))
        val pool = pool(core, api, { alice }, now = { now })
        val err = runCatching { pool.take(lookup) }.exceptionOrNull()
        assertTrue(err is AnonTokenQuotaExhausted)
        assertEquals(50L, (err as AnonTokenQuotaExhausted).retryAfterSecs)
        pool.runScheduledRefills()
        advanceUntilIdle()
        val again = runCatching { pool.take(lookup) }.exceptionOrNull()
        assertTrue(again is AnonTokenQuotaExhausted)
        assertEquals("the issuer is not asked again", 1, core.issuesFor(lookup).size)
        now = 1_060L
        pool.runScheduledRefills()
        advanceUntilIdle()
        pool.take(lookup)
        assertEquals(2, core.issuesFor(lookup).size)
    }

    // ------------------------------------------------------------ test builds

    @Test
    fun aReleaseTakeOnAnEmptyContextStartsNoIssuance() = runTest {
        val core = FakeCore()
        val pool = pool(core, FakeApi(), { alice })
        pool.bindSession()
        settle()
        repeat(lookupTarget) { pool.take(lookup) }
        val issued = core.issuesFor(lookup).size
        assertTrue(runCatching { pool.take(lookup) }.exceptionOrNull() is AnonTokenUnavailable)
        advanceUntilIdle()
        assertEquals(issued, core.issuesFor(lookup).size)
    }

    @Test
    fun aTestBuildTakeOnAnEmptyContextStartsTheIssuanceAndWaitsForIt() = runTest {
        val core = FakeCore()
        val api = FakeApi()
        val pool = pool(core, api, { alice }, refillOnEmptyTake = true)
        pool.bindSession()
        settle()
        repeat(lookupTarget) { pool.take(lookup) }
        val issued = core.issuesFor(lookup).size
        api.gate = CompletableDeferred()
        val taken = backgroundScope.async { pool.take(lookup) }
        runCurrent()
        assertTrue("it waits for the issuance it started", !taken.isCompleted)
        assertEquals(issued + 1, core.issuesFor(lookup).size)
        api.gate!!.complete(Unit)
        advanceUntilIdle()
        assertEquals(lookup, taken.await().context)
        assertEquals(lookupTarget - 1, pool.held(lookup))
    }

    @Test
    fun aTestBuildTakeReportsWhatItsIssuanceMet() = runTest {
        val core = FakeCore()
        val api = FakeApi()
        val pool = pool(core, api, { alice }, refillOnEmptyTake = true)
        pool.bindSession()
        settle()
        repeat(lookupTarget) { pool.take(lookup) }
        api.script(lookup, AnonIssueOutcome.QuotaExhausted(retryAfterSecs = 50))
        val err = runCatching { pool.take(lookup) }.exceptionOrNull()
        assertTrue(err is AnonTokenQuotaExhausted)
        assertEquals(50L, (err as AnonTokenQuotaExhausted).retryAfterSecs)
    }

    @Test
    fun aDisabledIssuerIsItsOwnFailure() = runTest {
        val api = FakeApi()
        api.script(AnonTokenContext.DS_KNOCK, AnonIssueOutcome.Disabled)
        val err = runCatching {
            pool(FakeCore(), api, { alice }).take(AnonTokenContext.DS_KNOCK)
        }.exceptionOrNull()
        assertTrue(err is AnonTokenDisabled)
    }

    @Test
    fun aDisagreeingEpochIsRebuiltOnceForTheIssuersEpoch() = runTest {
        val core = FakeCore()
        val api = FakeApi()
        api.script(lookup, AnonIssueOutcome.EpochNotIssuable(currentEpoch = 11))
        val pool = pool(core, api, { alice })
        val t = pool.take(lookup)
        advanceUntilIdle()
        assertEquals(11L, t.epoch)
        assertEquals(listOf(10L, 11L), core.issuesFor(lookup).map { it.second })
        // The correction sticks: the next batch is built for 11 straight away.
        repeat(3) { pool.take(lookup) }
        pool.runScheduledRefills()
        advanceUntilIdle()
        assertEquals(11L, core.issuesFor(lookup).last().second)
    }

    @Test
    fun aTakeThatOutwaitsTheRefillFailsAsUnavailable() = runTest {
        val api = FakeApi()
        api.gate = CompletableDeferred()
        val pool = pool(FakeCore(), api, { alice }, waitMs = 1_000)
        val err = runCatching { pool.take(lookup) }.exceptionOrNull()
        assertTrue(err is AnonTokenUnavailable)
        api.gate!!.complete(Unit)
        advanceUntilIdle()
        // The refill still lands for the next action.
        assertEquals(lookupTarget, pool.held(lookup))
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
        // time rather than the test scheduler's virtual time. Only the lookup
        // context is scheduled, and the schedule's own scope is cancelled at
        // the end.
        val scheduleScope = CoroutineScope(SupervisorJob())
        val pool = AnonTokenPool(
            core, api, { alice }, this, nowSecs = { 1_000L },
            target = { if (it == AnonTokenContext.DISCOVERY_LOOKUP) 2 else 0 },
            scheduleScope = scheduleScope,
            sleep = { ms -> if (ms >= REFILL_INTERVAL_MIN_MS) awaitCancellation() },
            random = { 0.0 },
        )
        pool.take(AnonTokenContext.DISCOVERY_LOOKUP)
        scheduleScope.cancel()
        assertEquals(listOf("GOOD"), core.keysUsed.toList())
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
