// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * ADR-0045 / ADR-0027 (G-3) — the ordering an external-join `Approve` has to
 * follow now that every role-tabled group carries a pending-proposal set.
 *
 * Extracted as a pure orchestration over injected steps for the same reason
 * [runSplitPoll] and [runCrashSafeCommit] are: the property being fixed is an
 * ORDERING property between a drain, two locks, and a state read, and an
 * ordering property is only actually pinned if a test can run it.
 * `ChatRepository` needs an Android `Context` and live FFI handles; this does
 * not.
 *
 * ## What this fixes
 *
 * `Group::approve_external_join` opens with `refuse_if_proposals_pending`, a
 * bare count of ANY pending proposal of ANY kind — an unrelated add, somebody's
 * leave, an update, a role change. Under ADR-0045 a non-empty pending set is the
 * ORDINARY state between any member's request and the next committer sweep, so
 * an admin tapping `Approve` hit a hard failure whenever anything at all was
 * waiting, and what reached the screen was the core's own diagnostic naming
 * `commit_pending_proposals` / `stage_pending_proposals` — two Rust entry points
 * no user can call.
 *
 * ## The ordering
 *
 *  1. [drain] — run the committer sweep, so a pending set THIS device is
 *     entitled to clear is cleared before the approve asks the core to commit.
 *     It is a no-op when this device is not the acting committer.
 *  2. Take the per-group [lock], then the global [providerLock] — always in that
 *     order, never the reverse.
 *  3. Re-read [pendingProposalCount] INSIDE [providerLock]. Anything left refuses
 *     with [MembershipChangesPendingException], whose message is the user-facing
 *     copy.
 *  4. [prepare] the commit + the durable record under [providerLock], and run the
 *     publish step it returns outside it.
 *
 * ## Why [drain] runs outside BOTH locks
 *
 * Because a kotlinx [Mutex] is NOT reentrant: re-locking one already held by the
 * same coroutine suspends that coroutine forever. The sweep takes both locks
 * itself, so calling it from inside either would deadlock — not throw, not
 * degrade: hang, silently, holding the lock every other operation on the group
 * queues behind. `ChatRepository.pollGroup` calls the sweep from outside its
 * locks for exactly this reason. `ApproveExternalJoinTest` proves it by running
 * the inverted shape against a timeout rather than asserting it in prose.
 *
 * ## The window that leaves, stated rather than wished away
 *
 * Between [drain] and [lock] this device's poll can apply a batch that stores a
 * fresh proposal. Step 3 is what makes that survivable: the window can produce a
 * readable "not yet", never the core's developer string, and never a half-done
 * approve. The next tap — or the sweep at the tail of the next poll — clears it.
 */
internal suspend fun <T> runApproveExternalJoin(
    drain: suspend () -> Unit,
    lock: Mutex,
    providerLock: Mutex,
    pendingProposalCount: () -> Int,
    prepare: () -> (suspend () -> T),
): T {
    drain()
    return lock.withLock {
        val publishStep = providerLock.withLock {
            if (pendingProposalCount() > 0) throw MembershipChangesPendingException()
            prepare()
        }
        publishStep()
    }
}
