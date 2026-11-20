// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The scheduling half of the background receive — polling every joined group
 * the user does NOT have open — kept free of DOM, wasm and network so its rules
 * can be tested.
 *
 * ## The defect
 *
 * The web client received a group's journal only while that group's chat view
 * was mounted (its own long poll in `routes/Chat.tsx`). A message, a 1x1 call
 * offer or a group-call announcement for any other chat stayed on the DS until
 * the user opened that chat — so an incoming call rang only if the caller
 * happened to be the chat on screen.
 *
 * ## Why short polls in a round robin, and not a long poll per group
 *
 * Every other receive loop on this client parks a long poll
 * (`ds/long-poll-loop.ts`), and the obvious extension — one per joined group —
 * does not work here. In development the app reaches the DS through the Vite
 * proxy over HTTP/1.1, where a browser allows six connections per origin, and
 * the open chat's long poll, the identity mailbox loop and the DeviceGroup sync
 * loop already hold three of them for up to 25 s each. A long poll per group
 * would exhaust the rest with the second or third chat, after which every send,
 * upload and KeyPackage fetch queues behind a parked request. Production over
 * HTTP/2 would not have the ceiling, but the dev stack is where this is
 * exercised and a design that works under the stricter limit works under both.
 *
 * So the background receive spends a small, fixed connection budget:
 *
 *   - **Short polls** (`waitSecs` 0): each request returns at once, so it holds
 *     a connection for one round trip, not 25 seconds.
 *   - **Round robin**, one sweep over every eligible group, then a pause of
 *     {@link BACKGROUND_SWEEP_INTERVAL_MS} ± {@link BACKGROUND_SWEEP_JITTER_MS}.
 *     The jitter keeps two tabs, or two devices on one network, from polling in
 *     lockstep.
 *   - **Bounded concurrency** ({@link BACKGROUND_CONCURRENCY}): at most two of
 *     these requests are in flight at once, whatever the number of groups.
 *   - **Immediate re-poll** of a group that just yielded rows, ahead of the
 *     rest of the sweep: a burst (a caller's offer followed by its ICE
 *     candidates) drains without waiting a whole interval per page. Capped at
 *     {@link MAX_IMMEDIATE_REPOLLS} per group per sweep so one busy chat cannot
 *     starve every other group of its turn.
 *
 * The cost of this choice is latency: a message to a chat that is not open can
 * take up to one interval plus a sweep to arrive. For a live 1x1 call that is
 * too slow — an answer or ICE candidate delayed by seconds stalls the
 * connection — so the call host's group gets a dedicated long poll for the
 * duration of the call instead (`BackgroundReceiveContext.tsx`), which is one
 * connection, held only while a call exists.
 *
 * ## What the scheduler does NOT own
 *
 * Correctness under overlap. `poll` is `ChatClient.pollMessages`, which
 * applies under the per-group MLS lock and re-reads a cursor shared with the
 * chat view (`receive-coordinator.ts`); a background poll that races the open
 * chat's poll is dropped as `cursor-moved` there, not prevented here.
 */
import { backoffFor } from "../ds/long-poll-loop";

/** Pause between two sweeps while something is arriving: a push wake's
 *  catch-up window, or a call in progress. */
export const BACKGROUND_SWEEP_INTERVAL_MS = 4_000;
/** Pause between two sweeps when idle and a push wake reaches this tab: the
 *  wake is what brings a message in, the sweep is only a safety net. */
export const BACKGROUND_IDLE_PUSH_MS = 120_000;
/** Pause between two sweeps when idle and no push wake is known to reach
 *  this tab: the sweep is the only way in, so it stays, but not every few
 *  seconds (seen live 2026-11-05: an idle client with six groups made 25
 *  journal reads in 20 s). Chosen, not measured. */
export const BACKGROUND_IDLE_NO_PUSH_MS = 30_000;

/**
 * The pause before the next background sweep (Telegram-scale: an idle client
 * must not poll every group every few seconds). Short only while something is
 * known to be arriving; otherwise long, longest when push covers the tab. A
 * push wake, the tab becoming visible and a call starting all end the pause
 * early (`BackgroundReceiveContext.tsx`), so a long pause costs no latency on
 * those paths.
 */
export function backgroundSweepIntervalMs(input: {
  readonly wakeWindowOpen: boolean;
  readonly callInProgress: boolean;
  readonly pushWake: "reachable" | "unreachable" | "unknown";
}): number {
  if (input.wakeWindowOpen || input.callInProgress) return BACKGROUND_SWEEP_INTERVAL_MS;
  return input.pushWake === "reachable" ? BACKGROUND_IDLE_PUSH_MS : BACKGROUND_IDLE_NO_PUSH_MS;
}
/**
 * Whether a group appeared that the receive loop has not seen: a chat this
 * device just joined. Its first messages and its name land within the next
 * minute and may bring no push wake (this device subscribes to the group's
 * pushes only once it holds the routing secret), so the caller opens the wake
 * window and sweeps at once. `null` before the first list of a session (the
 * groups a session starts with are not new). Walk 7 (2026-11-20): a joined
 * group read "No messages yet" for over 15 s on iOS; the same long idle pause
 * applies here.
 */
export function groupsAppeared(seen: ReadonlySet<string> | null, now: readonly string[]): boolean {
  if (seen === null) return false;
  return now.some((id) => !seen.has(id));
}
/** Uniform jitter applied to the pause, in both directions. */
export const BACKGROUND_SWEEP_JITTER_MS = 1_000;
/** Background requests in flight at once. */
export const BACKGROUND_CONCURRENCY = 2;
/** Times one group may be re-polled straight away within one sweep. */
export const MAX_IMMEDIATE_REPOLLS = 4;

/** What one group's poll reports back. */
export interface GroupPollResult {
  /** The poll consumed journal rows (the cursor moved), so more may follow. */
  readonly yieldedRows: boolean;
  /** Another path took this batch, so this poll says nothing about the head. */
  readonly cursorMoved?: boolean;
}

export interface SweepDeps {
  /** Every joined group, in a stable order. Re-read at the start of a sweep. */
  readonly groups: () => readonly string[];
  /** `true` for a group the scheduler must not poll right now (its chat view
   *  is open, or a dedicated loop owns it). Re-checked before each poll. */
  readonly isExcluded: (groupId: string) => boolean;
  /** One short poll of one group. Throws on failure. */
  readonly poll: (groupId: string, signal: AbortSignal) => Promise<GroupPollResult>;
  readonly concurrency: number;
  readonly maxImmediateRepolls: number;
  /** Per-group failure backoff; see {@link GroupBackoff}. */
  readonly backoff?: GroupBackoff;
  readonly now?: () => number;
  readonly onPollFailed?: (groupId: string, err: unknown) => void;
  /**
   * One sweep finished (not aborted): how many polls it made and how many
   * answered. The feed's "Couldn't check for new messages." reads this
   * (`feed/feed-refresh.ts`).
   */
  readonly onSweepDone?: (tally: { readonly tried: number; readonly answered: number }) => void;
}

/**
 * Per-group failure backoff, so one group whose journal keeps failing (a 5xx,
 * a decode error) is skipped until its backoff expires instead of spending a
 * request every sweep. A success clears it.
 */
export class GroupBackoff {
  private readonly state = new Map<string, { failures: number; until: number }>();

  failed(groupId: string, nowMs: number, kind: "network" | "other"): number {
    const failures = (this.state.get(groupId)?.failures ?? 0) + 1;
    const delay = backoffFor(kind, failures);
    this.state.set(groupId, { failures, until: nowMs + delay });
    return delay;
  }

  succeeded(groupId: string): void {
    this.state.delete(groupId);
  }

  isBackedOff(groupId: string, nowMs: number): boolean {
    const s = this.state.get(groupId);
    return s !== undefined && nowMs < s.until;
  }

  clear(): void {
    this.state.clear();
  }
}

/**
 * Poll every eligible group once (plus immediate re-polls), with at most
 * `concurrency` polls in flight. Resolves when the sweep is done or `signal`
 * aborts; never rejects.
 */
export async function runSweep(deps: SweepDeps, signal: AbortSignal): Promise<void> {
  const now = deps.now ?? Date.now;
  const queue: string[] = [...deps.groups()];
  const repolls = new Map<string, number>();
  const inFlight = new Set<string>();
  let tried = 0;
  let answered = 0;

  const next = (): string | undefined => {
    for (let i = 0; i < queue.length; i += 1) {
      const id = queue[i] as string;
      // The same group is never polled twice at once: its second poll would
      // only come back `cursor-moved`.
      if (inFlight.has(id)) continue;
      queue.splice(i, 1);
      return id;
    }
    return undefined;
  };

  const worker = async (): Promise<void> => {
    for (;;) {
      if (signal.aborted) return;
      const id = next();
      if (id === undefined) return;
      if (deps.isExcluded(id)) continue;
      if (deps.backoff?.isBackedOff(id, now()) === true) continue;
      inFlight.add(id);
      tried += 1;
      try {
        const result = await deps.poll(id, signal);
        answered += 1;
        deps.backoff?.succeeded(id);
        const used = repolls.get(id) ?? 0;
        if (result.yieldedRows && used < deps.maxImmediateRepolls && !signal.aborted) {
          repolls.set(id, used + 1);
          // Ahead of the rest of the sweep.
          queue.unshift(id);
        }
      } catch (err) {
        if (signal.aborted) return;
        deps.onPollFailed?.(id, err);
      } finally {
        inFlight.delete(id);
      }
    }
  };

  const workers: Promise<void>[] = [];
  for (let i = 0; i < Math.max(1, deps.concurrency); i += 1) workers.push(worker());
  await Promise.all(workers);
  if (!signal.aborted) deps.onSweepDone?.({ tried, answered });
}

export interface BackgroundLoopDeps extends SweepDeps {
  /** Signed in AND (tab visible OR a call is in progress). */
  readonly isActive: () => boolean;
  /** Resolves when `isActive` may have changed, or `signal` aborts. */
  readonly waitUntilActive: (signal: AbortSignal) => Promise<void>;
  /** Cancellable sleep; must resolve (not reject) on abort. */
  readonly sleep: (ms: number, signal: AbortSignal) => Promise<void>;
  /** `Math.random`-shaped, for the jitter. */
  readonly random: () => number;
  readonly intervalMs: number;
  readonly jitterMs: number;
  /** The pause for this moment, when it varies; `intervalMs` otherwise. The
   *  jitter is then a quarter of it. */
  readonly intervalFor?: () => number;
}

/** The pause after a sweep: `interval ± jitter`, never negative. */
export function sweepPauseMs(intervalMs: number, jitterMs: number, random: number): number {
  const offset = (random * 2 - 1) * jitterMs;
  return Math.max(0, Math.round(intervalMs + offset));
}

/**
 * Sweep, pause, repeat — until `signal` aborts (sign-out). Parks without
 * polling while `isActive` is false (a hidden tab with no call). Never rejects.
 */
export async function runBackgroundReceiveLoop(
  signal: AbortSignal,
  deps: BackgroundLoopDeps,
): Promise<void> {
  while (!signal.aborted) {
    if (!deps.isActive()) {
      await deps.waitUntilActive(signal);
      continue;
    }
    await runSweep(deps, signal);
    if (signal.aborted) return;
    const interval = deps.intervalFor?.() ?? deps.intervalMs;
    const jitter = deps.intervalFor === undefined ? deps.jitterMs : Math.round(interval / 4);
    await deps.sleep(sweepPauseMs(interval, jitter, deps.random()), signal);
  }
}
