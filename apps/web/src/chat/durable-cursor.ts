// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The receive path's ordering rule, in one place.
 *
 * # The rule
 *
 * **The DS journal cursor may LAG the durable MLS state. It must never LEAD
 * it.** The two directions are not symmetric, and the asymmetry is the whole
 * design:
 *
 * * **Behind is benign and self-healing.** Rows below the durable state get
 *   re-fetched. An already-applied commit is rejected as `WrongEpoch` and
 *   skipped exactly as the poll already skips a bad frame; an already-stored
 *   proposal is de-duplicated by `ProposalRef` in `lakko-mls`
 *   (`Group::gate_incoming_proposal`, which reports `stored: true` for one it
 *   already holds); an application row whose generation was consumed simply
 *   fails to decrypt and is skipped. The cursor catches up on the next batch.
 * * **Ahead is fatal and permanent.** ADR-0045 made the durable proposal store
 *   a PRECONDITION for applying a later by-reference commit: OpenMLS resolves
 *   the reference against the receiver's own store, so a peer that lost the
 *   proposal fails `InvalidCommit(MissingProposal)` before any Lakko gate runs.
 *   That commit is then logged and skipped, the cursor advances past it too,
 *   and the device sits at epoch N while the group is at N+1 — silently, for
 *   every subsequent message, with `isActive()` still reporting membership.
 *   Only a remove-and-re-add recovers it.
 *
 * Before ADR-0045 the receiver needed no prior state at all: a direct commit
 * inlines its proposals by value. The precondition is what turned a swallowed
 * IndexedDB error into a permanent brick.
 *
 * # Why an ordering rule rather than one atomic write
 *
 * ADR-0040 closed the same shape on the SEND side with atomicity
 * (`saveSnapshotWithCommitEntry` — one IndexedDB transaction over two stores in
 * one database, fail-closed). The receive side cannot copy that: the MLS
 * snapshot lives in the `lakko-mls` database and the cursor in
 * `lakko-message-log`, and IndexedDB has no cross-DATABASE transaction. Merging
 * them would mean folding the whole debounced, all-groups timeline blob into
 * every poll batch — and would still leave iOS and Android, where the two are
 * separate files with no cross-store transaction available at any price.
 *
 * It is also not needed. Atomicity would make the cursor exactly equal the
 * state; ordering lets it lag, and lag is the benign direction. One rule, the
 * same on three platforms, is worth more here than one platform's stronger
 * guarantee.
 *
 * # What counts as "touched"
 *
 * A row is marked as having touched durable state when it is HANDED to the
 * core, not when the call returns successfully. A throw is not proof that
 * nothing was written: `process_incoming_verifying_roles` runs
 * `process_message` — which decrypts, and ratchets — BEFORE the ADR-0022 role
 * gate can refuse the commit, and before the ADR-0045 receipt gate can drop the
 * proposal. Marking after the fact would leave exactly those rows unprotected.
 *
 * The cost of marking early is one extra snapshot for a batch in which every
 * non-echo row threw, which is a rare shape (a re-fetched duplicate). Rows the
 * echo filter drops never reach the core at all, so an all-echo batch still
 * persists nothing, and an empty batch — the common long-poll timeout — still
 * costs nothing.
 *
 * # Why the flag is sticky
 *
 * `Identity.snapshotStorage()` serialises the WHOLE provider, so a failed
 * persist leaves every group behind, and a later batch that touched nothing new
 * must not stroll past rows an earlier batch consumed but never wrote. The flag
 * therefore survives across batches and clears only on a persist that landed.
 *
 * A gate per `ChatClient` (rather than per group) is the accurate granularity
 * for the same reason: any successful persist flushes every group at once.
 */

/** What {@link DurableCursorGate.commit} did. */
export type CursorVerdict =
  /** The durable state is on disk; the cursor moved. */
  | "advanced"
  /** The durable state did not land; the cursor was HELD at its old value and
   *  the rows will be re-fetched. */
  | "held";

/**
 * Tracks whether this device's in-memory MLS state is ahead of its last
 * successful snapshot, and gates every journal-cursor advance on closing that
 * gap.
 */
export class DurableCursorGate {
  /** In-memory MLS state is ahead of the last successful snapshot. */
  private dirty = false;

  /** Record that a row has been handed to the MLS core. Call this BEFORE the
   *  call, never after — see the module docs. */
  markTouched(): void {
    this.dirty = true;
  }

  /** Diagnostics + tests: is the durable snapshot known to be behind? */
  get isBehind(): boolean {
    return this.dirty;
  }

  /**
   * Persist if anything is outstanding, then advance the cursor — or hold it.
   *
   * `advance` runs only on the `"advanced"` path, and `persist` is the
   * FAIL-CLOSED variant (it must reject on a failed write, not swallow). A
   * rejection is logged rather than re-thrown: the batch's events are real and
   * already decrypted, the caller should still render them, and the only thing
   * being withheld is the cursor.
   */
  async commit(
    persist: () => Promise<void>,
    advance: () => void,
  ): Promise<CursorVerdict> {
    if (this.dirty) {
      try {
        await persist();
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn(
          "MLS snapshot persist failed — holding the journal cursor so the " +
            "batch is re-fetched rather than lost (the cursor may lag the " +
            "durable state, never lead it)",
          err,
        );
        return "held";
      }
      this.dirty = false;
    }
    advance();
    return "advanced";
  }
}
