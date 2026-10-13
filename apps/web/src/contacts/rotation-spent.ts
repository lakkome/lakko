// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.5a rule 5 - what was granted before a rotation of the
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
 * A KEPT device owes the same two steps over ITS OWN links and requests; with
 * no rotation record there, a marker written at adoption keeps them owed
 * ({@link runOwedSpendRound}).
 *
 * `/c/` call links are signed by the device key, not the account key: the
 * ones this device recorded when it minted them (`minted-links.ts`) are
 * revoked at the call-invite ledger by the same `links` step.
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

/**
 * Whether a link this device keeps a local row for was minted before the
 * keys were changed, and is therefore spent. A row with no time was kept
 * before the time was recorded, which is before any rotation this build
 * observes. Pure.
 */
export function localLinkSpent(mintedAtMs: number | null, rotatedAtMs: number): boolean {
  return mintedAtMs === null || mintedAtMs < rotatedAtMs;
}

/** How a revoke of a link this device minted itself ended at the ledger. */
export type LocalRevokeAttempt =
  | "revoked"
  /** The ledger has no such row for the signer: another device of the
   *  account already revoked it, or it expired. */
  | "gone"
  | "unreachable";

/**
 * Fold the attempts made for ONE link this device minted before the
 * rotation (it is known to be spent, so "the ledger has no row" is as good
 * as a revoke): owed only while the server could not be reached. A link
 * with no ledger row (no attempts) is done. Pure.
 */
export function localLinkRevokeOutcome(attempts: readonly LocalRevokeAttempt[]): SpentOutcome {
  if (attempts.includes("revoked")) return "done";
  return attempts.includes("unreachable") ? "unreachable" : "done";
}

// ---------------------------------------------------------------------------
// The spend round on a KEPT device
// ---------------------------------------------------------------------------
//
// A device that adopted keys rotated on another device holds its OWN `/i/`
// and `/c/` links and its OWN unanswered requests (the sent list is per
// device), all granted under the key that was left. It owes the same two
// steps over them. There is no rotation record on a kept device, so what is
// owed is this marker: written at adoption, read at each session start,
// cleared when both steps answered done.

/** What a kept device still owes after adopting rotated keys. */
export interface SpendOwed {
  /** When this device adopted the new keys: everything it granted before
   *  that is spent. */
  readonly adoptedAtMs: number;
  readonly links: boolean;
  readonly requests: boolean;
}

interface MarkerStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** The storage key of the marker, named here so the wipe agrees on it. */
export function spendOwedStorageKey(accountIdB64: string): string {
  return `lakko.contacts.rotation-spend-owed.v1.${accountIdB64}`;
}

/** Parse a stored marker; anything else reads as nothing owed. Pure. */
export function decodeSpendOwed(raw: string | null | undefined): SpendOwed | null {
  if (raw === null || raw === undefined) return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return null;
    const p = parsed as Record<string, unknown>;
    const at = p["adoptedAtMs"];
    if (typeof at !== "number" || !Number.isFinite(at) || at <= 0) return null;
    if (typeof p["links"] !== "boolean" || typeof p["requests"] !== "boolean") return null;
    if (!p["links"] && !p["requests"]) return null;
    return { adoptedAtMs: at, links: p["links"], requests: p["requests"] };
  } catch {
    return null;
  }
}

/**
 * The marker after an adoption at `nowMs`: both steps owed. A marker still
 * held from an earlier adoption is replaced by the later time, which covers
 * everything the earlier one did. Pure.
 */
export function spendOwedAfterAdoption(held: SpendOwed | null, nowMs: number): SpendOwed {
  return {
    adoptedAtMs: Math.max(held?.adoptedAtMs ?? 0, Math.floor(nowMs)),
    links: true,
    requests: true,
  };
}

/** The marker after one run of the round: a step that answered done is no
 *  longer owed; `null` when nothing is. Pure. */
export function spendOwedAfterRun(
  held: SpendOwed,
  ran: { readonly links?: "done" | "later"; readonly requests?: "done" | "later" },
): SpendOwed | null {
  const links = held.links && ran.links !== "done";
  const requests = held.requests && ran.requests !== "done";
  return links || requests ? { adoptedAtMs: held.adoptedAtMs, links, requests } : null;
}

function plainStorage(): MarkerStorage | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

export function readSpendOwed(
  accountIdB64: string,
  storage: MarkerStorage | undefined = plainStorage(),
): SpendOwed | null {
  try {
    return decodeSpendOwed(storage?.getItem(spendOwedStorageKey(accountIdB64)));
  } catch {
    return null;
  }
}

/** Record that this (kept) device adopted rotated keys. Never throws. */
export function markSpendOwed(
  accountIdB64: string,
  nowMs: number,
  storage: MarkerStorage | undefined = plainStorage(),
): void {
  try {
    const next = spendOwedAfterAdoption(readSpendOwed(accountIdB64, storage), nowMs);
    storage?.setItem(spendOwedStorageKey(accountIdB64), JSON.stringify(next));
  } catch {
    /* storage blocked: the round is not owed on this device */
  }
}

/** Forget the marker (both wipes). */
export function clearSpendOwed(
  accountIdB64: string,
  storage: MarkerStorage | undefined = plainStorage(),
): void {
  try {
    storage?.removeItem(spendOwedStorageKey(accountIdB64));
  } catch {
    /* nothing was stored either */
  }
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

const spendRounds = new Map<string, Promise<void>>();

/**
 * Run what a kept device still owes (the marker), once per call: each owed
 * step through its installed runner, the marker rewritten with what is left.
 * Called at each session start, once the reach session of the new key runs.
 * One round per account at a time. Never throws.
 */
export function runOwedSpendRound(
  accountIdB64: string,
  storage: MarkerStorage | undefined = plainStorage(),
): Promise<void> {
  const running = spendRounds.get(accountIdB64);
  if (running !== undefined) return running;
  const round = (async (): Promise<void> => {
    const held = readSpendOwed(accountIdB64, storage);
    if (held === null) return;
    const ran: { links?: "done" | "later"; requests?: "done" | "later" } = {};
    if (held.links) ran.links = await revokeLinksAfterRotation(held.adoptedAtMs);
    if (held.requests) ran.requests = await resendRequestsAfterRotation(held.adoptedAtMs);
    // Read again: an adoption during the round re-armed the marker, and
    // that later time must not be lost to this round's verdict.
    const now = readSpendOwed(accountIdB64, storage);
    if (now === null || now.adoptedAtMs !== held.adoptedAtMs) return;
    const next = spendOwedAfterRun(now, ran);
    try {
      if (next === null) storage?.removeItem(spendOwedStorageKey(accountIdB64));
      else storage?.setItem(spendOwedStorageKey(accountIdB64), JSON.stringify(next));
    } catch {
      /* storage blocked: the round runs again at the next session start */
    }
  })().finally(() => {
    spendRounds.delete(accountIdB64);
  });
  spendRounds.set(accountIdB64, round);
  return round;
}
