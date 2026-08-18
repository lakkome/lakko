// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.launch
import kotlinx.coroutines.supervisorScope
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.sync.withPermit

/** The outcome of one [runSplitPoll] attempt. */
internal enum class SplitPollOutcome {
    /** The batch was fetched and applied under the lock. */
    APPLIED,

    /** Another poll for the same group was already in flight — this one did not
     *  fetch and did not apply. */
    SKIPPED_IN_FLIGHT,

    /** The cursor moved between the fetch and the lock acquisition, so the
     *  fetched batch is stale. Dropped WITHOUT applying and WITHOUT touching the
     *  cursor; the next poll re-fetches from the current cursor. */
    SKIPPED_CURSOR_MOVED,
}

/**
 * G-37 — the split-poll ordering, as a pure orchestration over injected steps so
 * it is unit-testable without an Android `Context` or a live FFI `Group`.
 *
 * Same shape (and same reason) as [runCrashSafeCommit]: the property being fixed
 * is an ORDERING property between a network wait, a lock, and a state mutation,
 * and an ordering property is only actually pinned if a test can run it.
 *
 * ## What this fixes
 *
 * `ChatRepository.pollGroup` used to take the single repository-wide mutex and
 * then await a DELIBERATE 30-second DS long poll inside it, driven by an
 * unbounded `while (isActive)` loop on the chat screen. That mutex was one per
 * repository — not per group — so while a chat screen was open it was held
 * near-continuously and every send / edit / reaction / membership commit on
 * EVERY group queued behind it. Bounding the poll with a timeout would be wrong:
 * it would sever a long poll that is working as designed. The fix is to move the
 * wait out of the critical section instead — and, in G-37 sprint 2, to narrow
 * what is left to one mutex per group plus a global provider mutex.
 *
 * ## The ordering
 *
 *  1. Claim the per-group in-flight guard. Two concurrent polls of the same group
 *     must not both fetch and then apply overlapping batches, so the loser
 *     returns [SplitPollOutcome.SKIPPED_IN_FLIGHT] immediately without fetching.
 *  2. Read the cursor, and [fetch] with it — OUTSIDE the lock. This is the long
 *     wait, and it touches no MLS state.
 *  3. Take the PER-GROUP [lock]. Re-read the cursor: if it moved while we were
 *     parked on the network, the batch we hold is stale (it starts at a row
 *     someone else has already consumed), so drop it —
 *     [SplitPollOutcome.SKIPPED_CURSOR_MOVED]. Applying it anyway would re-feed
 *     consumed rows to OpenMLS and, worse, let the stale batch's end cursor move
 *     the record's cursor BACKWARD.
 *  4. Take the global [providerLock] and [apply] the batch, advancing the cursor
 *     — so the apply is atomic against every other group's MLS work on the ONE
 *     shared OpenMLS provider, and the cursor read-modify-write is atomic against
 *     every other operation on THIS group.
 *
 * The in-flight guard is released in a `finally`, so a fetch that throws (a DS
 * error, a timeout, cancellation) never wedges the group's polling forever.
 *
 * ## Why two locks
 *
 * G-37 sprint 2 narrowed the repository-wide mutex to one mutex PER GROUP, so
 * different groups no longer queue behind each other's network I/O. But the
 * OpenMLS provider underneath is still ONE store shared by every group (a
 * whole-provider `snapshotStorage` / `restoreStorage` is the only granularity
 * the FFI offers), so provider-touching work still needs a global mutex. The
 * ordering is always per-group FIRST, provider SECOND — never the reverse.
 *
 * @param inFlight per-group guard; `false` means "no poll running for this group".
 * @param lock the PER-GROUP mutex — guards this group's cursor + timeline record.
 * @param providerLock the GLOBAL mutex guarding the shared OpenMLS provider.
 * @param readCursor reads the group record's current cursor. Called once before
 *   the fetch and again under the lock.
 * @param fetch the network half — given the pre-fetch cursor, returns the batch.
 *   Runs OUTSIDE both locks.
 * @param apply the MLS half — given the same cursor and the fetched batch,
 *   applies it and advances the cursor. Runs INSIDE both locks. Non-suspending by
 *   design: no network wait can be reintroduced here.
 */
internal suspend fun <T> runSplitPoll(
    inFlight: AtomicBoolean,
    lock: Mutex,
    providerLock: Mutex,
    readCursor: () -> Long,
    fetch: suspend (after: Long) -> T,
    apply: (after: Long, fetched: T) -> Unit,
): SplitPollOutcome {
    if (!inFlight.compareAndSet(false, true)) {
        return SplitPollOutcome.SKIPPED_IN_FLIGHT
    }
    try {
        val after = readCursor()
        // The long wait. Deliberately outside BOTH locks.
        val fetched = fetch(after)
        return lock.withLock {
            if (readCursor() != after) {
                SplitPollOutcome.SKIPPED_CURSOR_MOVED
            } else {
                providerLock.withLock { apply(after, fetched) }
                SplitPollOutcome.APPLIED
            }
        }
    } finally {
        inFlight.set(false)
    }
}

/**
 * G-37 — the per-group mutex registry that replaced `ChatRepository`'s single
 * repository-wide `groupsLock`.
 *
 * A tiny type rather than a bare `ConcurrentHashMap` field for one reason: the
 * property it provides — "two DIFFERENT groups get two DIFFERENT mutexes, the
 * SAME group always gets the SAME one" — is the whole correctness argument for
 * narrowing the lock, and a type is something a host test can drive. The
 * repository itself needs an Android `Context` and live FFI handles, so the
 * alternative would be a test that re-implements the lookup and proves nothing
 * about production. Same reasoning as [runSplitPoll] and [runCrashSafeCommit].
 *
 * ## Why `getOrPut` is safe here
 *
 * Kotlin resolves `getOrPut` on a [ConcurrentHashMap] to the `ConcurrentMap`
 * overload, which is backed by `putIfAbsent`, so racing callers converge on ONE
 * `Mutex` per key — the loser discards its speculatively-constructed instance
 * rather than installing it. A plain `MutableMap.getOrPut` would NOT give that.
 * Precedent: [me.lakko.app.push.GroupPushRouting]'s per-`(account, group)`
 * routing lock, whose review established the same point.
 *
 * ## What the per-group lock does and does not guarantee
 *
 * It preserves the SAME-group ordering exactly as the repository-wide mutex did:
 * one group's whole operation — create-commit, durable record, DS publish — runs
 * inside it, so commit A is on the wire before commit B is even created, which
 * is what stops a commit being built on an epoch peers have not reached
 * (ADR-0040's "device ahead" strand).
 *
 * It does NOT guard the shared OpenMLS provider — every group's state lives in
 * ONE provider, and `snapshotStorage` / `restoreStorage` are whole-provider
 * operations. That needs a separate global lock, held strictly INSIDE this one.
 */
internal class GroupLocks {
    private val locks = ConcurrentHashMap<String, Mutex>()

    /** The mutex for [groupIdHex], created on first use. */
    fun forGroup(groupIdHex: String): Mutex = locks.getOrPut(groupIdHex) { Mutex() }

    /** Drop every mutex — account switch / logout hygiene. */
    fun clear() {
        locks.clear()
    }

    /** How many groups have a mutex. Diagnostics + tests. */
    val size: Int get() = locks.size
}

/**
 * G-37 — the `pollAllGroups` fan-out, as a pure orchestration over an injected
 * per-group poll so it is unit-testable without an Android `Context`.
 *
 * ## What this fixes
 *
 * The chat-list poll used to iterate groups SERIALLY, each with its own
 * `waitSecs` long poll, so the worst-case latency to see a new message in ANY
 * group was `waitSecs * N`. With the DS fetch now outside the locks and the
 * locks now per-group, the polls can overlap.
 *
 * ## The two properties that make it correct
 *
 *  - **Bounded.** [concurrency] caps the in-flight polls. Unbounded fan-out
 *    would saturate OkHttp's per-host dispatcher slots with idle long polls and
 *    starve interactive requests to the same host — trading lock contention for
 *    connection starvation. See `POLL_ALL_CONCURRENCY` for the measured bound.
 *  - **Isolated.** One group's failure must not cancel the others. That is why
 *    this is a `supervisorScope` with a per-leg `try/catch`, not a
 *    `coroutineScope`: under structured concurrency a plain `coroutineScope`
 *    propagates the first child failure and cancels its siblings, which would be
 *    strictly WORSE than the serial loop this replaces (that loop swallowed each
 *    failure and carried on). [CancellationException] is deliberately re-thrown
 *    so a cancelled poll stays cancelled rather than being reported as a
 *    swallowed error.
 *
 * @param groupIds the groups to poll — snapshot by the caller.
 * @param concurrency maximum polls in flight at once; must be >= 1.
 * @param pollOne polls ONE group. May throw; the throw is contained to that leg.
 */
internal suspend fun runPollAllGroups(
    groupIds: List<String>,
    concurrency: Int,
    pollOne: suspend (String) -> Unit,
) {
    if (groupIds.isEmpty()) return
    val gate = Semaphore(concurrency.coerceAtLeast(1))
    supervisorScope {
        for (groupIdHex in groupIds) {
            launch {
                gate.withPermit {
                    try {
                        pollOne(groupIdHex)
                    } catch (cancel: CancellationException) {
                        // Structured concurrency — never swallowed.
                        throw cancel
                    } catch (_: Throwable) {
                        // Best-effort: one bad apple must not block the others.
                    }
                }
            }
        }
    }
}
