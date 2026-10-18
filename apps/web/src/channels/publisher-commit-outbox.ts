// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Durable record of a channel PublisherGroup commit (G-50, the ADR-0040
 * treatment the chat groups have).
 *
 * A PublisherGroup commit (a co-publisher approve, a rotation) eager-merges
 * epoch N -> N+1 in the account identity's in-memory provider. Its effects
 * then leave the device in steps: the N+1 epoch key is registered on the
 * Delivery Service, the Add commit is published to the journal and the
 * joiner's Welcome to its mailbox (approve), every subscriber is resealed to
 * the N+1 seed and the channels service moves to the next epoch, and the
 * newcomer's co-publisher key is appointed (approve). Before this record the
 * N+1 provider snapshot was written only afterwards, through a debounced,
 * error-swallowing flush: a reload or a failed write left the server and the
 * subscribers at N+1 while the device reopened the group at N.
 *
 * Persisting N+1 first is not enough on its own (a durable N+1 whose steps
 * never ran is the device-ahead case ADR-0040 describes), so the steps are
 * recorded WITH the snapshot: the entry and the N+1 provider snapshot are
 * written in one channel-log row (one IndexedDB put, so they land together)
 * BEFORE any step runs, fail-closed. Each step marks itself done as it lands;
 * the entry is dropped when none is left. On the next hydration every entry
 * still present is replayed:
 *
 *   - the restored group is below the entry's target epoch: the row with the
 *     entry never landed with its snapshot (a torn write; on web the single
 *     put rules it out, the rule is kept so the three clients agree), and
 *     no step has run, so the entry is dropped;
 *   - else the remaining steps run again. Each is idempotent: the DS journal
 *     drops a duplicate commit, a second Welcome for a group the joiner is
 *     already in is skipped by it, the reseal targets the epoch recorded in
 *     the entry and is done once the service is at or past it, and an
 *     appointment is an upsert.
 *
 * What an entry holds: the commit and Welcome (public MLS artefacts the DS
 * stores anyway), the joiner's key and its reply grant (to address the
 * Welcome again after a restart: the grant is otherwise held in memory
 * only), the epochs, and (ADR-0081) the access seed of the epoch the commit
 * LEFT. The commit merged eagerly, so after a restart the group holds only
 * the N+1 key; the DS takes a commit only under a proof of N (and an
 * identical re-send under any retained key), so without the seed a first
 * send that never reached the DS could never be sent again. With it a
 * replay always learns the answer: appended, the original row, or 409. It
 * lives inside the device-key-encrypted channel log, never in plain storage,
 * and goes with the entry.
 *
 * A journal step the DS answers with 409 (another commit took the epoch) or
 * a definitive refusal is LOST: the step is done, `journalLost` is set, and
 * an approve then delivers no Welcome and makes no appointment (a Welcome for
 * a commit nobody has would put the joiner into a group nobody else is in);
 * the reseal still runs, since the owner's posts are sealed under the merged
 * seed. Any other failure of a rotation's journal step defers it: the reseal
 * runs, and the entry stays for the journal step alone.
 */

/** Which commit an entry records. */
export type PublisherCommitKind = "approve" | "rotation";

/** The steps of an entry, in the order they run. */
export type PublisherCommitStep = "journal" | "welcome" | "reseal" | "appoint";

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
  /** Approve: standard base64 of the Add commit for the journal. */
  readonly commitB64: string | null;
  /** Approve: standard base64 of the joiner's Welcome. */
  readonly welcomeB64: string | null;
  /** Approve: standard base64 of the joiner's signature (= account) key. */
  readonly joinerKeyB64: string | null;
  /** Approve: the joiner's `reply` grant wire, standard base64. */
  readonly replyGrantB64: string | null;
  /** ADR-0081: the epoch the commit left (decimal `u64`), whose key proves
   *  the commit request; `null` on an entry written before it was kept. */
  readonly proofEpoch: string | null;
  /** ADR-0081: standard base64 of that epoch's 32-byte access seed. */
  readonly proofSeedB64: string | null;
  /** ADR-0081: the DS answered the journal step with 409 or a definitive
   *  refusal; the Welcome and the appointment are skipped. */
  readonly journalLost: boolean;
  /** The steps that have landed. */
  readonly done: readonly PublisherCommitStep[];
}

/** The steps an entry of `kind` runs, in order. */
export function stepsOf(kind: PublisherCommitKind): readonly PublisherCommitStep[] {
  return kind === "approve" ? ["journal", "welcome", "reseal", "appoint"] : ["journal", "reseal"];
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
 * at (`null`: the group did not load at all). `"discard"` when the commit's
 * N+1 never became durable (a torn write, or the group is gone), else
 * `"replay"`.
 */
export function replayVerdict(entry: PublisherCommitEntry, restoredEpoch: bigint | null): "discard" | "replay" {
  if (restoredEpoch === null) return "discard";
  return restoredEpoch < BigInt(entry.targetEpoch) ? "discard" : "replay";
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

const STEPS: readonly PublisherCommitStep[] = ["journal", "welcome", "reseal", "appoint"];

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
  if (r.kind !== "approve" && r.kind !== "rotation") return null;
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
  /** ADR-0081 - register the group's CURRENT epoch key (the N+1 one) with
   *  the channel's root AND append `commit` to the group journal, in one
   *  request (`POST .../commit`), proven under the recorded key of the epoch
   *  the commit left (`proof`; `null` on an entry that kept none). A re-send
   *  of the same commit is idempotent on the DS. */
  commitToJournal(
    commit: Uint8Array,
    proof: { readonly epoch: bigint; readonly seed: Uint8Array } | null,
  ): Promise<void>;
  /** ADR-0081: `"lost"` for a 409 or a definitive refusal (nothing of the
   *  commit is or will be on the DS), else `"unknown"`. */
  classifyCommitFailure(err: unknown): "lost" | "unknown";
  /** Write the Welcome to the joiner's mailbox under its reply grant. */
  deliverWelcome(welcome: Uint8Array, joinerKey: Uint8Array, replyGrantB64: string | null): Promise<void>;
  /** The channels service's current epoch. */
  serverEpoch(): Promise<number>;
  /** Reseal the group's CURRENT seed to every subscriber and move the
   *  service to `targetEpoch` (stage + commit). */
  reseal(targetEpoch: number): Promise<void>;
  /** Appoint the joiner's co-publisher key. Never fails the entry. */
  appoint(joinerKey: Uint8Array): Promise<void>;
  /** Durably record `entry` (a step landed). */
  save(entry: PublisherCommitEntry): Promise<void>;
  /** The entry is finished: drop it. */
  finish(entry: PublisherCommitEntry): Promise<void>;
}

function b64(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

/**
 * Run the remaining steps of `entry` in order, recording each as it lands.
 * Throws the first step's failure, leaving the entry (with what did land)
 * for the next run. A rotation's journal step that failed for an unknown
 * reason (the network) is the exception: the other steps run, and the entry
 * is kept, not finished, so the next replay sends the journal step again.
 */
export async function runPublisherCommit(entry: PublisherCommitEntry, ports: PublisherCommitPorts): Promise<void> {
  let current = entry;
  let journalDeferred = false;
  for (const step of remainingSteps(current)) {
    switch (step) {
      case "journal": {
        if (current.commitB64 === null) break;
        const proof =
          current.proofEpoch !== null && current.proofSeedB64 !== null
            ? { epoch: BigInt(current.proofEpoch), seed: b64(current.proofSeedB64) }
            : null;
        try {
          await ports.commitToJournal(b64(current.commitB64), proof);
        } catch (err) {
          if (ports.classifyCommitFailure(err) === "lost") {
            // ADR-0081: nothing of this commit is or will be on the DS (a
            // co-publisher's rotation is refused like this: its root is not
            // the journal's).
            console.warn("ds: PublisherGroup commit not taken by the journal", current.kind);
            current = { ...current, journalLost: true };
            break;
          }
          if (current.kind === "rotation") {
            // The subscribers' reseal does not wait on the DS: the journal
            // step stays owed and the entry is kept for it.
            console.warn("ds: PublisherGroup rotation commit deferred", err instanceof Error ? err.name : "");
            journalDeferred = true;
            continue;
          }
          throw err;
        }
        break;
      }
      case "welcome":
        if (current.journalLost) break;
        if (current.welcomeB64 !== null && current.joinerKeyB64 !== null) {
          await ports.deliverWelcome(b64(current.welcomeB64), b64(current.joinerKeyB64), current.replyGrantB64);
        }
        break;
      case "reseal":
        if (!resealLanded(current, await ports.serverEpoch())) await ports.reseal(current.serverTargetEpoch);
        break;
      case "appoint":
        if (current.journalLost) break;
        if (current.joinerKeyB64 !== null) {
          try {
            await ports.appoint(b64(current.joinerKeyB64));
          } catch (err) {
            // An appointment is retried by the next co-publisher open; it
            // never holds the entry.
            console.warn("co-publisher appointment after an approve failed", err);
          }
        }
        break;
    }
    current = withStepDone(current, step);
    await ports.save(current);
  }
  if (journalDeferred) return;
  await ports.finish(current);
}
