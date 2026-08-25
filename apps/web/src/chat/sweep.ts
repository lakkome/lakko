// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 S4a/S4b — the committer sweep: stage, publish, read the journal
 * verdict, then merge or abandon.
 *
 * ## What this closes
 *
 * G-38 residual 1: two devices build a membership commit against the same
 * epoch N, the DS journal orders them by arrival, and the loser has *already*
 * eager-merged its own N+1 — so it sits at an epoch nobody else reached,
 * silently and permanently. ADR-0045's answer is that members emit
 * **proposals** (not epoch-advancing, so a lost race costs nothing) and one
 * elected owner-or-admin sweeps them into a single commit.
 *
 * S4a made the committer the **owner only** (`ownCommitterRank() === 0`), so an
 * admin-only group simply waited. **S4b widened it to every candidate**, ordered
 * by the ADR-0045 liveness beacon: this device sweeps when it is a candidate and
 * has heard no fresh beacon from anyone **strictly** better placed — including
 * when it has heard nothing at all, which means nobody better placed is running.
 * The decision itself is not made here: {@link SweepPorts.actingCommitter} hands
 * it in, computed by `chat/committer-beacon.ts` over the core's own fold.
 *
 * ## Why the sweep still stages rather than eager-merging
 *
 * Because a committer that *can* lose a race must not merge before the journal
 * has ordered its commit, and with S4b that is no longer hypothetical: each
 * candidate folds over its own poll-timed view and a beacon is stale by up to
 * one interval even when its sender is healthy, so two candidates can both
 * reach `act`. **The beacon removes the wait, not the race** — the stand-down
 * re-read below and the staged-commit rollback are as required after S4b as
 * before it, and reading the acting verdict as a licence to drop either would
 * be a mis-reading. `commitPendingProposals` is exactly `stagePendingProposals`
 * followed at once by `mergeStagedSweep`; building on it would be building the
 * hazard.
 *
 * ## The five obligations, and where each one lives
 *
 * 1. **Reach a verdict; never leave a sweep staged.** A staged sweep is
 *    *durable* (OpenMLS persists `MlsGroupState::PendingCommit`) and freezes
 *    that group's own membership operations across restarts — `is_operational()`
 *    refuses `propose_*` / add / remove / `self_update` while one is pending.
 *    So {@link runCommitterSweep} adjudicates every sweep it stages, and
 *    {@link resolveOwedVerdict} is what a restart runs first: it is wired into
 *    ADR-0040's existing `sweepPendingCommits` seam rather than becoming a
 *    second restart path.
 * 2. **Publish the Welcome only AFTER the verdict.** ADR-0045 calls this
 *    arguably its most important single line: if the commit loses and a Welcome
 *    is already out, a joiner opens into a state nobody holds — a three-party
 *    fork, strictly worse than the two-party one. {@link publishWelcomes} is
 *    reached only on the `merge` branch.
 * 3. **The stand-down re-read.** Immediately before staging, re-read the
 *    journal head; if it has moved, stand down and re-evaluate. A required step
 *    of the algorithm, not a mitigation.
 * 4. **durably record -> publish (or re-publish) -> read the verdict -> merge
 *    or abandon.** ADR-0040's outbox and this pair answer *different* questions
 *    about the same commit — the outbox answers *did my commit reach the wire*,
 *    the staged pair answers *did it win the wire order* — and the first must be
 *    `yes` before the second is meaningful. So the outbox entry is written
 *    BEFORE any publish, exactly as it is on the four direct paths.
 * 5. Obligation 5 (the direct-commit paths become propose-or-sweep) lives in
 *    `contacts/client.ts`, which is this module's only production caller.
 *
 * ## The verdict is read by APPLYING, not by peeking at the epoch field
 *
 * The question is "did a **Commit** land below my own commit's DS id". The FFI
 * exposes `epochOfMessage` but no `contentTypeOfMessage` (ADR-0045 open
 * questions flags exactly this), and epoch alone cannot tell a Commit from an
 * Application message or a Proposal — every one of them is framed at the
 * current epoch. Guessing in the permissive direction is not an option:
 * abandoning a sweep that actually WON, after peers applied it, leaves this
 * device at N with the group at N+1 and its own commit un-re-appliable
 * (`CannotDecryptOwnMessage`) — the same brick, in reverse.
 *
 * So the verdict asks a question that needs no discrimination: **apply every
 * row below our own commit, then ask whether our epoch moved.** Only a Commit
 * moves it. Applying is the audited receive path, so nothing new is invented,
 * and it is safe while a sweep is staged: nothing on the receive path consults
 * `is_operational`, and OpenMLS discards a pending commit as part of merging a
 * foreign one.
 *
 * That the rows are applied here rather than by the poll is why the whole
 * region — stage, publish, fetch, verdict, merge/abandon — must hold the
 * per-group MLS lock: it is a read-modify-write over both the group epoch and
 * the journal cursor, and a poll interleaving inside it would make the verdict
 * non-deterministic. The lock is therefore deliberately held across two short
 * DS round-trips, against `group-lock.ts`'s usual advice to keep the network
 * outside; the alternative is not a slower verdict but a wrong one.
 *
 * ## Ports, not wasm
 *
 * Everything here is expressed over injected ports so the whole orchestration
 * is host-testable in the plain `node` vitest environment — no IndexedDB, no
 * wasm, no network. `contacts/client.ts` supplies the thin adapters.
 */

import type { ActingCommitter } from "./committer-beacon";

/** The MLS artifacts a staged sweep produced. Mirrors the wasm `StagedSweep`. */
export interface StagedSweepArtifacts {
  /** The commit wire bytes to publish. The local epoch has NOT advanced. */
  readonly commit: Uint8Array;
  /** The Welcome, present only when the sweep carried at least one add. */
  readonly welcome: Uint8Array | undefined;
  /**
   * The 32-byte Ed25519 signature key of every member this sweep ADDS.
   *
   * One sweep commit can carry several adds under a single Welcome, which is
   * why this is a list where the four direct paths have one destination. Each
   * key derives that member's mailbox pseudonym — the Welcome's delivery
   * address, since a not-yet-member does not poll the group journal.
   */
  readonly addedSignatureKeys: readonly Uint8Array[];
}

/** The MLS operations the sweep drives. Adapted onto the wasm `Group`. */
export interface SweepGroupPort {
  /** The group's `group_id` bytes — the DS journal pseudonym. */
  readonly groupId: Uint8Array;
  /** The current MLS epoch. Read before and after the verdict apply. */
  readonly epoch: bigint;
  /**
   * ADR-0045 committer rank: `0` for the owner, `k + 1` for the k-th admin,
   * `undefined` for a plain member or a group with no role table.
   *
   * `undefined` and `0` are NOT interchangeable and `if (rank)` is wrong in
   * both directions — `0` is the owner, the one device that always sweeps.
   */
  ownCommitterRank(): number | undefined;
  /** How many authorised proposals are waiting. `0` short-circuits the sweep
   *  before it touches the network. */
  pendingProposalCount(): number;
  /** `true` when a staged sweep is still awaiting its journal verdict. */
  hasStagedSweep(): boolean;
  /** Build the sweep commit and STOP before merging. `null` when nothing
   *  authorised is pending (deliberately not an empty commit, which would
   *  advance the epoch and race other committers for no effect). */
  stagePendingProposals(): StagedSweepArtifacts | null;
  /** Won — land at the new epoch, the one the peers reached. */
  mergeStagedSweep(): void;
  /** Lost — discard the staged commit and stay put, free to apply the winner.
   *  Per-group: it never reaches a sibling group, unlike a whole-provider
   *  `restoreStorage`. */
  abandonStagedSweep(): void;
  /**
   * Apply one journal row through the audited role-verifying receive path AND
   * surface whatever it produced.
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
   * port is handed the whole {@link SweepJournalRow} rather than just its
   * payload: the shared event constructor in `chat/client.ts` needs the DS row
   * id and the DS row `createdAt`, and every envelope kind must be dispatched by
   * that ONE ladder — a second copy of it here is how the poll path and the
   * sweep path would drift. The alternative (returning an opaque value for this
   * module to collect) was rejected because it would either weaken the event
   * type to `unknown` at the app boundary or make every `SweepOutcome` variant
   * generic, for no gain: this module has nothing to decide about an event.
   *
   * Asynchronous because that shared constructor is (it resolves the wasm
   * module to parse an application envelope).
   */
  applyIncoming(row: SweepJournalRow): Promise<void>;
}

/** One row of the DS group journal. */
export interface SweepJournalRow {
  readonly id: number;
  readonly payload: Uint8Array;
  /** The DS-assigned row timestamp, verbatim as the DS returned it.
   *
   *  Carried because an applied row becomes a timeline event and the shared
   *  event constructor requires it — and because it is the one clock every
   *  member reads identically for the identical row. */
  readonly createdAt: string;
}

/** The DS operations the sweep drives. */
export interface SweepJournalPort {
  /** Rows strictly after `after`, in ascending id order. */
  fetch(after: number): Promise<SweepJournalRow[]>;
  /** Publish to this group's journal; resolves to the DS-assigned id. */
  publishGroup(bytes: Uint8Array): Promise<number>;
  /** Publish to a member's mailbox pseudonym; resolves to the DS id. */
  publishMailbox(pseudonym: Uint8Array, bytes: Uint8Array): Promise<number>;
}

/** What the sweep durably records, per ADR-0040. */
export interface SweepDurableEntry {
  readonly id: string;
  readonly commitB64: string;
  readonly welcomeEnvelopeB64: string | undefined;
  readonly welcomeMailboxesB64: readonly string[];
}

/** The ADR-0040 outbox operations the sweep drives. */
export interface SweepOutboxPort {
  /** Mint a fresh entry id. */
  newEntryId(): string;
  /**
   * Durably record the entry AND the current MLS snapshot in one atomic write,
   * BEFORE any publish. A throw ABORTS the sweep fail-closed.
   */
  record(entry: SweepDurableEntry): Promise<void>;
  /** The un-acked verdict-owed entry for this group, or `null`. */
  findOwed(): Promise<SweepDurableEntry | null>;
  /** Mark the entry acked once every artifact is on the wire. */
  ack(id: string, commitMessageId: number, welcomeMessageId?: number): Promise<void>;
  /** Delete the entry. Used on a LOSS: a commit that lost the order must never
   *  be re-published by the restart sweep. */
  discard(id: string): Promise<void>;
}

/** The journal cursor, as the read/write pair `ChatClient.pollMessages` uses. */
export interface SweepCursorRef {
  read(): number;
  advance(to: number): void;
}

/** Everything the sweep needs. */
export interface SweepPorts {
  readonly group: SweepGroupPort;
  /**
   * ADR-0045 S4b — is this device the acting committer right now?
   *
   * Called with this device's CURRENT rank (never `undefined` — the caller has
   * already established it is a candidate) and answered by the core's
   * `actingCommitterVerdict` fold over the beacons this device has observed.
   * Asynchronous because the fold needs wasm and the observation store; it is
   * awaited before anything is staged, so a slow answer costs one attempt and
   * nothing else.
   *
   * A throw is treated as `defer` — fail-closed, because the alternative is a
   * device that sweeps on no evidence about who else is running.
   */
  actingCommitter(ownRank: number): Promise<ActingCommitter>;
  readonly journal: SweepJournalPort;
  readonly outbox: SweepOutboxPort;
  readonly cursor: SweepCursorRef;
  /** Derive a member's mailbox pseudonym from their 32-byte signature key. */
  welcomeMailbox(signatureKey: Uint8Array): Uint8Array;
  /** Wrap the raw Welcome in the stranger-inbox envelope the recipient's
   *  mailbox poll expects (`wrapEnvelope(KIND_WELCOME, welcome)`). */
  wrapWelcome(welcome: Uint8Array): Uint8Array;
  /** Echo-suppress a row this device published, so its own next poll drops it
   *  instead of hitting `CannotDecryptOwnMessage`. */
  markOwnPublished(messageId: number): void;
  /** Best-effort MLS snapshot persist after a merge / abandon. */
  persist(): Promise<void>;
  /** base64 helpers, injected so this module stays free of app imports. */
  readonly encodeB64: (bytes: Uint8Array) => string;
  readonly decodeB64: (b64: string) => Uint8Array;
}

/** What one sweep attempt did. */
export type SweepOutcome =
  /** This device is not in the committer set at all (a plain member, or a
   *  group with no role table). Nothing happened, and nothing ever will on
   *  this device — distinct from `deferred`, which re-evaluates. */
  | { readonly kind: "not-committer"; readonly rank: number | undefined }
  /** ADR-0045 S4b — this device IS a candidate, but a strictly better-placed
   *  one beaconed inside the freshness window. Stood down without staging;
   *  the next poll re-evaluates, and if that candidate goes quiet its beacon
   *  ages out of the window and this device takes over on its own. */
  | {
      readonly kind: "deferred";
      readonly rank: number;
      readonly toRank: number | undefined;
    }
  /** Nothing authorised was pending and nothing was staged. */
  | { readonly kind: "nothing-pending" }
  /** Obligation 3 — the journal moved since our cursor, so a commit may
   *  already be in flight. Stood down without staging; the poll drains the
   *  rows and the next attempt re-evaluates. */
  | { readonly kind: "stood-down"; readonly unappliedRows: number }
  /** Won the journal order: merged, and the Welcome (if any) published. */
  | {
      readonly kind: "committed";
      readonly commitMessageId: number;
      readonly welcomeMessageIds: readonly number[];
      readonly addedCount: number;
    }
  /** Lost the journal order: abandoned, no Welcome published, the proposals
   *  survive for the winner's next sweep. */
  | { readonly kind: "lost"; readonly commitMessageId: number }
  /** A staged sweep was found with no durable record of its commit, so the
   *  commit provably never reached the wire (ADR-0040 records before it
   *  publishes). Abandoned, which also unwedges the frozen membership ops. */
  | { readonly kind: "abandoned-orphan" }
  /** There was no verdict owed. Only ever returned by
   *  {@link resolveOwedVerdict}. */
  | { readonly kind: "no-verdict-owed" };

/**
 * The journal order could not be read, so the verdict is genuinely unknown.
 *
 * The sweep is left STAGED and its outbox entry un-acked on purpose. Both
 * alternatives are worse: merging on an unread verdict is the eager shape that
 * strands the loser, and abandoning would discard a commit that is already on
 * the wire and may have won. The next sweep attempt re-adjudicates through
 * {@link resolveOwedVerdict}, and the sweep runs at the tail of every poll, so
 * the retry is continuous. The cost is that this group's own membership
 * operations stay frozen until it succeeds — a temporary freeze that
 * self-heals, which is strictly better than either wrong answer.
 */
export class SweepVerdictUnavailable extends Error {
  override name = "SweepVerdictUnavailable";
  constructor(cause: unknown) {
    super(`ADR-0045: could not read the journal verdict — sweep left staged: ${String(cause)}`);
  }
}

/**
 * The pure verdict rule: did anything applied below our own commit advance the
 * group's epoch?
 *
 * Only a Commit advances an epoch. An Application message drains a ratchet, a
 * Proposal queues, a non-MLS envelope throws and is skipped — none of them
 * move it. So an unchanged epoch means our commit is first in the order and we
 * WON; a moved epoch means a foreign commit was ordered ahead of ours and we
 * LOST.
 */
export function sweepVerdict(epochBefore: bigint, epochAfter: bigint): "won" | "lost" {
  return epochAfter === epochBefore ? "won" : "lost";
}

/**
 * Obligation 3, as a pure rule: may this device stage a sweep right now?
 *
 * A row this device has not applied might be a Commit, and without a
 * `contentTypeOfMessage` peek there is no way to tell. Standing down is free —
 * the poll drains the rows and the next attempt re-evaluates — whereas staging
 * on top of an unapplied commit publishes a commit peers will drop as
 * wrong-epoch. So the rule errs deliberately in the conservative direction:
 * ANY unapplied row stands the sweep down, not only a Commit.
 *
 * This narrows the window; it does not close it. A commit can still land
 * between this check and our publish, which is exactly what the verdict step
 * repairs.
 */
export function shouldStandDown(unappliedRowCount: number): boolean {
  return unappliedRowCount > 0;
}

/**
 * Run one committer sweep. Callers MUST hold the per-group MLS lock.
 *
 * Short-circuits without touching the network when this device is not a
 * candidate, or when nothing is pending and no verdict is owed — so wiring
 * this into the tail of every poll costs one synchronous check in the common
 * case.
 *
 * @throws {@link SweepVerdictUnavailable} when the journal order could not be
 *   read. The sweep is left staged for the next attempt.
 */
export async function runCommitterSweep(ports: SweepPorts): Promise<SweepOutcome> {
  const { group } = ports;

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
  if (group.hasStagedSweep()) return resolveOwedVerdict(ports);

  // `undefined` is "not in the committer set at all" — a plain member or a
  // group with no role table. `0` is the OWNER, so `if (rank)` would be wrong
  // in both directions.
  const rank = group.ownCommitterRank();
  if (rank === undefined) return { kind: "not-committer", rank };

  if (group.pendingProposalCount() === 0) return { kind: "nothing-pending" };

  // ADR-0045 S4b — is anyone strictly better placed announcing itself? Asked
  // only once there is something to sweep, so a quiet group never pays for it.
  // A throw is a defer: sweeping on an unreadable view of who else is running
  // is the failure this whole election exists to avoid.
  let acting: ActingCommitter;
  try {
    acting = await ports.actingCommitter(rank);
  } catch {
    acting = { kind: "defer", toRank: undefined };
  }
  if (acting.kind !== "act") {
    return {
      kind: "deferred",
      rank,
      toRank: acting.kind === "defer" ? acting.toRank : undefined,
    };
  }

  // Obligation 3 — the stand-down re-read, immediately before creating the
  // commit.
  const cursorBefore = ports.cursor.read();
  let ahead: SweepJournalRow[];
  try {
    ahead = await ports.journal.fetch(cursorBefore);
  } catch (err) {
    // Nothing has been staged yet, so a failed re-read costs only this
    // attempt. Treat an unreadable head as "the journal may have moved".
    throw new SweepVerdictUnavailable(err);
  }
  if (shouldStandDown(ahead.length)) {
    return { kind: "stood-down", unappliedRows: ahead.length };
  }

  const staged = group.stagePendingProposals();
  if (staged === null) return { kind: "nothing-pending" };

  return publishAndAdjudicate(ports, staged, cursorBefore);
}

/**
 * Obligation 1, restart half — finish a verdict this device owes.
 *
 * A staged commit is persisted by OpenMLS and read back by `Group.load`, so it
 * outlives the page. A client that finds one after a reload still owes a
 * verdict on a commit it may already have published, and until it reaches one
 * the group's membership is frozen. This is wired into ADR-0040's existing
 * restart sweep rather than being a second restart path.
 *
 * The re-publish is idempotent by the same argument the ADR-0040 sweep already
 * rests on: peers that applied the original drop the duplicate as wrong-epoch,
 * and peers that never saw it apply it now. The verdict is then read exactly as
 * it would have been the first time — including the case where our ORIGINAL
 * commit sits in the journal below the re-published one, which applies as
 * `CannotDecryptOwnMessage`, is skipped, and correctly does not move our epoch.
 */
export async function resolveOwedVerdict(ports: SweepPorts): Promise<SweepOutcome> {
  const { group } = ports;
  if (!group.hasStagedSweep()) return { kind: "no-verdict-owed" };

  const owed = await ports.outbox.findOwed();
  if (owed === null) {
    // No durable record of the commit. ADR-0040 writes the record BEFORE the
    // publish and does so atomically with the snapshot, so a snapshot carrying
    // a staged sweep and no entry means the write never completed and the
    // commit never reached the wire. Abandoning is therefore correct, and it is
    // also what unwedges the frozen membership operations — the worse failure
    // of the two.
    group.abandonStagedSweep();
    await ports.persist();
    return { kind: "abandoned-orphan" };
  }

  return publishAndAdjudicate(
    ports,
    {
      commit: ports.decodeB64(owed.commitB64),
      welcome:
        owed.welcomeEnvelopeB64 === undefined
          ? undefined
          : ports.decodeB64(owed.welcomeEnvelopeB64),
      // Already wrapped + already addressed when the entry was written.
      addedSignatureKeys: [],
    },
    ports.cursor.read(),
    owed,
  );
}

/**
 * The shared tail of both entry points: durably record (first time only),
 * publish the commit, read the verdict, then merge or abandon.
 *
 * `owed` is supplied on the restart path, where the entry already exists and
 * its Welcome is already wrapped and addressed.
 */
async function publishAndAdjudicate(
  ports: SweepPorts,
  staged: StagedSweepArtifacts,
  cursorBefore: number,
  owed?: SweepDurableEntry,
): Promise<SweepOutcome> {
  const { group } = ports;

  let entry: SweepDurableEntry;
  if (owed === undefined) {
    // Obligation 4 — durably record BOTH artifacts before ANY publish. The
    // Welcome is wrapped and addressed HERE, so the bytes the restart sweep
    // would publish are the very bytes computed now; nothing can drift.
    const mailboxes = staged.addedSignatureKeys.map((k) =>
      ports.encodeB64(ports.welcomeMailbox(k)),
    );
    const wrapped =
      staged.welcome === undefined ? undefined : ports.wrapWelcome(staged.welcome);
    entry = {
      id: ports.outbox.newEntryId(),
      commitB64: ports.encodeB64(staged.commit),
      welcomeEnvelopeB64: wrapped === undefined ? undefined : ports.encodeB64(wrapped),
      welcomeMailboxesB64: mailboxes,
    };
    try {
      await ports.outbox.record(entry);
    } catch (err) {
      // FAIL-CLOSED. Nothing was published, so the group must look as though
      // the sweep never happened. Abandoning is the per-group rollback — it
      // touches this group only, unlike ADR-0040's whole-provider
      // `restoreStorage`, which would roll sibling groups back past commits
      // they have already published.
      group.abandonStagedSweep();
      throw err;
    }
  } else {
    entry = owed;
  }

  const commitMessageId = await ports.journal.publishGroup(
    ports.decodeB64(entry.commitB64),
  );
  ports.markOwnPublished(commitMessageId);

  const verdict = await adjudicate(ports, cursorBefore, commitMessageId);

  if (verdict === "lost") {
    group.abandonStagedSweep();
    await ports.persist();
    // A commit that lost must never be re-published: the restart sweep would
    // re-adjudicate a commit whose proposals the winner has already swept.
    // Discarding is safe because abandoning kept the proposals pending, so
    // nothing the user asked for is lost.
    await ports.outbox.discard(entry.id);
    return { kind: "lost", commitMessageId };
  }

  group.mergeStagedSweep();
  await ports.persist();

  // Obligation 2 — the Welcome is published HERE and nowhere earlier. Publishing
  // it before the verdict would let a joiner open into a state no other member
  // holds if the commit then lost: a three-party fork, strictly worse than the
  // two-party one this whole mechanism exists to close.
  const welcomeMessageIds = await publishWelcomes(ports, entry);
  await ports.outbox.ack(entry.id, commitMessageId, welcomeMessageIds[0]);

  return {
    kind: "committed",
    commitMessageId,
    welcomeMessageIds,
    addedCount: entry.welcomeMailboxesB64.length,
  };
}

/**
 * Apply every row below our own commit, then read the epoch.
 *
 * The cursor advances only to the last row actually applied — never past our
 * own commit, which the poll re-fetches and drops through the echo filter, and
 * never past a row this run did not consume.
 */
async function adjudicate(
  ports: SweepPorts,
  cursorBefore: number,
  ownCommitMessageId: number,
): Promise<"won" | "lost"> {
  const { group } = ports;
  const epochBefore = group.epoch;

  let rows: SweepJournalRow[];
  try {
    rows = await ports.journal.fetch(cursorBefore);
  } catch (err) {
    throw new SweepVerdictUnavailable(err);
  }

  let lastApplied = cursorBefore;
  for (const row of rows) {
    // Our own commit, and everything the DS ordered after it, is not part of
    // the question. Rows above it are left for the poll.
    if (row.id >= ownCommitMessageId) break;
    try {
      // The port applies AND surfaces — see {@link SweepGroupPort.applyIncoming}.
      // These rows are the only ones the poll will never see again, because the
      // cursor advance below moves past them.
      await group.applyIncoming(row);
    } catch {
      // Bad frame, wrong epoch, a non-MLS Lakko envelope (rendezvous /
      // attribution), a role-refused commit, or one of our own rows. Skipped,
      // exactly as the poll loop skips it — and none of those advance the
      // epoch, which is the only thing the verdict reads. A skipped row was
      // never decrypted, so it surfaces nothing.
    }
    lastApplied = row.id;
  }
  if (lastApplied !== cursorBefore) ports.cursor.advance(lastApplied);

  return sweepVerdict(epochBefore, group.epoch);
}

/**
 * Fan the (already wrapped) Welcome out to every added member's mailbox.
 *
 * A newly added member is not yet in the group and does not poll its journal,
 * so the Welcome goes to their stranger-inbox pseudonym — the same destination
 * the direct add path uses. One sweep commit can carry several adds under one
 * Welcome, which is why this is a fan-out where the direct path has a single
 * publish.
 *
 * A failure here leaves the entry un-acked, so the restart sweep re-publishes
 * and re-adjudicates. That re-run reaches the merge branch again (our commit is
 * skipped as our own, the epoch does not move, so it reports `won`) and retries
 * the fan-out — which is why this can be allowed to throw rather than being
 * swallowed.
 */
async function publishWelcomes(
  ports: SweepPorts,
  entry: SweepDurableEntry,
): Promise<number[]> {
  if (entry.welcomeEnvelopeB64 === undefined) return [];
  const bytes = ports.decodeB64(entry.welcomeEnvelopeB64);
  const ids: number[] = [];
  for (const mailboxB64 of entry.welcomeMailboxesB64) {
    ids.push(await ports.journal.publishMailbox(ports.decodeB64(mailboxB64), bytes));
  }
  return ids;
}
