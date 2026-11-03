// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.ds.MailboxMessage
import uniffi.lakko_ffi_uniffi.Role

/*
 * ADR-0053 section 4 — the knock queue is an UNAUTHENTICATED write: anyone
 * holding a group id (a member included) can post any bytes there. So a knock
 * row must never reach the ordinary receive path. Were a member to knock a
 * real commit, every admin reading knocks would apply it through
 * `processIncomingVerifyingRoles` and the group would fork; filtering the
 * resulting events afterwards is too late, the commit is already merged.
 *
 * Every knock row therefore goes through the core's knock gate
 * (`Group.processKnock` / `PublisherGroup.processKnock`), which refuses,
 * before any processing, anything that is not a stranger's external-join
 * proposal for this group. A refusal drops the row; the cursor still advances
 * past it, so one bad row can never wedge the knock poll.
 *
 * This file is the one place that loop lives, with the gate injected, so a
 * JVM test can assert that knock rows are routed to the gate and nowhere
 * else.
 */

/** What one pass over a knock batch produced. */
public data class KnockDrain<P>(
    /** Each row the gate accepted, with what the gate returned. */
    val accepted: List<Pair<MailboxMessage, P>>,
    /** Rows the gate refused (dropped). */
    val refused: Int,
    /** The cursor after the batch: the last row's id, accepted or not. */
    val cursor: Long,
)

/**
 * Run every row of a knock batch through [knockGate] and nothing else. A row
 * the gate throws on is refused and dropped. [after] seeds the cursor so an
 * empty batch does not rewind it.
 */
public fun <P> drainKnockRows(
    rows: List<MailboxMessage>,
    after: Long,
    knockGate: (ByteArray) -> P,
): KnockDrain<P> {
    val accepted = ArrayList<Pair<MailboxMessage, P>>(rows.size)
    var refused = 0
    var cursor = after
    for (row in rows) {
        cursor = row.id
        val processed = try {
            knockGate(row.payload)
        } catch (_: Throwable) {
            refused += 1
            continue
        }
        accepted += row to processed
    }
    return KnockDrain(accepted = accepted, refused = refused, cursor = cursor)
}

/**
 * ADR-0053 client contract point 7, as amended by ADR-0050 section 2.5a point 7
 * — whether this device reads a group's knock queue at all.
 *
 * Gated on the ROLE, the owner or an admin, and never on the committer rank:
 * a knock is a stranger's external-join request, and only an owner or admin
 * may approve one (ADR-0027). Since section 2.5a a plain member also holds a
 * committer rank while its pending store holds a device admission, so a rank
 * test would start reading knocks on devices that can do nothing with them.
 * `null` (no role table, or a read that failed) reads nothing.
 */
internal fun readsKnocks(ownRole: Role?): Boolean =
    ownRole == Role.OWNER || ownRole == Role.ADMIN

/** ADR-0087 section 5 — what one sealed knock row asks of the drain. */
public enum class SealedKnockStep {
    /** Opened and kept as a request to join. */
    KEEP,

    /** Not for this group, unreadable, or can never join: skipped. */
    DROP,

    /** The pending set is full: stop BEFORE this row, so it is read again
     *  once a request is decided and the backlog stays on the server. */
    STOP,
}

/**
 * ADR-0087 section 5 — [drainKnockRows] for sealed knock rows: each row through
 * [open] and nothing else; a throw drops the row; [SealedKnockStep.STOP] ends
 * the batch with the cursor before that row.
 */
public fun <P> drainSealedKnockRows(
    rows: List<MailboxMessage>,
    after: Long,
    open: (ByteArray) -> Pair<SealedKnockStep, P?>,
): KnockDrain<P> {
    val accepted = ArrayList<Pair<MailboxMessage, P>>(rows.size)
    var refused = 0
    var cursor = after
    for (row in rows) {
        val (step, value) = try {
            open(row.payload)
        } catch (_: Throwable) {
            SealedKnockStep.DROP to null
        }
        if (step == SealedKnockStep.STOP) break
        cursor = row.id
        if (step == SealedKnockStep.KEEP && value != null) accepted += row to value else refused += 1
    }
    return KnockDrain(accepted = accepted, refused = refused, cursor = cursor)
}
