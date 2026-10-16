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
 * only), and the epochs. It lives inside the device-key-encrypted channel
 * log, never in plain storage.
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
    done,
  };
}

// ---------------------------------------------------------------------------
// The runner
// ---------------------------------------------------------------------------

/** What running the steps of one entry needs; injected so the order is
 *  testable without a network. */
export interface PublisherCommitPorts {
  /** Register the group's CURRENT epoch key (the N+1 one) on the DS with the
   *  channel's root. Idempotent. Required before an approve publishes its
   *  commit; best effort for a rotation, which publishes none. */
  registerEpoch(): Promise<void>;
  /** Publish the Add commit to the group journal. Idempotent at the
   *  receivers (a duplicate is dropped as a wrong-epoch commit). */
  publishCommit(commit: Uint8Array): Promise<void>;
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
 * for the next run.
 */
export async function runPublisherCommit(entry: PublisherCommitEntry, ports: PublisherCommitPorts): Promise<void> {
  let current = entry;
  for (const step of remainingSteps(current)) {
    switch (step) {
      case "journal":
        if (current.kind === "rotation") {
          // A rotation commit is not published to the journal, so no peer can
          // hold an unregistered epoch: the registration stays best effort, as
          // it always was (a co-publisher's is refused, its root is not the
          // journal's), and the owner's lazy claim on a refused request
          // registers the current epoch with the root.
          try {
            await ports.registerEpoch();
          } catch (err) {
            console.warn("ds: PublisherGroup epoch registration after rotation failed", err);
          }
        } else {
          await ports.registerEpoch();
          if (current.commitB64 !== null) await ports.publishCommit(b64(current.commitB64));
        }
        break;
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
            console.warn("co-publisher appointment after an approve failed", err);
          }
        }
        break;
    }
    current = withStepDone(current, step);
    await ports.save(current);
  }
  await ports.finish(current);
}
