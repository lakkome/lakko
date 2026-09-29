// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/**
 * ADR-0045 S4a/S4b — the committer sweep: stage, publish, read the journal
 * verdict, then merge or abandon.
 *
 * Android mirror of `apps/web/src/chat/sweep.ts`. Same five obligations, same
 * ordering, same fail-closed directions; what differs is the concurrency model
 * (see "The two-tier lock" below), which is the one thing the web reference
 * could not express.
 *
 * ## What this closes
 *
 * G-38 residual 1: two devices build a membership commit against the same epoch
 * N, the DS journal orders them by arrival, and the loser has *already*
 * eager-merged its own N+1 — so it sits at an epoch nobody else reached,
 * silently and permanently. ADR-0045's answer is that members emit **proposals**
 * (not epoch-advancing, so a lost race costs nothing) and one elected
 * owner-or-admin sweeps them into a single commit.
 *
 * Rank 0 is the owner and acts without consulting a beacon; any other candidate
 * acts only when [SweepPorts.actingCommitter] — the core's own
 * `actingCommitterVerdict` fold over the beacons this device observed — says so.
 * The decision itself is never made here.
 *
 * ## Why the sweep stages rather than eager-merging
 *
 * A committer that *can* lose a race must not merge before the journal has
 * ordered its commit, and that is not hypothetical: each candidate folds over
 * its own poll-timed view and a beacon is stale by up to one interval even when
 * its sender is healthy, so two candidates can both reach `act`. **The beacon
 * removes the wait, not the race** — the stand-down re-read below and the
 * staged-commit rollback are as required with a beacon as without one, and
 * reading the acting verdict as a licence to drop either would be a mis-reading.
 * `commitPendingProposals` is exactly `stagePendingProposals` followed at once
 * by `mergeStagedSweep`; building on it would be building the hazard.
 *
 * ## The five obligations, and where each one lives
 *
 * 1. **Reach a verdict; never leave a sweep staged.** A staged sweep is
 *    *durable* (OpenMLS persists `MlsGroupState::PendingCommit`) and freezes
 *    that group's own membership operations across restarts —
 *    `is_operational()` refuses `propose_*` / add / remove / `self_update` while
 *    one is pending. So [runCommitterSweep] adjudicates every sweep it stages,
 *    and [resolveOwedVerdict] is what a restart runs first: it is wired into
 *    ADR-0040's existing `ChatRepository.sweepPendingCommits` seam rather than
 *    becoming a second restart path.
 * 2. **Publish the Welcome only AFTER the verdict.** If the commit loses and a
 *    Welcome is already out, a joiner opens into a state nobody holds — a
 *    three-party fork, strictly worse than the two-party one. [publishWelcomes]
 *    is reached only on the merge branch.
 * 3. **The stand-down re-read.** Immediately before staging, re-read the journal
 *    head; if it has moved, stand down and re-evaluate. A required step of the
 *    algorithm, not a mitigation.
 * 4. **durably record -> publish (or re-publish) -> read the verdict -> merge or
 *    abandon.** ADR-0040's outbox and this pair answer *different* questions
 *    about the same commit — the outbox answers *did my commit reach the wire*,
 *    the staged pair answers *did it win the wire order* — and the first must be
 *    `yes` before the second is meaningful. So the outbox entry is written
 *    BEFORE any publish, exactly as it is on the direct paths.
 * 5. Obligation 5 (the direct-commit paths become propose-or-sweep) lives in
 *    [ChatRepository], this module's only production caller.
 *
 * ## The verdict is read by APPLYING, not by peeking at the epoch field
 *
 * The question is "did a **Commit** land below my own commit's DS id". The FFI
 * exposes no `contentTypeOfMessage`, and epoch alone cannot tell a Commit from
 * an Application message or a Proposal — every one of them is framed at the
 * current epoch. Guessing in the permissive direction is not an option:
 * abandoning a sweep that actually WON, after peers applied it, leaves this
 * device at N with the group at N+1 and its own commit un-re-appliable
 * (`CannotDecryptOwnMessage`) — the same brick, in reverse.
 *
 * So the verdict asks a question that needs no discrimination: **apply every row
 * below our own commit, then ask whether our epoch moved.** Only a Commit moves
 * it. Applying is the audited receive path, so nothing new is invented, and it
 * is safe while a sweep is staged: nothing on the receive path consults
 * `is_operational`, and OpenMLS discards a pending commit as part of merging a
 * foreign one.
 *
 * ## The two-tier lock — the Android-specific part
 *
 * The caller holds the **per-group** `Mutex` across this whole call, including
 * both DS round-trips. That is deliberate and it is the one place Android's
 * usual "keep the network outside the lock" advice (G-37) is overridden: the
 * verdict is a read-modify-write over both the group epoch and the journal
 * cursor, and a poll interleaving inside it would not merely slow the sweep
 * down, it would make the verdict *wrong*. The blast radius is one group.
 *
 * The **global** provider lock is taken inside, and only around
 * provider-touching regions, via [SweepGroupPort.exclusiveProvider]. One
 * OpenMLS provider serves every group, so a sibling group's whole-provider
 * `snapshotStorage` taken mid-region would capture this group torn, and its
 * `restoreStorage` (the ADR-0040 F1 abort path) would roll this group back
 * outright. That is why [adjudicate] brackets *the whole apply loop plus both
 * epoch reads plus the cursor advance* in ONE `exclusiveProvider` region rather
 * than taking the lock per row: between two rows this group is applied-but-not-
 * persisted, i.e. exactly the state a sibling restore must never observe.
 *
 * `Mutex` is not reentrant, so no code reached from inside an
 * [SweepGroupPort.exclusiveProvider] block may take it again. Every port method
 * on [SweepGroupPort] is therefore documented as "call inside
 * [SweepGroupPort.exclusiveProvider]" and this file never nests one.
 *
 * ## Ports, not FFI
 *
 * Everything here is expressed over injected ports so the whole orchestration is
 * host-testable on the plain JVM — no `Context`, no UniFFI `.so`, no network.
 * [ChatRepository] supplies the thin adapters. Same rationale (and same shape)
 * as [runCrashSafeCommit] and [runSplitPoll].
 */

/**
 * ADR-0045 — what a membership action did.
 *
 * The two arms are not two flavours of success, they are two different things
 * that happened, and the UI has to say so: a [Committed] action took effect
 * across the group, a [Proposed] one is a request that lands when the committer
 * sweeps. Collapsing them into one shape would force the panel to assert an
 * effect that has not happened, which is exactly the optimistic-update lie the
 * UX spec forbids.
 */
public sealed class MembershipOutcome {
    /** A direct commit landed. Only reachable in a group with NO role table,
     *  where ADR-0045's proposal path does not apply. */
    data class Committed(
        val commitMessageId: Long,
        val welcomeMessageId: Long?,
    ) : MembershipOutcome()

    /** A proposal was published. It takes effect when a committer sweeps it. */
    data class Proposed(val proposalMessageId: Long) : MembershipOutcome()
}

/** The MLS artifacts a staged sweep produced. Mirrors the FFI `StagedSweep`. */
internal class StagedSweepArtifacts(
    /** The commit wire bytes to publish. The local epoch has NOT advanced. */
    val commit: ByteArray,
    /** The Welcome, present only when the sweep carried at least one add. */
    val welcome: ByteArray?,
    /**
     * The 32-byte Ed25519 signature key of every member this sweep ADDS.
     *
     * One sweep commit can carry several adds under a single Welcome, which is
     * why this is a list where the direct add path has one destination. Each key
     * derives that member's mailbox pseudonym — the Welcome's delivery address,
     * since a not-yet-member does not poll the group journal.
     */
    val addedSignatureKeys: List<ByteArray>,
    /** ADR-0057 — the leaves this sweep removes (Remove targets, Leave
     *  proposers), so the registration can revoke the evicted accounts'
     *  registrar keys. Empty on the restart path (the registration is already
     *  behind the owed entry). */
    val removedLeaves: List<UInt> = emptyList(),
)

/**
 * One row of the DS group journal.
 *
 * [createdAt] is not decoration: an applied row becomes a timeline event, and
 * the shared event constructor requires the DS row timestamp — it is the one
 * clock every member reads identically for the identical row. A row shape that
 * carried only the payload is exactly what made the verdict apply unable to
 * surface anything.
 */
internal class SweepJournalRow(
    val id: Long,
    val payload: ByteArray,
    val createdAt: String,
)

/**
 * The MLS operations the sweep drives, adapted onto the FFI `Group`.
 *
 * Every method except [exclusiveProvider] MUST be invoked from inside an
 * [exclusiveProvider] block, and [exclusiveProvider] must never be nested (the
 * underlying `Mutex` is not reentrant).
 */
internal interface SweepGroupPort {
    /**
     * Run [block] holding the global provider lock.
     *
     * The adapter maps this onto `providerLock.withLock`; a test maps it onto a
     * recorder so the atomicity of the adjudication region is assertable.
     */
    suspend fun <T> exclusiveProvider(block: suspend () -> T): T

    /** The current MLS epoch. Read before and after the verdict apply. */
    fun epoch(): ULong

    /**
     * ADR-0045 committer rank: `0` for the owner, `k + 1` for the k-th admin,
     * `null` for a plain member or a group with no role table.
     *
     * `null` and `0` are NOT interchangeable — `0` is the owner, the one device
     * that always sweeps.
     */
    fun ownCommitterRank(): UInt?

    /** How many authorised proposals are waiting. `0` short-circuits the sweep
     *  before it touches the network. */
    fun pendingProposalCount(): Int

    /** `true` when a staged sweep is still awaiting its journal verdict. */
    fun hasStagedSweep(): Boolean

    /** Build the sweep commit and STOP before merging. `null` when nothing
     *  authorised is pending (deliberately not an empty commit, which would
     *  advance the epoch and race other committers for no effect). */
    fun stagePendingProposals(): StagedSweepArtifacts?

    /** Won — land at the new epoch, the one the peers reached. */
    fun mergeStagedSweep()

    /** Lost — discard the staged commit and stay put, free to apply the winner.
     *  Per-group: it never reaches a sibling group, unlike a whole-provider
     *  `restoreStorage`. */
    fun abandonStagedSweep()

    /**
     * Apply one journal row through the audited role-verifying receive path AND
     * surface whatever it produced to the timeline.
     *
     * THROWS on a bad frame / wrong epoch / refused commit — the caller skips,
     * exactly as the poll loop does, and a skipped row must surface nothing.
     *
     * ## Why the WHOLE row, and why the port also surfaces
     *
     * The verdict consumes every row the DS ordered below our own commit and then
     * advances the shared cursor over them, so the poll never fetches them again.
     * Anything one of those rows carried — an ordinary chat message another member
     * sent in the window between the stand-down re-read and our publish — is gone
     * from the UI forever unless it is surfaced HERE. Applying it and discarding
     * the result decrypts the message, advances the ratchet, and loses the
     * plaintext silently.
     *
     * Surfacing therefore belongs to the port rather than to this module, and the
     * port is handed the whole [SweepJournalRow] rather than just its payload: the
     * shared event constructor in `ChatClient.kt` needs the DS row id and the DS
     * row timestamp, and every envelope kind must be dispatched by that ONE ladder
     * — a second copy of it here is how the poll path and the sweep path would
     * drift. This module has nothing to decide about an event, so returning one
     * for it to collect would buy nothing.
     *
     * Because surfacing happens inline, per row, a LOST verdict and a thrown
     * verdict both leave the already-applied rows on the timeline — which is
     * correct: whether our own commit won the order says nothing about the rows
     * below it, which were applied and consumed either way.
     */
    fun applyIncoming(row: SweepJournalRow)
}

/** The DS operations the sweep drives. */
internal interface SweepJournalPort {
    /** Rows strictly after [after], in ascending id order. */
    suspend fun fetch(after: Long): List<SweepJournalRow>

    /** Publish to this group's journal; returns the DS-assigned id. */
    suspend fun publishGroup(bytes: ByteArray): Long

    /** Publish to a member's mailbox pseudonym; returns the DS id. */
    suspend fun publishMailbox(pseudonym: ByteArray, bytes: ByteArray): Long
}

/**
 * What the sweep durably records, per ADR-0040.
 *
 * Deliberately NOT [PendingCommitEntry]: this module must stay free of the
 * on-disk schema so it is drivable from a host test, and the sweep's Welcome
 * fans out to *several* mailboxes where a direct-path entry has at most one.
 */
internal data class SweepDurableEntry(
    val id: String,
    val commitB64: String,
    val welcomeEnvelopeB64: String?,
    /**
     * base64 of the 32-byte signature key of every leaf the sweep ADDS.
     *
     * ADR-0058 §6 — the Welcome goes to each added leaf's ACCOUNT mailbox,
     * and a leaf's account is read from its device binding in the roster,
     * which holds the leaf only once the sweep merged. So the entry records
     * the leaves, and [SweepPorts.welcomeTargets] resolves them to mailboxes
     * at publish time (after the merge, on the first run and on a restart
     * alike).
     */
    val welcomeLeafKeysB64: List<String>,
    /**
     * The DS id of the published commit, recorded alongside the welcomes-owed
     * marker. Diagnostics only — it is carried so the eventual ack can name the
     * commit it belongs to rather than inventing one. `null` until the verdict
     * settled.
     */
    val commitMessageId: Long? = null,
)

/** The ADR-0040 outbox operations the sweep drives. */
internal interface SweepOutboxPort {
    /** Mint a fresh entry id. */
    fun newEntryId(): String

    /**
     * Durably record the entry AND the current MLS snapshot, BEFORE any publish.
     * A throw ABORTS the sweep fail-closed.
     */
    suspend fun record(entry: SweepDurableEntry)

    /**
     * The un-acked entry whose **verdict** is still owed, or `null`.
     *
     * A welcomes-owed entry is deliberately NOT returned here: its verdict is
     * settled and its commit is merged, so re-adjudicating it would re-publish a
     * commit whose order was already read.
     */
    suspend fun findOwed(): SweepDurableEntry?

    /**
     * Flip the entry from *verdict owed* to *Welcome fan-out owed*, and record
     * the DS id its commit published under, in ONE durable write.
     *
     * ## What a restart must, and must not, do with this
     *
     * MUST: re-publish the entry's Welcome to every mailbox in
     * [SweepDurableEntry.welcomeLeafKeysB64] (resolved to account mailboxes), then ack. MUST NOT: re-publish
     * the commit, re-adjudicate, or apply another journal row — the verdict is
     * already known and the commit is already merged.
     *
     * The marker is written strictly AFTER [SweepGroupPort.mergeStagedSweep] and
     * its persist, and that ordering is what keeps obligation 2 intact: the
     * marker IS the durable statement "the verdict said WON", so recording it any
     * earlier would let a pre-verdict restart publish a Welcome for a commit that
     * then lost — the three-party fork. A restart that finds a staged sweep
     * instead finds no marker, and [resolveOwedWelcomes] refuses outright while
     * one is staged, so the two states cannot be confused even if a write were
     * ever to land out of order.
     *
     * A throw is NOT fatal — see the call site.
     */
    suspend fun markWelcomesOwed(id: String, commitMessageId: Long)

    /** The un-acked entry whose verdict settled but whose Welcome fan-out is
     *  still owed, or `null`. */
    suspend fun findOwedWelcomes(): SweepDurableEntry?

    /** Mark the entry acked once every artifact is on the wire. */
    suspend fun ack(id: String, commitMessageId: Long?, welcomeMessageId: Long?)

    /** Delete the entry. Used on a LOSS: a commit that lost the order must never
     *  be re-published by the restart sweep. */
    suspend fun discard(id: String)
}

/** The journal cursor, as the read/write pair `ChatRepository.pollGroup` drives. */
internal interface SweepCursorRef {
    fun read(): Long

    fun advance(to: Long)
}

/**
 * ADR-0045 S4b — is this device the acting committer right now?
 *
 * Called with this device's CURRENT rank (never `null` — the caller has already
 * established it is a candidate) and answered by the core's
 * `actingCommitterVerdict` fold over the beacons this device has observed.
 * Suspending because the fold reads the observation store; it is awaited before
 * anything is staged, so a slow answer costs one attempt and nothing else.
 *
 * A throw is treated as `Defer` — fail-closed, because the alternative is a
 * device that sweeps on no evidence about who else is running.
 */
internal fun interface ActingCommitterOracle {
    suspend fun decide(ownRank: UInt): ActingCommitter
}

/** Everything the sweep needs. */
internal class SweepPorts(
    val group: SweepGroupPort,
    val journal: SweepJournalPort,
    val outbox: SweepOutboxPort,
    val cursor: SweepCursorRef,
    val actingCommitter: ActingCommitterOracle,
    /**
     * ADR-0058 §6 — the mailboxes one Welcome goes to, for the added leaves'
     * 32-byte signature keys: one per added ACCOUNT (resolved through each
     * leaf's device binding in the merged roster), deduplicated. Called only
     * after the sweep merged.
     */
    val welcomeTargets: (List<ByteArray>) -> List<ByteArray>,
    /** Wrap the raw Welcome in the stranger-inbox envelope the recipient's
     *  mailbox poll expects (`wrapEnvelope(KIND_WELCOME, welcome)`). */
    val wrapWelcome: (ByteArray) -> ByteArray,
    /** Echo-suppress a row this device published, so its own next poll drops it
     *  instead of hitting `CannotDecryptOwnMessage`. */
    val markOwnPublished: (Long) -> Unit,
    /** Best-effort MLS snapshot persist after a merge / abandon. */
    val persist: suspend () -> Unit,
    /**
     * FAIL-CLOSED MLS snapshot persist — THROWS on a failed write, and is called
     * from INSIDE an [SweepGroupPort.exclusiveProvider] region (so, unlike
     * [persist], it must NOT take the provider lock itself — `Mutex` is not
     * reentrant).
     *
     * Used by `adjudicate` only, and it has to be a separate port from [persist]
     * because the two want opposite things from a failure. The verdict apply
     * consumes journal rows the poll will never fetch again, so it advances the
     * shared cursor over them — which makes it subject to exactly the ordering
     * rule the poll obeys: the cursor may lag the durable MLS state, never lead
     * it. See [DurableCursorGate].
     *
     * NON-suspending on purpose: that is what makes "does not take the provider
     * lock" a compile-time property rather than a comment. [persist] is
     * `suspend` precisely because it DOES take it.
     */
    val persistApplied: () -> Unit,
    val encodeB64: (ByteArray) -> String,
    val decodeB64: (String) -> ByteArray,
    /**
     * ADR-0057 obligation 5 — register the access key of the epoch the STAGED
     * sweep moves to (`dsStagedAccessSigner`) with this account's root or
     * registrar signer (revoking the evicted accounts' registrar keys when the
     * owner commits), BEFORE the sweep is durably recorded or published.
     * THROWS when the Delivery Service does not take it; the sweep is then
     * abandoned exactly as a failed outbox write abandons it, so no peer ever
     * applies a commit whose epoch the server does not know.
     */
    val registerStagedEpoch: suspend (staged: StagedSweepArtifacts) -> Unit = {},
)

/** What one sweep attempt did. */
public sealed class SweepOutcome {
    /** This device is not in the committer set at all (a plain member, or a
     *  group with no role table). Nothing happened, and nothing ever will on
     *  this device — distinct from [Deferred], which re-evaluates. */
    data class NotCommitter(val rank: UInt?) : SweepOutcome()

    /** This device IS a candidate, but a strictly better-placed one beaconed
     *  inside the freshness window. Stood down without staging; the next poll
     *  re-evaluates, and if that candidate goes quiet its beacon ages out of the
     *  window and this device takes over on its own. */
    data class Deferred(val rank: UInt, val toRank: UInt?) : SweepOutcome()

    /** Nothing authorised was pending and nothing was staged. */
    object NothingPending : SweepOutcome()

    /** Obligation 3 — the journal moved since our cursor, so a commit may
     *  already be in flight. Stood down without staging; the poll drains the
     *  rows and the next attempt re-evaluates. */
    data class StoodDown(val unappliedRows: Int) : SweepOutcome()

    /** Won the journal order: merged, and the Welcome (if any) published. */
    data class Committed(
        val commitMessageId: Long,
        val welcomeMessageIds: List<Long>,
        val addedCount: Int,
    ) : SweepOutcome()

    /** Lost the journal order: abandoned, no Welcome published, the proposals
     *  survive for the winner's next sweep. */
    data class Lost(val commitMessageId: Long) : SweepOutcome()

    /** A previous run merged a winning sweep but could not get its Welcome onto
     *  the wire. The fan-out has now been retried and acked. */
    data class WelcomesRepublished(
        val welcomeMessageIds: List<Long>,
        val addedCount: Int,
    ) : SweepOutcome()

    /** No settled-verdict entry was waiting on its Welcome fan-out. The common
     *  case, and what a staged sweep also reports — that one owes a VERDICT. */
    object NoWelcomesOwed : SweepOutcome()

    /** A staged sweep was found with no durable record of its commit, so the
     *  commit provably never reached the wire (ADR-0040 records before it
     *  publishes). Abandoned, which also unwedges the frozen membership ops. */
    object AbandonedOrphan : SweepOutcome()

    /** There was no verdict owed. Only ever returned by [resolveOwedVerdict]. */
    object NoVerdictOwed : SweepOutcome()
}

/**
 * The journal order could not be read, so the verdict is genuinely unknown.
 *
 * The sweep is left STAGED and its outbox entry un-acked on purpose. Both
 * alternatives are worse: merging on an unread verdict is the eager shape that
 * strands the loser, and abandoning would discard a commit that is already on
 * the wire and may have won. The next sweep attempt re-adjudicates through
 * [resolveOwedVerdict], and the sweep runs at the tail of every poll, so the
 * retry is continuous. The cost is that this group's own membership operations
 * stay frozen until it succeeds — a temporary freeze that self-heals, which is
 * strictly better than either wrong answer.
 */
internal class SweepVerdictUnavailable(cause: Throwable) : Exception(
    "ADR-0045: could not read the journal verdict — sweep left staged: $cause",
    cause,
)

/**
 * The pure verdict rule: did anything applied below our own commit advance the
 * group's epoch?
 *
 * Only a Commit advances an epoch. An Application message drains a ratchet, a
 * Proposal queues, a non-MLS envelope throws and is skipped — none of them move
 * it. So an unchanged epoch means our commit is first in the order and we WON; a
 * moved epoch means a foreign commit was ordered ahead of ours and we LOST.
 */
internal fun sweepVerdict(epochBefore: ULong, epochAfter: ULong): Boolean =
    epochAfter == epochBefore

/**
 * Obligation 3, as a pure rule: may this device stage a sweep right now?
 *
 * A row this device has not applied might be a Commit, and without a
 * content-type peek there is no way to tell. Standing down is free — the poll
 * drains the rows and the next attempt re-evaluates — whereas staging on top of
 * an unapplied commit publishes a commit peers will drop as wrong-epoch. So the
 * rule errs deliberately in the conservative direction: ANY unapplied row stands
 * the sweep down, not only a Commit.
 *
 * This narrows the window; it does not close it. A commit can still land between
 * this check and our publish, which is exactly what the verdict step repairs.
 */
internal fun shouldStandDown(unappliedRowCount: Int): Boolean = unappliedRowCount > 0

/**
 * Run one committer sweep. Callers MUST hold the per-group MLS lock and MUST NOT
 * hold the provider lock (this takes it itself, and it is not reentrant).
 *
 * Short-circuits without touching the network when this device is not a
 * candidate, or when nothing is pending and no verdict is owed — so wiring this
 * into the tail of every poll costs one short provider region in the common
 * case.
 *
 * @throws SweepVerdictUnavailable when the journal order could not be read. The
 *   sweep is left staged for the next attempt.
 */
internal suspend fun runCommitterSweep(ports: SweepPorts): SweepOutcome {
    val group = ports.group

    // Obligation 1, and it runs BEFORE the rank check on purpose.
    //
    // A staged sweep is DURABLE and freezes this group's membership operations
    // across restarts, so it must be adjudicated by whoever staged it —
    // INCLUDING a device that has since been demoted out of the committer set.
    // Gating this behind the rank check would leave such a device frozen
    // permanently, with nothing that could ever unwedge it. Re-publishing a
    // demoted device's commit is safe: every peer's role gate refuses it, so it
    // loses the order and is abandoned — which is exactly the unwedge.
    //
    // It also takes precedence over starting a NEW sweep, because
    // `stagePendingProposals` refuses a second stage fail-closed: trying to
    // start one first would throw and leave the freeze in place.
    val prelude = group.exclusiveProvider {
        if (group.hasStagedSweep()) {
            SweepPrelude.OwedVerdict
        } else {
            val rank = group.ownCommitterRank()
            when {
                rank == null -> SweepPrelude.NotCommitter
                group.pendingProposalCount() == 0 -> SweepPrelude.NothingPending
                else -> SweepPrelude.Candidate(rank)
            }
        }
    }
    if (prelude == SweepPrelude.OwedVerdict) return resolveOwedVerdict(ports)

    // The same obligation, one step further along: a sweep whose verdict settled
    // but whose Welcome never reached the wire owes that fan-out, and nothing
    // else will ever notice — the merge cleared `hasStagedSweep()` and emptied
    // the pending set, so both prelude answers below would report an idle group
    // over a member who can never join.
    //
    // Placed BEFORE the rank branch for the same reason the owed verdict is: a
    // device demoted since the merge still owes the Welcome for a member it
    // successfully added, and nobody else holds those bytes. Run OUTSIDE the
    // prelude's provider region — it takes its own, and `Mutex` is not
    // reentrant.
    //
    // The cost is one small store read per attempt where there was none. That is
    // deliberate: the outbox holds at most one entry per in-flight commit, and
    // the alternative is a repair that only runs when the user happens to reopen
    // the chat.
    val owedWelcomes = resolveOwedWelcomes(ports)
    if (owedWelcomes !is SweepOutcome.NoWelcomesOwed) return owedWelcomes

    val rank = when (prelude) {
        SweepPrelude.OwedVerdict -> return resolveOwedVerdict(ports)
        SweepPrelude.NotCommitter -> return SweepOutcome.NotCommitter(null)
        SweepPrelude.NothingPending -> return SweepOutcome.NothingPending
        is SweepPrelude.Candidate -> prelude.rank
    }

    // ADR-0045 S4b — is anyone strictly better placed announcing itself? Asked
    // only once there is something to sweep, so a quiet group never pays for it.
    // A throw is a defer: sweeping on an unreadable view of who else is running
    // is the failure this whole election exists to avoid.
    val acting = try {
        ports.actingCommitter.decide(rank)
    } catch (_: Throwable) {
        ActingCommitter.Defer(null)
    }
    if (acting !is ActingCommitter.Act) {
        return SweepOutcome.Deferred(
            rank = rank,
            toRank = (acting as? ActingCommitter.Defer)?.toRank,
        )
    }

    // Obligation 3 — the stand-down re-read, immediately before creating the
    // commit. Runs OUTSIDE the provider lock: it is pure network and touches no
    // MLS state, and the provider lock is global.
    val cursorBefore = ports.cursor.read()
    val ahead = try {
        ports.journal.fetch(cursorBefore)
    } catch (cause: Throwable) {
        // Nothing has been staged yet, so a failed re-read costs only this
        // attempt. Treat an unreadable head as "the journal may have moved".
        throw SweepVerdictUnavailable(cause)
    }
    if (shouldStandDown(ahead.size)) return SweepOutcome.StoodDown(ahead.size)

    val staged = group.exclusiveProvider { group.stagePendingProposals() }
        ?: return SweepOutcome.NothingPending

    return publishAndAdjudicate(ports, staged, cursorBefore, owed = null)
}

/** The short synchronous prelude decision, taken in ONE provider region. */
private sealed class SweepPrelude {
    object OwedVerdict : SweepPrelude()

    object NotCommitter : SweepPrelude()

    object NothingPending : SweepPrelude()

    data class Candidate(val rank: UInt) : SweepPrelude()
}

/**
 * Obligation 1, restart half — finish a verdict this device owes.
 *
 * A staged commit is persisted by OpenMLS and read back by `Identity.loadGroup`,
 * so it outlives the process. A client that finds one after a relaunch still
 * owes a verdict on a commit it may already have published, and until it reaches
 * one the group's membership is frozen. This is wired into ADR-0040's existing
 * restart sweep rather than being a second restart path.
 *
 * The re-publish is idempotent by the same argument the ADR-0040 sweep already
 * rests on: peers that applied the original drop the duplicate as wrong-epoch,
 * and peers that never saw it apply it now. The verdict is then read exactly as
 * it would have been the first time — including the case where our ORIGINAL
 * commit sits in the journal below the re-published one, which applies as
 * `CannotDecryptOwnMessage`, is skipped, and correctly does not move our epoch.
 *
 * Callers MUST hold the per-group MLS lock and MUST NOT hold the provider lock.
 */
internal suspend fun resolveOwedVerdict(ports: SweepPorts): SweepOutcome {
    val group = ports.group
    if (!group.exclusiveProvider { group.hasStagedSweep() }) {
        return SweepOutcome.NoVerdictOwed
    }

    val owed = ports.outbox.findOwed()
    if (owed == null) {
        // No durable record of the commit. ADR-0040 writes the record BEFORE the
        // publish, so a snapshot carrying a staged sweep and no entry means the
        // write never completed and the commit never reached the wire.
        // Abandoning is therefore correct, and it is also what unwedges the
        // frozen membership operations — the worse failure of the two.
        group.exclusiveProvider { group.abandonStagedSweep() }
        ports.persist()
        return SweepOutcome.AbandonedOrphan
    }

    return publishAndAdjudicate(
        ports,
        StagedSweepArtifacts(
            commit = ports.decodeB64(owed.commitB64),
            welcome = owed.welcomeEnvelopeB64?.let(ports.decodeB64),
            // Already wrapped + already addressed when the entry was written.
            addedSignatureKeys = emptyList(),
        ),
        ports.cursor.read(),
        owed,
    )
}

/**
 * Finish a Welcome fan-out a previous run merged but could not publish.
 *
 * ## The failure this repairs
 *
 * `publishAndAdjudicate` merges, persists, and only then publishes the Welcome.
 * A transient DS failure in that last step used to be unrecoverable: the merge
 * had already cleared `hasStagedSweep()` AND emptied the pending-proposal set,
 * which are exactly the two signals [runCommitterSweep] and [resolveOwedVerdict]
 * branch on, so both reported "nothing to do" while the added member sat in
 * every peer's roster unable to ever join. The durable welcomes-owed marker is
 * what gives that state a name, and this is what reads it.
 *
 * ## What it must NOT do
 *
 * Not re-adjudicate, and not re-publish the commit. The verdict is settled and
 * the commit merged; re-reading the journal order would apply rows against a
 * question that is no longer open, and a second commit publish is noise every
 * peer drops as wrong-epoch.
 *
 * ## Ordering — why a staged sweep is refused before the store is even read
 *
 * A staged sweep owes a VERDICT. ADR-0045 obligation 2 forbids its Welcome
 * reaching the wire until that verdict says the commit won, so this refuses
 * outright rather than trusting that the marker cannot be present.
 *
 * ## Idempotency
 *
 * The retry re-publishes the SAME recorded envelope bytes, so a first publish
 * that actually landed leaves the joiner with two identical mailbox rows. That
 * is inert: `StagedWelcome::into_group` deletes the joiner's KeyPackage bundle
 * (openmls-0.8.1 `group/mls_group/creation.rs`), so the second copy cannot be
 * staged and every client's mailbox handler catches the failure and skips the
 * row. Nothing forks, and no ratchet moves twice.
 *
 * A publish failure THROWS and leaves the marker in place for the next attempt.
 *
 * Callers MUST hold the per-group MLS lock and MUST NOT hold the provider lock.
 */
internal suspend fun resolveOwedWelcomes(ports: SweepPorts): SweepOutcome {
    if (ports.group.exclusiveProvider { ports.group.hasStagedSweep() }) {
        return SweepOutcome.NoWelcomesOwed
    }

    val owed = ports.outbox.findOwedWelcomes() ?: return SweepOutcome.NoWelcomesOwed

    val welcomeMessageIds = publishWelcomes(ports, owed)
    ports.outbox.ack(owed.id, owed.commitMessageId, welcomeMessageIds.firstOrNull())

    return SweepOutcome.WelcomesRepublished(
        welcomeMessageIds = welcomeMessageIds,
        addedCount = owed.welcomeLeafKeysB64.size,
    )
}

/**
 * The shared tail of both entry points: durably record (first time only),
 * publish the commit, read the verdict, then merge or abandon.
 *
 * [owed] is supplied on the restart path, where the entry already exists and its
 * Welcome is already wrapped and addressed.
 */
private suspend fun publishAndAdjudicate(
    ports: SweepPorts,
    staged: StagedSweepArtifacts,
    cursorBefore: Long,
    owed: SweepDurableEntry?,
): SweepOutcome {
    val group = ports.group

    val entry: SweepDurableEntry
    if (owed == null) {
        // ADR-0053 client contract point 3 — the Delivery Service learns the
        // key of the epoch this sweep moves to BEFORE anything about the sweep
        // is durable or on the wire. A refusal abandons the staged sweep (the
        // per-group rollback) and nothing is published: a commit whose epoch
        // the server does not know would leave every peer that applies it
        // refused on its next request.
        try {
            ports.registerStagedEpoch(staged)
        } catch (cause: Throwable) {
            group.exclusiveProvider { group.abandonStagedSweep() }
            throw cause
        }
        // Obligation 4 — durably record BOTH artifacts before ANY publish. The
        // Welcome is wrapped and addressed HERE, so the bytes the restart sweep
        // would publish are the very bytes computed now; nothing can drift.
        //
        // ADR-0053 §4 / client contract point 8 — the Welcome goes to each
        // added member's mailbox and nowhere else. A `/g/` joiner now waits on
        // its own device mailbox, so no `WelcomeRendezvous` is written to the
        // group journal any more (C-69 amended).
        val leaves = staged.addedSignatureKeys.map(ports.encodeB64)
        val wrapped = staged.welcome?.let(ports.wrapWelcome)
        entry = SweepDurableEntry(
            id = ports.outbox.newEntryId(),
            commitB64 = ports.encodeB64(staged.commit),
            welcomeEnvelopeB64 = wrapped?.let(ports.encodeB64),
            welcomeLeafKeysB64 = leaves,
        )
        try {
            ports.outbox.record(entry)
        } catch (cause: Throwable) {
            // FAIL-CLOSED. Nothing was published, so the group must look as
            // though the sweep never happened. Abandoning is the per-group
            // rollback — it touches this group only, unlike ADR-0040's
            // whole-provider `restoreStorage`, which would roll sibling groups
            // back past commits they have already published.
            group.exclusiveProvider { group.abandonStagedSweep() }
            throw cause
        }
    } else {
        entry = owed
    }

    val commitMessageId = ports.journal.publishGroup(ports.decodeB64(entry.commitB64))
    ports.markOwnPublished(commitMessageId)

    val won = adjudicate(ports, cursorBefore, commitMessageId)

    if (!won) {
        group.exclusiveProvider { group.abandonStagedSweep() }
        ports.persist()
        // A commit that lost must never be re-published: the restart sweep would
        // re-adjudicate a commit whose proposals the winner has already swept.
        // Discarding is safe because abandoning kept the proposals pending, so
        // nothing the user asked for is lost.
        ports.outbox.discard(entry.id)
        return SweepOutcome.Lost(commitMessageId)
    }

    group.exclusiveProvider { group.mergeStagedSweep() }
    ports.persist()

    // The verdict is settled and the merge is durable, so from here on the ONLY
    // thing this entry still owes is the mailbox fan-out. Say so durably before
    // attempting it, or a transient DS failure below leaves a GHOST MEMBER:
    // added to every peer's roster, addressed by every later message, and unable
    // to construct group state — with nothing left that would retry, because the
    // merge cleared both signals the two recovery paths look for
    // (`hasStagedSweep()` and the pending-proposal count).
    //
    // A failure to write the marker is NOT fatal and deliberately does not abort
    // the fan-out. The marker only buys a RETRY; the verdict is already in, so
    // publishing now is correct with or without it, and refusing to publish
    // because a bookkeeping write failed would manufacture the very ghost member
    // this records against.
    //
    // Swallowed WITHOUT logging here on purpose: this module is host-testable on
    // the plain JVM (no `Context`, no `.so`), and `android.util.Log` is not
    // mocked there — a call would turn every unit test that reaches this line
    // into a `RuntimeException`. The adapter that owns the store logs it before
    // re-throwing, which is where the diagnostic belongs anyway.
    if (entry.welcomeEnvelopeB64 != null) {
        try {
            ports.outbox.markWelcomesOwed(entry.id, commitMessageId)
        } catch (_: Throwable) {
            // Non-fatal by design — see above.
        }
    }

    // Obligation 2 — the Welcome is published HERE and nowhere earlier.
    // Publishing it before the verdict would let a joiner open into a state no
    // other member holds if the commit then lost: a three-party fork, strictly
    // worse than the two-party one this whole mechanism exists to close.
    val welcomeMessageIds = publishWelcomes(ports, entry)
    ports.outbox.ack(entry.id, commitMessageId, welcomeMessageIds.firstOrNull())

    return SweepOutcome.Committed(
        commitMessageId = commitMessageId,
        welcomeMessageIds = welcomeMessageIds,
        addedCount = entry.welcomeLeafKeysB64.size,
    )
}

/**
 * Apply every row below our own commit, then read the epoch.
 *
 * The whole region — both epoch reads, the apply loop and the cursor advance —
 * is ONE provider-lock hold. Between two rows this group is applied-but-not-
 * persisted, and a sibling group's `restoreStorage` observing that state would
 * roll the applied rows back while the cursor advanced past them.
 *
 * The cursor advances only to the last row actually applied — never past our own
 * commit, which the poll re-fetches and drops through the echo filter, and never
 * past a row this run did not consume.
 *
 * ## ...and never past a row whose durable write did not land
 *
 * This path advances the SAME cursor the poll drives, over rows the poll will
 * never fetch again, so it carries the same hazard and obeys the same rule:
 * persist first, advance second, hold the cursor when the persist does not land.
 * Since ADR-0045 a proposal applied here but not persisted, with the cursor
 * already past it, makes the commit that references it permanently unappliable —
 * the device sits at epoch N forever. See [DurableCursorGate] for why lagging is
 * the only safe direction.
 *
 * The write is [SweepPorts.persistApplied], which runs INSIDE this
 * `exclusiveProvider` region deliberately: releasing the lock to persist would
 * let a sibling group's `restoreStorage` roll these rows back between the apply
 * and the write, and the cursor would then advance over rows the snapshot does
 * not contain — the very state this is closing.
 *
 * A held cursor does NOT fail the verdict: the epoch reads either side of the
 * loop are in-memory and still correct, so the sweep learns whether it won. The
 * only thing withheld is the journal position, and the rows are re-fetched by the
 * next poll.
 */
private suspend fun adjudicate(
    ports: SweepPorts,
    cursorBefore: Long,
    ownCommitMessageId: Long,
): Boolean {
    val rows = try {
        ports.journal.fetch(cursorBefore)
    } catch (cause: Throwable) {
        throw SweepVerdictUnavailable(cause)
    }

    return ports.group.exclusiveProvider {
        val epochBefore = ports.group.epoch()
        val gate = DurableCursorGate()
        var lastApplied = cursorBefore
        for (row in rows) {
            // Our own commit, and everything the DS ordered after it, is not
            // part of the question. Rows above it are left for the poll.
            if (row.id >= ownCommitMessageId) break
            // Marked BEFORE the apply, not after: a throw is not proof that
            // nothing durable changed (`process_message` decrypts and ratchets
            // before either ADR-0022's role gate or ADR-0045's receipt gate can
            // refuse).
            gate.markTouched()
            try {
                // The port applies AND surfaces — see [SweepGroupPort.applyIncoming].
                // These rows are the only ones the poll will never see again,
                // because the cursor advance below moves past them, so the WHOLE
                // row goes over (the DS id and timestamp are what let the adapter
                // build a timeline event).
                ports.group.applyIncoming(row)
            } catch (_: Throwable) {
                // Bad frame, wrong epoch, a non-MLS Lakko envelope (rendezvous /
                // attribution), a role-refused commit, or one of our own rows.
                // Skipped, exactly as the poll loop skips it — and none of those
                // advance the epoch, which is the only thing the verdict reads.
                // A skipped row was never decrypted, so it surfaces nothing.
            }
            lastApplied = row.id
        }
        if (lastApplied != cursorBefore) {
            // No logging here — this file touches no Android API on purpose, so
            // a failed write surfaces through the repository's `persistApplied`
            // adapter instead.
            gate.commit(
                persist = ports.persistApplied,
                advance = { ports.cursor.advance(lastApplied) },
            )
        }
        sweepVerdict(epochBefore, ports.group.epoch())
    }
}

/**
 * Fan the (already wrapped) Welcome out to every added member's mailbox.
 *
 * A newly added member is not yet in the group and does not poll its journal, so
 * the Welcome goes to their stranger-inbox pseudonym — the same destination the
 * direct add path uses. One sweep commit can carry several adds under one
 * Welcome, which is why this is a fan-out where the direct path has a single
 * publish.
 *
 * A failure here leaves the entry un-acked, so the restart sweep re-publishes
 * and re-adjudicates. That re-run reaches the merge branch again (our commit is
 * skipped as our own, the epoch does not move, so it reports won) and retries
 * the fan-out — which is why this can be allowed to throw rather than being
 * swallowed.
 */
private suspend fun publishWelcomes(
    ports: SweepPorts,
    entry: SweepDurableEntry,
): List<Long> {
    val ids = mutableListOf<Long>()
    entry.welcomeEnvelopeB64?.let { envelopeB64 ->
        val bytes = ports.decodeB64(envelopeB64)
        val leaves = entry.welcomeLeafKeysB64.map(ports.decodeB64)
        for (mailbox in ports.welcomeTargets(leaves)) {
            ids.add(ports.journal.publishMailbox(mailbox, bytes))
        }
    }
    return ids
}
