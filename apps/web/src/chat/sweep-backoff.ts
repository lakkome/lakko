// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 x ADR-0057 - how soon a committer may START another sweep after
 * the server refused the staged epoch's registration.
 *
 * The sweep runs at the tail of every poll. Before this, a sweep whose
 * `PUT .../access` was refused (`401` in a journal nobody had claimed, `403`,
 * `400`) was staged, refused, abandoned and staged again every ~5 s, in every
 * group, forever (the live walk of 2026-09-29). A refusal of that kind does
 * not change between two polls, so retrying at poll cadence buys nothing but
 * load and log noise.
 *
 * After such a refusal the group waits {@link SWEEP_BACKOFF_INITIAL_MS} before
 * its next sweep may start, doubling on every further refusal up to
 * {@link SWEEP_BACKOFF_MAX_MS}. The wait ends early when anything that could
 * change the answer happens:
 *
 *   - a registration succeeds;
 *   - the membership or the role table changes (the fingerprint differs);
 *   - the owner's claim of the journal lands (`DsAccessRegistry.onOwnerClaimed`).
 *
 * **Only the cadence of STARTING a sweep changes.** ADR-0045's obligations
 * (C-67) are untouched: an owed verdict and an owed Welcome fan-out are
 * resolved before this gate is consulted (`runCommitterSweep` checks them
 * first), and a refused registration leaves nothing staged - the sweep
 * abandons before anything reaches the wire.
 *
 * Which refusals count: `400`, `401` and `403` from the DS, and a device with
 * no authority to register. A `409` (another committer registered the epoch
 * first) is a lost race that the next poll resolves, and a network failure is
 * transient; neither is backed off.
 */

/** The first wait after a refused registration. Chosen, not measured. */
export const SWEEP_BACKOFF_INITIAL_MS = 30_000;

/** The longest wait. Chosen, not measured. */
export const SWEEP_BACKOFF_MAX_MS = 600_000;

/**
 * The schedule: the wait after a refusal, given the wait that preceded it
 * (`null` for the first refusal). Pure.
 */
export function nextSweepBackoffMs(previousMs: number | null): number {
  if (previousMs === null || !Number.isFinite(previousMs) || previousMs < SWEEP_BACKOFF_INITIAL_MS) {
    return SWEEP_BACKOFF_INITIAL_MS;
  }
  return Math.min(previousMs * 2, SWEEP_BACKOFF_MAX_MS);
}

/** Is `err` a registration refusal the sweep should back off from? */
export function isBackoffRefusal(err: unknown): boolean {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== undefined && current !== null; depth += 1) {
    if (typeof current !== "object") return false;
    const r = current as { status?: unknown; name?: unknown; cause?: unknown };
    if (r.name === "NoJournalAuthorityError") return true;
    if (r.status === 400 || r.status === 401 || r.status === 403) return true;
    if (typeof r.status === "number") return false;
    current = r.cause;
  }
  return false;
}

interface Entry {
  delayMs: number;
  notBefore: number;
  fingerprint: string;
}

/** Per-group backoff state. One instance is the app's ({@link sweepBackoff});
 *  tests build their own. */
export class SweepBackoff {
  private readonly entries = new Map<string, Entry>();

  /**
   * May a sweep of `groupIdB64` start now? `fingerprint` is the group's
   * membership + role table as it stands; a different one than at the
   * refusal ends the wait.
   */
  mayStart(groupIdB64: string, nowMs: number, fingerprint: string): boolean {
    const entry = this.entries.get(groupIdB64);
    if (entry === undefined) return true;
    if (entry.fingerprint !== fingerprint) {
      this.entries.delete(groupIdB64);
      return true;
    }
    return nowMs >= entry.notBefore;
  }

  /** A registration was refused: start or lengthen the wait. Returns it. */
  noteRefused(groupIdB64: string, nowMs: number, fingerprint: string): number {
    const prior = this.entries.get(groupIdB64);
    const previous = prior !== undefined && prior.fingerprint === fingerprint ? prior.delayMs : null;
    const delayMs = nextSweepBackoffMs(previous);
    this.entries.set(groupIdB64, { delayMs, notBefore: nowMs + delayMs, fingerprint });
    return delayMs;
  }

  /** A registration succeeded, or the owner's claim landed: no wait. */
  reset(groupIdB64: string): void {
    this.entries.delete(groupIdB64);
  }

  /** The wait still ahead of `groupIdB64`, in ms (0 when none). */
  remainingMs(groupIdB64: string, nowMs: number): number {
    const entry = this.entries.get(groupIdB64);
    return entry === undefined ? 0 : Math.max(0, entry.notBefore - nowMs);
  }

  /** Forget everything: sign-out. */
  clear(): void {
    this.entries.clear();
  }
}

/** The app's backoff state, shared by the chat view's sweep and the
 *  background receive's. */
export const sweepBackoff = new SweepBackoff();
