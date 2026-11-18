// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 3.6 / UX 7.5 — the anonymous-ballot queue.
 *
 * A ballot leaves the device after a random delay uniform in `0..=10 s`, from
 * a queue that survives a restart (one sealed `account-kv` row), never before
 * this run's session landing succeeded, never with a push wake and never in
 * the same request as anything else. The vote shows as cast at once
 * ("Sending your vote…"); a post that fails leaves the entry `failed` with
 * `Try again`; an entry still waiting when the app closed is sent at the next
 * open.
 *
 * The row is computed when the vote is cast (off the UI thread), so a
 * restart re-posts the very same row. A newer vote in the same poll replaces
 * a queued one that has not left yet.
 */
import { createSignal } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";

/** UX 7.5: "a random wait of up to ten seconds". The maintainer's choice. */
export const BALLOT_DELAY_MAX_MS = 10_000;

/** A uniform delay in `0..=BALLOT_DELAY_MAX_MS` from one `[0, 1)` draw. */
export function ballotDelayMs(rand01: number): number {
  const r = Number.isFinite(rand01) ? Math.min(Math.max(rand01, 0), 0.999_999_999) : 0;
  return Math.floor(r * (BALLOT_DELAY_MAX_MS + 1));
}

/** A uniform draw from the platform CSPRNG. */
export function secureRandom01(): number {
  const b = new Uint32Array(1);
  crypto.getRandomValues(b);
  return (b[0] ?? 0) / 2 ** 32;
}

export interface QueuedBallot {
  readonly id: string;
  readonly groupIdB64: string;
  readonly pollMessageId: number;
  readonly choices: readonly number[];
  /** The sealed row (standard base64), computed when the vote was cast. */
  readonly rowB64: string;
  /** The set version the cast used, recorded in the own-choice register
   *  once the row is posted. */
  readonly setVersion: number;
  readonly notBeforeMs: number;
  readonly state: "waiting" | "failed";
}

// ----- pure rules -------------------------------------------------------

/** Enqueue: a newer vote in the same poll replaces one still waiting. */
export function withQueued(queue: readonly QueuedBallot[], next: QueuedBallot): QueuedBallot[] {
  return [
    ...queue.filter((q) => !(q.groupIdB64 === next.groupIdB64 && q.pollMessageId === next.pollMessageId)),
    next,
  ];
}

/** The entries due now, oldest first; none before the session landed. */
export function dueBallots(queue: readonly QueuedBallot[], nowMs: number, sessionLanded: boolean): QueuedBallot[] {
  if (!sessionLanded) return [];
  return queue
    .filter((q) => q.state === "waiting" && q.notBeforeMs <= nowMs)
    .sort((a, b) => a.notBeforeMs - b.notBeforeMs);
}

/** `Try again`: the same row, a fresh random delay. */
export function retried(q: QueuedBallot, nowMs: number, rand01: number): QueuedBallot {
  return { ...q, state: "waiting", notBeforeMs: nowMs + ballotDelayMs(rand01) };
}

/** At the next open: every entry still waiting keeps its row and goes after
 *  a fresh delay (the old deadline is meaningless across a restart). */
export function rescheduledAtOpen(queue: readonly QueuedBallot[], nowMs: number, rand: () => number): QueuedBallot[] {
  return queue.map((q) => (q.state === "waiting" ? { ...q, notBeforeMs: nowMs + ballotDelayMs(rand()) } : q));
}

/** The next time a timer has to fire, or `null` when nothing waits. */
export function nextDueAt(queue: readonly QueuedBallot[]): number | null {
  let at: number | null = null;
  for (const q of queue) {
    if (q.state !== "waiting") continue;
    at = at === null ? q.notBeforeMs : Math.min(at, q.notBeforeMs);
  }
  return at;
}

// ----- runtime ----------------------------------------------------------

export interface BallotPorts {
  /** Post one sealed row to the group's journal (the epoch access proof,
   *  no wake). Resolves with the DS message id. */
  post(groupIdB64: string, row: Uint8Array): Promise<number>;
  /** Record the own choice in the DeviceGroup register. */
  recordChoice(q: QueuedBallot): Promise<void>;
  /** This run's session landing succeeded. */
  sessionLanded(): boolean;
  now(): number;
  rand(): number;
}

const ROW = "attach-ballots";
const [queue, setQueue] = createSignal<readonly QueuedBallot[]>([]);
let account: string | null = null;
let ports: BallotPorts | null = null;
let timer: ReturnType<typeof setTimeout> | null = null;
let draining = false;

/** The queue, reactive. */
export function ballotQueue(): readonly QueuedBallot[] {
  return queue();
}

/** The queued entry of one poll, reactive. */
export function queuedBallotOf(groupIdB64: string, pollMessageId: number): QueuedBallot | undefined {
  return queue().find((q) => q.groupIdB64 === groupIdB64 && q.pollMessageId === pollMessageId);
}

function save(next: readonly QueuedBallot[]): void {
  setQueue(next);
  const acct = account;
  if (acct === null) return;
  void saveAccountJson(acct, ROW, { v: 1, q: next }).catch((err: unknown) =>
    console.warn("[attach] ballot queue not saved", err instanceof Error ? err.name : "unknown"),
  );
}

function schedule(): void {
  if (timer !== null) clearTimeout(timer);
  timer = null;
  const p = ports;
  if (p === null) return;
  const at = nextDueAt(queue());
  if (at === null) return;
  timer = setTimeout(() => void drain(), Math.max(0, at - p.now()));
}

async function drain(): Promise<void> {
  const p = ports;
  if (p === null || draining) return;
  draining = true;
  try {
    for (const q of dueBallots(queue(), p.now(), p.sessionLanded())) {
      try {
        await p.post(q.groupIdB64, b64(q.rowB64));
        save(queue().filter((x) => x.id !== q.id));
        try {
          await p.recordChoice(q);
        } catch (err) {
          console.warn("[attach] own choice not recorded", err instanceof Error ? err.name : "unknown");
        }
      } catch (err) {
        console.warn("[attach] ballot not posted", err instanceof Error ? err.name : "unknown");
        save(queue().map((x) => (x.id === q.id ? { ...x, state: "failed" as const } : x)));
      }
    }
  } finally {
    draining = false;
    schedule();
  }
}

/** Bind the queue to an account (loads and reschedules what waited). */
export async function bindBallotQueue(accountId: string | null, next: BallotPorts | null): Promise<void> {
  ports = next;
  if (account === accountId) {
    schedule();
    return;
  }
  account = accountId;
  setQueue([]);
  if (accountId === null || next === null) {
    schedule();
    return;
  }
  try {
    const raw = (await loadAccountJson(accountId, ROW)) as { q?: unknown } | null;
    const list = Array.isArray(raw?.q) ? (raw.q as QueuedBallot[]) : [];
    if (account === accountId) save(rescheduledAtOpen(list, next.now(), next.rand));
  } catch {
    /* starts empty */
  }
  schedule();
}

/** Enqueue a cast ballot. */
export function enqueueBallot(entry: Omit<QueuedBallot, "id" | "notBeforeMs" | "state">): void {
  const p = ports;
  const now = p?.now() ?? Date.now();
  const rand = p?.rand() ?? secureRandom01();
  const q: QueuedBallot = {
    ...entry,
    id: `${entry.groupIdB64}|${entry.pollMessageId}|${now}`,
    notBeforeMs: now + ballotDelayMs(rand),
    state: "waiting",
  };
  save(withQueued(queue(), q));
  schedule();
}

/** `Try again` on a failed entry. */
export function retryBallot(groupIdB64: string, pollMessageId: number): void {
  const p = ports;
  const now = p?.now() ?? Date.now();
  const rand = p?.rand() ?? secureRandom01();
  save(
    queue().map((q) =>
      q.groupIdB64 === groupIdB64 && q.pollMessageId === pollMessageId ? retried(q, now, rand) : q,
    ),
  );
  schedule();
}

/** Tests only: drop the module state. */
export function resetBallotQueueForTest(): void {
  if (timer !== null) clearTimeout(timer);
  timer = null;
  ports = null;
  account = null;
  setQueue([]);
}

/** Tests only: run the drain now. */
export async function drainBallotsForTest(): Promise<void> {
  await drain();
}

function b64(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

registerAccountMemory("attach-ballots", (ids) => {
  if (account !== null && ids.includes(account)) {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    account = null;
    setQueue([]);
  }
});
