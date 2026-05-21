// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * 1-second SFrame overlap-window timer (Phase 3c5 —
 * Android mirror of `apps/web/src/calls/overlap-timer.ts`
 * and `apps/ios/Lakko/Calls/OverlapTimer.swift`).
 *
 * The Rust core (`crates/lakko-calls/src/group_call.rs::
 * GroupCallSession`) is wall-clock-free by design: on
 * every `refresh_call_key` it stashes the prior CallKey
 * into `previous_call_key`, and the caller must invoke
 * `drop_previous_call_key` after a ~1-second overlap
 * window. This class is the **platform-side timer** that
 * owns that schedule on Android.
 *
 * Usage:
 *
 * ```kotlin
 * val scope = CoroutineScope(Dispatchers.Default)
 * val timer = OverlapTimer(scope)
 *
 * // Right after refresh_call_key():
 * timer.arm { callSession.dropPreviousCallKey() }
 * // ...the callback fires ~1 s later.
 *
 * // To cancel before firing (e.g. session torn down):
 * timer.cancel()
 * ```
 *
 * Properties (mirror of the web + iOS helpers):
 *
 *   * **Single-shot.** Arming twice without cancellation
 *     replaces the prior callback (newer rotation subsumes
 *     older); the first callback never fires.
 *   * **Idempotent cancel.** `cancel()` on a disarmed
 *     timer is a no-op.
 *   * **Scope-bound lifetime.** The underlying coroutine
 *     runs on the [scope] passed to the constructor;
 *     cancelling the scope cancels the timer too. Caller
 *     typically passes a `viewModelScope` or session-bound
 *     scope so cleanup is automatic.
 *
 * Closes part of G-18 (known-gaps): "Overlap-window timer
 * lives in the platform layer". The Rust side stays
 * wall-clock-free by design; this helper is the canonical
 * Android-side schedule. Wiring it into a live call flow
 * lands with the SFU integration in 3b.
 *
 * See:
 *   * `docs/security/calls.md` C-9 — overlap window
 *     invariant.
 *   * `docs/security/known-gaps.md` G-18 — what this helper
 *     snaps.
 */
public class OverlapTimer(
    /** Coroutine scope the schedule task runs on. Typically
     *  a session-bound `CoroutineScope` so cancelling the
     *  scope cleans up an in-flight timer automatically. */
    private val scope: CoroutineScope,
    /** Time-to-live in milliseconds. Clamped to
     *  `[MIN_OVERLAP_TTL_MS, MAX_OVERLAP_TTL_MS]`. Defaults
     *  to [DEFAULT_OVERLAP_TTL_MS] (1 s per ADR-0008). */
    ttlMs: Long = DEFAULT_OVERLAP_TTL_MS,
) {
    /** Effective TTL after clamping. Honest accessor — never
     *  out of `[MIN_OVERLAP_TTL_MS, MAX_OVERLAP_TTL_MS]`. */
    public val ttlMs: Long = ttlMs.coerceIn(MIN_OVERLAP_TTL_MS, MAX_OVERLAP_TTL_MS)

    /** Lifecycle state. Useful for diagnostics + tests. */
    public enum class State { DISARMED, ARMED, FIRED }

    private val mutex = Mutex()
    private var job: Job? = null
    private var currentState: State = State.DISARMED

    /** Current state. */
    public suspend fun state(): State = mutex.withLock { currentState }

    /** `true` iff a callback is scheduled and hasn't fired
     *  or been cancelled. */
    public suspend fun isArmed(): Boolean =
        mutex.withLock { currentState == State.ARMED }

    /**
     * Schedule [callback] to fire after [ttlMs]. Replaces
     * any prior schedule — the first callback will never
     * fire. Caller invokes this immediately after
     * `refresh_call_key` on the Rust core.
     *
     * `callback` runs on [Dispatchers.Default]; if it needs
     * `Main` semantics the caller wraps it in a
     * `withContext(Dispatchers.Main) { ... }` inside the
     * lambda.
     */
    public fun arm(callback: () -> Unit) {
        // Snapshot + cancel prior job under the lock to
        // serialise concurrent arm() calls.
        val newJob = scope.launch(Dispatchers.Default) {
            // Yield once so the new job is reachable in the
            // shared `job` slot before the parent path
            // races past the launch.
            kotlinx.coroutines.yield()
            try {
                delay(this@OverlapTimer.ttlMs)
            } catch (_: Throwable) {
                // Cancellation drops the schedule silently.
                return@launch
            }
            if (!isActive) return@launch
            // Run callback FIRST so a re-entrant arm()
            // inside it observes the prior shot as
            // completed.
            callback()
            mutex.withLock {
                if (currentState == State.ARMED) {
                    currentState = State.FIRED
                }
                if (job?.isActive == false) {
                    job = null
                }
            }
        }

        scope.launch {
            mutex.withLock {
                job?.cancel()
                job = newJob
                currentState = State.ARMED
            }
        }
    }

    /** Cancel the pending callback, if any. No-op when
     *  disarmed. */
    public fun cancel() {
        scope.launch {
            mutex.withLock {
                job?.cancel()
                job = null
                currentState = State.DISARMED
            }
        }
    }

    public companion object {
        /** Default overlap window — 1 s per ADR-0008. */
        public const val DEFAULT_OVERLAP_TTL_MS: Long = 1_000L
        /** Anything less skips real in-flight frames. */
        public const val MIN_OVERLAP_TTL_MS: Long = 50L
        /** Anything more extends the seed-boundary
         *  forward-secrecy window unacceptably. */
        public const val MAX_OVERLAP_TTL_MS: Long = 10_000L
    }
}
