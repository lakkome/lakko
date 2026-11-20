// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 1 - a device does not skip the commit that moves its
 * conversation.
 *
 * Until this, a conversation row that failed to apply was logged and skipped
 * and the cursor moved past it (`chat/client.ts`, `applyPolledRows`), so a
 * device that failed the ONE commit taking its group from epoch N to N+1 could
 * never read the group again, and nothing noticed (seen live 2026-11-18).
 *
 * ## The rule
 *
 * Since ADR-0081 an epoch is registered on the DS only together with its
 * commit row, and a signed journal read names, for every epoch above the one
 * it was signed at, the row registered with it (`commits`, read as
 * `FetchResult.registeredCommits`). A failing row that is the registered
 * commit leaving this device's epoch (`registeredCommits.get(local + 1)` is
 * its id) is HELD: the batch stops at it, the cursor stays before it, and it
 * is retried on later polls with a bounded backoff. Every other failing row is
 * skipped as before, so a row that can never apply still never wedges the
 * poll, and a commit-shaped row a member posted through the plain publish
 * route is skipped at once (ADR-0091 open question 6, closed).
 *
 * After {@link REGISTERED_COMMIT_MAX_ATTEMPTS} failures the row is skipped as
 * before and the group keeps a behind mark (for ADR-0091 phase 2, and for the
 * journal-repair guard: an owner or admin that is behind must not Replace the
 * real epochs above it).
 *
 * The verdict, the constants and the log text are the same on iOS
 * (`Chat/RegisteredCommitHold.swift`) and Android
 * (`chat/RegisteredCommitHold.kt`).
 *
 * ## The log line
 *
 * One plain line per failed apply, with NO identifier and no content: the
 * failure kind, the epochs, the verdict and the attempt. Never a group id, a
 * row id, a key, an account, a name or text.
 *
 * Pure apart from the clock and the log sink, both injectable.
 */

import { refusalReasonOf } from "./membership-refusal";

/** Failures of one held row before it is skipped. Chosen, not measured. */
export const REGISTERED_COMMIT_MAX_ATTEMPTS = 8;

/** Seconds to the next attempt after failure k (k = 1..7). Chosen, not
 *  measured. */
export const REGISTERED_COMMIT_RETRY_DELAYS_SECS: readonly number[] = [2, 5, 15, 30, 60, 120, 300];

export type RegisteredCommitVerdict = "hold" | "skip";

/**
 * The id of the row the DS registered as the commit leaving `local`, or
 * `undefined` when the read named none (an unclaimed journal, a device at the
 * head, a read signed above `local`).
 */
export function registeredCommitLeaving(
  registeredCommits: ReadonlyMap<bigint, number> | undefined,
  local: bigint | undefined,
): number | undefined {
  if (registeredCommits === undefined || local === undefined) return undefined;
  return registeredCommits.get(local + 1n);
}

/**
 * Hold or skip a row that failed to apply: hold exactly when it is the
 * registered commit leaving this device's epoch. `local`: this device's epoch
 * before the row; `registeredCommits`: the read's `commits`.
 */
export function registeredCommitVerdict(input: {
  readonly isOwnEcho: boolean;
  readonly rowId: number;
  readonly local: bigint | undefined;
  readonly registeredCommits: ReadonlyMap<bigint, number> | undefined;
}): RegisteredCommitVerdict {
  if (input.isOwnEcho) return "skip";
  const leaving = registeredCommitLeaving(input.registeredCommits, input.local);
  return leaving !== undefined && leaving === input.rowId ? "hold" : "skip";
}

/** What a failed row was, from a fixed set (never the error text). */
export function failedApplyKind(err: unknown): string {
  const reason = refusalReasonOf(err);
  if (reason !== undefined) return `refused:${reason}`;
  const text = err instanceof Error ? `${err.name} ${err.message}` : String(err);
  if (text.includes("WrongEpoch")) return "wrong-epoch";
  if (text.includes("CannotDecryptOwnMessage")) return "own-message";
  if (text.includes("MissingProposal")) return "missing-proposal";
  if (/decrypt|aead/i.test(text)) return "decrypt";
  return "other";
}

function epochText(n: bigint | undefined): string {
  return n === undefined ? "-" : n.toString();
}

/** The failed-row line. */
export function failedRowLine(input: {
  readonly kind: string;
  readonly sealed: bigint | undefined;
  readonly after: bigint | undefined;
  readonly local: bigint | undefined;
  readonly latest: bigint | undefined;
  readonly verdict: RegisteredCommitVerdict;
  readonly attempt: number | undefined;
}): string {
  const attempt = input.attempt === undefined ? "-" : String(input.attempt);
  return (
    `chat row not applied: kind=${input.kind} sealed=${epochText(input.sealed)} ` +
    `after=${epochText(input.after)} local=${epochText(input.local)} ` +
    `latest=${epochText(input.latest)} verdict=${input.verdict} ` +
    `attempt=${attempt}/${REGISTERED_COMMIT_MAX_ATTEMPTS}`
  );
}

/** The line when the attempts are spent. */
export function exhaustionLine(local: bigint, latest: bigint | undefined): string {
  return (
    `chat registered commit skipped after ${REGISTERED_COMMIT_MAX_ATTEMPTS} attempts: ` +
    `this device is now behind in a conversation (local=${local} latest=${epochText(latest)})`
  );
}

/** The divergence line: the other devices applied what this one refuses. */
export function divergenceLine(kind: string): string {
  return (
    `chat registered commit refused by this device's rules (${kind}): ` +
    `the other devices applied it; a bug or version skew`
  );
}

/** Where the lines go. */
export interface HoldLog {
  warn(line: string): void;
  error(line: string): void;
}

const consoleLog: HoldLog = {
  warn: (line) => {
    // eslint-disable-next-line no-console
    console.warn(line);
  },
  error: (line) => {
    // eslint-disable-next-line no-console
    console.error(line);
  },
};

interface Hold {
  readonly rowId: number;
  /** The epoch the held commit leaves. */
  readonly local: bigint;
  attempts: number;
  readonly firstFailedAtMs: number;
  nextAttemptAtMs: number;
}

interface BehindMark {
  readonly epoch: bigint;
  readonly atMs: number;
}

/** What one failed row came to. */
export type FailureOutcome =
  | { readonly kind: "held"; readonly attempt: number }
  | { readonly kind: "exhausted" }
  | { readonly kind: "skipped" };

/** The per-process holds, keyed by the group id in standard base64. */
export class RegisteredCommitHolds {
  private readonly holds = new Map<string, Hold>();
  private readonly behind = new Map<string, BehindMark>();

  constructor(
    private readonly now: () => number = () => Date.now(),
    readonly log: HoldLog = consoleLog,
  ) {}

  /** The held row of the group, if any. */
  heldRowId(group: string): number | undefined {
    return this.holds.get(group)?.rowId;
  }

  /** The group's behind mark, if any. */
  behindEpoch(group: string): bigint | undefined {
    return this.behind.get(group)?.epoch;
  }

  /**
   * A row failed to apply. Judges it, logs the line (no identifier), and
   * updates the hold. `held` means: stop the batch at this row. `sealed` /
   * `after` / `latest` only feed the line.
   */
  recordFailure(
    group: string,
    row: {
      readonly id: number;
      readonly isOwnEcho: boolean;
      readonly sealed: bigint | undefined;
      readonly after: bigint | undefined;
      readonly local: bigint | undefined;
      readonly latest: bigint | undefined;
      readonly registeredCommits: ReadonlyMap<bigint, number> | undefined;
      readonly error: unknown;
    },
  ): FailureOutcome {
    const kind = failedApplyKind(row.error);
    const verdict = registeredCommitVerdict({
      isOwnEcho: row.isOwnEcho,
      rowId: row.id,
      local: row.local,
      registeredCommits: row.registeredCommits,
    });
    if (verdict === "skip") {
      // A held row read again as a skip (another path moved the group) no
      // longer needs holding.
      if (this.holds.get(group)?.rowId === row.id) this.holds.delete(group);
      this.log.warn(failedRowLine({ ...row, kind, verdict, attempt: undefined }));
      return { kind: "skipped" };
    }
    const nowMs = this.now();
    const existing = this.holds.get(group);
    const hold: Hold =
      existing !== undefined && existing.rowId === row.id
        ? existing
        : {
            rowId: row.id,
            local: row.local as bigint,
            attempts: 0,
            firstFailedAtMs: nowMs,
            nextAttemptAtMs: nowMs,
          };
    hold.attempts += 1;
    this.log.warn(failedRowLine({ ...row, kind, verdict, attempt: hold.attempts }));
    if (kind.startsWith("refused:")) this.log.error(divergenceLine(kind));
    if (hold.attempts >= REGISTERED_COMMIT_MAX_ATTEMPTS) {
      this.holds.delete(group);
      this.behind.set(group, { epoch: row.local as bigint, atMs: nowMs });
      this.log.error(exhaustionLine(row.local as bigint, row.latest));
      return { kind: "exhausted" };
    }
    const delaySecs =
      REGISTERED_COMMIT_RETRY_DELAYS_SECS[hold.attempts - 1] ??
      REGISTERED_COMMIT_RETRY_DELAYS_SECS[REGISTERED_COMMIT_RETRY_DELAYS_SECS.length - 1] ??
      300;
    hold.nextAttemptAtMs = nowMs + delaySecs * 1000;
    this.holds.set(group, hold);
    return { kind: "held", attempt: hold.attempts };
  }

  /** A row applied: a hold on it is over. */
  recordApplied(group: string, rowId: number): void {
    if (this.holds.get(group)?.rowId === rowId) this.holds.delete(group);
  }

  /** The group's epoch now. Drops a hold the group is already past (another
   *  path applied the commit) and a behind mark it has moved above. */
  observeEpoch(group: string, epoch: bigint | undefined): void {
    if (epoch === undefined) return;
    const hold = this.holds.get(group);
    if (hold !== undefined && epoch > hold.local) this.holds.delete(group);
    const mark = this.behind.get(group);
    if (mark !== undefined && epoch > mark.epoch) this.behind.delete(group);
  }

  /** Milliseconds until the group's held row may be retried; 0 when it may be
   *  now or nothing is held. */
  msUntilDue(group: string): number {
    const hold = this.holds.get(group);
    if (hold === undefined) return 0;
    return Math.max(0, hold.nextAttemptAtMs - this.now());
  }

  /** Journal repair (ADR-0057) must not Replace at `epoch`: this device holds
   *  a registered commit, or is behind at or above that epoch. */
  blocksRepair(group: string, epoch: bigint): boolean {
    if (this.holds.has(group)) return true;
    const mark = this.behind.get(group);
    return mark !== undefined && mark.epoch >= epoch;
  }

  /** Session end. */
  clear(): void {
    this.holds.clear();
    this.behind.clear();
  }
}

/** The app's instance. */
export const registeredCommitHolds = new RegisteredCommitHolds();
