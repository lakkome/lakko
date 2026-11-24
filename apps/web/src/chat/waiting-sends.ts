// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Messages that could not leave because of the network, and go by themselves
 * when it returns (`docs/ux/connection-state.md` section 7).
 *
 * A send that failed because a request got no answer leaves its row
 * WAITING, not failed. When the connection state enters Online after Waiting
 * or Connecting, the conversation's waiting rows are sent again, oldest
 * first, one at a time; a pass stops at the first one that fails again for
 * the network, so a later message never overtakes an earlier one. Each row
 * gets at most {@link MAX_AUTO_SENDS} automatic sends, then reads "Not sent".
 *
 * Kept out of `routes/Chat.tsx` so it can be run by a test: vitest has no
 * jsdom and no Solid transform here.
 */

import { accountSuspended } from "../auth/suspension-state";
import { DsNetworkError } from "../ds/client";
import { StorageNetworkError } from "../storage/client";
import {
  isNetworkFailure,
  onConnectionPhaseChange,
} from "../net/connection-state";

/** Automatic sends per message before it reads "Not sent". Chosen, not
 *  measured. */
export const MAX_AUTO_SENDS = 5;

/** When a send fails for the network while the state still reads Online
 *  (another request got an answer in between), the pass runs this long
 *  after. Chosen, not measured. */
export const WAITING_WHILE_ONLINE_RECHECK_MS = 3_000;

/** How a send was started: the first send, a tap on the row, or the pass. */
export type SendMode = "first" | "tap" | "automatic";

/** How one send of a row ended. */
export type SendOutcome =
  /** Published. */
  | "sent"
  /** No answer: the row waits. */
  | "network"
  /** Anything else: the row is "Not sent". */
  | "refused";

/**
 * Whether a failed send failed for the network: a request with no answer,
 * looked for through the wrappers the chat and contacts layers put around it.
 * A suspended account's refusal is raised as a transport failure by the DS
 * client but is not one, so it never counts.
 */
export function isNetworkSendFailure(
  err: unknown,
  suspended: boolean = accountSuspended(),
): boolean {
  if (suspended) return false;
  let current: unknown = err;
  for (
    let depth = 0;
    depth < 8 && current !== undefined && current !== null;
    depth += 1
  ) {
    if (
      current instanceof DsNetworkError ||
      current instanceof StorageNetworkError
    )
      return true;
    if (isNetworkFailure(current)) return true;
    if (typeof current !== "object") return false;
    const record = current as {
      cause?: unknown;
      variant?: { cause?: unknown };
    };
    current = record.cause ?? record.variant?.cause;
  }
  return false;
}

/** The outcome of one failed send. */
export function sendOutcomeOf(err: unknown, suspended?: boolean): SendOutcome {
  return isNetworkSendFailure(err, suspended) ? "network" : "refused";
}

/**
 * The row's state after an AUTOMATIC send failed for the network, counting
 * that send: still waiting under the bound, "Not sent" at it.
 */
export function ackAfterAutoNetworkFailure(
  autoSendsSoFar: number,
): "waiting" | "failed" {
  return autoSendsSoFar >= MAX_AUTO_SENDS ? "failed" : "waiting";
}

/** What a pass needs from the conversation. */
export interface WaitingPassDeps {
  /** The waiting rows' ids, oldest first (ids grow with send order). */
  readonly waitingInOrder: () => readonly number[];
  /** Send one row; resolves with how it ended. Never throws. */
  readonly send: (id: number) => Promise<SendOutcome>;
}

/**
 * One conversation's pass. Only one runs at a time; a request while one runs
 * makes it go round again when it ends, so a row queued meanwhile is not left
 * behind. A row is tried at most once per round, and a round stops at the
 * first network failure.
 */
export class WaitingSendPass {
  private running = false;
  private again = false;
  private readonly inFlight = new Set<number>();

  constructor(private readonly deps: WaitingPassDeps) {}

  /** Whether a pass is running (a new message then queues behind it). */
  get isRunning(): boolean {
    return this.running;
  }

  /** Whether `id` is being sent by the pass right now. */
  isInFlight(id: number): boolean {
    return this.inFlight.has(id);
  }

  /** Run the pass now, or once more after the one running ends. */
  async run(): Promise<void> {
    if (this.running) {
      this.again = true;
      return;
    }
    this.running = true;
    try {
      do {
        this.again = false;
        await this.round();
      } while (this.again);
    } finally {
      this.running = false;
    }
  }

  private async round(): Promise<void> {
    const tried = new Set<number>();
    for (;;) {
      const next = this.deps
        .waitingInOrder()
        .find((id) => !tried.has(id) && !this.inFlight.has(id));
      if (next === undefined) return;
      tried.add(next);
      this.inFlight.add(next);
      let outcome: SendOutcome;
      try {
        outcome = await this.deps.send(next);
      } finally {
        this.inFlight.delete(next);
      }
      // A network failure ends the round: the rows after it keep waiting and
      // spend nothing, so none of them goes ahead of this one.
      if (outcome === "network") {
        this.again = false;
        return;
      }
    }
  }
}

/**
 * Section 7.2 — run `pass` each time the servers answer again: the state
 * entering Online from Waiting or Connecting. Never on the system's signal
 * alone. Returns the stop.
 */
export function runWhenServersAnswer(
  pass: Pick<WaitingSendPass, "run">,
): () => void {
  return onConnectionPhaseChange((now, before) => {
    if (now === "online" && before !== "online") void pass.run();
  });
}

/**
 * Section 7.2 — whether a send of `id` must join the end of the queue rather
 * than go now: a pass is running, or another row of the conversation waits.
 */
export function mustQueueBehind(
  waitingIds: readonly number[],
  id: number,
  passRunning: boolean,
): boolean {
  return passRunning || waitingIds.some((w) => w !== id);
}
