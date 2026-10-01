// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.async
import kotlinx.coroutines.withTimeoutOrNull
import java.util.concurrent.ConcurrentHashMap

/**
 * ADR-0045 x ADR-0057 — how often a committer may re-stage a sweep whose epoch
 * registration the Delivery Service refused.
 *
 * Found live: after the ADR-0057 flag day dropped every earlier claim, a sweep
 * in an unclaimed journal was refused (`401`, or `400`) on every poll tail,
 * roughly every five seconds, forever. The refusal is not transient — nothing
 * changes until the owner claims the journal or the group itself changes — so
 * retrying at poll cadence only spends requests.
 *
 * The schedule: the first refusal holds re-staging for [INITIAL_DELAY_MS], each
 * further refusal at the SAME epoch doubles the hold, capped at [MAX_DELAY_MS].
 * A different epoch (any membership or role change advances it) lifts the hold
 * at once, as does a registration that lands or the owner's claim landing
 * (the caller's [SweepRegistrationBackoffTracker.reset]).
 *
 * Only the cadence changes. The sweep that was refused has already been
 * abandoned (nothing of it was recorded or published), and a sweep that IS
 * staged still owes its verdict (C-67): the hold is consulted only before a NEW
 * stage, after the owed verdict and owed Welcome paths have run.
 *
 * Pure and clock-free, so the host suite pins every rule.
 */
internal object SweepRegistrationBackoff {
    /** The hold after the first refusal. Chosen, not measured. */
    const val INITIAL_DELAY_MS: Long = 30_000L

    /** The longest hold. Chosen, not measured. */
    const val MAX_DELAY_MS: Long = 600_000L

    /** One group's hold: its length, when it ends, and the epoch it was
     *  imposed at (`null` when the epoch could not be read). */
    data class State(val delayMs: Long, val notBeforeMs: Long, val epoch: ULong?)

    /** Whether a registration refusal with [status] starts or extends a hold:
     *  the explicit refusals `400`, `401` and `403`. A `409` belongs to the
     *  repair check, and a network failure is not a refusal. */
    fun isBackoffStatus(status: Int): Boolean = status == 400 || status == 401 || status == 403

    /** The hold after a refusal at [epoch], given the [previous] one. */
    fun afterRefusal(previous: State?, nowMs: Long, epoch: ULong?): State {
        val delay = if (previous == null || previous.epoch != epoch) {
            INITIAL_DELAY_MS
        } else {
            minOf(previous.delayMs * 2, MAX_DELAY_MS)
        }
        return State(delayMs = delay, notBeforeMs = nowMs + delay, epoch = epoch)
    }

    /** Whether a new sweep may be staged at [epoch] now. */
    fun mayStage(state: State?, nowMs: Long, epoch: ULong?): Boolean =
        state == null || state.epoch != epoch || nowMs >= state.notBeforeMs
}

/** The per-group holds of [SweepRegistrationBackoff], with an injected clock. */
internal class SweepRegistrationBackoffTracker(
    private val clock: () -> Long = System::currentTimeMillis,
) {
    private val states = ConcurrentHashMap<String, SweepRegistrationBackoff.State>()

    fun mayStage(groupIdHex: String, epoch: ULong?): Boolean =
        SweepRegistrationBackoff.mayStage(states[groupIdHex], clock(), epoch)

    /** Record a refusal at [epoch]; returns the new hold. */
    fun onRefused(groupIdHex: String, epoch: ULong?): SweepRegistrationBackoff.State {
        // One group's sweeps run under its per-group lock, so this
        // read-then-write has no concurrent writer for the same key.
        val next = SweepRegistrationBackoff.afterRefusal(states[groupIdHex], clock(), epoch)
        states[groupIdHex] = next
        return next
    }

    fun reset(groupIdHex: String) {
        states.remove(groupIdHex)
    }
}

/**
 * ADR-0057 — the owner's claim of a group journal on open, made visible to the
 * sweep.
 *
 * The claim on open is launched, never awaited by the open itself, so without
 * this a sweep at the tail of the first poll could register its epoch before
 * the claim had reached the Delivery Service and be refused against a journal
 * that was about to be claimed. [awaitBeforeRegistering] lets the sweep wait
 * (bounded) for a claim still in flight; with none in flight it returns at
 * once. The wait never fails the caller: a claim that failed or timed out
 * leaves the registration to be refused and backed off.
 *
 * The caller must not hold anything the claim needs: the repository's sweep
 * holds only the per-group lock, and the claim takes the provider lock.
 */
internal class OwnerClaimGate {
    private val inFlight = ConcurrentHashMap<String, Deferred<Boolean>>()

    /** Launch [claim] for [groupIdHex] on [scope] and track it until it ends.
     *  [claim] must not throw (catch and answer `false`). */
    fun start(groupIdHex: String, scope: CoroutineScope, claim: suspend () -> Boolean): Deferred<Boolean> {
        val deferred = scope.async { claim() }
        inFlight[groupIdHex] = deferred
        deferred.invokeOnCompletion { inFlight.remove(groupIdHex, deferred) }
        return deferred
    }

    /** Wait up to [timeoutMs] for a claim of [groupIdHex] still in flight. */
    suspend fun awaitBeforeRegistering(groupIdHex: String, timeoutMs: Long = DEFAULT_WAIT_MS) {
        val pending = inFlight[groupIdHex] ?: return
        withTimeoutOrNull(timeoutMs) { pending.join() }
    }

    companion object {
        /** The longest a sweep waits for the claim, the bound the other
         *  network calls under the per-group lock use. */
        const val DEFAULT_WAIT_MS: Long = 15_000L
    }
}
