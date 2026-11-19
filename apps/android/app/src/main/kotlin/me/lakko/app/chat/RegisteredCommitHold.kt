// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.concurrent.ConcurrentHashMap
import me.lakko.app.ds.MailboxMessage

/*
 * ADR-0091 phase 1 — a device does not skip the commit that moves its
 * conversation.
 *
 * Until now a conversation row that failed to apply was dropped and the
 * cursor moved past it, with no log. When that row was the commit the DS
 * registered as this device's next epoch, the device stayed at epoch N while
 * the group went on without it, silently and for good ([DurableCursorGate]
 * describes the same end state from the persist side).
 *
 * Since ADR-0081 an epoch is registered on the DS only together with the
 * commit row that reaches it, and a signed journal read names, for every epoch
 * above the one it was signed at, the row registered with it (`commits`, read
 * as [me.lakko.app.ds.FetchResult.registeredCommits]). A failed row that is the
 * registered commit leaving this device's epoch (`registeredCommits[local + 1]`
 * is its id) is HELD: the batch stops at it, the cursor stays before it, and it
 * is retried on later polls with a bounded backoff. Every other failed row is
 * skipped as before, so a row that can never apply still never wedges the
 * poll, and a commit-shaped row a member posted through the plain publish
 * route is skipped at once (ADR-0091 open question 6, closed). The same rule
 * and truth table as web (`chat/registered-commit-hold.ts`) and iOS.
 *
 * Everything here is pure Kotlin with the epochs passed in, so the host JVM
 * tests (which do not load the native core) can drive it.
 */

/** What a failed conversation row means. */
internal enum class RegisteredCommitVerdict {
    /** The registered commit for this device's next epoch: do not skip it. */
    HOLD,

    /** Anything else: skip it as before. */
    SKIP,
}

/**
 * The id of the row the DS registered as the commit leaving [localEpoch], or
 * `null` when the read named none (an unclaimed journal, a device at the head,
 * a read signed above [localEpoch]).
 */
internal fun registeredCommitLeaving(registeredCommits: Map<ULong, Long>?, localEpoch: ULong?): Long? {
    if (registeredCommits == null || localEpoch == null) return null
    // No next epoch exists to hold for.
    if (localEpoch == ULong.MAX_VALUE) return null
    return registeredCommits[localEpoch + 1uL]
}

/**
 * The verdict for a conversation row that failed to apply: HOLD exactly when
 * it is the registered commit leaving this device's epoch.
 *
 * [isOwnEcho]: the failure was this device's own row (`CannotDecryptOwnMessage`).
 * [rowId]: the row's DS id. [localEpoch]: the group's epoch read just before
 * the row was processed. [registeredCommits]: the same fetch's `commits`.
 */
internal fun registeredCommitVerdict(
    isOwnEcho: Boolean,
    rowId: Long,
    localEpoch: ULong?,
    registeredCommits: Map<ULong, Long>?,
): RegisteredCommitVerdict {
    if (isOwnEcho) return RegisteredCommitVerdict.SKIP
    val leaving = registeredCommitLeaving(registeredCommits, localEpoch) ?: return RegisteredCommitVerdict.SKIP
    return if (leaving == rowId) RegisteredCommitVerdict.HOLD else RegisteredCommitVerdict.SKIP
}

/** Attempts at one held row before it is skipped. Chosen, not measured. */
internal const val REGISTERED_COMMIT_MAX_ATTEMPTS: Int = 8

/** The wait after failure k (k = 1..7). Chosen, not measured. */
internal val REGISTERED_COMMIT_RETRY_DELAYS_SECS: List<Long> = listOf(2L, 5L, 15L, 30L, 60L, 120L, 300L)

private const val MS_PER_SECOND: Long = 1_000L

/** One conversation's held row. */
internal data class RegisteredCommitHold(
    val rowId: Long,
    /** The epoch the held commit leaves. */
    val localEpoch: ULong,
    val attempts: Int,
    val firstFailedAtMs: Long,
    val nextAttemptAtMs: Long,
)

/** This device skipped a registered commit after every attempt: it is behind
 *  in the conversation from [epoch] on. Kept for phase 2. */
internal data class BehindMark(val epoch: ULong, val atMs: Long)

/** What [RegisteredCommitHoldTracker.recordFailure] decided. */
internal data class HoldDecision(
    /** The attempt that just failed, 1..[REGISTERED_COMMIT_MAX_ATTEMPTS]. */
    val attempt: Int,
    /** `true`: stop the batch at the row. `false`: the attempts are spent,
     *  skip it. */
    val held: Boolean,
)

/**
 * The in-memory, process-wide record of held rows and behind marks, keyed by
 * group id (hex, case-insensitive). Nothing here is persisted: a restart reads
 * the journal from the held position and so retries at once.
 */
internal class RegisteredCommitHoldTracker(
    private val clock: () -> Long = System::currentTimeMillis,
) {
    private val holds = ConcurrentHashMap<String, RegisteredCommitHold>()
    private val behind = ConcurrentHashMap<String, BehindMark>()

    fun hold(groupIdHex: String): RegisteredCommitHold? = holds[key(groupIdHex)]

    fun behindMark(groupIdHex: String): BehindMark? = behind[key(groupIdHex)]

    /** A HOLD row failed to apply. */
    fun recordFailure(groupIdHex: String, rowId: Long, localEpoch: ULong): HoldDecision {
        val k = key(groupIdHex)
        val now = clock()
        val previous = holds[k]
        val attempts = if (previous != null && previous.rowId == rowId) previous.attempts + 1 else 1
        if (attempts >= REGISTERED_COMMIT_MAX_ATTEMPTS) {
            holds.remove(k)
            behind[k] = BehindMark(localEpoch, now)
            return HoldDecision(attempt = REGISTERED_COMMIT_MAX_ATTEMPTS, held = false)
        }
        val delayMs = REGISTERED_COMMIT_RETRY_DELAYS_SECS[attempts - 1] * MS_PER_SECOND
        holds[k] = RegisteredCommitHold(
            rowId = rowId,
            localEpoch = localEpoch,
            attempts = attempts,
            firstFailedAtMs = if (previous != null && previous.rowId == rowId) previous.firstFailedAtMs else now,
            nextAttemptAtMs = now + delayMs,
        )
        return HoldDecision(attempt = attempts, held = true)
    }

    /** [rowId] applied, or was skipped as no longer the registered commit:
     *  a hold on it is dropped. */
    fun release(groupIdHex: String, rowId: Long) {
        holds.computeIfPresent(key(groupIdHex)) { _, h -> if (h.rowId == rowId) null else h }
    }

    /** The cursor is at or past the held row (another path consumed it). */
    fun releaseIfPassed(groupIdHex: String, cursor: Long) {
        holds.computeIfPresent(key(groupIdHex)) { _, h -> if (h.rowId <= cursor) null else h }
    }

    /** The group's epoch is now [epoch]: a behind mark below it is dropped. */
    fun observeEpoch(groupIdHex: String, epoch: ULong) {
        behind.computeIfPresent(key(groupIdHex)) { _, m -> if (epoch > m.epoch) null else m }
    }

    /** `true` when no hold waits, or its next attempt is due. */
    fun isDue(groupIdHex: String, now: Long = clock()): Boolean {
        val h = holds[key(groupIdHex)] ?: return true
        return now >= h.nextAttemptAtMs
    }

    /** Milliseconds until the held row may be tried again; `null` with no
     *  hold, `0` when due. */
    fun msUntilDue(groupIdHex: String, now: Long = clock()): Long? {
        val h = holds[key(groupIdHex)] ?: return null
        return (h.nextAttemptAtMs - now).coerceAtLeast(0L)
    }

    /**
     * ADR-0057 journal repair must not Replace at [epoch] while this group
     * holds a registered commit, or carries a behind mark at or above it:
     * the epochs above are then real.
     */
    fun repairBlocked(groupIdHex: String, epoch: ULong): Boolean {
        val k = key(groupIdHex)
        if (holds.containsKey(k)) return true
        val mark = behind[k] ?: return false
        return mark.epoch >= epoch
    }

    /** Session end. */
    fun clear() {
        holds.clear()
        behind.clear()
    }

    private fun key(groupIdHex: String): String = groupIdHex.lowercase()

    companion object {
        /** The process-wide tracker the conversation poll uses. */
        val shared: RegisteredCommitHoldTracker = RegisteredCommitHoldTracker()
    }
}

// ---------------------------------------------------------------------------
// Log lines (no identifiers: never a group id, row id, key, account, name or
// text)
// ---------------------------------------------------------------------------

/** The Android log tag of the failed-row lines. */
internal const val FAILED_ROW_LOG_TAG: String = "ChatReceive"

/** Log level of a failed-row line. */
internal enum class FailedRowLogLevel { WARN, ERROR }

private fun epochText(epoch: ULong?): String = epoch?.toString() ?: "-"

/** The one line for a failed apply. [attempt] `null` on a skip. */
internal fun failedRowLine(
    kind: String,
    sealedEpoch: ULong?,
    epochAfter: ULong?,
    localEpoch: ULong?,
    latestEpoch: ULong?,
    verdict: RegisteredCommitVerdict,
    attempt: Int?,
): String {
    val verdictText = if (verdict == RegisteredCommitVerdict.HOLD) "hold" else "skip"
    return "chat row not applied: kind=$kind sealed=${epochText(sealedEpoch)} " +
        "after=${epochText(epochAfter)} local=${epochText(localEpoch)} " +
        "latest=${epochText(latestEpoch)} verdict=$verdictText " +
        "attempt=${attempt?.toString() ?: "-"}/$REGISTERED_COMMIT_MAX_ATTEMPTS"
}

/** The line when the attempts at a registered commit are spent. */
internal fun exhaustionLine(localEpoch: ULong?, latestEpoch: ULong?): String =
    "chat registered commit skipped after $REGISTERED_COMMIT_MAX_ATTEMPTS attempts: " +
        "this device is now behind in a conversation " +
        "(local=${epochText(localEpoch)} latest=${epochText(latestEpoch)})"

/** The line when this device refuses, by its own rules, a commit the DS
 *  registered as the next epoch. */
internal fun divergenceLine(kind: String): String =
    "chat registered commit refused by this device's rules ($kind): " +
        "the other devices applied it; a bug or version skew"

/** `NOT_A_MEMBER` -> `notAMember`, the core's refusal code spelling. */
internal fun refusalCode(enumName: String): String {
    val parts = enumName.lowercase().split('_').filter { it.isNotEmpty() }
    if (parts.isEmpty()) return enumName
    return parts.first() + parts.drop(1).joinToString("") { part ->
        part.replaceFirstChar { it.uppercaseChar() }
    }
}

/**
 * The failure kind: `refused:<code>` from a typed refusal first, then from the
 * error text: `wrong-epoch`, `own-message`, `missing-proposal`, `decrypt`,
 * else `other`.
 */
internal fun failedRowKind(error: Throwable): String {
    val reason = try {
        membershipRefusalReason(error)
    } catch (_: Throwable) {
        null
    }
    if (reason != null) return "refused:" + refusalCode(reason.name)
    return failedRowKindFromText(
        generateSequence(error) { it.cause }
            .take(MAX_CAUSE_DEPTH)
            .joinToString(" ") { "${it.javaClass.simpleName} ${it.message.orEmpty()}" },
    )
}

private const val MAX_CAUSE_DEPTH: Int = 8

/** The text half of [failedRowKind]. */
internal fun failedRowKindFromText(text: String): String = when {
    text.contains("WrongEpoch") -> "wrong-epoch"
    text.contains("CannotDecryptOwnMessage") -> "own-message"
    text.contains("MissingProposal") -> "missing-proposal"
    text.contains("decrypt", ignoreCase = true) || text.contains("aead", ignoreCase = true) -> "decrypt"
    else -> "other"
}

// ---------------------------------------------------------------------------
// The apply loop
// ---------------------------------------------------------------------------

/** What [applyJournalRowsHolding] did with one batch. */
internal data class JournalRowsApplied(
    /** The cursor to store: the last row consumed, never the held one. */
    val cursor: Long,
    /** `true` when the batch stopped at a held row. */
    val held: Boolean,
    /** `true` when at least one row was handed to the MLS core. */
    val touchedMlsState: Boolean,
)

/**
 * Apply [rows] in order, holding the registered commit for this device's next
 * epoch instead of skipping it.
 *
 * [isOwnEchoRow]: the own-publish echo filter (consumes the entry; the row is
 * passed without reaching the core). [takeSideRow]: a row that is not an MLS
 * message and is consumed by the caller (ADR-0092 section 3.6: an anonymous
 * ballot); it is passed without reaching the core, in order. [localEpoch]: the group's epoch now.
 * [process]: the MLS apply, which throws on a failed row. [framedEpochs]: the
 * row's `epochOfMessage` / `epochAfterMessage`, `null` when unknown (a throw
 * is unknown). [isOwnEchoFailure]: the failure is `CannotDecryptOwnMessage`.
 * [onApplied]: the row applied; called in order.
 *
 * [registeredCommits]: the fetch's `commits` (the hold rule). [readFrom]: where
 * the rows start; below [after] only on an ADR-0091 phase-1b rewind
 * ([snapshotRewindFrom]). The returned cursor is never below [after].
 */
internal fun <T> applyJournalRowsHolding(
    groupIdHex: String,
    rows: List<MailboxMessage>,
    after: Long,
    latestEpoch: ULong?,
    registeredCommits: Map<ULong, Long>?,
    holds: RegisteredCommitHoldTracker,
    readFrom: Long = after,
    isOwnEchoRow: (Long) -> Boolean,
    takeSideRow: (MailboxMessage) -> Boolean = { false },
    localEpoch: () -> ULong?,
    process: (ByteArray) -> T,
    framedEpochs: (ByteArray) -> Pair<ULong?, ULong?>,
    isOwnEchoFailure: (Throwable) -> Boolean,
    classify: (Throwable) -> String = ::failedRowKind,
    log: (FailedRowLogLevel, String) -> Unit,
    /** ADR-0091 phase 2 — a row was skipped: its framed epoch and the local
     *  epoch before it, for the behind bookkeeping. */
    onSkipped: (row: MailboxMessage, sealedEpoch: ULong?, localEpoch: ULong?) -> Unit = { _, _, _ -> },
    onApplied: (MailboxMessage, T) -> Unit,
): JournalRowsApplied {
    var cursor = readFrom
    var touched = false
    holds.releaseIfPassed(groupIdHex, readFrom)
    for (row in rows) {
        if (isOwnEchoRow(row.id)) {
            cursor = row.id
            continue
        }
        if (takeSideRow(row)) {
            cursor = row.id
            continue
        }
        touched = true
        val local = safeEpoch(localEpoch)
        val processed = try {
            process(row.payload)
        } catch (failure: Throwable) {
            val (sealed, epochAfter) = try {
                framedEpochs(row.payload)
            } catch (_: Throwable) {
                null to null
            }
            val verdict = registeredCommitVerdict(
                isOwnEcho = isOwnEchoFailure(failure),
                rowId = row.id,
                localEpoch = local,
                registeredCommits = registeredCommits,
            )
            val kind = try {
                classify(failure)
            } catch (_: Throwable) {
                "other"
            }
            if (verdict == RegisteredCommitVerdict.HOLD && local != null) {
                val decision = holds.recordFailure(groupIdHex, row.id, local)
                log(
                    FailedRowLogLevel.WARN,
                    failedRowLine(kind, sealed, epochAfter, local, latestEpoch, verdict, decision.attempt),
                )
                if (kind.startsWith("refused:")) log(FailedRowLogLevel.ERROR, divergenceLine(kind))
                if (decision.held) {
                    return JournalRowsApplied(cursor = maxOf(cursor, after), held = true, touchedMlsState = touched)
                }
                log(FailedRowLogLevel.ERROR, exhaustionLine(local, latestEpoch))
            } else {
                holds.release(groupIdHex, row.id)
                log(
                    FailedRowLogLevel.WARN,
                    failedRowLine(kind, sealed, epochAfter, local, latestEpoch, RegisteredCommitVerdict.SKIP, null),
                )
            }
            // Skipped: a row that can never apply must not wedge the poll.
            // An own echo is not a row this device failed to read.
            try {
                if (!isOwnEchoFailure(failure)) onSkipped(row, sealed, local)
            } catch (_: Throwable) {
                // bookkeeping only
            }
            cursor = row.id
            continue
        }
        holds.release(groupIdHex, row.id)
        cursor = row.id
        onApplied(row, processed)
    }
    safeEpoch(localEpoch)?.let { holds.observeEpoch(groupIdHex, it) }
    return JournalRowsApplied(cursor = maxOf(cursor, after), held = false, touchedMlsState = touched)
}

private fun safeEpoch(read: () -> ULong?): ULong? = try {
    read()
} catch (_: Throwable) {
    null
}
