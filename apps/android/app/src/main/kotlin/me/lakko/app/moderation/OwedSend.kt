// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.withTimeoutOrNull

/*
 * UX `reporting-and-moderation.md` section 1.4 — "The block takes effect on
 * this device at once, and on the account's other devices when they sync",
 * and "Block saved, sync failed: Nothing visible. The block holds on this
 * device and syncs later."
 *
 * So the send to the account's other devices never holds the block: the
 * register is folded first, then [sendOwed] runs the send in the background.
 * Each attempt is bounded (a publish that hangs counts as a failure once its
 * time is up); a failed attempt is tried again after the next delay; when the
 * delays run out the send stays owed (the sync runtime's pending flag).
 */

/** How long one attempt may take (chosen, not measured). */
public const val OWED_SEND_ATTEMPT_MS: Long = 20_000L

/** When a failed send is tried again (chosen, not measured). */
public val OWED_SEND_RETRY_DELAYS_MS: List<Long> = listOf(30_000L, 120_000L, 600_000L)

/**
 * Run `attempt(0)`, then `attempt(1)`, ... after each of [retryDelaysMs]
 * until one returns `true`. A throw or an attempt past [attemptMs] counts as
 * a failure; [onFailure] runs after each. Stops when [isCurrent] turns
 * `false` (the work no longer belongs to the signed-in account). Returns
 * whether the send went. Never throws but for cancellation.
 */
public suspend fun sendOwed(
    attempt: suspend (index: Int) -> Boolean,
    attemptMs: Long = OWED_SEND_ATTEMPT_MS,
    retryDelaysMs: List<Long> = OWED_SEND_RETRY_DELAYS_MS,
    isCurrent: () -> Boolean = { true },
    onFailure: () -> Unit = {},
): Boolean {
    for (index in 0..retryDelaysMs.size) {
        if (index > 0) delay(retryDelaysMs[index - 1])
        if (!isCurrent()) return false
        val went = withTimeoutOrNull(attemptMs) {
            try {
                attempt(index)
            } catch (cause: CancellationException) {
                throw cause
            } catch (_: Throwable) {
                false
            }
        } == true
        if (went) return true
        onFailure()
    }
    return false
}
