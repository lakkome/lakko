// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

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
 * `ChatRepository.pollGroup` used to take the single repository-wide `groupsLock`
 * and then await a DELIBERATE 30-second DS long poll inside it, driven by an
 * unbounded `while (isActive)` loop on the chat screen. The lock is one mutex per
 * repository — not per group — so while a chat screen was open the global lock
 * was held near-continuously and every send / edit / reaction / membership commit
 * on EVERY group queued behind it. Bounding the poll with a timeout would be
 * wrong: it would sever a long poll that is working as designed. The fix is to
 * move the wait out of the critical section instead.
 *
 * ## The ordering
 *
 *  1. Claim the per-group in-flight guard. Two concurrent polls of the same group
 *     must not both fetch and then apply overlapping batches, so the loser
 *     returns [SplitPollOutcome.SKIPPED_IN_FLIGHT] immediately without fetching.
 *  2. Read the cursor, and [fetch] with it — OUTSIDE the lock. This is the long
 *     wait, and it touches no MLS state.
 *  3. Take the lock. Re-read the cursor: if it moved while we were parked on the
 *     network, the batch we hold is stale (it starts at a row someone else has
 *     already consumed), so drop it — [SplitPollOutcome.SKIPPED_CURSOR_MOVED].
 *     Applying it anyway would re-feed consumed rows to OpenMLS and, worse, let
 *     the stale batch's end cursor move the record's cursor BACKWARD.
 *  4. [apply] the batch and advance the cursor — both inside the same critical
 *     section, so they are atomic with respect to each other and with respect to
 *     every other MLS mutation on the shared provider.
 *
 * The in-flight guard is released in a `finally`, so a fetch that throws (a DS
 * error, a timeout, cancellation) never wedges the group's polling forever.
 *
 * @param inFlight per-group guard; `false` means "no poll running for this group".
 * @param lock the repository-wide MLS mutex.
 * @param readCursor reads the group record's current cursor. Called once before
 *   the fetch and again under the lock.
 * @param fetch the network half — given the pre-fetch cursor, returns the batch.
 *   Runs OUTSIDE [lock].
 * @param apply the MLS half — given the same cursor and the fetched batch,
 *   applies it and advances the cursor. Runs INSIDE [lock]. Non-suspending by
 *   design: no network wait can be reintroduced here.
 */
internal suspend fun <T> runSplitPoll(
    inFlight: AtomicBoolean,
    lock: Mutex,
    readCursor: () -> Long,
    fetch: suspend (after: Long) -> T,
    apply: (after: Long, fetched: T) -> Unit,
): SplitPollOutcome {
    if (!inFlight.compareAndSet(false, true)) {
        return SplitPollOutcome.SKIPPED_IN_FLIGHT
    }
    try {
        val after = readCursor()
        // The long wait. Deliberately outside `lock`.
        val fetched = fetch(after)
        return lock.withLock {
            if (readCursor() != after) {
                SplitPollOutcome.SKIPPED_CURSOR_MOVED
            } else {
                apply(after, fetched)
                SplitPollOutcome.APPLIED
            }
        }
    } finally {
        inFlight.set(false)
    }
}
