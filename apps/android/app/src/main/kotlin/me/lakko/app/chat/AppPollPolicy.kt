// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/**
 * How long the app-level receive loop waits per group, so that EVERY group is
 * observed often enough for an incoming call to still be ringing when its offer
 * is read — `docs/ux/calls.md` section 2.1.
 *
 * ## The defect this closes
 *
 * An incoming call rang only while the callee had that chat open. The routing
 * was not the problem: `ChatRepository.oneToOneCallInbound` is installed once in
 * `MainActivity.onCreate` and every poll of every group feeds it. The problem
 * was WHEN the offer was read.
 *
 * `pollAllGroups` fans out with a bounded concurrency ([POLL_ALL_CONCURRENCY],
 * 3 — the DS shares one OkHttp host with a five-slot budget) and waits for the
 * whole fan-out before starting again. So the groups do not all long-poll at
 * once: they go in **waves**, and a full sweep takes `waitSecs x ceil(N /
 * concurrency)`. At the previous fixed 30 s that is 60 s for four chats, 90 s
 * for seven, five minutes for thirty. An offer published just after a group's
 * wave finished is therefore not read for a whole sweep — long past the
 * caller's 45 s ring window ([me.lakko.app.calls.CALL_RING_WINDOW_MS]), and
 * often past the host's 60 s stale-offer cut-off, at which point it is
 * deliberately never rung at all.
 *
 * The one group that escaped this was the one whose chat screen was open: that
 * screen runs its own uninterrupted long poll for it. That is exactly the
 * screen dependence the spec forbids — a call rang where the user happened to
 * be looking, and nowhere else.
 *
 * ## The rule
 *
 * Divide the caller's budget across the waves, so one sweep costs about one
 * wait rather than one wait per wave. A set that fits in a single wave is
 * untouched, which is the common case and the one that was never broken.
 *
 * ## What bounds the cost
 *
 * Shortening the wait means more requests, so the floor is what keeps that
 * honest: at most [POLL_ALL_CONCURRENCY] polls are ever in flight and each
 * lasts at least [APP_POLL_MIN_WAIT_SECS], so the request rate is bounded by
 * `concurrency / minWait` — about 0.6 per second — **no matter how many chats
 * the account has**. What grows with the number of chats is the sweep period,
 * not the load.
 *
 * Very large accounts still sweep slower than the ring window (thirty chats
 * sweep in about 50 s rather than 300 s). Stated rather than hidden: closing
 * that last gap needs one multiplexed DS subscription instead of a poll per
 * group, which is a server change and not this one.
 *
 * Pure and Android-free, so the arithmetic is unit-tested on the host JVM.
 */

/**
 * Target duration of one full sweep, in seconds: every group polled at least
 * once within this. Chosen against the 45 s ring window with room to spare —
 * an offer read at the end of a sweep still leaves the callee time to answer.
 */
public const val APP_POLL_SWEEP_BUDGET_SECS: Int = 25

/**
 * Floor for a per-group wait. This, with the concurrency cap, is what bounds
 * the request rate; without it a large account would poll in a tight loop.
 */
public const val APP_POLL_MIN_WAIT_SECS: Int = 5

/** How many sequential waves [groupCount] groups take at [concurrency]. */
public fun appPollWaves(groupCount: Int, concurrency: Int): Int {
    if (groupCount <= 0) return 0
    val lanes = concurrency.coerceAtLeast(1)
    return (groupCount + lanes - 1) / lanes
}

/**
 * The per-group long-poll wait for a sweep of [groupCount] groups.
 *
 * [maxWaitSecs] is the caller's own budget and is never exceeded. A
 * non-positive [maxWaitSecs] means "do not long-poll at all" (the pull-to-
 * refresh pass) and is returned unchanged — shortening a wait that is already
 * zero would be meaningless, and dividing it would not be zero.
 */
public fun appPollWaitSecs(
    groupCount: Int,
    concurrency: Int,
    maxWaitSecs: Int,
    budgetSecs: Int = APP_POLL_SWEEP_BUDGET_SECS,
    minWaitSecs: Int = APP_POLL_MIN_WAIT_SECS,
): Int {
    if (maxWaitSecs <= 0) return maxWaitSecs
    val waves = appPollWaves(groupCount, concurrency)
    // One wave, or none: the whole set long-polls at once, so the sweep IS one
    // wait and there is nothing to divide.
    if (waves <= 1) return maxWaitSecs
    val floor = minWaitSecs.coerceAtLeast(1).coerceAtMost(maxWaitSecs)
    return (budgetSecs / waves).coerceIn(floor, maxWaitSecs)
}

/**
 * How long a full sweep of [groupCount] groups takes at the wait
 * [appPollWaitSecs] would choose. The number the ring window has to beat.
 */
public fun appPollSweepSecs(
    groupCount: Int,
    concurrency: Int,
    maxWaitSecs: Int,
    budgetSecs: Int = APP_POLL_SWEEP_BUDGET_SECS,
    minWaitSecs: Int = APP_POLL_MIN_WAIT_SECS,
): Int = appPollWaves(groupCount, concurrency) *
    appPollWaitSecs(groupCount, concurrency, maxWaitSecs, budgetSecs, minWaitSecs)
