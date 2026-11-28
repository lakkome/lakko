// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.CancellationException

/*
 * ADR-0074 "T5 as built" items 17, 24 and 25 - two retries an account
 * statement's sender owes, as pure decisions a host JVM test runs.
 *
 *   * `401 {"error":"statement-stale","server_time_ms":T}` is answered ONLY
 *     to a statement whose signature verified: the device holds the right
 *     key and only its clock is off. It signs the same claims again at the
 *     server's time and repeats the request ONCE. Nothing was counted, and
 *     the answer is never a reason to destroy a recall record or to treat a
 *     session as superseded.
 *   * `recall/finish` is safely retryable: the byte-identical request sent
 *     again within ten minutes is answered `200` again with the same
 *     attestation. A lost answer is therefore retried with the same bytes
 *     instead of abandoning the new phrase.
 */

/** The error code of a statement that is right in every way but its time. */
public const val STATEMENT_STALE: String = "statement-stale"

/** The server's clock as a `statement-stale` answer states it, or `null`
 *  when [err] is anything else (a stale answer without a usable time
 *  included: there is nothing to sign at). */
public fun statementStaleServerTimeMs(err: Throwable): Long? {
    if (err !is AuthHttpError) return null
    if (err.status != HTTP_UNAUTHORIZED || err.code != STATEMENT_STALE) return null
    return me.lakko.app.anon.longFieldOf(err.body, "server_time_ms")?.takeIf { it > 0 }
}

/**
 * Send a request that carries an account statement. [send] is called with
 * `null` first (sign at this device's clock); when the answer is
 * `statement-stale`, it is called ONCE more with the server's time plus the
 * milliseconds that passed since the answer was read. A second stale answer
 * propagates like any other failure.
 */
public suspend fun <T> sendWithStaleStatementRetry(
    monotonicMs: () -> Long = { System.nanoTime() / NANOS_PER_MS },
    send: suspend (signAtMs: Long?) -> T,
): T {
    val serverTimeMs: Long
    val readAt: Long
    try {
        return send(null)
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (err: Throwable) {
        serverTimeMs = statementStaleServerTimeMs(err) ?: throw err
        readAt = monotonicMs()
    }
    val elapsed = (monotonicMs() - readAt).coerceAtLeast(0)
    return send(serverTimeMs + elapsed)
}

/** How long the server answers a repeated `recall/finish` again (the
 *  attempt's own ten minutes). */
public const val RECALL_FINISH_REPLAY_WINDOW_MS: Long = 10L * 60 * 1000

/** The waits between repeats of an unanswered `recall/finish` inside one
 *  call (chosen, not measured). */
public val RECALL_FINISH_RETRY_DELAYS_MS: List<Long> = listOf(1_000L, 3_000L, 8_000L)

/** Whether [err] means "no answer was read": the request may have landed. */
public fun recallFinishAnswerLost(err: Throwable): Boolean = when (err) {
    is AuthNetworkError -> true
    is AuthHttpError -> err.status in LOST_ANSWER_STATUSES
    else -> false
}

/**
 * The wait before the byte-identical `finish` is sent again, or `null` when
 * it is not sent again from here: [err] is a real answer, the in-call
 * repeats are used up, or the replay window has passed.
 */
public fun recallFinishRetryDelayMs(
    err: Throwable,
    repeatsMade: Int,
    firstSentAtMs: Long,
    nowMs: Long,
): Long? {
    if (!recallFinishAnswerLost(err)) return null
    if (!recallFinishReplayable(firstSentAtMs, nowMs)) return null
    return RECALL_FINISH_RETRY_DELAYS_MS.getOrNull(repeatsMade)
}

/** Whether a `finish` first sent at [firstSentAtMs] can still be asked
 *  again at [nowMs]. A clock that went backwards reads as "no". */
public fun recallFinishReplayable(firstSentAtMs: Long, nowMs: Long): Boolean =
    nowMs >= firstSentAtMs && nowMs - firstSentAtMs < RECALL_FINISH_REPLAY_WINDOW_MS

/**
 * `recall/finish` was sent and no answer was read, in-call repeats included.
 * The request and what it would make the account's are kept by the caller
 * and the SAME request is sent again ([AuthClient.recallFinishResend]) while
 * [recallFinishReplayable]; the generated phrase is not abandoned.
 */
public class RecallFinishUnanswered(
    public val request: RecallFinishRequest,
    public val firstSentAtMs: Long,
    cause: Throwable,
    /** The new password's export key the request was made with, kept with
     *  the request (ADR-0101 section 10). Secret. */
    public val exportKey: ByteArray? = null,
) : RuntimeException("recall/finish was sent and no answer was read", cause)

private const val NANOS_PER_MS: Long = 1_000_000L
private const val HTTP_UNAUTHORIZED: Int = 401
private val LOST_ANSWER_STATUSES: Set<Int> = setOf(502, 503, 504)
