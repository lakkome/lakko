// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The receive path's ordering rule, in one place.
///
/// # The rule
///
/// **The DS journal cursor may LAG the durable MLS state. It must never LEAD
/// it.** The two directions are not symmetric, and the asymmetry is the whole
/// design:
///
///   * **Behind is benign and self-healing.** Rows below the durable state get
///     re-fetched. An already-applied commit is rejected as `WrongEpoch` and
///     skipped exactly as the poll already skips a bad frame; an already-stored
///     proposal is de-duplicated by `ProposalRef` in `lakko-mls`
///     (`Group::gate_incoming_proposal`, which reports `stored: true` for one it
///     already holds); an application row whose generation was consumed simply
///     fails to decrypt and is skipped. The cursor catches up on the next batch.
///   * **Ahead is fatal and permanent.** ADR-0045 made the durable proposal
///     store a PRECONDITION for applying a later by-reference commit: OpenMLS
///     resolves the reference against the receiver's own store, so a peer that
///     lost the proposal fails `InvalidCommit(MissingProposal)` before any Lakko
///     gate runs. That commit is then logged and skipped, the cursor advances
///     past it too, and the device sits at epoch N while the group is at N+1 —
///     silently, for every subsequent message, with `isActive()` still reporting
///     membership. Only a remove-and-re-add recovers it.
///
/// Before ADR-0045 the receiver needed no prior state at all: a direct commit
/// inlines its proposals by value. The precondition is what turned a swallowed
/// filesystem error into a permanent brick.
///
/// # Why an ordering rule rather than one atomic write
///
/// ADR-0040 closed the same shape on the SEND side with atomicity. The receive
/// side cannot copy that here: the MLS snapshot is one App-Group file and the
/// cursor lives inside the ``MessageLogStore`` envelope, a different file with
/// no cross-store transaction available at any price. Web is no better off —
/// its two stores are two IndexedDB *databases*, and IndexedDB has no
/// cross-database transaction.
///
/// It is also not needed. Atomicity would make the cursor exactly equal the
/// state; ordering lets it lag, and lag is the benign direction. One rule, the
/// same on three platforms, is worth more here than one platform's stronger
/// guarantee.
///
/// # What counts as "touched"
///
/// A row is marked as having touched durable state when it is HANDED to the
/// core, not when the call returns successfully. A throw is not proof that
/// nothing was written: `process_incoming_verifying_roles` runs
/// `process_message` — which decrypts, and ratchets — BEFORE the ADR-0022 role
/// gate can refuse the commit, and before the ADR-0045 receipt gate can drop the
/// proposal. Marking after the fact would leave exactly those rows unprotected.
/// See ``PollMessagesResult/touchedMlsState``.
///
/// # Why the flag is sticky — defence in depth, stated as such
///
/// `Identity.snapshotStorage()` serialises the WHOLE provider, so a failed
/// persist leaves every group behind. The flag therefore survives across batches
/// and clears only on a persist that landed, and there is ONE gate per
/// ``ChatRepository`` rather than one per group, for the same reason: any
/// successful persist flushes every group at once.
///
/// **Honest scope: no test pins this, because no reachable case was found.** A
/// held cursor is exactly what makes the batch be re-fetched, and every row that
/// touched the core is a row the echo filter did not drop, so it is handed to
/// the core again and re-marks the gate. Under the current call shape the flag
/// therefore never changes an outcome. It is kept because that argument is a
/// property of the CALLERS, not of this type: the day something advances a
/// cursor from a path that did not just re-fetch those rows, the stateless
/// version would silently start leading the durable state again. It costs one
/// bool, and it makes the guarantee local.

/// What ``applyDurableCursorBatch(gate:touchedMlsState:persist:advance:)`` did.
public enum CursorVerdict: Equatable {
    /// The durable state is on disk; the cursor moved.
    case advanced
    /// The durable state did not land; the cursor was HELD at its old value and
    /// the rows will be re-fetched.
    case held
}

/// Tracks whether this device's in-memory MLS state is ahead of its last
/// successful snapshot, and gates every journal-cursor advance on closing that
/// gap. See the file docs for the rule.
public final class DurableCursorGate {
    /// In-memory MLS state is ahead of the last successful snapshot.
    private var dirty = false

    public init() {}

    /// Record that a row has been handed to the MLS core. Call BEFORE the call,
    /// never after — see the file docs.
    public func markTouched() {
        dirty = true
    }

    /// Diagnostics + tests: is the durable snapshot known to be behind?
    public var isBehind: Bool { dirty }

    /// Persist if anything is outstanding, then advance the cursor — or hold it.
    ///
    /// `advance` runs only on the ``CursorVerdict/advanced`` path, and `persist`
    /// is the FAIL-CLOSED variant (it must throw on a failed write, not swallow
    /// it). The throw is logged rather than rethrown: the batch's events are
    /// real and already decrypted, the caller should still render them, and the
    /// only thing being withheld is the cursor.
    @discardableResult
    public func commit(
        persist: () throws -> Void,
        advance: () -> Void
    ) -> CursorVerdict {
        if dirty {
            do {
                try persist()
            } catch {
                NSLog(
                    "MLS snapshot persist failed — holding the journal cursor so "
                        + "the batch is re-fetched rather than lost (the cursor may "
                        + "lag the durable state, never lead it): \(error)"
                )
                return .held
            }
            dirty = false
        }
        advance()
        return .advanced
    }
}

/// The receive path's whole ordering, as ONE function, so the poll and the
/// ADR-0045 committer sweep cannot drift and a test can drive the rule itself
/// rather than a paraphrase of it.
///
/// Same reason ``runCrashSafeCommit`` and the split-poll orchestration are
/// shaped this way: the property being defended is an ORDERING between a state
/// mutation, a durable write and a cursor advance, and an ordering property is
/// only actually pinned if a test can run it. ``ChatRepository`` needs a live
/// FFI provider and the App Group container, so a test that re-implemented this
/// sequence would prove nothing about production.
@discardableResult
public func applyDurableCursorBatch(
    gate: DurableCursorGate,
    touchedMlsState: Bool,
    persist: () throws -> Void,
    advance: () -> Void
) -> CursorVerdict {
    if touchedMlsState { gate.markTouched() }
    return gate.commit(persist: persist, advance: advance)
}
