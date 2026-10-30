// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0045 S4a/S4b — the committer sweep: stage, publish, read the journal
/// verdict, then merge or abandon. iOS mirror of the web reference
/// `apps/web/src/chat/sweep.ts`.
///
/// ## What this closes
///
/// G-38 residual 1: two devices build a membership commit against the same
/// epoch N, the DS journal orders them by arrival, and the loser has *already*
/// eager-merged its own N+1 — so it sits at an epoch nobody else reached,
/// silently and permanently. ADR-0045's answer is that members emit
/// **proposals** (not epoch-advancing, so a lost race costs nothing) and one
/// elected owner-or-admin sweeps them into a single commit.
///
/// S4a made the committer the **owner only** (`ownCommitterRank() == 0`), so an
/// admin-only group simply waited. **S4b widened it to every candidate**,
/// ordered by the ADR-0045 liveness beacon: this device sweeps when it is a
/// candidate and has heard no fresh beacon from anyone better placed — a
/// strictly lower rank, or (ADR-0050 2.5a item 6, every leaf of a ranked
/// account holding the account's rank) the same rank and a lower signature
/// key — including when it has heard nothing at all, which means nobody
/// better placed is running. The decision itself is not made here:
/// ``SweepPorts/actingCommitter`` hands it in, computed by `CommitterBeacon`
/// over the core's own fold.
///
/// ## Why the sweep still stages rather than eager-merging
///
/// Because a committer that *can* lose a race must not merge before the journal
/// has ordered its commit, and with S4b that is no longer hypothetical: each
/// candidate folds over its own poll-timed view and a beacon is stale by up to
/// one interval even when its sender is healthy, so two candidates can both
/// reach `act`. **The beacon removes the wait, not the race** — the stand-down
/// re-read below and the staged-commit rollback are as required after S4b as
/// before it. `commitPendingProposals` is exactly `stagePendingProposals`
/// followed at once by `mergeStagedSweep`; building on it would be building the
/// hazard.
///
/// ## The five obligations, and where each one lives
///
/// 1. **Reach a verdict; never leave a sweep staged.** A staged sweep is
///    *durable* (OpenMLS persists `MlsGroupState::PendingCommit`) and freezes
///    that group's own membership operations across restarts — `is_operational()`
///    refuses `propose_*` / add / remove / `self_update` while one is pending.
///    So ``runCommitterSweep(_:)`` adjudicates every sweep it stages, and
///    ``resolveOwedVerdict(_:)`` is what a restart runs first: it is wired into
///    ADR-0040's existing `sweepPendingCommits(record:)` seam rather than
///    becoming a second restart path.
/// 2. **Publish the Welcome only AFTER the verdict.** If the commit loses and a
///    Welcome is already out, a joiner opens into a state nobody holds — a
///    three-party fork, strictly worse than the two-party one. `publishWelcomes`
///    is reached only on the `merge` branch.
/// 3. **The stand-down re-read.** Immediately before staging, re-read the
///    journal head; if it has moved, stand down and re-evaluate. A required step
///    of the algorithm, not a mitigation.
/// 4. **durably record -> publish (or re-publish) -> read the verdict -> merge
///    or abandon.** ADR-0040's outbox and this pair answer *different* questions
///    about the same commit — the outbox answers *did my commit reach the wire*,
///    the staged pair answers *did it win the wire order* — and the first must be
///    `yes` before the second is meaningful. So the outbox entry is written
///    BEFORE any publish, exactly as it is on the four direct paths.
/// 5. Obligation 5 (the direct-commit paths become propose-or-sweep) lives in
///    `ChatRepository`, which is this module's only production caller.
///
/// ## The verdict is read by APPLYING, not by peeking at the epoch field
///
/// The question is "did a **Commit** land below my own commit's DS id". The FFI
/// exposes `epochOfMessage` but no `contentTypeOfMessage`, and epoch alone
/// cannot tell a Commit from an Application message or a Proposal — every one of
/// them is framed at the current epoch. Guessing in the permissive direction is
/// not an option: abandoning a sweep that actually WON, after peers applied it,
/// leaves this device at N with the group at N+1 and its own commit
/// un-re-appliable (`CannotDecryptOwnMessage`) — the same brick, in reverse.
///
/// So the verdict asks a question that needs no discrimination: **apply every
/// row below our own commit, then ask whether our epoch moved.** Only a Commit
/// moves it. Applying is the audited receive path, so nothing new is invented,
/// and it is safe while a sweep is staged: nothing on the receive path consults
/// `is_operational`, and OpenMLS discards a pending commit as part of merging a
/// foreign one.
///
/// That the rows are applied here rather than by the poll is why the whole
/// region — stage, publish, fetch, verdict, merge/abandon — must hold the
/// per-group commit lock (`ChatRepository.withCommitLock`): it is a
/// read-modify-write over both the group epoch and the journal cursor, and a
/// poll interleaving inside it would make the verdict non-deterministic. The
/// lock is therefore deliberately held across two short DS round-trips; the
/// alternative is not a slower verdict but a wrong one.
///
/// ## Ports, not `Group`
///
/// Everything here is expressed over injected closures so the whole
/// orchestration is host-testable with no MLS provider, no filesystem and no
/// network. `ChatRepository` supplies the thin adapters. This is the same split
/// `ChatRepository.planSweep` already uses for ADR-0040's torn-write rule, one
/// level up.

/// The MLS artifacts a staged sweep produced. Mirrors the UniFFI `StagedSweep`.
public struct StagedSweepArtifacts: Equatable {
    /// The commit wire bytes to publish. The local epoch has NOT advanced.
    public var commit: Data
    /// The Welcome, present only when the sweep carried at least one add.
    public var welcome: Data?
    /// The 32-byte Ed25519 signature key of every member this sweep ADDS.
    ///
    /// One sweep commit can carry several adds under a single Welcome, which is
    /// why this is a list where the four direct paths have one destination. Each
    /// key derives that member's mailbox pseudonym — the Welcome's delivery
    /// address, since a not-yet-member does not poll the group journal.
    public var addedSignatureKeys: [Data]

    public init(commit: Data, welcome: Data?, addedSignatureKeys: [Data]) {
        self.commit = commit
        self.welcome = welcome
        self.addedSignatureKeys = addedSignatureKeys
    }
}

/// One row of the DS group journal, as the sweep reads it.
public struct SweepJournalRow: Equatable {
    public var id: Int64
    public var payload: Data
    /// The DS-assigned row timestamp, verbatim as the DS returned it.
    ///
    /// Not decoration: an applied row becomes a timeline event, and the shared
    /// event constructor requires the DS row timestamp — it is the one clock
    /// every member reads identically for the identical row. A row shape that
    /// carried only the payload is exactly what made the verdict apply unable to
    /// surface anything.
    public var createdAt: String

    public init(id: Int64, payload: Data, createdAt: String) {
        self.id = id
        self.payload = payload
        self.createdAt = createdAt
    }
}

/// What the sweep durably records, per ADR-0040.
public struct SweepDurableEntry: Equatable {
    public var id: String
    public var commitB64: String
    public var welcomeEnvelopeB64: String?
    public var welcomeMailboxesB64: [String]
    /// The DS id of the published commit, recorded alongside the welcomes-owed
    /// marker. Diagnostics only — it is carried so the eventual ack can name the
    /// commit it belongs to rather than inventing one. `nil` until the verdict
    /// settled.
    public var commitMessageId: Int64?

    public init(
        id: String,
        commitB64: String,
        welcomeEnvelopeB64: String? = nil,
        welcomeMailboxesB64: [String] = [],
        commitMessageId: Int64? = nil
    ) {
        self.id = id
        self.commitB64 = commitB64
        self.welcomeEnvelopeB64 = welcomeEnvelopeB64
        self.welcomeMailboxesB64 = welcomeMailboxesB64
        self.commitMessageId = commitMessageId
    }
}

/// The MLS operations the sweep drives. Adapted onto a live `Group`.
///
/// A struct of closures rather than a protocol: every member is bound to one
/// already-resolved `(identity, group)` pair, so a protocol would only add a
/// witness type per call site. `@MainActor` throughout because every real
/// implementation reads `ChatRepository` state.
@MainActor
public struct SweepGroupPort {
    /// The group's `group_id` bytes — the DS journal pseudonym.
    public var groupId: Data
    /// The current MLS epoch. Read before and after the verdict apply; a read
    /// this device cannot perform makes the verdict unreadable, never a guess.
    public var epoch: () throws -> UInt64
    /// ADR-0045 committer rank: `0` for the owner, `k + 1` for the k-th admin,
    /// and (ADR-0050 2.5a) a rank after the admins for a plain member's acting
    /// leaf only while it holds a device admission or its own account's
    /// self-service proposal it could sweep; `nil` otherwise, and for a group
    /// with no role table. A plain member's sweep commits only that subset.
    ///
    /// `nil` and `0` are NOT interchangeable — `0` is the owner. Since ADR-0050
    /// 2.5a item 6 every leaf of a ranked account holds the account's rank, so
    /// the owner's devices all hold `0` and the beacon fold breaks their tie by
    /// signature key.
    public var ownCommitterRank: () -> UInt32?
    /// How many authorised proposals are waiting. `0` short-circuits the sweep
    /// before it touches the network.
    public var pendingProposalCount: () -> Int
    /// `true` when a staged sweep is still awaiting its journal verdict.
    public var hasStagedSweep: () -> Bool
    /// Build the sweep commit and STOP before merging. `nil` when nothing
    /// authorised is pending (deliberately not an empty commit, which would
    /// advance the epoch and race other committers for no effect).
    public var stagePendingProposals: () throws -> StagedSweepArtifacts?
    /// Won — land at the new epoch, the one the peers reached.
    public var mergeStagedSweep: () throws -> Void
    /// Lost — discard the staged commit and stay put, free to apply the winner.
    /// Per-group: it never reaches a sibling group, unlike a whole-provider
    /// snapshot restore.
    public var abandonStagedSweep: () throws -> Void
    /// Apply one journal row through the audited role-verifying receive path AND
    /// surface whatever it produced to the timeline.
    ///
    /// THROWS on a bad frame / wrong epoch / refused commit — the caller skips,
    /// exactly as the poll loop does, and a skipped row must surface nothing.
    ///
    /// ## Why the WHOLE row, and why the port also surfaces
    ///
    /// The verdict consumes every row the DS ordered below our own commit and
    /// then advances the shared cursor over them, so the poll never fetches them
    /// again. Anything one of those rows carried — an ordinary chat message
    /// another member sent in the window between the stand-down re-read and our
    /// publish — is gone from the UI forever unless it is surfaced HERE.
    /// Applying it and discarding the result decrypts the message, advances the
    /// ratchet, and loses the plaintext silently.
    ///
    /// Surfacing therefore belongs to the port rather than to this module, and
    /// the port is handed the whole ``SweepJournalRow`` rather than just its
    /// payload: the shared event constructor in `ChatClient.swift` needs the DS
    /// row id and the DS row timestamp, and every envelope kind must be
    /// dispatched by that ONE ladder — a second copy of it here is how the poll
    /// path and the sweep path would drift. This module has nothing to decide
    /// about an event, so returning one for it to collect would buy nothing.
    ///
    /// Because surfacing happens inline, per row, a LOST verdict and a thrown
    /// verdict both leave the already-applied rows on the timeline — which is
    /// correct: whether our own commit won the order says nothing about the rows
    /// below it, which were applied and consumed either way.
    public var applyIncoming: (SweepJournalRow) throws -> Void

    public init(
        groupId: Data,
        epoch: @escaping () throws -> UInt64,
        ownCommitterRank: @escaping () -> UInt32?,
        pendingProposalCount: @escaping () -> Int,
        hasStagedSweep: @escaping () -> Bool,
        stagePendingProposals: @escaping () throws -> StagedSweepArtifacts?,
        mergeStagedSweep: @escaping () throws -> Void,
        abandonStagedSweep: @escaping () throws -> Void,
        applyIncoming: @escaping (SweepJournalRow) throws -> Void
    ) {
        self.groupId = groupId
        self.epoch = epoch
        self.ownCommitterRank = ownCommitterRank
        self.pendingProposalCount = pendingProposalCount
        self.hasStagedSweep = hasStagedSweep
        self.stagePendingProposals = stagePendingProposals
        self.mergeStagedSweep = mergeStagedSweep
        self.abandonStagedSweep = abandonStagedSweep
        self.applyIncoming = applyIncoming
    }
}

/// The DS operations the sweep drives.
@MainActor
public struct SweepJournalPort {
    /// Rows strictly after `after`, in ascending id order.
    public var fetch: (Int64) async throws -> [SweepJournalRow]
    /// ADR-0081 decision 1 — send the staged commit with the registration of
    /// the epoch it moves to (`dsStagedAccessSigner`) in ONE request,
    /// `POST .../commit`, proven under the epoch the group still sits at;
    /// resolves to the commit row's DS id. The identical re-send of a commit
    /// that landed answers its original id, so the owed verdict re-sends. A
    /// throw is classified by ``SweepPorts/classifyCommitFailure``.
    public var commitStaged: (Data) async throws -> Int64
    /// Publish to a member's mailbox pseudonym; resolves to the DS id.
    ///
    /// ADR-0050 2.5a item 7 — throws ``SweepWelcomeUndeliverable`` when this
    /// device holds no key the (closed) mailbox accepts: nothing was written,
    /// and the Welcome stays owed to that mailbox (``publishWelcomes``) without
    /// blocking the group's later sweeps. Any other throw is transient.
    public var publishMailbox: (Data, Data) async throws -> Int64
    /// ADR-0057 obligation 5 — whether this device holds a key the DS accepts
    /// an epoch registration from (the journal root, or a registrar seed the
    /// owner sent). Without one a commit could never be registered, so the
    /// sweep is never staged: it is left to an admin or the owner.
    public var canRegister: () -> Bool
    /// Whether a NEW sweep may register an epoch now, or the last refusal's
    /// backoff (``SweepRegistrationBackoff``) is still running. Asked only
    /// where a fresh sweep would start: an owed verdict and an owed Welcome
    /// fan-out are resolved first and never wait on it (C-67).
    public var registrationDue: () -> Bool

    public init(
        fetch: @escaping (Int64) async throws -> [SweepJournalRow],
        commitStaged: @escaping (Data) async throws -> Int64,
        publishMailbox: @escaping (Data, Data) async throws -> Int64,
        canRegister: @escaping () -> Bool = { true },
        registrationDue: @escaping () -> Bool = { true }
    ) {
        self.fetch = fetch
        self.commitStaged = commitStaged
        self.publishMailbox = publishMailbox
        self.canRegister = canRegister
        self.registrationDue = registrationDue
    }
}

/// The ADR-0040 outbox operations the sweep drives.
@MainActor
public struct SweepOutboxPort {
    /// Mint a fresh entry id.
    public var newEntryId: () -> String
    /// Durably record the entry AND the current MLS snapshot, BEFORE any
    /// publish. A throw ABORTS the sweep fail-closed.
    public var record: (SweepDurableEntry) throws -> Void
    /// The un-acked entry whose **verdict** is still owed, or `nil`.
    ///
    /// A welcomes-owed entry is deliberately NOT returned here: its verdict is
    /// settled and its commit is merged, so re-adjudicating it would re-publish
    /// a commit whose order was already read.
    public var findOwed: () -> SweepDurableEntry?
    /// Flip the entry from *verdict owed* to *Welcome fan-out owed*, and record
    /// the DS id its commit published under, in ONE durable write.
    ///
    /// ## What a restart must, and must not, do with this
    ///
    /// MUST: re-publish the entry's Welcome to every mailbox in
    /// ``SweepDurableEntry/welcomeMailboxesB64``, then ack. MUST NOT: re-publish
    /// the commit, re-adjudicate, or apply another journal row — the verdict is
    /// already known and the commit is already merged.
    ///
    /// The marker is written strictly AFTER ``SweepGroupPort/mergeStagedSweep``
    /// and its persist, and that ordering is what keeps obligation 2 intact: the
    /// marker IS the durable statement "the verdict said WON", so recording it
    /// any earlier would let a pre-verdict restart publish a Welcome for a
    /// commit that then lost — the three-party fork. A restart that finds a
    /// staged sweep instead finds no marker, and ``resolveOwedWelcomes(_:)``
    /// refuses outright while one is staged, so the two states cannot be
    /// confused even if a write were ever to land out of order.
    ///
    /// A throw is NOT fatal — see the call site.
    public var markWelcomesOwed: (String, Int64) throws -> Void
    /// Every un-acked entry whose verdict settled but whose Welcome fan-out is
    /// still owed (empty when none). All of them, not the first: one waiting
    /// for a key to its addressee's mailbox must not starve the others.
    public var findOwedWelcomes: () -> [SweepDurableEntry]
    /// ADR-0050 2.5a item 7 — keep the entry Welcome-owed, now addressed to
    /// only the mailboxes still waiting for a key (`id`, the commit's DS id
    /// when known, the remaining mailboxes). Also sets the welcomes-owed
    /// marker, so an entry whose marker write failed earlier is not lost.
    public var keepOwedWelcomes: (String, Int64?, [String]) throws -> Void
    /// Mark the entry acked once every artifact is on the wire.
    public var ack: (String, Int64?, Int64?) -> Void
    /// Delete the entry. Used on a LOSS: a commit that lost the order must never
    /// be re-published by the restart sweep.
    public var discard: (String) -> Void

    public init(
        newEntryId: @escaping () -> String,
        record: @escaping (SweepDurableEntry) throws -> Void,
        findOwed: @escaping () -> SweepDurableEntry?,
        markWelcomesOwed: @escaping (String, Int64) throws -> Void,
        findOwedWelcomes: @escaping () -> [SweepDurableEntry],
        keepOwedWelcomes: @escaping (String, Int64?, [String]) throws -> Void = { _, _, _ in },
        ack: @escaping (String, Int64?, Int64?) -> Void,
        discard: @escaping (String) -> Void
    ) {
        self.newEntryId = newEntryId
        self.record = record
        self.findOwed = findOwed
        self.markWelcomesOwed = markWelcomesOwed
        self.findOwedWelcomes = findOwedWelcomes
        self.keepOwedWelcomes = keepOwedWelcomes
        self.ack = ack
        self.discard = discard
    }
}

/// The journal cursor, as the read/write pair `ChatRepository.pollGroup` uses.
@MainActor
public struct SweepCursorRef {
    public var read: () -> Int64
    public var advance: (Int64) -> Void

    public init(read: @escaping () -> Int64, advance: @escaping (Int64) -> Void) {
        self.read = read
        self.advance = advance
    }
}

/// Everything the sweep needs.
@MainActor
public struct SweepPorts {
    public var group: SweepGroupPort
    /// ADR-0045 S4b — is this device the acting committer right now?
    ///
    /// Called with this device's CURRENT rank (never `nil` — the caller has
    /// already established it is a candidate) and answered by the core's
    /// `actingCommitterVerdict` fold over the beacons this device has observed.
    /// Asynchronous because the fold reads the observation store; it is awaited
    /// before anything is staged, so a slow answer costs one attempt.
    ///
    /// A throw is treated as `defer` — fail-closed, because the alternative is a
    /// device that sweeps on no evidence about who else is running.
    public var actingCommitter: (UInt32) async throws -> ActingCommitter
    public var journal: SweepJournalPort
    public var outbox: SweepOutboxPort
    public var cursor: SweepCursorRef
    /// The mailbox address a member's Welcome is recorded under, from their
    /// 32-byte signature key (ADR-0078: a verified mailbox when one is
    /// known, else the key itself, resolved once the sweep merged).
    public var welcomeMailbox: (Data) throws -> Data
    /// The bytes recorded for the Welcome. ADR-0078: the raw Welcome; the
    /// publish port seals it as a `.welcome` mailbox row to the verified
    /// mailbox at publish time.
    public var wrapWelcome: (Data) -> Data
    /// Echo-suppress a row this device published, so its own next poll drops it
    /// instead of hitting `CannotDecryptOwnMessage`.
    ///
    /// Takes the id alone because the ports are built per group: the production
    /// wiring binds `groupIdHex` at construction, and
    /// ``ChatClient/markOwnPublishedCommit(groupIdHex:messageId:)`` will not
    /// accept a bare id. The sweep publishes only to its own group's journal,
    /// so there is nothing here for a second argument to vary over.
    public var markOwnPublished: (Int64) -> Void
    /// Best-effort MLS snapshot persist after a merge / abandon.
    public var persist: () -> Void
    /// FAIL-CLOSED MLS snapshot persist — THROWS on a failed write.
    ///
    /// Used by ``adjudicate(ports:cursorBefore:ownCommitMessageId:)`` only, and
    /// it has to be a separate port from ``persist`` because the two want
    /// opposite things from a failure. The verdict apply consumes journal rows
    /// the poll will never fetch again, so it advances the shared cursor over
    /// them — which makes it subject to exactly the ordering rule the poll
    /// obeys: the cursor may lag the durable MLS state, never lead it. See
    /// `Chat/DurableCursor.swift`.
    public var persistApplied: () throws -> Void
    /// ADR-0081 — what a failed ``SweepJournalPort/commitStaged`` means:
    ///
    /// - ``SweepCommitFailure/lost``: the commit route answered `409` —
    ///   another commit owns the staged epoch, nothing of this commit is on
    ///   the DS. The sweep abandons exactly like a lost verdict;
    /// - ``SweepCommitFailure/refused``: a definitive refusal (the
    ///   registration was refused, the proof did not verify): nothing is on
    ///   the DS either; the sweep abandons, drops its entry and throws;
    /// - ``SweepCommitFailure/unknown``: the DS could not be reached, or
    ///   answered in a way that does not say whether the commit landed. The
    ///   sweep stays staged with its entry; the owed verdict re-sends it
    ///   (idempotent on the DS).
    public var classifyCommitFailure: (Error) -> SweepCommitFailure

    public init(
        group: SweepGroupPort,
        actingCommitter: @escaping (UInt32) async throws -> ActingCommitter,
        journal: SweepJournalPort,
        outbox: SweepOutboxPort,
        cursor: SweepCursorRef,
        welcomeMailbox: @escaping (Data) throws -> Data,
        wrapWelcome: @escaping (Data) -> Data,
        markOwnPublished: @escaping (Int64) -> Void,
        persist: @escaping () -> Void,
        persistApplied: @escaping () throws -> Void,
        classifyCommitFailure: @escaping (Error) -> SweepCommitFailure = { _ in .unknown }
    ) {
        self.group = group
        self.actingCommitter = actingCommitter
        self.journal = journal
        self.outbox = outbox
        self.cursor = cursor
        self.welcomeMailbox = welcomeMailbox
        self.wrapWelcome = wrapWelcome
        self.markOwnPublished = markOwnPublished
        self.persist = persist
        self.persistApplied = persistApplied
        self.classifyCommitFailure = classifyCommitFailure
    }
}

/// What a failed commit of a staged sweep means (``SweepPorts/classifyCommitFailure``).
public enum SweepCommitFailure: Equatable {
    case lost
    case refused
    case unknown
}

/// What one sweep attempt did.
public enum SweepOutcome: Equatable {
    /// This device is not in the committer set right now (a plain member with
    /// no device admission to sweep — ADR-0050 2.5a gives it a rank while it
    /// holds one — or a group with no role table). Nothing happened — distinct
    /// from ``deferred``, which re-evaluates the same candidacy.
    case notCommitter(rank: UInt32?)
    /// ADR-0045 S4b — this device IS a candidate, but a better-placed one (a
    /// strictly lower rank, or the same rank and a lower signature key — a
    /// sibling device of the same account) beaconed inside the freshness
    /// window. Stood down without staging;
    /// the next poll re-evaluates, and if that candidate goes quiet its beacon
    /// ages out of the window and this device takes over on its own.
    case deferred(rank: UInt32, toRank: UInt32?)
    /// Nothing authorised was pending and nothing was staged.
    case nothingPending
    /// ADR-0057 — this device is a committer candidate but holds neither the
    /// journal root nor a registrar seed, so it cannot register an epoch and
    /// did not stage. The sweep is left to an appointed admin or the owner.
    case noJournalAuthority(rank: UInt32)
    /// The DS refused this group's last epoch registration (`400` / `401` /
    /// `403`) and the retry backoff has not run out, so no new sweep was
    /// staged. Owed verdicts and Welcomes were still resolved first.
    case registrationBackedOff(rank: UInt32)
    /// Obligation 3 — the journal moved since our cursor, so a commit may
    /// already be in flight. Stood down without staging; the poll drains the
    /// rows and the next attempt re-evaluates.
    case stoodDown(unappliedRows: Int)
    /// Won the journal order: merged, and the Welcome (if any) published.
    case committed(commitMessageId: Int64, welcomeMessageIds: [Int64], addedCount: Int)
    /// Lost the journal order: abandoned, no Welcome published, the proposals
    /// survive for the winner's next sweep. `commitMessageId` is `nil` when
    /// the commit route refused the commit outright (ADR-0081: another commit
    /// owns the epoch, so nothing of this one is on the DS).
    case lost(commitMessageId: Int64?)
    /// A previous run merged a winning sweep but could not get its Welcome onto
    /// the wire. The fan-out has now been retried and acked.
    case welcomesRepublished(welcomeMessageIds: [Int64], addedCount: Int)
    /// No settled-verdict entry was waiting on its Welcome fan-out. The common
    /// case, and what a staged sweep also reports — that one owes a VERDICT.
    case noWelcomesOwed
    /// ADR-0050 2.5a item 7 — owed Welcomes were retried and `undelivered`
    /// mailboxes still have no key this device holds; they stay owed (retried
    /// on the next poll and when a grant for that person arrives). Never
    /// returned by ``runCommitterSweep(_:)``, which goes on to sweep: a
    /// Welcome waiting for a key must not freeze the group's membership.
    case welcomesAwaitingGrant(undelivered: Int)
    /// A staged sweep was found with no durable record of its commit, so the
    /// commit provably never reached the wire (ADR-0040 records before it
    /// publishes). Abandoned, which also unwedges the frozen membership ops.
    case abandonedOrphan
    /// There was no verdict owed. Only ever returned by
    /// ``resolveOwedVerdict(_:)``.
    case noVerdictOwed
    /// The attempt did not complete — a durable record that would not land, or
    /// a journal order that could not be read.
    ///
    /// Deliberately its own case rather than folding into ``noVerdictOwed``:
    /// those two describe opposite situations. `noVerdictOwed` says there was
    /// nothing to finish; this says something was left unfinished, possibly with
    /// a sweep still staged and this group's membership frozen until the next
    /// attempt adjudicates it. A caller that logged them the same way would
    /// report a frozen group as an idle one.
    case unresolved
}

/// The journal order could not be read, so the verdict is genuinely unknown.
///
/// The sweep is left STAGED and its outbox entry un-acked on purpose. Both
/// alternatives are worse: merging on an unread verdict is the eager shape that
/// strands the loser, and abandoning would discard a commit that is already on
/// the wire and may have won. The next sweep attempt re-adjudicates through
/// ``resolveOwedVerdict(_:)``, and the sweep runs at the tail of every poll, so
/// the retry is continuous. The cost is that this group's own membership
/// operations stay frozen until it succeeds — a temporary freeze that
/// self-heals, which is strictly better than either wrong answer.
public struct SweepVerdictUnavailable: Error, CustomStringConvertible {
    public let underlying: Error

    public init(_ underlying: Error) {
        self.underlying = underlying
    }

    public var description: String {
        "ADR-0045: could not read the journal verdict — sweep left staged: \(underlying)"
    }
}

/// The pure verdict rule: did anything applied below our own commit advance the
/// group's epoch?
///
/// Only a Commit advances an epoch. An Application message drains a ratchet, a
/// Proposal queues, a non-MLS envelope throws and is skipped — none of them
/// move it. So an unchanged epoch means our commit is first in the order and we
/// WON; a moved epoch means a foreign commit was ordered ahead of ours and we
/// LOST.
public func sweepVerdict(epochBefore: UInt64, epochAfter: UInt64) -> SweepVerdictResult {
    epochAfter == epochBefore ? .won : .lost
}

/// The two answers ``sweepVerdict(epochBefore:epochAfter:)`` can give.
public enum SweepVerdictResult: Equatable {
    case won
    case lost
}

/// Obligation 3, as a pure rule: may this device stage a sweep right now?
///
/// A row this device has not applied might be a Commit, and without a
/// `contentTypeOfMessage` peek there is no way to tell. Standing down is free —
/// the poll drains the rows and the next attempt re-evaluates — whereas staging
/// on top of an unapplied commit publishes a commit peers will drop as
/// wrong-epoch. So the rule errs deliberately in the conservative direction:
/// ANY unapplied row stands the sweep down, not only a Commit.
///
/// This narrows the window; it does not close it. A commit can still land
/// between this check and our publish, which is exactly what the verdict step
/// repairs.
public func shouldStandDown(unappliedRowCount: Int) -> Bool {
    unappliedRowCount > 0
}

/// Run one committer sweep. Callers MUST hold the per-group commit lock.
///
/// Short-circuits without touching the network when this device is not a
/// candidate, or when nothing is pending and no verdict is owed — so wiring
/// this into the tail of every poll costs one synchronous check in the common
/// case.
///
/// - Throws: ``SweepVerdictUnavailable`` when the journal order could not be
///   read. The sweep is left staged for the next attempt.
@MainActor
public func runCommitterSweep(_ ports: SweepPorts) async throws -> SweepOutcome {
    let group = ports.group

    // Obligation 1, and it runs BEFORE the rank check on purpose.
    //
    // A staged sweep is DURABLE and freezes this group's membership operations
    // across restarts, so it must be adjudicated by whoever staged it —
    // including a device that has since been demoted out of the committer set,
    // a case S4b made newly reachable (S4a's only committer was the owner, whom
    // no role change can demote). Gating this behind the rank check would leave
    // such a device frozen permanently, with nothing that could ever unwedge it.
    // Re-publishing a demoted device's commit is safe: every peer's role gate
    // refuses it, so it loses the order and is abandoned — which is exactly the
    // unwedge.
    //
    // It also takes precedence over starting a NEW sweep, because
    // `stagePendingProposals` refuses a second stage fail-closed: trying to
    // start one first would throw and leave the freeze in place.
    if group.hasStagedSweep() {
        return try await resolveOwedVerdict(ports)
    }

    // The same obligation, one step further along: a sweep whose verdict settled
    // but whose Welcome never reached the wire owes that fan-out, and nothing
    // else will ever notice — the merge cleared `hasStagedSweep()` and emptied
    // the pending set, so both of the checks below would report an idle group
    // over a member who can never join.
    //
    // Placed BEFORE the rank check for the same reason the owed verdict is: a
    // device demoted since the merge still owes the Welcome for a member it
    // successfully added, and nobody else holds those bytes.
    //
    // The cost is one small store read per attempt where there was none. That is
    // deliberate: the outbox holds at most one entry per in-flight commit, and
    // the alternative is a repair that only runs when the user happens to reopen
    // the chat.
    let owedWelcomes = try await resolveOwedWelcomes(ports)
    switch owedWelcomes {
    case .noWelcomesOwed, .welcomesAwaitingGrant:
        // Nothing owed, or owed only to mailboxes this device holds no key for
        // yet: neither is a reason to hold the group's next sweep back.
        break
    default:
        return owedWelcomes
    }

    // `nil` is "not in the committer set right now" — a plain member holding no
    // device admission it could sweep (ADR-0050 2.5a), or a group with no role
    // table. `0` is the OWNER.
    guard let rank = group.ownCommitterRank() else {
        return .notCommitter(rank: nil)
    }

    if group.pendingProposalCount() == 0 { return .nothingPending }

    // ADR-0045 S4b — is anyone better placed announcing itself? Asked
    // only once there is something to sweep, so a quiet group never pays for it.
    // A throw is a defer: sweeping on an unreadable view of who else is running
    // is the failure this whole election exists to avoid.
    let acting: ActingCommitter
    do {
        acting = try await ports.actingCommitter(rank)
    } catch {
        // Fail-closed to a stand-down with no named successor. Deliberately NOT
        // expressed as `ActingCommitter.defer(toRank:toSignatureKey:)`: that case carries a
        // non-optional rank on the FFI, so synthesising one here would have to
        // name somebody — most plausibly this device — and "I deferred to
        // myself" is a lie in a log line that exists to answer *to whom*.
        return .deferred(rank: rank, toRank: nil)
    }
    switch acting {
    case .act:
        break
    case .defer(let toRank, _):
        return .deferred(rank: rank, toRank: toRank)
    case .notACandidate:
        // The fold disagrees with the rank read a few lines above — a role
        // table that changed under us mid-attempt. Stand down and re-evaluate
        // on the next poll rather than sweep on a rank the fold rejects.
        return .deferred(rank: rank, toRank: nil)
    }

    // ADR-0057 obligation 5 — the ACTING committer must hold a key the DS
    // takes an epoch registration from. A candidate the owner has not
    // appointed yet (no registrar seed) could only produce a commit that can
    // never be published, so it does not stage. Asked after the election, so
    // a device that would have deferred anyway still reports the deferral.
    guard ports.journal.canRegister() else { return .noJournalAuthority(rank: rank) }

    // Live walk 2026-09-29 — a refused registration is not retried on every
    // poll tail: the DS refuses the same statement the same way until
    // something changes (a claim, a membership or role change). Asked after
    // the owed-verdict and owed-Welcome obligations above, which never wait,
    // and before any network, so a backed-off group costs nothing.
    guard ports.journal.registrationDue() else { return .registrationBackedOff(rank: rank) }

    // Obligation 3 — the stand-down re-read, immediately before creating the
    // commit.
    let cursorBefore = ports.cursor.read()
    let ahead: [SweepJournalRow]
    do {
        ahead = try await ports.journal.fetch(cursorBefore)
    } catch {
        // Nothing has been staged yet, so a failed re-read costs only this
        // attempt. Treat an unreadable head as "the journal may have moved".
        throw SweepVerdictUnavailable(error)
    }
    if shouldStandDown(unappliedRowCount: ahead.count) {
        return .stoodDown(unappliedRows: ahead.count)
    }

    guard let staged = try group.stagePendingProposals() else {
        return .nothingPending
    }

    return try await publishAndAdjudicate(
        ports: ports,
        staged: staged,
        cursorBefore: cursorBefore,
        owed: nil
    )
}

/// Obligation 1, restart half — finish a verdict this device owes.
///
/// A staged commit is persisted by OpenMLS and read back by `Group.load`, so it
/// outlives the process. A client that finds one after a relaunch still owes a
/// verdict on a commit it may already have published, and until it reaches one
/// the group's membership is frozen. This is wired into ADR-0040's existing
/// restart sweep rather than being a second restart path.
///
/// The re-publish is idempotent by the same argument the ADR-0040 sweep already
/// rests on: peers that applied the original drop the duplicate as wrong-epoch,
/// and peers that never saw it apply it now. The verdict is then read exactly as
/// it would have been the first time — including the case where our ORIGINAL
/// commit sits in the journal below the re-published one, which applies as
/// `CannotDecryptOwnMessage`, is skipped, and correctly does not move our epoch.
@MainActor
public func resolveOwedVerdict(_ ports: SweepPorts) async throws -> SweepOutcome {
    let group = ports.group
    guard group.hasStagedSweep() else { return .noVerdictOwed }

    guard let owed = ports.outbox.findOwed() else {
        // No durable record of the commit. ADR-0040 writes the record BEFORE the
        // publish, so a snapshot carrying a staged sweep and no entry means the
        // write never completed and the commit never reached the wire.
        // Abandoning is therefore correct, and it is also what unwedges the
        // frozen membership operations — the worse failure of the two.
        try? group.abandonStagedSweep()
        ports.persist()
        return .abandonedOrphan
    }

    return try await publishAndAdjudicate(
        ports: ports,
        staged: StagedSweepArtifacts(
            commit: base64Decode(owed.commitB64) ?? Data(),
            welcome: owed.welcomeEnvelopeB64.flatMap { base64Decode($0) },
            // Already wrapped + already addressed when the entry was written.
            addedSignatureKeys: []
        ),
        cursorBefore: ports.cursor.read(),
        owed: owed
    )
}

/// Finish a Welcome fan-out a previous run merged but could not publish.
///
/// ## The failure this repairs
///
/// `publishAndAdjudicate` merges, persists, and only then publishes the Welcome.
/// A transient DS failure in that last step used to be unrecoverable: the merge
/// had already cleared `hasStagedSweep()` AND emptied the pending-proposal set,
/// which are exactly the two signals ``runCommitterSweep(_:)`` and
/// ``resolveOwedVerdict(_:)`` branch on, so both reported "nothing to do" while
/// the added member sat in every peer's roster unable to ever join. The durable
/// welcomes-owed marker is what gives that state a name, and this is what reads
/// it.
///
/// ## What it must NOT do
///
/// Not re-adjudicate, and not re-publish the commit. The verdict is settled and
/// the commit merged; re-reading the journal order would apply rows against a
/// question that is no longer open, and a second commit publish is noise every
/// peer drops as wrong-epoch.
///
/// ## Ordering — why a staged sweep is refused before the store is even read
///
/// A staged sweep owes a VERDICT. ADR-0045 obligation 2 forbids its Welcome
/// reaching the wire until that verdict says the commit won, so this refuses
/// outright rather than trusting that the marker cannot be present.
///
/// ## Idempotency
///
/// The retry re-publishes the SAME recorded envelope bytes, so a first publish
/// that actually landed leaves the joiner with two identical mailbox rows. That
/// is inert: `StagedWelcome::into_group` deletes the joiner's KeyPackage bundle
/// (openmls-0.8.1 `group/mls_group/creation.rs`), so the second copy cannot be
/// staged and every client's mailbox handler catches the failure and skips the
/// row. Nothing forks, and no ratchet moves twice.
///
/// A publish failure THROWS and leaves the marker in place for the next attempt.
///
/// Callers MUST hold the per-group commit lock.
@MainActor
public func resolveOwedWelcomes(_ ports: SweepPorts) async throws -> SweepOutcome {
    if ports.group.hasStagedSweep() { return .noWelcomesOwed }

    let owedEntries = ports.outbox.findOwedWelcomes()
    guard !owedEntries.isEmpty else { return .noWelcomesOwed }

    var welcomeMessageIds: [Int64] = []
    var addedCount = 0
    var undelivered = 0
    for owed in owedEntries {
        let delivery = try await publishWelcomes(ports: ports, entry: owed)
        welcomeMessageIds += delivery.ids
        if delivery.undelivered.isEmpty {
            ports.outbox.ack(owed.id, owed.commitMessageId, delivery.ids.first)
            addedCount += owed.welcomeMailboxesB64.count
        } else {
            keepAwaitingGrant(ports: ports, entry: owed, commitMessageId: owed.commitMessageId,
                              undelivered: delivery.undelivered)
            addedCount += owed.welcomeMailboxesB64.count - delivery.undelivered.count
            undelivered += delivery.undelivered.count
        }
    }
    if undelivered > 0 { return .welcomesAwaitingGrant(undelivered: undelivered) }
    return .welcomesRepublished(welcomeMessageIds: welcomeMessageIds, addedCount: addedCount)
}

/// ADR-0050 2.5a item 7 — keep `entry` owed to the mailboxes it could not
/// reach. A failed write is logged and not fatal: the in-memory state already
/// says the Welcome was not delivered, and the next attempt re-reads the
/// entry as it stands (all its mailboxes), which only costs a duplicate write
/// to a mailbox that already has it.
@MainActor
private func keepAwaitingGrant(
    ports: SweepPorts, entry: SweepDurableEntry, commitMessageId: Int64?, undelivered: [String]
) {
    do {
        try ports.outbox.keepOwedWelcomes(entry.id, commitMessageId, undelivered)
    } catch {
        NSLog("ADR-0050: could not record the Welcome still owed for %@: %@",
              entry.id, String(describing: error))
    }
    NSLog("ADR-0050: %d Welcome(s) wait for a key to the addressee's mailbox", undelivered.count)
}

/// The shared tail of both entry points: durably record (first time only),
/// publish the commit, read the verdict, then merge or abandon.
///
/// `owed` is supplied on the restart path, where the entry already exists and
/// its Welcome is already wrapped and addressed.
@MainActor
private func publishAndAdjudicate(
    ports: SweepPorts,
    staged: StagedSweepArtifacts,
    cursorBefore: Int64,
    owed: SweepDurableEntry?
) async throws -> SweepOutcome {
    let group = ports.group

    let entry: SweepDurableEntry
    if let owed {
        // ADR-0081 — the restart half: the commit call below re-sends the
        // same row with a freshly signed registration. The DS answers the
        // original id when the first send landed, `409` when another commit
        // took the epoch; it never replaces the winner's epoch.
        entry = owed
    } else {
        // Obligation 4 — durably record the artifacts before ANY publish; the
        // staged epoch's key and the commit row then reach the DS together,
        // in the commit call below (ADR-0081). The Welcome is wrapped and
        // addressed HERE, so the bytes the restart sweep would publish are the
        // very bytes computed now; nothing can drift.
        do {
            var mailboxes: [String] = []
            for key in staged.addedSignatureKeys {
                mailboxes.append(base64Encode(try ports.welcomeMailbox(key)))
            }
            let wrapped = staged.welcome.map { ports.wrapWelcome($0) }
            let fresh = SweepDurableEntry(
                id: ports.outbox.newEntryId(),
                commitB64: base64Encode(staged.commit),
                welcomeEnvelopeB64: wrapped.map { base64Encode($0) },
                welcomeMailboxesB64: mailboxes
            )
            try ports.outbox.record(fresh)
            entry = fresh
        } catch {
            // FAIL-CLOSED. Nothing was published, so the group must look as
            // though the sweep never happened. Abandoning is the per-group
            // rollback — it touches this group only, unlike a whole-provider
            // snapshot restore, which would roll sibling groups back past
            // commits they have already published.
            try? group.abandonStagedSweep()
            throw error
        }
    }

    guard let commitBytes = base64Decode(entry.commitB64) else {
        // An entry whose commit does not decode can never be published or
        // adjudicated. Abandon (which unwedges the frozen membership ops) and
        // drop it rather than leaving a permanently unresolvable verdict.
        try? group.abandonStagedSweep()
        ports.persist()
        ports.outbox.discard(entry.id)
        return .abandonedOrphan
    }

    let commitMessageId: Int64
    do {
        commitMessageId = try await ports.journal.commitStaged(commitBytes)
    } catch {
        let meaning = ports.classifyCommitFailure(error)
        if meaning == .unknown { throw error }
        // Nothing of this commit is on the DS: abandon like a lost verdict.
        // The proposals stay pending for the next sweep (the winner's, or this
        // device's after a backoff).
        try? group.abandonStagedSweep()
        ports.persist()
        ports.outbox.discard(entry.id)
        if meaning == .refused { throw error }
        return .lost(commitMessageId: nil)
    }
    ports.markOwnPublished(commitMessageId)

    let verdict = try await adjudicate(
        ports: ports,
        cursorBefore: cursorBefore,
        ownCommitMessageId: commitMessageId
    )

    if verdict == .lost {
        try? group.abandonStagedSweep()
        ports.persist()
        // A commit that lost must never be re-published: the restart sweep would
        // re-adjudicate a commit whose proposals the winner has already swept.
        // Discarding is safe because abandoning kept the proposals pending, so
        // nothing the user asked for is lost.
        ports.outbox.discard(entry.id)
        return .lost(commitMessageId: commitMessageId)
    }

    try group.mergeStagedSweep()
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
    if entry.welcomeEnvelopeB64 != nil {
        do {
            try ports.outbox.markWelcomesOwed(entry.id, commitMessageId)
        } catch {
            NSLog(
                "ADR-0045: could not record the owed Welcome fan-out for %@ "
                + "— a failure below is not retryable: %@",
                entry.id,
                String(describing: error)
            )
        }
    }

    // Obligation 2 — the Welcome is published HERE and nowhere earlier.
    // Publishing it before the verdict would let a joiner open into a state no
    // other member holds if the commit then lost: a three-party fork, strictly
    // worse than the two-party one this whole mechanism exists to close.
    let delivery = try await publishWelcomes(ports: ports, entry: entry)
    if delivery.undelivered.isEmpty {
        ports.outbox.ack(entry.id, commitMessageId, delivery.ids.first)
    } else {
        // ADR-0050 2.5a item 7 — no key this device holds opens those
        // mailboxes. Nothing was written to them (never unsigned: a closed
        // mailbox drops an unsigned write and answers as if it stored it); the
        // entry stays owed to exactly them, and the commit is reported as the
        // win it is so the rest of the post-commit work runs.
        keepAwaitingGrant(ports: ports, entry: entry, commitMessageId: commitMessageId,
                          undelivered: delivery.undelivered)
    }

    return .committed(
        commitMessageId: commitMessageId,
        welcomeMessageIds: delivery.ids,
        addedCount: entry.welcomeMailboxesB64.count
    )
}

/// Apply every row below our own commit, then read the epoch.
///
/// The cursor advances only to the last row actually applied — never past our
/// own commit, which the poll re-fetches and drops through the echo filter, and
/// never past a row this run did not consume.
///
/// ## ...and never past a row whose durable write did not land
///
/// This path advances the SAME cursor the poll drives, over rows the poll will
/// never fetch again, so it carries the same hazard and obeys the same rule:
/// persist first, advance second, hold the cursor when the persist does not
/// land. Since ADR-0045 a proposal applied here but not persisted, with the
/// cursor already past it, makes the commit that references it permanently
/// unappliable — the device sits at epoch N forever. See
/// `Chat/DurableCursor.swift` for why lagging is the only safe direction.
///
/// A held cursor does NOT fail the verdict: the epoch reads either side of the
/// loop are in-memory and still correct, so the sweep learns whether it won. The
/// only thing withheld is the journal position, and the rows are re-fetched by
/// the next poll.
@MainActor
private func adjudicate(
    ports: SweepPorts,
    cursorBefore: Int64,
    ownCommitMessageId: Int64
) async throws -> SweepVerdictResult {
    let group = ports.group
    let epochBefore: UInt64
    do {
        epochBefore = try group.epoch()
    } catch {
        throw SweepVerdictUnavailable(error)
    }

    let rows: [SweepJournalRow]
    do {
        rows = try await ports.journal.fetch(cursorBefore)
    } catch {
        throw SweepVerdictUnavailable(error)
    }

    let gate = DurableCursorGate()
    var lastApplied = cursorBefore
    for row in rows {
        // Our own commit, and everything the DS ordered after it, is not part of
        // the question. Rows above it are left for the poll.
        if row.id >= ownCommitMessageId { break }
        // Marked BEFORE the apply, not after: a throw is not proof that nothing
        // durable changed (`process_message` decrypts and ratchets before either
        // ADR-0022's role gate or ADR-0045's receipt gate can refuse).
        gate.markTouched()
        // Bad frame, wrong epoch, a non-MLS Lakko envelope (rendezvous /
        // attribution), a role-refused commit, or one of our own rows. Skipped,
        // exactly as the poll loop skips it — and none of those advance the
        // epoch, which is the only thing the verdict reads. A skipped row was
        // never decrypted, so it surfaces nothing.
        //
        // The port applies AND surfaces — see ``SweepGroupPort/applyIncoming``.
        // These rows are the only ones the poll will never see again, because
        // the cursor advance below moves past them, so the WHOLE row goes over
        // (the DS id and timestamp are what let the adapter build a timeline
        // event). Before that, a row applied here was consumed for MLS and
        // silently dropped from the chat timeline.
        try? group.applyIncoming(row)
        lastApplied = row.id
    }
    if lastApplied != cursorBefore {
        gate.commit(
            persist: ports.persistApplied,
            advance: { ports.cursor.advance(lastApplied) }
        )
    }

    let epochAfter: UInt64
    do {
        epochAfter = try group.epoch()
    } catch {
        throw SweepVerdictUnavailable(error)
    }
    return sweepVerdict(epochBefore: epochBefore, epochAfter: epochAfter)
}

/// Fan the (already wrapped) Welcome out to every added member's mailbox.
///
/// A newly added member is not yet in the group and does not poll its journal,
/// so the Welcome goes to their stranger-inbox pseudonym — the same destination
/// the direct add path uses. One sweep commit can carry several adds under one
/// Welcome, which is why this is a fan-out where the direct path has a single
/// publish.
///
/// A failure here leaves the entry un-acked, so the restart sweep re-publishes
/// and re-adjudicates. That re-run reaches the merge branch again (our commit is
/// skipped as our own, the epoch does not move, so it reports `won`) and retries
/// the fan-out — which is why this can be allowed to throw rather than being
/// swallowed.
///
/// ADR-0050 2.5a item 7 — a mailbox this device holds no key for
/// (``SweepWelcomeUndeliverable``) is skipped and returned in `undelivered`;
/// it never fails the fan-out to the others.
@MainActor
private func publishWelcomes(
    ports: SweepPorts,
    entry: SweepDurableEntry
) async throws -> (ids: [Int64], undelivered: [String]) {
    var ids: [Int64] = []
    var undelivered: [String] = []
    if let welcomeB64 = entry.welcomeEnvelopeB64, let bytes = base64Decode(welcomeB64) {
        for mailboxB64 in entry.welcomeMailboxesB64 {
            guard let pseudonym = base64Decode(mailboxB64) else { continue }
            do {
                ids.append(try await ports.journal.publishMailbox(pseudonym, bytes))
            } catch is SweepWelcomeUndeliverable {
                undelivered.append(mailboxB64)
            }
        }
    }
    // ADR-0053 point 8 — nothing goes to the group journal. A `/g/` joiner
    // cannot read a claimed journal, and it does not need to: its Welcome is
    // in its own mailbox like every other added member's.
    return (ids, undelivered)
}

/// ADR-0050 2.5a item 7 — a sweep Welcome whose addressee's mailbox no key
/// this device holds opens: every grant was refused (`401`), or none is held.
/// Nothing was written. Thrown by ``SweepJournalPort/publishMailbox``.
public struct SweepWelcomeUndeliverable: Error, Equatable {
    /// How many keys were tried (each refused with `401`); `0` when none was held.
    public let triedKeys: Int

    public init(triedKeys: Int) {
        self.triedKeys = triedKeys
    }
}

/// ADR-0050 2.5a item 7 — deliver one sweep Welcome to a closed mailbox:
/// try each candidate key in order, move to the next on a `401`, and when none
/// is left throw ``SweepWelcomeUndeliverable`` without writing. Never an
/// unsigned write: a closed mailbox answers one as stored and drops it, which
/// is how a Welcome used to be counted delivered and lost. Any other failure
/// is rethrown as it came (transient; the fan-out is retried).
public func deliverSweepWelcome<Auth>(
    candidates: [Auth],
    publish: (Auth) async throws -> Int64
) async throws -> Int64 {
    var refused = 0
    for candidate in candidates {
        do {
            return try await publish(candidate)
        } catch DsClientError.http(let status, _, _) where status == 401 {
            refused += 1
        }
    }
    throw SweepWelcomeUndeliverable(triedKeys: refused)
}

/// ADR-0060 section 6 — deliver one sweep Welcome to the open mailbox of a
/// person found in Everyone: one write without a proof, spending a
/// `mailbox-write` token. Delivered when the DS answers 2xx (`publish`
/// returns). A refusal (`401`) is ``SweepWelcomeUndeliverable`` with one key
/// tried, and an anonymous token that could not be had is one with none
/// tried: the Welcome stays owed and the fan-out to the others goes on. Any
/// other failure is rethrown as it came (transient; the fan-out is retried).
public func deliverOpenSweepWelcome(publish: () async throws -> Int64) async throws -> Int64 {
    do {
        return try await publish()
    } catch DsClientError.http(let status, _, _) where status == 401 {
        throw SweepWelcomeUndeliverable(triedKeys: 1)
    } catch is AnonTokenUnavailable {
        throw SweepWelcomeUndeliverable(triedKeys: 0)
    }
}

// MARK: - Registration backoff

/// The retry schedule of a sweep whose epoch registration the DS refused.
///
/// Live walk 2026-09-29: an old device's sweep was refused `401` (an unclaimed
/// journal) or `400` on `PUT /ds/group/{id}/access` and retried at the tail of
/// every poll, about every 5 s, in every group, forever. A refusal of that kind
/// does not resolve on its own: the DS answers the same statement the same way
/// until the journal is claimed, the roster or roles change, or the owner
/// appoints this device. So after a refusal the next NEW sweep waits
/// ``firstDelayMs``, doubling per consecutive refusal up to ``maxDelayMs``.
///
/// What resets it: a registration that lands, the owner's claim of the journal
/// landing (the caller drops the value), and an epoch change — a membership or
/// role change is a commit, and a commit moves the epoch, so a value recorded
/// at another epoch no longer holds anything back and the next refusal counts
/// from one again.
///
/// Only the cadence changes. A staged sweep's owed verdict and an owed Welcome
/// fan-out are resolved before this is consulted (ADR-0045 C-67).
///
/// Pure: the caller supplies the clock and the epoch.
public struct SweepRegistrationBackoff: Equatable {
    /// The wait after the first refusal. Chosen, not measured.
    public static let firstDelayMs: Int64 = 30_000
    /// The ceiling the doubling stops at. Chosen, not measured.
    public static let maxDelayMs: Int64 = 600_000

    /// Consecutive refusals at ``epoch``, at least 1.
    public let failures: Int
    /// The group epoch the last refusal happened at.
    public let epoch: UInt64
    /// When the last refusal happened (caller's clock, ms).
    public let failedAtMs: Int64

    public init(failures: Int, epoch: UInt64, failedAtMs: Int64) {
        self.failures = max(1, failures)
        self.epoch = epoch
        self.failedAtMs = failedAtMs
    }

    /// The wait after `failures` consecutive refusals: 30 s, 60 s, 120 s, ...
    /// capped at 10 minutes. `0` for no refusal.
    public static func delayMs(failures: Int) -> Int64 {
        guard failures > 0 else { return 0 }
        var delay = firstDelayMs
        var step = 1
        while step < failures && delay < maxDelayMs {
            delay = min(delay * 2, maxDelayMs)
            step += 1
        }
        return delay
    }

    /// The earliest time a new sweep may register again.
    public var notBeforeMs: Int64 { failedAtMs + Self.delayMs(failures: failures) }

    /// Whether a new sweep may register at `epoch` at `nowMs`. `true` once the
    /// wait ran out, when the epoch moved since the refusal, or when the clock
    /// went backwards past the refusal (a wait measured on a clock that moved
    /// back is not a wait anyone can reason about).
    public func allows(nowMs: Int64, epoch: UInt64) -> Bool {
        epoch != self.epoch || nowMs >= notBeforeMs || nowMs < failedAtMs
    }

    /// The value after a refusal at `epoch`: one more consecutive refusal when
    /// `previous` is at the same epoch, else the first.
    public static func afterRefusal(
        _ previous: SweepRegistrationBackoff?,
        nowMs: Int64,
        epoch: UInt64
    ) -> SweepRegistrationBackoff {
        let failures = previous.map { $0.epoch == epoch ? $0.failures + 1 : 1 } ?? 1
        return SweepRegistrationBackoff(failures: failures, epoch: epoch, failedAtMs: nowMs)
    }

    /// Whether `error` is a refusal the backoff applies to: `400`, `401` or
    /// `403` on the registration. A `409` is a lost append race (another
    /// committer registered first) and a network failure is transient; both
    /// keep the ordinary cadence.
    public static func appliesTo(_ error: Error) -> Bool {
        switch error as? DsStatementError {
        case .malformed?, .unauthorized?, .rankShortfall?: return true
        default: return false
        }
    }
}
