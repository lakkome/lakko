// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.anon

import java.util.EnumMap
import kotlin.random.Random
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.coroutines.cancellation.CancellationException

/*
 * ADR-0054 S3 - the anonymous-token pool.
 *
 * A handful of anonymous actions (looking someone up, registering a channel
 * name, consuming an invite, knocking on a group link, registering this device
 * for pushes) each spend one token that `services/auth` issued to the signed-in
 * session. The token names no account: the verifier learns only that some
 * account the issuer rationed produced it. So the pool draws tokens WITH the
 * session, in batches and ahead of need, and the routes that spend them go out
 * with no session attached (see [AnonymousHttp]).
 *
 * ADR-0078 section 8: the pool is refilled on a randomised schedule that does
 * not look at spending. When a session is bound each context is topped up to
 * its target (about a day of use plus a reserve, [anonPoolTargetFor]) once at a
 * random moment within [INITIAL_REFILL_SPREAD_MS], and after that at
 * independent random intervals between [REFILL_INTERVAL_MIN_MS] and
 * [REFILL_INTERVAL_MAX_MS]. A take never starts an issuance: before ADR-0078 a
 * take below a low-water mark started one, so the bearer issuance landed
 * seconds from the anonymous spend it refilled for and the issuer and the
 * spending service could pair them by time. An action that finds its context
 * empty waits only for a refill the schedule already has running, else fails
 * until the next scheduled refill.
 *
 * Issuance and spending do not share a connection: the issuer runs on its own
 * Ktor client ([KtorAnonTokenIssuer], its own OkHttp connection pool), and
 * every spend goes out on the anonymous clients ([AnonymousHttp]), which never
 * carry the session.
 *
 * Everything that touches the native core sits behind [AnonTokenCore] and
 * everything that touches the network behind [AnonTokenIssuerApi], so the pool
 * rules (per-context targets up to thirty-two, the schedule, drop a dead token,
 * persist per account across a relaunch and a logout, wait bounded then fail)
 * run in a host JVM test with no native library.
 */

/** The rationed actions, in wire order (`anonTokenContexts()`, whose source
 *  of truth is `TokenContext::ALL` in `crates/lakko-anon-token/src/context.rs`);
 *  `channel-subscribe` and `directory-search` are ADR-0059, `vault-claim` is
 *  ADR-0064, `blob-upload` and `blob-read` are ADR-0065 (nothing spends those
 *  three yet), `mailbox-write` is ADR-0078 phase 1, `call-join` is ADR-0078
 *  section 2 (one per call token). */
public enum class AnonTokenContext(public val wire: String) {
    DISCOVERY_LOOKUP("discovery-lookup"),
    DISCOVERY_REGISTER("discovery-register"),
    INVITE_CONSUME("invite-consume"),
    DS_KNOCK("ds-knock"),
    PUSH_REGISTER("push-register"),
    CHANNEL_SUBSCRIBE("channel-subscribe"),
    DIRECTORY_SEARCH("directory-search"),
    VAULT_CLAIM("vault-claim"),
    BLOB_UPLOAD("blob-upload"),
    BLOB_READ("blob-read"),
    MAILBOX_WRITE("mailbox-write"),
    CALL_JOIN("call-join"),
}

/**
 * How many tokens a context is topped up to at each scheduled refill
 * (ADR-0078 section 8): about a day of use plus a reserve, so no spend waits
 * for an issuance. The same numbers as web (`POOL_TARGET`) and iOS
 * (`AnonTokenContext.poolTarget`); chosen, not measured.
 */
public fun anonPoolTargetFor(context: AnonTokenContext): Int = when (context) {
    AnonTokenContext.DISCOVERY_LOOKUP -> 16
    AnonTokenContext.PUSH_REGISTER -> 8
    AnonTokenContext.INVITE_CONSUME -> 4
    AnonTokenContext.DS_KNOCK -> 8
    AnonTokenContext.DISCOVERY_REGISTER -> 2
    AnonTokenContext.CHANNEL_SUBSCRIBE -> 4
    AnonTokenContext.DIRECTORY_SEARCH -> 4
    AnonTokenContext.VAULT_CLAIM -> 8
    AnonTokenContext.BLOB_UPLOAD -> 4
    AnonTokenContext.BLOB_READ -> 4
    AnonTokenContext.MAILBOX_WRITE -> 4
    // ADR-0078 section 2: a join, a reconnect, the TURN credential of a
    // one-to-one call. Chosen, not measured.
    AnonTokenContext.CALL_JOIN -> 4
}

/** The first scheduled refill of a newly bound session runs at a random
 *  moment within this many milliseconds (ADR-0078 section 8; chosen). */
public const val INITIAL_REFILL_SPREAD_MS: Long = 20_000L

/** After the first, each context refills at an independent random interval
 *  between these two (ADR-0078 section 8; chosen). */
public const val REFILL_INTERVAL_MIN_MS: Long = 30L * 60_000L
public const val REFILL_INTERVAL_MAX_MS: Long = 90L * 60_000L

/** One finalized token, ready to spend in the `x-lakko-anon-token` header. */
public data class PooledAnonToken(
    val context: AnonTokenContext,
    val epoch: Long,
    val headerValue: String,
)

/** The session a batch is issued to. [accountKey] keys the pool, so an account
 *  switch never hands one account's tokens to another. */
public data class AnonSession(val accountKey: String, val bearer: String)

/** The native half, behind an interface so the pool is host-testable. */
public interface AnonTokenCore {
    /** `anonTokenHeaderName()`. */
    public val headerName: String

    /** `anonTokenDefaultWindowSecs()`. */
    public val defaultWindowSecs: Long

    /** `anonTokenEpochAt(unixSecs, windowSecs)`. */
    public fun epochAt(unixSecs: Long, windowSecs: Long): Long

    /** `anonTokenIsLive(tokenEpoch, currentEpoch)`. */
    public fun isLive(tokenEpoch: Long, currentEpoch: Long): Boolean

    /** `AnonTokenRequest(context, epoch, count)`. */
    public fun begin(context: AnonTokenContext, epoch: Long, count: Int): AnonIssuance
}

/** One blinded batch awaiting the issuer. Holds the blinding factors. */
public interface AnonIssuance : AutoCloseable {
    /** The `POST /auth/tokens` body. */
    public fun issueBodyJson(): String

    /** Verify the proof against [publicKey] (from the key route, never from the
     *  issuance response) and unblind. All or nothing.
     *  @throws AnonFinalizeFailed on any refusal, `proof-verification` included. */
    public fun finalize(
        evaluations: List<String>,
        proof: String,
        publicKey: String,
    ): List<PooledAnonToken>
}

/** A batch the core refused to finalize; [code] is the core's frozen code
 *  (`proof-verification` when the issuer did not evaluate under the key). The
 *  batch is discarded. */
public class AnonFinalizeFailed(public val code: String, cause: Throwable? = null) :
    RuntimeException("anon-token finalize refused: $code", cause)

/** What the issuer answered to one batch. */
public sealed interface AnonIssueOutcome {
    /** 200: evaluations for [epoch]. */
    public data class Issued(
        val epoch: Long,
        val evaluations: List<String>,
        val proof: String,
    ) : AnonIssueOutcome

    /** 400 `epoch-not-issuable`: the issuer is at [currentEpoch]. */
    public data class EpochNotIssuable(val currentEpoch: Long) : AnonIssueOutcome

    /** 429 `anon-token-quota-exhausted`: the account's allowance for this
     *  context and window is used up. */
    public data class QuotaExhausted(val retryAfterSecs: Long) : AnonIssueOutcome

    /** 503 `anon-tokens-disabled`. */
    public data object Disabled : AnonIssueOutcome

    /** Anything else (network, 5xx, a malformed answer). */
    public data class Failed(val cause: Throwable?) : AnonIssueOutcome
}

/** The issuer's two routes. */
public interface AnonTokenIssuerApi {
    /** `POST /auth/tokens` WITH the session. */
    public suspend fun issue(session: AnonSession, bodyJson: String): AnonIssueOutcome

    /** `GET /auth/tokens/key/{context}/{epoch}` (anonymous), or `null` when it
     *  could not be fetched. */
    public suspend fun publicKey(context: AnonTokenContext, epoch: Long): String?
}

// -----------------------------------------------------------------------------
// Failures a spending action sees
// -----------------------------------------------------------------------------

/** No token could be had for an action. Never a lost session: nothing here
 *  signs anyone out. */
public sealed class AnonTokenException(message: String, cause: Throwable? = null) :
    RuntimeException(message, cause) {
    public abstract val context: AnonTokenContext
}

/** The account's allowance for [context] is used up until the window rolls
 *  over ([retryAfterSecs]). The action shows its own "try again tomorrow". */
public class AnonTokenQuotaExhausted(
    override val context: AnonTokenContext,
    public val retryAfterSecs: Long,
) : AnonTokenException("anonymous-token allowance for ${context.wire} used up")

/** Tokens are switched off on the server: the generic network failure copy. */
public class AnonTokenDisabled(override val context: AnonTokenContext) :
    AnonTokenException("anonymous tokens are disabled on the server")

/** No token arrived in time (no session, a network failure, a batch whose
 *  proof did not verify): the generic network failure copy. */
public class AnonTokenUnavailable(
    override val context: AnonTokenContext,
    reason: String,
    cause: Throwable? = null,
) : AnonTokenException("no anonymous token for ${context.wire}: $reason", cause)

/** `true` when [error] (or anything in its cause chain) is a used-up daily
 *  allowance for an anonymous action. */
public fun isAnonTokenQuotaExhausted(error: Throwable): Boolean = anonTokenQuota(error) != null

/** The quota failure in [error]'s cause chain, or `null`. */
public fun anonTokenQuota(error: Throwable): AnonTokenQuotaExhausted? {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is AnonTokenQuotaExhausted) return cause
        cause = cause.cause
    }
    return null
}

/** UX `docs/ux/anti-abuse.md` section 3 - search. */
public const val LOOKUP_ALLOWANCE_USED_COPY: String =
    "You have looked up a lot of people today. Try again tomorrow."

/** UX section 3 - invite link, group link, channel name. */
public const val ACTION_ALLOWANCE_USED_COPY: String =
    "You have used today's allowance for this. Try again tomorrow."

// -----------------------------------------------------------------------------
// The pool
// -----------------------------------------------------------------------------

/** Where a spending route draws its token. */
public interface AnonTokenSource {
    /** The request header a token travels in. */
    public val headerName: String

    /** One live token for [context]; waits (bounded) for a refill when none is
     *  at hand. @throws AnonTokenException when none can be had. */
    public suspend fun take(context: AnonTokenContext): PooledAnonToken

    /** One token drawn with an explicit [session] and never pooled: the
     *  logout path, which spends one push-register token after the live
     *  session is gone. Takes a pooled token of that account first. */
    public suspend fun takeDetached(session: AnonSession, context: AnonTokenContext): PooledAnonToken

    /** Drop the tokens held in memory and forget which account they belong
     *  to (Log out). The account's persisted tokens stay for its next
     *  sign-in; nothing on disk is touched. */
    public fun releaseSession()
}

/**
 * Per (account, context), in memory and mirrored to [store] so the pool
 * survives a relaunch (tokens are live in their epoch and the next). Each
 * context is topped up to its [target] (never past [max] held) on the
 * schedule above, run in [scheduleScope] with [sleep] between refills; an
 * action with no token at hand waits up to [waitMs] for a refill the schedule
 * already runs, then fails. A refill that met a used-up quota remembers it
 * until the window rolls over, so the issuer is not asked again meanwhile.
 *
 * Tokens are account data (maintainer decision, 2026-10-01). The memory holds
 * one account at a time; the store holds every account's pool separately.
 * Adopting a different account drops the memory and loads that account's own
 * persisted tokens, leaving every other account's on disk; [releaseSession]
 * (Log out) drops the memory only; [forgetAccount] (the Forget device / Delete
 * account wipe) removes one account's tokens from memory and disk, and nothing
 * is written for that account again until [accountLanded]. A token is
 * only ever handed to a request made under the account it was issued to.
 * Dead-epoch tokens are dropped on load.
 */
public class AnonTokenPool(
    private val core: AnonTokenCore,
    private val api: AnonTokenIssuerApi,
    private val session: () -> AnonSession?,
    private val scope: CoroutineScope,
    private val nowSecs: () -> Long = { System.currentTimeMillis() / 1000L },
    private val waitMs: Long = DEFAULT_WAIT_MS,
    private val target: (AnonTokenContext) -> Int = ::anonPoolTargetFor,
    private val max: Int = MAX_HELD,
    private val store: AnonTokenStore = NoAnonTokenStore,
    /** Accounts forgotten before this pool was built ([AnonTokens] records
     *  a wipe that ran while no pool existed). */
    forgottenAccounts: Set<String> = emptySet(),
    /** Where the refill schedule runs (tests pass a background scope). */
    private val scheduleScope: CoroutineScope = scope,
    /** The wait between scheduled refills (tests drive the schedule). */
    private val sleep: suspend (Long) -> Unit = { delay(it) },
    /** Uniform in `[0, 1)`: where in its window each refill falls. */
    private val random: () -> Double = { Random.nextDouble() },
) : AnonTokenSource {

    override val headerName: String get() = core.headerName

    private val lock = Any()
    private var generation: Long = 0
    private var owner: String? = null

    /**
     * Accounts whose tokens a wipe removed ([forgetAccount]) and that have
     * not landed on this device again ([accountLanded]). Guarded by [lock]:
     * checked on the same lock every write to [store] runs under, so no take,
     * refill or detached spend that raced the wipe can write a forgotten
     * account's file back. A sign-out is not a forget: the logout's push
     * unregister still spends from and saves its account's pool.
     *
     * Kept here rather than asked of [me.lakko.app.account.AccountStoreGate]:
     * the wipe holds the gate's exclusive lock while it takes this one, so a
     * gate read under [lock] would invert that order.
     */
    private val forgotten: HashSet<String> = HashSet(forgottenAccounts)
    private val queues = EnumMap<AnonTokenContext, ArrayDeque<PooledAnonToken>>(AnonTokenContext::class.java)
    private val refills = EnumMap<AnonTokenContext, Refill>(AnonTokenContext::class.java)
    private val exhaustedUntil = EnumMap<AnonTokenContext, Long>(AnonTokenContext::class.java)

    /** Each context's schedule for the bound account. */
    private val schedules = EnumMap<AnonTokenContext, Job>(AnonTokenContext::class.java)

    /** Completed once a context's first scheduled refill of this session
     *  ended: what a take on an empty context waits for before it. */
    private val initials = EnumMap<AnonTokenContext, CompletableDeferred<Unit>>(AnonTokenContext::class.java)

    /** How the last scheduled refill of each context ended, when it brought
     *  nothing: what a take that finds the context empty reports. */
    private val lastFailure = EnumMap<AnonTokenContext, RefillResult>(AnonTokenContext::class.java)

    /** The issuer's epoch minus the local one, learned from
     *  `epoch-not-issuable`; zero until the two disagree. */
    private var epochCorrection: Long = 0

    private class Refill(val id: Long, val result: Deferred<RefillResult>)

    private var nextRefillId: Long = 0

    /** How one issuance batch ended. */
    internal sealed interface RefillResult {
        data class Filled(val tokens: List<PooledAnonToken>) : RefillResult
        data class Quota(val retryAfterSecs: Long) : RefillResult
        data object Disabled : RefillResult
        data class Failed(val reason: String, val cause: Throwable?) : RefillResult
    }

    /** Tokens held for [context] right now (live or not); for tests and logs. */
    public fun held(context: AnonTokenContext): Int = synchronized(lock) { queues[context]?.size ?: 0 }

    /**
     * Bind the signed-in session, if any, and start its refill schedule
     * (ADR-0078 section 8). `AuthRepository` calls this whenever the session
     * changes; a take binds too, for a pool nothing bound yet. Idempotent.
     */
    public fun bindSession() {
        val s = session() ?: return
        synchronized(lock) {
            if (s.accountKey in forgotten) return
            adoptLocked(s)
        }
    }

    override suspend fun take(context: AnonTokenContext): PooledAnonToken {
        val s = session() ?: throw AnonTokenUnavailable(context, "no signed-in session")
        val (token, pending) = synchronized(lock) {
            // A take that read the session before a forget-device cleared it
            // neither re-adopts the account nor refills: either would write
            // its file back after the wipe removed it.
            if (s.accountKey in forgotten) throw AnonTokenUnavailable(context, "the account was forgotten on this device")
            adoptLocked(s)
            val t = popLiveLocked(context)
            // ADR-0078 section 8: a take never starts an issuance. It may
            // wait for a refill the schedule runs (in flight, or this
            // session's first).
            val waitFor: Deferred<*>? = refills[context]?.result ?: initials[context]
            t to waitFor
        }
        if (token != null) return token
        synchronized(lock) {
            val until = exhaustedUntil[context]
            val now = nowSecs()
            if (until != null && now < until) throw AnonTokenQuotaExhausted(context, until - now)
        }
        if (pending == null) {
            throw failureFor(context, synchronized(lock) { lastFailure[context] }, timedOut = false, empty = true)
        }
        val finished = withTimeoutOrNull(waitMs) { pending.await() } != null
        synchronized(lock) {
            if (owner == s.accountKey) popLiveLocked(context)?.let { return it }
        }
        throw failureFor(context, synchronized(lock) { lastFailure[context] }, timedOut = !finished, empty = false)
    }

    /** Run one scheduled top-up of every context now, awaited (what the
     *  schedule does one context at a time; tests and diagnostics). */
    public suspend fun runScheduledRefills() {
        val gen = synchronized(lock) { generation }
        for (context in AnonTokenContext.entries) scheduledRefill(context, gen)
    }

    override suspend fun takeDetached(session: AnonSession, context: AnonTokenContext): PooledAnonToken {
        synchronized(lock) {
            if (owner == session.accountKey) {
                popLiveLocked(context)?.let { return it }
            } else {
                // The logout path: the memory was already released, the
                // account's tokens are still on disk.
                popPersistedLocked(session.accountKey, context)?.let { return it }
            }
        }
        val result = issueBatch(context, session, count = 1)
        if (result is RefillResult.Filled && result.tokens.isNotEmpty()) return result.tokens.first()
        throw failureFor(context, result, timedOut = false, empty = false)
    }

    override fun releaseSession() {
        synchronized(lock) { resetLocked(newOwner = null) }
    }

    /** Remove [accountKey]'s tokens, held and persisted, and write nothing
     *  for it again until it lands on this device ([accountLanded]). Another
     *  account's stay, in memory and on disk. */
    public fun forgetAccount(accountKey: String) {
        synchronized(lock) {
            forgotten.add(accountKey)
            if (owner == accountKey) resetLocked(newOwner = null)
            runCatching { store.delete(accountKey) }
        }
    }

    /** [accountKey] signed in on this device again: its pool is kept and
     *  written as before. */
    public fun accountLanded(accountKey: String) {
        synchronized(lock) { forgotten.remove(accountKey) }
    }

    private fun failureFor(
        context: AnonTokenContext,
        result: RefillResult?,
        timedOut: Boolean,
        empty: Boolean,
    ): AnonTokenException = when {
        timedOut -> AnonTokenUnavailable(context, "the refill did not finish in $waitMs ms")
        result is RefillResult.Quota -> AnonTokenQuotaExhausted(context, result.retryAfterSecs)
        result == RefillResult.Disabled -> AnonTokenDisabled(context)
        result is RefillResult.Failed -> AnonTokenUnavailable(context, result.reason, result.cause)
        result is RefillResult.Filled -> AnonTokenUnavailable(context, "the refill brought no live token")
        empty -> AnonTokenUnavailable(context, "pool empty until the next scheduled refill")
        else -> AnonTokenUnavailable(context, "no refill possible")
    }

    private fun adoptLocked(s: AnonSession) {
        if (owner != s.accountKey) {
            resetLocked(newOwner = s.accountKey)
            loadLocked(s.accountKey)
            startScheduleLocked()
        }
    }

    /** Arm every context's schedule for the bound account: a first refill
     *  within [INITIAL_REFILL_SPREAD_MS], then one every
     *  [REFILL_INTERVAL_MIN_MS]..[REFILL_INTERVAL_MAX_MS], independently. */
    private fun startScheduleLocked() {
        val gen = generation
        for (context in AnonTokenContext.entries) {
            val first = CompletableDeferred<Unit>()
            initials[context] = first
            schedules[context] = scheduleScope.launch {
                try {
                    sleep((random() * INITIAL_REFILL_SPREAD_MS).toLong())
                    scheduledRefill(context, gen)
                } finally {
                    first.complete(Unit)
                    synchronized(lock) { if (initials[context] === first) initials.remove(context) }
                }
                val span = REFILL_INTERVAL_MAX_MS - REFILL_INTERVAL_MIN_MS
                while (true) {
                    sleep(REFILL_INTERVAL_MIN_MS + (random() * span).toLong())
                    if (synchronized(lock) { generation != gen }) return@launch
                    scheduledRefill(context, gen)
                }
            }
        }
    }

    /** One scheduled top-up of [context] to its target. Never throws; a
     *  refill that brought nothing is remembered for the next take. */
    private suspend fun scheduledRefill(context: AnonTokenContext, gen: Long) {
        val s = session() ?: return
        val pending = synchronized(lock) {
            if (generation != gen || owner != s.accountKey || s.accountKey in forgotten) return
            val queue = queues[context]
            if (queue != null) {
                val current = currentEpochLocked()
                if (queue.removeAll { !core.isLive(it.epoch, current) }) persistLocked()
            }
            if ((queues[context]?.size ?: 0) >= target(context)) return
            ensureRefillLocked(context, s)
        } ?: return
        val result = try {
            pending.await()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            RefillResult.Failed("refill threw", e)
        }
        synchronized(lock) {
            if (generation != gen) return
            if (result is RefillResult.Filled && result.tokens.isNotEmpty()) {
                lastFailure.remove(context)
            } else {
                lastFailure[context] = result
            }
        }
    }

    /** Take over what [store] holds for [accountKey], dropping dead-epoch
     *  tokens. Other accounts' persisted pools are not touched. */
    private fun loadLocked(accountKey: String) {
        val persisted = runCatching { store.load(accountKey) }.getOrNull()
        if (persisted == null || persisted.owner != accountKey) return
        val current = currentEpochLocked()
        for (t in persisted.tokens) {
            if (!core.isLive(t.epoch, current)) continue
            val queue = queues.getOrPut(t.context) { ArrayDeque() }
            if (queue.size < max) queue.addLast(t)
        }
        persistLocked()
    }

    /** One live token of [context] from [accountKey]'s persisted pool, for an
     *  account the memory does not hold; the rest is written back (dead
     *  tokens dropped). */
    private fun popPersistedLocked(accountKey: String, context: AnonTokenContext): PooledAnonToken? {
        // Its file was removed by the wipe; whatever is there now was not
        // left by this account's session and is neither spent nor rewritten.
        if (accountKey in forgotten) return null
        val persisted = runCatching { store.load(accountKey) }.getOrNull() ?: return null
        if (persisted.owner != accountKey) return null
        val current = currentEpochLocked()
        val live = persisted.tokens.filter { core.isLive(it.epoch, current) }
        val found = live.firstOrNull { it.context == context }
        val rest = if (found == null) live else live - found
        if (rest != persisted.tokens) runCatching { store.save(PersistedAnonPool(accountKey, rest)) }
        return found
    }

    /** Mirror the queues to [store]. A store failure only costs the relaunch
     *  survival; the pool itself carries on. */
    private fun persistLocked() {
        val who = owner ?: return
        if (who in forgotten) return
        val tokens = queues.values.flatten()
        runCatching { store.save(PersistedAnonPool(who, tokens)) }
    }

    private fun resetLocked(newOwner: String?) {
        generation += 1
        owner = newOwner
        queues.clear()
        for (refill in refills.values) refill.result.cancel()
        refills.clear()
        for (job in schedules.values) job.cancel()
        schedules.clear()
        for (first in initials.values) first.complete(Unit)
        initials.clear()
        lastFailure.clear()
        exhaustedUntil.clear()
        epochCorrection = 0
    }

    private fun currentEpochLocked(): Long =
        core.epochAt(nowSecs(), core.defaultWindowSecs) + epochCorrection

    private fun popLiveLocked(context: AnonTokenContext): PooledAnonToken? {
        val queue = queues[context] ?: return null
        if (queue.isEmpty()) return null
        val current = currentEpochLocked()
        var found: PooledAnonToken? = null
        while (queue.isNotEmpty()) {
            val t = queue.removeFirst()
            if (core.isLive(t.epoch, current)) {
                found = t
                break
            }
        }
        persistLocked()
        return found
    }

    private fun ensureRefillLocked(context: AnonTokenContext, s: AnonSession): Deferred<RefillResult>? {
        refills[context]?.let { return it.result }
        val until = exhaustedUntil[context]
        if (until != null) {
            val now = nowSecs()
            if (now < until) return CompletableDeferred(RefillResult.Quota(until - now))
            exhaustedUntil.remove(context)
        }
        val held = queues[context]?.size ?: 0
        val count = minOf(target(context), max) - held
        if (count <= 0) return null
        val gen = generation
        val id = nextRefillId++
        val deferred = scope.async {
            val result = try {
                issueBatch(context, s, count)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Throwable) {
                RefillResult.Failed("refill threw", e)
            }
            synchronized(lock) {
                if (generation == gen) {
                    when (result) {
                        is RefillResult.Filled -> {
                            queues.getOrPut(context) { ArrayDeque() }.addAll(result.tokens)
                            persistLocked()
                        }
                        is RefillResult.Quota ->
                            exhaustedUntil[context] = nowSecs() + result.retryAfterSecs
                        else -> Unit
                    }
                    if (refills[context]?.id == id) refills.remove(context)
                }
            }
            result
        }
        // An eager dispatcher can finish the batch before this line; a stored
        // completed job would then shadow every later refill.
        if (!deferred.isCompleted) refills[context] = Refill(id, deferred)
        return deferred
    }

    /** One batch, rebuilt once for the issuer's epoch when it disagrees. */
    private suspend fun issueBatch(
        context: AnonTokenContext,
        s: AnonSession,
        count: Int,
    ): RefillResult {
        var epoch = synchronized(lock) { currentEpochLocked() }
        var rebuilt = false
        while (true) {
            val step = issueOnce(context, s, count, epoch)
            if (step is EpochStep.Retry && !rebuilt) {
                rebuilt = true
                synchronized(lock) {
                    val local = core.epochAt(nowSecs(), core.defaultWindowSecs)
                    epochCorrection = step.currentEpoch - local
                }
                epoch = step.currentEpoch
                continue
            }
            return when (step) {
                is EpochStep.Done -> step.result
                is EpochStep.Retry -> RefillResult.Failed("issuer epoch disagreed twice", null)
            }
        }
    }

    private sealed interface EpochStep {
        data class Done(val result: RefillResult) : EpochStep
        data class Retry(val currentEpoch: Long) : EpochStep
    }

    private suspend fun issueOnce(
        context: AnonTokenContext,
        s: AnonSession,
        count: Int,
        epoch: Long,
    ): EpochStep {
        val issuance = try {
            core.begin(context, epoch, count)
        } catch (e: Throwable) {
            return EpochStep.Done(RefillResult.Failed("could not build the request", e))
        }
        issuance.use { req ->
            return when (val outcome = api.issue(s, req.issueBodyJson())) {
                is AnonIssueOutcome.Issued -> {
                    val key = api.publicKey(context, outcome.epoch)
                        ?: return EpochStep.Done(RefillResult.Failed("no public key", null))
                    val tokens = try {
                        req.finalize(outcome.evaluations, outcome.proof, key)
                    } catch (e: AnonFinalizeFailed) {
                        // `proof-verification` and every other refusal: the
                        // batch is discarded, never partly kept.
                        return EpochStep.Done(RefillResult.Failed("batch discarded: ${e.code}", e))
                    }
                    EpochStep.Done(RefillResult.Filled(tokens))
                }
                is AnonIssueOutcome.EpochNotIssuable -> EpochStep.Retry(outcome.currentEpoch)
                is AnonIssueOutcome.QuotaExhausted ->
                    EpochStep.Done(RefillResult.Quota(outcome.retryAfterSecs))
                AnonIssueOutcome.Disabled -> EpochStep.Done(RefillResult.Disabled)
                is AnonIssueOutcome.Failed ->
                    EpochStep.Done(RefillResult.Failed("issuance failed", outcome.cause))
            }
        }
    }

    public companion object {
        /** The most a context holds. */
        public const val MAX_HELD: Int = 32

        /** How long an action waits for a refill before failing. */
        public const val DEFAULT_WAIT_MS: Long = 8_000L
    }
}

// -----------------------------------------------------------------------------
// Spending with one retry
// -----------------------------------------------------------------------------

/** The `401` codes after which a route is retried ONCE with another token.
 *  None of them means a lost session: no token route carries one.
 *  `anon-token-required` is not here - it is a client bug. */
public val ANON_TOKEN_RETRYABLE_REFUSALS: Set<String> = setOf(
    "anon-token-spent",
    "anon-token-invalid",
    "anon-token-expired",
    "anon-token-wrong-context",
    "anon-token-malformed",
)

/**
 * Take a token, [send] with it; when [refusalCode] reads a retryable token
 * refusal off the answer, drop that token, take another and send once more.
 * The second answer is returned whatever it is.
 */
public suspend fun <R> AnonTokenSource.spendWithRetry(
    context: AnonTokenContext,
    send: suspend (PooledAnonToken) -> R,
    refusalCode: suspend (R) -> String?,
): R {
    val first = send(take(context))
    val code = refusalCode(first) ?: return first
    if (code !in ANON_TOKEN_RETRYABLE_REFUSALS) return first
    return send(take(context))
}

/**
 * A source bound to one explicit [session], for work that outlives the live
 * session (the push unregister a logout fires). Every take is a
 * [AnonTokenSource.takeDetached] on [base]; [releaseSession] is a no-op.
 */
public class DetachedAnonTokenSource(
    private val base: AnonTokenSource,
    private val session: AnonSession,
) : AnonTokenSource {
    override val headerName: String get() = base.headerName

    override suspend fun take(context: AnonTokenContext): PooledAnonToken =
        base.takeDetached(session, context)

    override suspend fun takeDetached(session: AnonSession, context: AnonTokenContext): PooledAnonToken =
        base.takeDetached(session, context)

    override fun releaseSession() = Unit
}

/** The generic network-failure sentence an action shows when no token could
 *  be had for a reason other than a used-up allowance (UX section 3). */
public const val ANON_TOKEN_NETWORK_COPY: String = "No connection. Check your network and try again."

/**
 * The sentence for a failure caused by the token pool, or `null` when
 * [error] has nothing to do with it: [quotaCopy] for a used-up allowance,
 * [ANON_TOKEN_NETWORK_COPY] for every other token failure. Never a sign-out.
 */
public fun anonTokenFailureCopy(error: Throwable, quotaCopy: String): String? {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is AnonTokenQuotaExhausted) return quotaCopy
        if (cause is AnonTokenException) return ANON_TOKEN_NETWORK_COPY
        cause = cause.cause
    }
    return null
}

// -----------------------------------------------------------------------------
// Persistence (ADR-0054 S5)
// -----------------------------------------------------------------------------

/** What the pool keeps between launches for one account. */
public data class PersistedAnonPool(
    /** [AnonSession.accountKey] of the account the tokens were issued to. */
    val owner: String,
    val tokens: List<PooledAnonToken>,
)

/**
 * Where each account's pool survives a relaunch and a logout. The tokens are
 * anonymous bearer credentials: an implementation keeps them in app-private,
 * never-backed-up storage, never logs them and never syncs them between
 * devices. Accounts are kept apart: no call reads or removes another
 * account's pool.
 */
public interface AnonTokenStore {
    /** [accountKey]'s persisted pool, or `null` when none (or it is
     *  unreadable). */
    public fun load(accountKey: String): PersistedAnonPool?

    /** Replace what is persisted for [PersistedAnonPool.owner]. */
    public fun save(pool: PersistedAnonPool)

    /** Drop what is persisted for [accountKey]. */
    public fun delete(accountKey: String)
}

/** A store that keeps nothing (tests that do not care, and a process with no
 *  context to put a file in). */
public object NoAnonTokenStore : AnonTokenStore {
    override fun load(accountKey: String): PersistedAnonPool? = null
    override fun save(pool: PersistedAnonPool) = Unit
    override fun delete(accountKey: String) = Unit
}
