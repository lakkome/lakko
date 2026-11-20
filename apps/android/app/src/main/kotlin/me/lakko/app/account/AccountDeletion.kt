// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.account

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withTimeoutOrNull

/**
 * The order of Delete account once the person confirmed it:
 *
 *   0. [prepare]: what must be had while the account still exists (the
 *      `push-register` tokens the push removals spend: auth issues tokens to
 *      a session, so none can be had after the delete). Bounded; a failure
 *      is logged and the deletion goes on;
 *   1. the auth delete (`DELETE /account`). It may be refused (the recall
 *      window, a missing fresh-auth proof); a refusal throws out of [run] and
 *      nothing below runs, so nothing is removed for an account that stays;
 *      Once it succeeded, [afterAuth] tells the session provider, so no
 *      refresh runs and no refusal signs the device out from here on
 *      (ADR-0098: a sign-out would stop the account-deleted statements);
 *   2. the cleanups only this device can do, because the servers cannot find
 *      the rows by the account (push subscriptions name no account), or as a
 *      fast path (the backup blob, the directory). They run side by side,
 *      each retried a few times within its own bound, all within one overall
 *      bound; a failure is logged by the cleanup's name only and stops
 *      nothing;
 *   3. the device wipe, always, once the account is gone at auth.
 */
public class AccountDeletion(
    private val prepare: suspend () -> Unit = {},
    private val deleteAtAuth: suspend () -> Unit,
    /** Runs once the auth delete succeeded, before any cleanup (ADR-0098):
     *  tells the session provider the account is gone, so a cleanup's `401`
     *  cannot sign the device out (and seal the account store) while the
     *  conversations are still being told. A failure is logged only. */
    private val afterAuth: () -> Unit = {},
    /** ADR-0098 (walk 10): the account-deleted statement. Runs to the end
     *  before any of [cleanups] starts: removing the directory entry at the
     *  same time rebuilt this device's conversation identity and stopped the
     *  statement in every conversation. Shares the overall bound. */
    private val first: List<Cleanup> = emptyList(),
    private val cleanups: List<Cleanup>,
    private val wipe: () -> Unit,
    private val attempts: Int = DEFAULT_ATTEMPTS,
    private val retryDelayMs: Long = DEFAULT_RETRY_DELAY_MS,
    private val perCleanupBoundMs: Long = DEFAULT_PER_CLEANUP_BOUND_MS,
    private val overallBoundMs: Long = DEFAULT_OVERALL_BOUND_MS,
    private val prepareBoundMs: Long = DEFAULT_PREPARE_BOUND_MS,
    private val log: (String) -> Unit = { android.util.Log.w(TAG, it) },
) {

    /** One server-side removal. [name] is what a failure is logged as: a
     *  fixed word, never an identifier. [run] throws on a failure, so it is
     *  tried again. */
    public class Cleanup(public val name: String, public val run: suspend () -> Unit)

    /** Runs the steps. Throws only what the auth delete threw. */
    public suspend fun run() {
        try {
            val prepared = withTimeoutOrNull(prepareBoundMs) { prepare() }
            if (prepared == null) log("account deletion: preparing ran out of time")
        } catch (cancel: CancellationException) {
            currentCoroutineContext().ensureActive()
            log("account deletion: preparing cancelled")
        } catch (cause: Throwable) {
            log("account deletion: preparing failed: ${cause.javaClass.simpleName}")
        }
        deleteAtAuth()
        try {
            afterAuth()
        } catch (cause: Throwable) {
            log("account deletion: after-auth step failed: ${cause.javaClass.simpleName}")
        }
        try {
            val finished = withTimeoutOrNull(overallBoundMs) {
                for (cleanup in first) runOne(cleanup)
                coroutineScope {
                    cleanups.map { cleanup -> async { runOne(cleanup) } }.awaitAll()
                }
            }
            if (finished == null) log("account deletion: cleanups ran out of time")
        } catch (cancel: CancellationException) {
            // A cancellation of this run goes on; one a cleanup threw by
            // itself is only a failed cleanup, and the wipe still runs.
            currentCoroutineContext().ensureActive()
            log("account deletion: cleanups cancelled")
        } catch (cause: Throwable) {
            log("account deletion: cleanups failed: ${cause.javaClass.simpleName}")
        }
        wipe()
    }

    private suspend fun runOne(cleanup: Cleanup) {
        val done = withTimeoutOrNull(perCleanupBoundMs) {
            var lastFailure: Throwable? = null
            for (attempt in 1..attempts.coerceAtLeast(1)) {
                if (attempt > 1) delay(retryDelayMs)
                try {
                    cleanup.run()
                    return@withTimeoutOrNull true
                } catch (cancel: CancellationException) {
                    // The bound or the caller: stop. Otherwise a failure.
                    currentCoroutineContext().ensureActive()
                    lastFailure = cancel
                } catch (cause: Throwable) {
                    lastFailure = cause
                }
            }
            log("account deletion: ${cleanup.name} failed: ${lastFailure?.javaClass?.simpleName}")
            false
        }
        if (done == null) log("account deletion: ${cleanup.name} ran out of time")
    }

    public companion object {
        /** Logcat tag. */
        public const val TAG: String = "lakko-account"

        /** Tries per cleanup. Chosen, not measured. */
        public const val DEFAULT_ATTEMPTS: Int = 3

        /** Pause between two tries. Chosen, not measured. */
        public const val DEFAULT_RETRY_DELAY_MS: Long = 500

        /** One cleanup's bound across its tries. Chosen, not measured. */
        public const val DEFAULT_PER_CLEANUP_BOUND_MS: Long = 8_000

        /** All cleanups together. Chosen, not measured. */
        public const val DEFAULT_OVERALL_BOUND_MS: Long = 10_000

        /** How long [prepare] may hold the deletion back. Chosen. */
        public const val DEFAULT_PREPARE_BOUND_MS: Long = 6_000

        /** Spare tokens beyond one per subscription, for the one retry a
         *  refused token gets. Chosen. */
        public const val PUSH_TOKEN_SPARE: Int = 2

        /** How many `push-register` tokens to take before the auth delete for
         *  [subscriptions] push subscriptions: one each plus [PUSH_TOKEN_SPARE],
         *  none when there are none. */
        public fun pushTokensNeeded(subscriptions: Int): Int =
            if (subscriptions <= 0) 0 else subscriptions + PUSH_TOKEN_SPARE
    }
}

/**
 * ADR-0098 section 1 — the cleanup that tells every conversation the account
 * was deleted. Being a cleanup puts it where the ADR wants it: after the auth
 * delete succeeded (a refusal throws before any cleanup runs, so the
 * statement is never a false claim about an account that stays) and before
 * the wipe, inside the shared budget. [announce] posts the statement to every
 * conversation not yet in the set it is handed and throws when any is left,
 * so a retry reaches only the rest. A failure is logged by this cleanup's
 * name ("conversations") and never stops the wipe.
 */
public fun accountDeletedAnnouncement(
    seeds: List<ByteArray>,
    announce: suspend (seeds: List<ByteArray>, sent: MutableSet<String>) -> Unit,
): AccountDeletion.Cleanup {
    val sent: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()
    return AccountDeletion.Cleanup("conversations") { announce(seeds, sent) }
}
