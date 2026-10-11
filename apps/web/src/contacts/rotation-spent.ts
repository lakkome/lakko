// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0066 section 3.5a rule 5 - what was granted before a rotation of the
 * account keys is spent.
 *
 * The account's `/i/` links and the `reply` grants of its unanswered contact
 * requests named the mailbox that was left, and the holder of the old seed
 * knows the same links. On the ROTATING device two owed steps of the
 * rotation record (`devices/key-rotation.ts`) follow from that:
 *
 *   - `links`: every link minted before the rotation is revoked at the
 *     ledger and forgotten locally, so the list never offers a dead link;
 *   - `requests`: every unanswered outgoing contact request is sent again
 *     from the new key (a fresh request with a new `reply` grant) and its
 *     stored entry replaced.
 *
 * The decisions are pure; the mailbox provider installs the two runners (it
 * holds the directory client and the contacts client).
 *
 * Neither step may keep a rotation owed for a reason that will never go
 * away: only a failure to REACH the server answers `later`.
 */
import type { SentRequestRecord } from "./contact-list-model";

/** How one revoke or one re-send ended. */
export type SpentOutcome =
  /** Revoked / sent. */
  | "done"
  /** Not this device's to act on, or refused for a reason a retry would not
   *  change (a link the old key did not sign, a recipient that publishes
   *  nothing, keys reset and unverified). Left as it is. */
  | "skipped"
  /** The server could not be reached: owed. */
  | "unreachable";

/**
 * What a thrown failure means for one item: `unreachable` only when the
 * server could not be reached or asked to come back (a network error, a
 * `5xx`, `408`, `429`), found anywhere in the cause chain; everything else
 * is `skipped`, including a failure this cannot read. Pure.
 */
export function spentFailureOutcome(err: unknown): SpentOutcome {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== undefined && current !== null; depth += 1) {
    if (typeof current !== "object") break;
    const record = current as {
      name?: unknown;
      status?: unknown;
      cause?: unknown;
      variant?: { cause?: unknown };
    };
    if (typeof record.status === "number") {
      const status = record.status;
      return status >= 500 || status === 408 || status === 429 ? "unreachable" : "skipped";
    }
    if (typeof record.name === "string" && record.name.endsWith("NetworkError")) return "unreachable";
    // `fetch` itself rejects with a TypeError when the network is gone.
    if (current instanceof TypeError) return "unreachable";
    current = record.cause ?? record.variant?.cause;
  }
  return "skipped";
}

/** The step's verdict over every outcome. Pure. */
export function spentStepVerdict(outcomes: readonly SpentOutcome[]): "done" | "later" {
  return outcomes.includes("unreachable") ? "later" : "done";
}

/**
 * The unanswered requests still to be sent again: those sent BEFORE the keys
 * were changed (`rotatedAtMs`). A request re-sent by an earlier run of the
 * step carries a later time and is not sent twice. Oldest first. Pure.
 */
export function requestsToResend(
  waiting: readonly SentRequestRecord[],
  rotatedAtMs: number,
): SentRequestRecord[] {
  return waiting
    .filter((r) => r.sentAtMs < rotatedAtMs)
    .sort((a, b) => a.sentAtMs - b.sentAtMs);
}

/**
 * The seeds a pre-rotation link's revocation may be signed with: every own
 * predecessor, the newest first (a link is signed by the account key of the
 * moment it was minted). The CURRENT seed is never among them, so a link
 * minted after the rotation cannot be revoked by this step. Pure.
 */
export function revocationSeedOrder<S>(predecessorSeedsOldestFirst: readonly S[]): S[] {
  return [...predecessorSeedsOldestFirst].reverse();
}

/** How a revoke attempt under one seed ended. */
export type RevokeAttempt = "revoked" | "not-owner" | "unreachable";

/**
 * Fold the attempts made for ONE link, in order: revoked by any seed is
 * done; unreachable is owed; refused by every predecessor seed means the
 * link is not a pre-rotation link of this account and is left alone. Pure.
 */
export function linkRevokeOutcome(attempts: readonly RevokeAttempt[]): SpentOutcome {
  if (attempts.includes("revoked")) return "done";
  if (attempts.includes("unreachable")) return "unreachable";
  return "skipped";
}

/** When the keys were changed, for {@link requestsToResend}: the
 *  attestation's time, else when the rotation was prepared or started. */
export function rotatedAtMsOf(record: {
  readonly signedAtMs?: number;
  readonly preparedAtMs?: number;
  readonly startedAtMs: number;
}): number {
  // The LATEST known: a request sent between the start and the commit was
  // sent from the old key too.
  return Math.max(record.signedAtMs ?? 0, record.preparedAtMs ?? 0, record.startedAtMs);
}

/** A runner of one of the two steps: `null` when it cannot run yet. */
export type SpentRunner = (rotatedAtMs: number) => Promise<SpentOutcome[] | null>;

let linksRunner: SpentRunner | null = null;
let requestsRunner: SpentRunner | null = null;

export function installSpentRunners(runners: {
  readonly links: SpentRunner;
  readonly requests: SpentRunner;
} | null): void {
  linksRunner = runners?.links ?? null;
  requestsRunner = runners?.requests ?? null;
}

async function run(runner: SpentRunner | null, rotatedAtMs: number): Promise<"done" | "later"> {
  if (runner === null) return "later";
  try {
    const outcomes = await runner(rotatedAtMs);
    return outcomes === null ? "later" : spentStepVerdict(outcomes);
  } catch {
    return "later";
  }
}

/** The rotation's `links` step. Never throws. */
export function revokeLinksAfterRotation(rotatedAtMs: number): Promise<"done" | "later"> {
  return run(linksRunner, rotatedAtMs);
}

/** The rotation's `requests` step. Never throws. */
export function resendRequestsAfterRotation(rotatedAtMs: number): Promise<"done" | "later"> {
  return run(requestsRunner, rotatedAtMs);
}
