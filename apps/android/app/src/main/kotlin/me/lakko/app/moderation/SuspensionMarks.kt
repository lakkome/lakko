// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import java.util.Base64
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.withTimeoutOrNull

/*
 * ADR-0098 step 4, UX `reporting-and-moderation.md` section 3 — whether
 * ANOTHER person's account is suspended, as the moderation log says.
 *
 * The moderation service publishes a snapshot of active suspensions in 65,536
 * buckets; the device asks for the one bucket its account key falls in, and
 * the core checks the answer against the pinned moderation-log operator key
 * and the transparency witnesses before reading it. Nothing the device cannot
 * confirm in the log is shown: every failure is [AccountStanding.UNKNOWN],
 * and UNKNOWN reads exactly like NOT_SUSPENDED everywhere (a mark is a
 * warning, never access control).
 *
 * The answer is kept in memory only, per bucket, until the snapshot's reuse
 * time; nothing about a standing reaches disk, so when a suspension ends
 * nothing about it is kept. Nothing here logs an account key or a prefix.
 *
 * ADR-0098 F2: the snapshot alone never marks anyone. A `suspended` answer
 * is acted on only after the moderation log entry behind it is fetched and
 * proven to be a statement a listed moderator signed (`moderationProve...`,
 * [StandingProofs]); until then, and whenever that fails, the standing is
 * UNKNOWN. A proof is kept with the answer it was made for.
 */

/** What the log says about one account. */
public enum class AccountStanding { SUSPENDED, NOT_SUSPENDED, UNKNOWN }

/** The pins an answer is verified under. */
public class ModerationLogPins(
    public val operatorKey: ByteArray,
    public val witnessKeys: List<ByteArray>,
    public val witnessThreshold: Int,
)

/** One verified answer, read for one account. [logIndex] is the moderation
 *  log index of the statement behind the account's snapshot entry, the entry
 *  a `suspended` standing must be proven with (ADR-0098 F2). */
public data class VerifiedStanding(val suspended: Boolean, val reuseUntilMs: Long, val logIndex: ULong? = null)

/** What [SuspensionStandings.answerOf] found: the standing, and until when a
 *  verified answer may be reused (`null` when nothing verified it). */
public data class StandingAnswer(val standing: AccountStanding, val reuseUntilMs: Long?) {
    public companion object {
        public val UNKNOWN: StandingAnswer = StandingAnswer(AccountStanding.UNKNOWN, null)
    }
}

/** The pinned moderation-log operator key (`BuildConfig.LAKKO_MODERATION_LOG_KEY`, base64url), or `null`. */
public fun pinnedModerationLogKey(pinned: String = me.lakko.app.BuildConfig.LAKKO_MODERATION_LOG_KEY): ByteArray? {
    val text = pinned.trim().takeIf { it.isNotEmpty() } ?: return null
    return try {
        Base64.getUrlDecoder().decode(text.trimEnd('=')).takeIf { it.size == LOG_KEY_BYTES }
    } catch (_: IllegalArgumentException) {
        null
    }
}

private const val LOG_KEY_BYTES: Int = 32

/** A bucket prefix as the route takes it: four lower-case hex characters. */
internal val BUCKET_PREFIX: Regex = Regex("^[0-9a-f]{4}$")

/**
 * The standings service, with every dependency injected so the host tests
 * drive it without the native core.
 *
 * - one request in flight per prefix; a caller that goes away does not
 *   cancel it (it runs in [scope]);
 * - a verified answer is reused for its prefix until its `reuseUntilMs`;
 * - a failed fetch or an answer that does not verify is not asked again for
 *   that prefix for [FAILURE_BACKOFF_MS];
 * - no pins: no request at all;
 * - a `suspended` answer counts only once [prove] succeeded for that subject
 *   and log entry (one proof in flight per subject; a success is kept with
 *   the answer, a failure is not tried again for [FAILURE_BACKOFF_MS]); no
 *   [prove], no log index, or a failed proof is UNKNOWN.
 */
public class SuspensionStandings(
    private val pins: ModerationLogPins?,
    private val prefixOf: (accountKey: ByteArray) -> String,
    private val fetch: suspend (prefix: String) -> String?,
    private val verify: (answer: String, accountKey: ByteArray, pins: ModerationLogPins, nowMs: Long) -> VerifiedStanding,
    private val scope: CoroutineScope,
    private val nowMs: () -> Long = System::currentTimeMillis,
    /**
     * ADR-0098 F2: prove a `suspended` [answer] for [key] from the log entry
     * at [logIndex]; `true` only when a listed moderator's statement proves
     * it. `null` proves nothing, so no standing is ever SUSPENDED.
     */
    private val prove: (suspend (answer: String, key: ByteArray, logIndex: ULong, nowMs: Long) -> Boolean)? = null,
) {
    private class Cached(val answer: String, val reuseUntilMs: Long) {
        /** Subjects proven under this answer (guarded by `lock`). */
        val proven = HashSet<String>()

        /** When a proof last failed, per subject (guarded by `lock`). */
        val proofFailedAt = HashMap<String, Long>()

        /** The proof in flight, per subject (guarded by `lock`). */
        val proving = HashMap<String, Deferred<Boolean>>()
    }

    // Plain monitor: no region below suspends while holding it.
    private val lock = Any()
    private val cache = HashMap<String, Cached>()
    private val failedAt = HashMap<String, Long>()
    private val inFlight = HashMap<String, Deferred<Cached?>>()

    /** Advanced by [clear]: an answer fetched before it is never kept. */
    private var generation = 0L

    /**
     * Forget every kept answer, back-off and request in flight (sign-out,
     * forget device): the next account on this device starts empty. A
     * request still running answers its waiters UNKNOWN and keeps nothing.
     */
    public fun clear() {
        synchronized(lock) {
            generation += 1
            cache.clear()
            failedAt.clear()
            inFlight.clear()
        }
    }

    /** Whether any check can ever run (pins present). */
    public val enabled: Boolean get() = pins != null

    /** The standing of the account whose key is [accountKeyB64] (any base64 spelling). */
    public suspend fun standing(accountKeyB64: String?): AccountStanding {
        val key = accountKeyB64?.let(::accountKeyBytes) ?: return AccountStanding.UNKNOWN
        return answerOf(key).standing
    }

    /**
     * The standing of the subject whose raw identifier is [key] (an account
     * key, or, for the channel instance of ADR-0098 step 5, a 16-byte
     * channel id), with the time the answer may be reused until; `null` when
     * nothing verified it.
     */
    public suspend fun answerOf(key: ByteArray): StandingAnswer {
        val pins = pins ?: return StandingAnswer.UNKNOWN
        val prefix = try {
            prefixOf(key)
        } catch (cause: CancellationException) {
            throw cause
        } catch (_: Throwable) {
            return StandingAnswer.UNKNOWN
        }
        if (!BUCKET_PREFIX.matches(prefix)) return StandingAnswer.UNKNOWN
        val cached = answerFor(prefix, key, pins) ?: return StandingAnswer.UNKNOWN
        val verified = verifyOrNull(cached.answer, key, pins)
        if (verified == null) {
            // The kept answer no longer verifies (gone stale at this clock):
            // drop it and back off, like a failed fetch.
            synchronized(lock) {
                if (cache[prefix] === cached) {
                    cache.remove(prefix)
                    failedAt[prefix] = nowMs()
                }
            }
            return StandingAnswer.UNKNOWN
        }
        if (!verified.suspended) return StandingAnswer(AccountStanding.NOT_SUSPENDED, verified.reuseUntilMs)
        // ADR-0098 F2: suspended only when the log entry proves it.
        if (!provenFor(cached, key, verified.logIndex)) return StandingAnswer.UNKNOWN
        return StandingAnswer(AccountStanding.SUSPENDED, verified.reuseUntilMs)
    }

    private suspend fun provenFor(cached: Cached, key: ByteArray, logIndex: ULong?): Boolean {
        val prove = prove ?: return false
        val index = logIndex ?: return false
        val subject = subjectId(key, index)
        val job = synchronized(lock) {
            if (subject in cached.proven) return true
            cached.proofFailedAt[subject]?.let {
                if (nowMs() - it < FAILURE_BACKOFF_MS) return false else cached.proofFailedAt.remove(subject)
            }
            val started = generation
            cached.proving[subject]
                ?: scope.async { runProof(prove, cached, subject, key, index, started) }
                    .also { cached.proving[subject] = it }
        }
        return try {
            job.await()
        } catch (cause: CancellationException) {
            throw cause
        } catch (_: Throwable) {
            false
        }
    }

    private suspend fun runProof(
        prove: suspend (String, ByteArray, ULong, Long) -> Boolean,
        cached: Cached,
        subject: String,
        key: ByteArray,
        index: ULong,
        started: Long,
    ): Boolean {
        val ok = try {
            prove(cached.answer, key, index, nowMs())
        } catch (_: CancellationException) {
            false
        } catch (_: Throwable) {
            false
        }
        return synchronized(lock) {
            cached.proving.remove(subject)
            if (generation != started) return false
            if (ok) cached.proven += subject else cached.proofFailedAt[subject] = nowMs()
            ok
        }
    }

    private fun subjectId(key: ByteArray, index: ULong): String =
        key.joinToString("") { "%02x".format(it.toInt() and 0xff) } + ":" + index

    private suspend fun answerFor(prefix: String, key: ByteArray, pins: ModerationLogPins): Cached? {
        val job = synchronized(lock) {
            val now = nowMs()
            cache[prefix]?.let { if (now < it.reuseUntilMs) return it else cache.remove(prefix) }
            failedAt[prefix]?.let { if (now - it < FAILURE_BACKOFF_MS) return null else failedAt.remove(prefix) }
            val started = generation
            inFlight[prefix] ?: scope.async { fetchAndKeep(prefix, key, pins, started) }.also { inFlight[prefix] = it }
        }
        return try {
            job.await()
        } catch (cause: CancellationException) {
            throw cause
        } catch (_: Throwable) {
            null
        }
    }

    private suspend fun fetchAndKeep(prefix: String, key: ByteArray, pins: ModerationLogPins, started: Long): Cached? {
        val answer = try {
            fetch(prefix)
        } catch (cause: CancellationException) {
            null
        } catch (_: Throwable) {
            null
        }
        val verified = answer?.let { verifyOrNull(it, key, pins) }
        return synchronized(lock) {
            if (generation != started) return null
            inFlight.remove(prefix)
            if (answer == null || verified == null) {
                failedAt[prefix] = nowMs()
                null
            } else {
                Cached(answer, verified.reuseUntilMs).also {
                    cache[prefix] = it
                    failedAt.remove(prefix)
                }
            }
        }
    }

    private fun verifyOrNull(answer: String, key: ByteArray, pins: ModerationLogPins): VerifiedStanding? = try {
        verify(answer, key, pins, nowMs())
    } catch (_: Throwable) {
        null
    }

    public companion object {
        /** A failed prefix is not asked again for ten minutes (chosen). */
        public const val FAILURE_BACKOFF_MS: Long = 10 * 60 * 1000L
    }
}

/**
 * The process-wide marks. Bound to the real moderation route and the native
 * check; screens and the request poll ask here.
 */
public object SuspensionMarks {
    /** How long the contact-request poll waits for a standing (chosen). */
    public const val REQUEST_CHECK_TIMEOUT_MS: Long = 3_000L

    /** This device's own account key (standard base64); its own standing is never a mark. */
    @Volatile
    public var ownAccountKey: () -> String? = { null }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val client: ModerationClient by lazy { ModerationClient() }

    /** ADR-0098 F2: the log-entry proof both services run before a mark. */
    private val proofs: StandingProofs by lazy {
        StandingProofs(
            rootKey = { pinnedModerationRootKey() },
            fetchEntry = { index, size -> client.logEntry(index, size) },
            moderatorList = ModeratorListCache.shared,
        )
    }

    /** Bind this account's own key at a session landing ([ownAccountKey]). */
    public fun bindOwnAccountKey(provider: () -> String?) {
        ownAccountKey = provider
    }

    /**
     * Sign-out, forget device: drop every kept standing and the own-key
     * binding, so the next account on this device starts empty.
     */
    public fun clear() {
        ownAccountKey = { null }
        override?.clear()
        if (liveBuilt) live.clear()
        channelOverride?.clear()
        if (channelLiveBuilt) channelLive.clear()
    }

    @Volatile
    private var liveBuilt = false

    @Volatile
    private var override: SuspensionStandings? = null

    @Volatile
    private var channelLiveBuilt = false

    @Volatile
    private var channelOverride: SuspensionStandings? = null

    private fun pins(): ModerationLogPins? = pinnedModerationLogKey()?.let { key ->
        ModerationLogPins(
            operatorKey = key,
            witnessKeys = me.lakko.app.transparency.TransparencyConfig.pinnedWitnessKeys(),
            witnessThreshold = me.lakko.app.transparency.TransparencyConfig.witnessThreshold(),
        )
    }

    /**
     * ADR-0098 step 5 — the same service for channels: a removed channel is
     * an entry of kind Channel in the same snapshot, keyed on the 16-byte
     * channel id. A separate instance, so the per-prefix cache of accounts
     * and of channels never mix.
     */
    private val channelLive: SuspensionStandings by lazy {
        channelLiveBuilt = true
        SuspensionStandings(
            pins = pins(),
            prefixOf = { uniffi.lakko_ffi_uniffi.moderationChannelBucketPrefix(it) },
            fetch = { client.snapshotBucket(it) },
            verify = { answer, id, pins, now ->
                val s = uniffi.lakko_ffi_uniffi.moderationChannelStanding(
                    answer,
                    id,
                    pins.operatorKey,
                    pins.witnessKeys,
                    pins.witnessThreshold.coerceAtLeast(0).toUInt(),
                    now,
                )
                VerifiedStanding(suspended = s.suspended, reuseUntilMs = s.reuseUntilMs, logIndex = s.logIndex)
            },
            scope = scope,
            prove = { answer, id, index, now ->
                proofs.prove(answer, index) { entry, list, root ->
                    uniffi.lakko_ffi_uniffi.moderationProveChannelStanding(answer, entry, list, root, id, now)
                }
            },
        )
    }

    /** Tests replace the channel service. */
    internal fun overrideChannelsForTests(value: SuspensionStandings?) {
        channelOverride = value
    }

    /**
     * Whether the log says the channel [channelIdHex] (32 hex characters) was
     * removed by moderators; `suspended` means removed. A malformed id, no
     * pins or any failure is [StandingAnswer.UNKNOWN].
     */
    public suspend fun channelAnswer(channelIdHex: String): StandingAnswer {
        val id = channelIdBytes(channelIdHex) ?: return StandingAnswer.UNKNOWN
        return (channelOverride ?: channelLive).answerOf(id)
    }

    private val live: SuspensionStandings by lazy {
        liveBuilt = true
        SuspensionStandings(
            pins = pins(),
            prefixOf = { uniffi.lakko_ffi_uniffi.moderationBucketPrefix(it) },
            fetch = { client.snapshotBucket(it) },
            verify = { answer, key, pins, now ->
                val s = uniffi.lakko_ffi_uniffi.moderationAccountStanding(
                    answer,
                    key,
                    pins.operatorKey,
                    pins.witnessKeys,
                    pins.witnessThreshold.coerceAtLeast(0).toUInt(),
                    now,
                )
                VerifiedStanding(suspended = s.suspended, reuseUntilMs = s.reuseUntilMs, logIndex = s.logIndex)
            },
            scope = scope,
            prove = { answer, key, index, now ->
                proofs.prove(answer, index) { entry, list, root ->
                    uniffi.lakko_ffi_uniffi.moderationProveAccountStanding(answer, entry, list, root, key, now)
                }
            },
        )
    }

    private val standings: SuspensionStandings get() = override ?: live

    /** Tests replace the service. */
    internal fun overrideForTests(value: SuspensionStandings?) {
        override = value
    }

    /** The standing of [accountKeyB64]; this device's own account is never marked. */
    public suspend fun standing(accountKeyB64: String?): AccountStanding {
        if (accountKeyB64 == null) return AccountStanding.UNKNOWN
        if (isOwn(accountKeyB64)) return AccountStanding.UNKNOWN
        return standings.standing(accountKeyB64)
    }

    /** Whether [accountKeyB64] is confirmed suspended in the log. */
    public suspend fun isSuspended(accountKeyB64: String?): Boolean =
        standing(accountKeyB64) == AccountStanding.SUSPENDED

    /**
     * The contact requests of [requests] that survive the suspension check:
     * a request is dropped unseen only on a verified suspension found within
     * [timeoutMs]; a slow or failed check shows it as before.
     */
    public suspend fun <T> withoutSuspendedRequests(
        requests: List<T>,
        keyOf: (T) -> ByteArray,
        timeoutMs: Long = REQUEST_CHECK_TIMEOUT_MS,
    ): List<T> = filterSuspendedRequests(requests, keyOf, timeoutMs) { standing(it) }

    private fun isOwn(accountKeyB64: String): Boolean {
        val own = try {
            ownAccountKey()
        } catch (_: Throwable) {
            null
        } ?: return false
        return try {
            canonicalKey(own) == canonicalKey(accountKeyB64)
        } catch (_: Throwable) {
            false
        }
    }
}

/** The one-to-one banner shows only when the person is not blocked (the block banner wins). */
public fun suspendedBannerShows(blocked: Boolean, suspended: Boolean): Boolean = !blocked && suspended

/** The card's muted line shows only when the person is not blocked (`block.card_line` wins). */
public fun suspendedCardLineShows(blocked: Boolean, suspended: Boolean): Boolean = !blocked && suspended

/**
 * UX section 3 — whether **Add** on an invite page may go ahead: it waits for
 * the inviter's standing for at most [timeoutMs] and stops only on a verified
 * suspension; a slow, failed or unknown check lets it proceed as before.
 */
public suspend fun inviteAddMayProceed(inviterKey: ByteArray): Boolean =
    inviteAddMayProceed(inviterKey, SuspensionMarks.REQUEST_CHECK_TIMEOUT_MS, liveStanding)

/** [inviteAddMayProceed] with the bound and the standing lookup given (tests). */
public suspend fun inviteAddMayProceed(
    inviterKey: ByteArray,
    timeoutMs: Long,
    standingOf: suspend (String) -> AccountStanding,
): Boolean = !suspendedWithin(inviterKey, timeoutMs, standingOf)

/**
 * Whether the account [accountKey] is confirmed suspended within [timeoutMs];
 * a slow, failed or unknown check is `false`.
 */
public suspend fun suspendedWithin(
    accountKey: ByteArray,
    timeoutMs: Long,
    standingOf: suspend (String) -> AccountStanding,
): Boolean {
    val keyB64 = Base64.getEncoder().encodeToString(accountKey)
    return dropsSuspendedRequest(withTimeoutOrNull(timeoutMs) { standingOf(keyB64) })
}

/** A channel id as 16 bytes, from its 32 hex characters; `null` otherwise. */
internal fun channelIdBytes(hex: String): ByteArray? {
    val h = hex.trim().lowercase()
    if (h.length != 32 || !h.all { it in '0'..'9' || it in 'a'..'f' }) return null
    return ByteArray(16) { i -> h.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
}

/** The live standing lookup, as a value. */
internal val liveStanding: suspend (String) -> AccountStanding = { SuspensionMarks.standing(it) }

/** A contact request is dropped only on a verified suspension. */
public fun dropsSuspendedRequest(standing: AccountStanding?): Boolean = standing == AccountStanding.SUSPENDED

/** Search rows without the accounts confirmed suspended so far. */
public fun <T> withoutSuspendedRows(rows: List<T>, suspendedKeysB64: Set<String>, keyOf: (T) -> String): List<T> {
    if (suspendedKeysB64.isEmpty()) return rows
    val canonical = suspendedKeysB64.mapTo(HashSet()) { canonicalKeyOrSelf(it) }
    return rows.filterNot { canonicalKeyOrSelf(keyOf(it)) in canonical }
}

private fun canonicalKeyOrSelf(keyB64: String): String = try {
    canonicalKey(keyB64)
} catch (_: Throwable) {
    keyB64
}

/** The request filter, with the standing lookup injected (the host tests drive it). */
internal suspend fun <T> filterSuspendedRequests(
    requests: List<T>,
    keyOf: (T) -> ByteArray,
    timeoutMs: Long,
    standingOf: suspend (String) -> AccountStanding,
): List<T> {
    if (requests.isEmpty()) return requests
    val drops = coroutineScope {
        requests.map { request ->
            async {
                val keyB64 = Base64.getEncoder().encodeToString(keyOf(request))
                dropsSuspendedRequest(withTimeoutOrNull(timeoutMs) { standingOf(keyB64) })
            }
        }.awaitAll()
    }
    return requests.filterIndexed { i, _ -> !drops[i] }
}
