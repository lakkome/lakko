// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Durable record of a channel PublisherGroup commit (G-50; ADR-0090
 * section 3: every PublisherGroup commit is STAGED and merged only when the
 * Delivery Service took it).
 *
 * A PublisherGroup commit (a rotation, an approve, a publisher removal, an
 * admission or retirement of devices) is staged at epoch N in the DEVICE
 * identity's provider: the group stays at N holding the commit as pending.
 * Before anything leaves the device the commit is recorded: the entry
 * (commit, Welcome, the epoch-N access seed that proves the commit request)
 * is written to the channel log, then the provider snapshot carrying the
 * staged commit is written, both durably, fail-closed (a refused write
 * abandons the stage and sends nothing). Then the steps run:
 *
 *   - **journal**: `POST /ds/group/{hex}/commit` registers N+1 and appends
 *     the commit in one request. `201`, or the identical re-send's `200`:
 *     the stage is MERGED, the provider written, the step done. `409` (another
 *     owner device took the epoch) or a definitive refusal: the stage is
 *     ABANDONED, the entry is finished as lost and nothing else runs (the
 *     caller reads the journal and applies the winner). Anything else: the
 *     entry stays owed and is re-sent later; nothing after it runs.
 *   - **welcome**: the Welcome to the added account's mailbox.
 *   - **reseal**: the merged seed to every subscriber (C-90 proposed: a seed
 *     reaches subscribers only from a merged commit; the `journalLost`
 *     reseal of the eager design is gone).
 *   - **appoint** (approve): the newcomer's co-publisher key.
 *   - **handover** (admit): the added account's hand-over records are reset
 *     so the publisher pass sends it the read key, the admission seeds and
 *     the newest epoch seeds again (records are per device, ADR-0090 s. 6).
 *
 * A restart replays each entry against the restored group:
 *
 *   - group at or past the target epoch: the stage merged; the remaining
 *     steps run;
 *   - group one below the target and holding a staged commit: the journal
 *     step is sent again (the DS recognises an identical re-send);
 *   - anything else (the snapshot never carried the stage, or it was
 *     abandoned): the entry is dropped, nothing of it was sent.
 *
 * And a group that holds a staged commit no entry names is abandoned: the
 * entry is always written before the commit leaves, so nothing was sent.
 */

import { nextSweepBackoffMs, SWEEP_BACKOFF_INITIAL_MS } from "../chat/sweep-backoff";

/** Which commit an entry records. `removal` (ADR-0087 phase B): the owner
 *  removed a publisher. `admit` (ADR-0090 section 2): devices of an account
 *  that already publishes were added and/or retired. `entry` (ADR-0064
 *  section 7, amendment of 2026-11-14): this owner device entered the group
 *  from a GroupInfo another owner device saved, by an external commit. */
export type PublisherCommitKind = "approve" | "rotation" | "removal" | "admit" | "entry";

/** The steps of an entry, in the order they run. */
export type PublisherCommitStep = "journal" | "welcome" | "reseal" | "appoint" | "handover";

/** One recorded PublisherGroup commit. */
export interface PublisherCommitEntry {
  /** Random hex id. */
  readonly id: string;
  readonly kind: PublisherCommitKind;
  /** 32-char hex channel id. */
  readonly channelIdHex: string;
  /** 64-char hex PublisherGroup id (the DS journal). */
  readonly groupIdHex: string;
  /** The MLS epoch the commit moved the group to (decimal `u64`). */
  readonly targetEpoch: string;
  /** The channels-service epoch the reseal binds the envelopes to. */
  readonly serverTargetEpoch: number;
  /** Standard base64 of the staged commit for the journal. */
  readonly commitB64: string | null;
  /** Approve / admit: standard base64 of the Welcome (absent when an
   *  admission only retired devices). */
  readonly welcomeB64: string | null;
  /** Approve: the joiner's device leaf key. Admit: the ACCOUNT key whose
   *  devices were added (the Welcome's mailbox). Standard base64. */
  readonly joinerKeyB64: string | null;
  /** Approve: the joiner's `reply` grant wire. Admit of a co-publisher's
   *  device: the committer-delegation envelope its request carried (ADR-0090
   *  section 2.2). Standard base64. */
  readonly replyGrantB64: string | null;
  /** Approve: the joiner's ACCOUNT key (the leaf key names a device,
   *  ADR-0090), standard base64; `null` on an older entry. */
  readonly joinerAccountKeyB64?: string | null;
  /** ADR-0081: the epoch the commit left (decimal `u64`), whose key proves
   *  the commit request; `null` on an entry written before it was kept. */
  readonly proofEpoch: string | null;
  /** ADR-0081: standard base64 of that epoch's 32-byte access seed. */
  readonly proofSeedB64: string | null;
  /** ADR-0081 / ADR-0090: the DS answered the journal step with 409 or a
   *  definitive refusal; the stage was abandoned and nothing else runs. */
  readonly journalLost: boolean;
  /** The steps that have landed. */
  readonly done: readonly PublisherCommitStep[];
}

/** The steps an entry of `kind` runs, in order. */
export function stepsOf(kind: PublisherCommitKind): readonly PublisherCommitStep[] {
  // ADR-0090: the reseal before the Welcome, so a device that joins finds
  // the channels service already at the epoch the commit moved it to (it
  // learns the channel epoch its group epoch serves from it at landing).
  if (kind === "approve") return ["journal", "reseal", "welcome", "appoint"];
  if (kind === "admit") return ["journal", "reseal", "welcome", "handover"];
  return ["journal", "reseal"];
}

/** The steps still to run, in order. */
export function remainingSteps(entry: PublisherCommitEntry): PublisherCommitStep[] {
  return stepsOf(entry.kind).filter((s) => !entry.done.includes(s));
}

/** The entry with `step` marked done (once). */
export function withStepDone(entry: PublisherCommitEntry, step: PublisherCommitStep): PublisherCommitEntry {
  if (entry.done.includes(step)) return entry;
  return { ...entry, done: [...entry.done, step] };
}

/**
 * What a restart does with an entry, given the epoch the restored group is
 * at (`null`: the group did not load at all) and whether it holds a staged
 * commit:
 *
 * - `"merged"`: at or past the target; the remaining steps run (the journal
 *   step counts as landed: a stage is merged only on the DS's answer);
 * - `"resend"`: one below the target with a staged commit; the journal step
 *   is sent again;
 * - `"discard"`: anything else (the stage never became durable, was
 *   abandoned, or the group is gone): nothing of it was sent.
 */
export function replayVerdict(
  entry: PublisherCommitEntry,
  restoredEpoch: bigint | null,
  hasStagedCommit = false,
): "discard" | "resend" | "merged" {
  if (restoredEpoch === null) return "discard";
  const target = BigInt(entry.targetEpoch);
  if (restoredEpoch >= target) return "merged";
  if (restoredEpoch === target - 1n && hasStagedCommit) return "resend";
  return "discard";
}

/** Whether the reseal of an entry has already landed: the channels service
 *  is at or past the epoch the entry resealed to. */
export function resealLanded(entry: PublisherCommitEntry, serverEpoch: number): boolean {
  return serverEpoch >= entry.serverTargetEpoch;
}

/** A fresh random entry id. */
export function newEntryId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

// ---------------------------------------------------------------------------
// Codec (inside the channel log; see `channel-log-codec.ts`)
// ---------------------------------------------------------------------------

const STEPS: readonly PublisherCommitStep[] = ["journal", "welcome", "reseal", "appoint", "handover"];

function isHex(v: unknown, len: number): v is string {
  return typeof v === "string" && v.length === len && /^[0-9a-f]*$/.test(v);
}

function optString(v: unknown): string | null | undefined {
  if (v === null || v === undefined) return null;
  return typeof v === "string" ? v : undefined;
}

/** Parse one persisted entry; `null` when it is malformed (dropped alone). */
export function parsePublisherCommitEntry(raw: unknown): PublisherCommitEntry | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  if (!isHex(r.id, 32)) return null;
  if (
    r.kind !== "approve" &&
    r.kind !== "rotation" &&
    r.kind !== "removal" &&
    r.kind !== "admit" &&
    r.kind !== "entry"
  ) {
    return null;
  }
  if (!isHex(r.channelIdHex, 32) || !isHex(r.groupIdHex, 64)) return null;
  if (typeof r.targetEpoch !== "string" || !/^\d+$/.test(r.targetEpoch)) return null;
  if (typeof r.serverTargetEpoch !== "number" || !Number.isInteger(r.serverTargetEpoch) || r.serverTargetEpoch < 0) {
    return null;
  }
  const commitB64 = optString(r.commitB64);
  const welcomeB64 = optString(r.welcomeB64);
  const joinerKeyB64 = optString(r.joinerKeyB64);
  const replyGrantB64 = optString(r.replyGrantB64);
  if (commitB64 === undefined || welcomeB64 === undefined || joinerKeyB64 === undefined || replyGrantB64 === undefined) {
    return null;
  }
  if (r.kind === "approve" && (commitB64 === null || welcomeB64 === null || joinerKeyB64 === null)) return null;
  const joinerAccountKeyB64 = optString(r.joinerAccountKeyB64);
  if (joinerAccountKeyB64 === undefined) return null;
  const proofEpoch = optString(r.proofEpoch);
  const proofSeedB64 = optString(r.proofSeedB64);
  if (proofEpoch === undefined || proofSeedB64 === undefined) return null;
  if (proofEpoch !== null && !/^\d+$/.test(proofEpoch)) return null;
  if (r.journalLost !== undefined && typeof r.journalLost !== "boolean") return null;
  if (!Array.isArray(r.done)) return null;
  const done: PublisherCommitStep[] = [];
  for (const s of r.done) {
    if (!STEPS.includes(s as PublisherCommitStep)) return null;
    if (!done.includes(s as PublisherCommitStep)) done.push(s as PublisherCommitStep);
  }
  return {
    id: r.id,
    kind: r.kind,
    channelIdHex: r.channelIdHex,
    groupIdHex: r.groupIdHex,
    targetEpoch: r.targetEpoch,
    serverTargetEpoch: r.serverTargetEpoch,
    commitB64,
    welcomeB64,
    joinerKeyB64,
    replyGrantB64,
    ...(joinerAccountKeyB64 === null ? {} : { joinerAccountKeyB64 }),
    proofEpoch,
    proofSeedB64,
    journalLost: r.journalLost === true,
    done,
  };
}

// ---------------------------------------------------------------------------
// The runner
// ---------------------------------------------------------------------------

/** What running the steps of one entry needs; injected so the order is
 *  testable without a network. */
export interface PublisherCommitPorts {
  /** ADR-0081 - register the staged N+1 epoch key with the channel's root
   *  AND append `commit` to the group journal, in one request
   *  (`POST .../commit`), proven under the recorded key of the epoch the
   *  commit left (`proof`; `null` on an entry that kept none). A re-send of
   *  the same commit is idempotent on the DS. */
  commitToJournal(
    commit: Uint8Array,
    proof: { readonly epoch: bigint; readonly seed: Uint8Array } | null,
  ): Promise<void>;
  /** ADR-0081: `"lost"` for a 409 or a definitive refusal (nothing of the
   *  commit is or will be on the DS), else `"unknown"`. */
  classifyCommitFailure(err: unknown): "lost" | "unknown";
  /** ADR-0090: the DS took the commit; merge the stage and write the
   *  provider durably. Resolves `false` when there was no stage to merge
   *  (already merged: a replay after the merge landed). */
  merge(): Promise<boolean>;
  /** ADR-0090: the DS refused the commit; abandon the stage (the group stays
   *  at N) and write the provider. */
  abandon(): Promise<void>;
  /** Write the Welcome to the added account's mailbox (`auth`: the joiner's
   *  reply grant, or a co-publisher device's delegation envelope). */
  deliverWelcome(welcome: Uint8Array, joinerKey: Uint8Array, auth: string | null): Promise<void>;
  /** The channels service's current epoch. */
  serverEpoch(): Promise<number>;
  /** Reseal the group's CURRENT (merged) seed to every subscriber and move
   *  the service to `targetEpoch` (stage + commit). */
  reseal(targetEpoch: number): Promise<void>;
  /** Appoint the joiner's co-publisher key. Never fails the entry. */
  appoint(joinerKey: Uint8Array): Promise<void>;
  /** Reset the hand-over records of the account `accountKey`, so the
   *  publisher pass sends its new devices what they need. */
  handover(accountKey: Uint8Array): Promise<void>;
  /** Durably record `entry` (a step landed). */
  save(entry: PublisherCommitEntry): Promise<void>;
  /** The entry is finished: drop it. */
  finish(entry: PublisherCommitEntry): Promise<void>;
  /** ADR-0064 section 7 (amendment of 2026-11-14): save the epoch the staged
   *  commit moves to in the channel's vault head, right before the journal
   *  step. Never stops the commit: a failure is ignored. Not run for an
   *  entry (the entering device saves the head once it has merged). */
  writeVaultHead?(): Promise<unknown>;
}

function b64(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

/** How a run ended: every step landed, the commit was refused (abandoned),
 *  or the journal answer is still owed (the entry stays). */
export type PublisherCommitOutcome = "landed" | "lost" | "owed";

/**
 * Run the remaining steps of `entry` in order, recording each as it lands.
 * Throws the first failure of a step after the journal, leaving the entry
 * (with what did land) for the next run. A journal step the DS did not
 * answer leaves the entry owed and runs nothing after it (`"owed"`): a seed
 * is never resealed from a commit that did not merge.
 */
export async function runPublisherCommit(
  entry: PublisherCommitEntry,
  ports: PublisherCommitPorts,
): Promise<PublisherCommitOutcome> {
  let current = entry;
  for (const step of remainingSteps(current)) {
    switch (step) {
      case "journal": {
        if (current.commitB64 === null) break;
        const proof =
          current.proofEpoch !== null && current.proofSeedB64 !== null
            ? { epoch: BigInt(current.proofEpoch), seed: b64(current.proofSeedB64) }
            : null;
        if (current.kind !== "entry" && ports.writeVaultHead !== undefined) {
          try {
            await ports.writeVaultHead();
          } catch {
            /* the head only helps a device that lost everything; the commit goes on */
          }
        }
        try {
          await ports.commitToJournal(b64(current.commitB64), proof);
        } catch (err) {
          if (ports.classifyCommitFailure(err) === "lost") {
            // Another owner device took the epoch (or the DS refused the
            // commit for good): the stage goes, nothing else of it runs.
            // eslint-disable-next-line no-console
            console.info(`ds: PublisherGroup ${current.kind} commit not taken by the journal (lost)`);
            await ports.abandon();
            const lost = { ...current, journalLost: true };
            await ports.finish(lost);
            return "lost";
          }
          // eslint-disable-next-line no-console
          console.info(`ds: PublisherGroup ${current.kind} commit unanswered; owed`);
          return "owed";
        }
        // eslint-disable-next-line no-console
        console.info(`ds: PublisherGroup ${current.kind} commit taken`);
        await ports.merge();
        break;
      }
      case "welcome":
        if (current.welcomeB64 !== null && current.joinerKeyB64 !== null) {
          await ports.deliverWelcome(b64(current.welcomeB64), b64(current.joinerKeyB64), current.replyGrantB64);
        }
        break;
      case "reseal":
        if (!resealLanded(current, await ports.serverEpoch())) await ports.reseal(current.serverTargetEpoch);
        break;
      case "appoint":
        if (current.joinerKeyB64 !== null) {
          try {
            await ports.appoint(b64(current.joinerKeyB64));
          } catch (err) {
            // An appointment is retried by the next co-publisher open; it
            // never holds the entry.
            // eslint-disable-next-line no-console
            console.warn("co-publisher appointment after an approve failed", err);
          }
        }
        break;
      case "handover":
        if (current.joinerKeyB64 !== null) await ports.handover(b64(current.joinerKeyB64));
        break;
    }
    current = withStepDone(current, step);
    await ports.save(current);
  }
  await ports.finish(current);
  return "landed";
}

// ---------------------------------------------------------------------------
// Retry of recorded commits (2026-11-20)
// ---------------------------------------------------------------------------

/**
 * What a failed step of a recorded PublisherGroup commit means for its next
 * try (seen live 2026-11-20 on iOS: a reseal refused `401
 * owner-proof-required` was "retried" once per entry, back to back, every
 * pass, while every pass recorded one more rotation).
 *
 * - `"refused"`: the channels service answered `401 owner-proof-required`
 *   (this device's publisher proof is not accepted for the channel) or `410
 *   channel-deleted`; the same request gets the same answer.
 * - `"transient"`: anything else (network, a 5xx, an unanswered journal
 *   step); it may succeed later.
 */
export type PublisherCommitFailure = "refused" | "transient";

/** The channels-service codes that are refusals: `PUBLISHER_REFUSED` and
 *  `CHANNEL_DELETED` of `./api`, repeated here so this module stays free of
 *  the client (a test pins them equal). */
export const PUBLISHER_COMMIT_REFUSAL_CODES: readonly string[] = ["owner-proof-required", "channel-deleted"];

/** The kind of a step failure. Looks through `cause` chains. Pure. */
export function publisherCommitFailureOf(err: unknown): PublisherCommitFailure {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && typeof current === "object" && current !== null; depth += 1) {
    const r = current as { code?: unknown; cause?: unknown };
    if (typeof r.code === "string" && PUBLISHER_COMMIT_REFUSAL_CODES.includes(r.code)) return "refused";
    current = r.cause;
  }
  return "transient";
}

/** A label for a log line: the service's status and code, or the error's
 *  name. Never the message (a URL in it names the channel). Pure. */
export function publisherCommitFailureLabel(err: unknown): string {
  if (typeof err === "object" && err !== null) {
    const r = err as { status?: unknown; code?: unknown; name?: unknown };
    if (typeof r.status === "number" && typeof r.code === "string") return `${r.status} ${r.code}`;
    if (typeof r.name === "string") return r.name;
  }
  return "error";
}

/** The wait after `failures` consecutive transient failures: the schedule
 *  of the owed DS registrations (`../chat/sweep-backoff`), 30 s doubling to
 *  10 minutes. Pure. */
export function publisherCommitBackoffMs(failures: number): number {
  if (failures <= 0) return 0;
  let delay = SWEEP_BACKOFF_INITIAL_MS;
  for (let i = 1; i < failures; i += 1) delay = nextSweepBackoffMs(delay);
  return delay;
}

/** One channel's hold after a failure. */
export interface PublisherCommitHold {
  /** A refusal: no try before the holds are reset (the next start). */
  readonly refused: boolean;
  /** Consecutive failures, at least 1. */
  readonly failures: number;
  /** When the last failure happened (ms). */
  readonly failedAtMs: number;
}

/** Whether a hold lets the channel be tried at `nowMs`. A clock that went
 *  back past the failure allows it. Pure. */
export function holdAllows(hold: PublisherCommitHold, nowMs: number): boolean {
  if (hold.refused) return false;
  return nowMs >= hold.failedAtMs + publisherCommitBackoffMs(hold.failures) || nowMs < hold.failedAtMs;
}

/**
 * The per-channel holds of the recorded PublisherGroup commits, in memory
 * (the tab's life). A refusal holds the channel until {@link reset} (a reload,
 * a new account): the entries are KEPT, since their commit merged and is on
 * the journal, so the reseal (and an approve's Welcome) are still owed for
 * the channel to be consistent, and a refusal that clears (an owner handover
 * that landed, a fixed clock) lets the next start finish them. A transient
 * failure waits {@link publisherCommitBackoffMs}. While a channel holds an
 * owed entry no new rotation of it is staged ({@link hasOwedPublisherCommit}).
 */
export class PublisherCommitRetry {
  private readonly holds = new Map<string, PublisherCommitHold>();

  constructor(private readonly clock: () => number = () => Date.now()) {}

  hold(channelIdHex: string): PublisherCommitHold | undefined {
    return this.holds.get(channelIdHex.toLowerCase());
  }

  mayAttempt(channelIdHex: string): boolean {
    const hold = this.hold(channelIdHex);
    return hold === undefined || holdAllows(hold, this.clock());
  }

  noteFailure(channelIdHex: string, err: unknown): PublisherCommitHold {
    const key = channelIdHex.toLowerCase();
    const previous = this.holds.get(key);
    const next: PublisherCommitHold = {
      refused: publisherCommitFailureOf(err) === "refused",
      failures: (previous?.failures ?? 0) + 1,
      failedAtMs: this.clock(),
    };
    this.holds.set(key, next);
    return next;
  }

  noteSuccess(channelIdHex: string): void {
    this.holds.delete(channelIdHex.toLowerCase());
  }

  reset(): void {
    this.holds.clear();
  }

  /**
   * Run `entries` in their recorded order through `run`. An entry that owes
   * its journal step ({@link owesJournal}) always runs; any other is skipped
   * while its channel's hold does not allow a try. A channel stops at its
   * first failure in this call (a thrown error, or `run` resolving `"owed"`),
   * so a pile of entries of one channel costs one request, not one per
   * entry. A failure holds the channel only when it came from a step after
   * the journal (`reread` gives the entry as recorded after the run). Logs
   * once per failure, with no identifier.
   */
  async replay(
    entries: readonly PublisherCommitEntry[],
    run: (entry: PublisherCommitEntry) => Promise<PublisherCommitOutcome | "skipped">,
    reread: (id: string) => PublisherCommitEntry | undefined = () => undefined,
  ): Promise<void> {
    const stopped = new Set<string>();
    for (const entry of entries) {
      const channel = entry.channelIdHex.toLowerCase();
      if (stopped.has(channel)) continue;
      const journal = owesJournal(entry);
      if (!journal && !this.mayAttempt(channel)) {
        stopped.add(channel);
        continue;
      }
      let failure: unknown = null;
      try {
        const outcome = await run(entry);
        if (outcome === "owed") failure = new Error("journal step unanswered");
      } catch (err) {
        failure = err;
      }
      if (failure === null) {
        if (!journal) this.noteSuccess(channel);
        continue;
      }
      stopped.add(channel);
      if (owesJournal(reread(entry.id) ?? entry)) {
        // eslint-disable-next-line no-console
        console.warn(
          `G-50: a recorded PublisherGroup commit is still unanswered by the journal (${publisherCommitFailureLabel(failure)}); sent again next pass`,
        );
        continue;
      }
      const hold = this.noteFailure(channel, failure);
      // eslint-disable-next-line no-console
      console.warn(
        hold.refused
          ? `G-50: a recorded PublisherGroup commit was refused by the channels service (${publisherCommitFailureLabel(failure)}); kept, not tried again until the next start`
          : `G-50: a recorded PublisherGroup commit did not finish (${publisherCommitFailureLabel(failure)}); tried again in ${publisherCommitBackoffMs(hold.failures) / 1000} s`,
      );
    }
  }
}

/** Whether `entry` still owes its DS journal step (a staged commit whose
 *  verdict the DS has not given). That step is never held: a staged commit
 *  freezes the group's membership operations, so it is sent again every
 *  pass (ADR-0090 / ADR-0081); the pass cadence bounds it. Pure. */
export function owesJournal(entry: PublisherCommitEntry): boolean {
  return !entry.journalLost && remainingSteps(entry)[0] === "journal";
}

/** The tab's holds. */
export const publisherCommitRetry = new PublisherCommitRetry();

/** Whether `entries` hold one of `channelIdHex` that still owes its journal
 *  step or its reseal: no new rotation of the channel is staged then (its
 *  reseal, when it lands, seals a seed no subscriber holds yet to the
 *  subscribers listed at that moment, which is what a rotation is for). An
 *  entry owing only a Welcome, an appointment or a hand-over does not hold
 *  it. Pure. */
export function hasOwedPublisherCommit(channelIdHex: string, entries: readonly PublisherCommitEntry[]): boolean {
  const channel = channelIdHex.toLowerCase();
  return entries.some(
    (e) =>
      e.channelIdHex.toLowerCase() === channel &&
      !e.journalLost &&
      remainingSteps(e).some((step) => step === "journal" || step === "reseal"),
  );
}
