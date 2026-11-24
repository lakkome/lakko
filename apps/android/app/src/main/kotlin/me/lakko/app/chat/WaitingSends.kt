// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.transport.causedByNetworkFailure

/**
 * `docs/ux/connection-state.md` section 7.2 — how many times a message that
 * failed for a network reason is sent again by itself. After this many
 * automatic sends that also failed for a network reason it reads
 * "Not sent" with Retry, and only the person's tap sends it again. Chosen,
 * not measured.
 */
public const val MAX_AUTO_RESENDS: Int = 5

/**
 * How long a conversation waits before its pass when a message was left
 * waiting while the connection state already read Online (an answer to
 * another request came in between the failure and the mark, so no transition
 * to Online is still to come). Chosen, not measured.
 */
internal const val WAITING_WHILE_ONLINE_RECHECK_MS: Long = 3_000L

/** How a send that did not land is filed (section 7.1). */
public enum class SendFailureKind {
    /** No answer from the network: the message waits and goes by itself. */
    WAITING,

    /** Anything else: "Not sent", and nothing sends it by itself. */
    FAILED,
}

/**
 * Section 7.1 — a send that failed because a request got no answer (through
 * any wrapper) waits; every other failure (an HTTP answer, a refusal, a
 * suspended account, a conversation that is behind, a compose error) is
 * "Not sent". Pure.
 */
public fun classifySendFailure(cause: Throwable): SendFailureKind =
    if (causedByNetworkFailure(cause)) SendFailureKind.WAITING else SendFailureKind.FAILED

/**
 * Section 7.2 — the state a message is left in after an AUTOMATIC send that
 * failed, given how many automatic sends of it (this one included) have now
 * failed for a network reason. Pure.
 */
public fun ackAfterAutoFailure(kind: SendFailureKind, networkFailuresSoFar: Int): Ack = when {
    kind == SendFailureKind.FAILED -> Ack.FAILED
    networkFailuresSoFar >= MAX_AUTO_RESENDS -> Ack.FAILED
    else -> Ack.WAITING
}

/**
 * The waiting rows of one conversation in the order they go: oldest first. A
 * row's local id counts DOWN as rows are created ([nextLocalRowId]), so the
 * oldest is the largest. Rows in flight are `PENDING` and never listed. Pure.
 */
public fun waitingInSendOrder(rows: List<ChatMessage>): List<Long> =
    rows.asSequence()
        .filter { it.ack == Ack.WAITING && it.localId != 0L }
        .map { it.localId }
        .sortedDescending()
        .toList()

/** What one send of a waiting row did. */
public enum class ResendOutcome {
    /** It landed. */
    SENT,

    /** No answer from the network: it is waiting again. */
    NETWORK,

    /** Refused for another reason: it reads "Not sent". */
    REFUSED,

    /** The row or its conversation is gone: nothing to do. */
    GONE,
}

/**
 * Section 7.2 — the passes that send waiting messages again, one
 * conversation at a time.
 *
 * - One pass per conversation at a time; a request while one runs makes the
 *   running pass look again before it ends.
 * - Oldest first, one at a time. A pass stops at the first message that fails
 *   again for a network reason; the ones after it keep waiting and spend no
 *   attempt.
 * - At most [MAX_AUTO_RESENDS] automatic sends per message; the send of the
 *   row the person tapped does not count as automatic.
 * - A new message in a conversation with waiting rows, or with a pass
 *   running, is queued behind them ([queueBehindIfWaiting]).
 *
 * The rows themselves live in the repository; this class only orders and
 * counts. The effects are injected so the rules are host-testable.
 */
public class WaitingSendPasses(
    private val scope: CoroutineScope,
    /** The conversation's waiting rows, in [waitingInSendOrder]. */
    private val waitingRows: (groupIdHex: String) -> List<Long>,
    /** Send the row once. It marks the row `PENDING` while it goes, then
     *  `SENT`, `WAITING` (on [ResendOutcome.NETWORK]) or `FAILED`.
     *  `automatic` is `false` for the row the person tapped. */
    private val resend: suspend (groupIdHex: String, localId: Long, automatic: Boolean) -> ResendOutcome,
    /** Turn a waiting row into "Not sent" (the attempts bound). */
    private val markFailed: (groupIdHex: String, localId: Long) -> Unit,
    private val isOnline: () -> Boolean,
    /** Returns once the connection state reads Online (at once if it does
     *  now). Section 7.2: only an answer from the servers starts a pass. */
    private val awaitOnline: suspend () -> Unit,
    private val onLog: (String) -> Unit = {},
    private val recheckDelayMs: Long = WAITING_WHILE_ONLINE_RECHECK_MS,
    /** Section 7.3 — the count of automatic sends of a row that failed for a
     *  network reason changed (the outbox stores it with the entry). */
    private val onNetworkFailureCount: (groupIdHex: String, localId: Long, count: Int) -> Unit = { _, _, _ -> },
) {
    private val lock = Any()
    private val running = HashSet<String>()
    private val lookAgain = HashSet<String>()
    private val rechecks = HashSet<String>()
    private val networkFailures = HashMap<Pair<String, Long>, Int>()

    /** How many automatic sends of the row failed for a network reason. */
    public fun networkFailuresOf(groupIdHex: String, localId: Long): Int =
        synchronized(lock) { networkFailures[groupIdHex to localId] ?: 0 }

    /** Section 7.3 — a row read back from the outbox carries the count of its
     *  automatic sends that already failed. */
    public fun seedNetworkFailures(groupIdHex: String, localId: Long, count: Int) {
        synchronized(lock) {
            if (count > 0) networkFailures[groupIdHex to localId] = count else networkFailures.remove(groupIdHex to localId)
        }
    }

    /** Forget a row's count (it was deleted, or the person sent it again). */
    public fun forgetNetworkFailures(groupIdHex: String, localId: Long) {
        synchronized(lock) { networkFailures.remove(groupIdHex to localId) }
    }

    /** Whether a pass of the conversation runs now. */
    public fun isRunning(groupIdHex: String): Boolean = synchronized(lock) { groupIdHex in running }

    /**
     * A new message (or a Retry of a "Not sent" one) is about to go. If the
     * conversation has waiting rows or a running pass, [markWaiting] files it
     * as waiting behind them - under the same lock the pass uses to pick its
     * next row and to end, so it is never stranded - and a pass is asked for
     * when the state is Online. Returns whether it was queued; `false` means
     * the caller sends it now.
     */
    public fun queueBehindIfWaiting(groupIdHex: String, markWaiting: () -> Unit): Boolean {
        val start = synchronized(lock) {
            if (groupIdHex !in running && waitingRows(groupIdHex).isEmpty()) return false
            markWaiting()
            if (groupIdHex in running) {
                lookAgain += groupIdHex
                false
            } else {
                true
            }
        }
        if (start) waitForOnline(groupIdHex)
        return true
    }

    /**
     * A row of the conversation was left waiting: its pass starts once the
     * state reads Online (section 7.2: the first answer from the servers).
     * When it already reads Online - an answer to another request came in
     * between the failure and now, so no transition is still to come - the
     * pass starts after [recheckDelayMs] instead of at once, so a server
     * that keeps dropping one request while answering others does not spin.
     * One waiter per conversation. [recheck] `false` (the rows were read
     * back at a session landing) starts the pass at once when the state
     * already reads Online.
     */
    public fun waitForOnline(groupIdHex: String, recheck: Boolean = true) {
        val first = synchronized(lock) { rechecks.add(groupIdHex) }
        if (!first) return
        scope.launch {
            try {
                if (recheck && isOnline()) delay(recheckDelayMs)
                awaitOnline()
            } finally {
                synchronized(lock) { rechecks.remove(groupIdHex) }
            }
            if (waitingRows(groupIdHex).isNotEmpty()) request(groupIdHex)
        }
    }

    /**
     * Start the conversation's pass now (a tap on a waiting row, the state
     * entering Online). [tappedLocalId]'s send, when reached, is not counted
     * as automatic.
     */
    public fun request(groupIdHex: String, tappedLocalId: Long? = null) {
        val start = synchronized(lock) {
            if (groupIdHex in running) {
                lookAgain += groupIdHex
                false
            } else {
                running += groupIdHex
                true
            }
        }
        if (start) scope.launch { runPass(groupIdHex, tappedLocalId) }
    }

    /** Drop everything (the session ended). */
    public fun clear() {
        synchronized(lock) {
            lookAgain.clear()
            networkFailures.clear()
            rechecks.clear()
        }
    }

    private suspend fun runPass(groupIdHex: String, tappedFirst: Long?) {
        var tapped = tappedFirst
        var stoppedOnNetwork = false
        try {
            while (true) {
                val next = synchronized(lock) {
                    val head = waitingRows(groupIdHex).firstOrNull()
                    if (head == null) {
                        // About to end: a request that came while this pass
                        // ran is answered by looking once more.
                        if (lookAgain.remove(groupIdHex)) waitingRows(groupIdHex).firstOrNull() else null
                    } else {
                        head
                    }
                } ?: break
                val automatic = next != tapped
                if (!automatic) tapped = null
                val outcome = try {
                    resend(groupIdHex, next, automatic)
                } catch (cancel: CancellationException) {
                    throw cancel
                } catch (cause: Throwable) {
                    onLog("waiting send threw ${cause.javaClass.simpleName}")
                    ResendOutcome.REFUSED
                }
                when (outcome) {
                    ResendOutcome.SENT, ResendOutcome.REFUSED, ResendOutcome.GONE ->
                        synchronized(lock) { networkFailures.remove(groupIdHex to next) }
                    ResendOutcome.NETWORK -> {
                        if (automatic) {
                            val count = synchronized(lock) {
                                val n = (networkFailures[groupIdHex to next] ?: 0) + 1
                                networkFailures[groupIdHex to next] = n
                                n
                            }
                            onNetworkFailureCount(groupIdHex, next, count)
                            if (ackAfterAutoFailure(SendFailureKind.WAITING, count) == Ack.FAILED) {
                                synchronized(lock) { networkFailures.remove(groupIdHex to next) }
                                markFailed(groupIdHex, next)
                                onLog("waiting send gave up after $count automatic sends")
                            }
                        }
                        stoppedOnNetwork = true
                        break
                    }
                }
            }
        } finally {
            val again = synchronized(lock) {
                running -= groupIdHex
                lookAgain.remove(groupIdHex) && !stoppedOnNetwork
            }
            if (again) request(groupIdHex)
        }
        if (stoppedOnNetwork && waitingRows(groupIdHex).isNotEmpty()) waitForOnline(groupIdHex)
    }
}
