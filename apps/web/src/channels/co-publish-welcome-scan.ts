// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The co-publisher's wait for its Welcome reads its account mailbox every
 * few seconds (`copublisher.ts` `pollForCoPublishWelcome`). That mailbox
 * holds every Welcome of every device of the account, none of which opens as
 * a PublisherGroup; reading it whole and trying each one again on every pass
 * logged 22 failures every 3.4 s for minutes (seen live 2026-11-07).
 *
 * This is what a session remembers instead: per mailbox, the newest row
 * already read, and the rows whose Welcome did not open. Opening a Welcome
 * is local and deterministic, so one that failed never opens later.
 *
 * Pure bookkeeping, no I/O; bounded so a long session cannot grow it without
 * end.
 */

/** Rows remembered as failed before the oldest are forgotten (chosen). */
export const MAX_FAILED_WELCOMES = 4096;

export interface WelcomeScanState {
  /** The newest row id read from a mailbox (hex address), 0 when none. */
  cursorOf(addressHex: string): number;
  /** Record that a pass read a mailbox up to `id`; never moves back. */
  advance(addressHex: string, id: number): void;
  /** A row (`<address hex>:<row id>`) whose Welcome did not open. */
  hasFailed(rowKey: string): boolean;
  markFailed(rowKey: string): void;
}

export function createWelcomeScanState(maxFailed = MAX_FAILED_WELCOMES): WelcomeScanState {
  const cursors = new Map<string, number>();
  const failed = new Set<string>();
  return {
    cursorOf: (addressHex) => cursors.get(addressHex) ?? 0,
    advance(addressHex, id) {
      if (id > (cursors.get(addressHex) ?? 0)) cursors.set(addressHex, id);
    },
    hasFailed: (rowKey) => failed.has(rowKey),
    markFailed(rowKey) {
      failed.add(rowKey);
      while (failed.size > maxFailed) {
        const oldest = failed.values().next().value;
        if (oldest === undefined) break;
        failed.delete(oldest);
      }
    },
  };
}

/**
 * The wait between two passes of the co-publisher Welcome wait: 3 s first,
 * growing by half each pass to at most 15 s (chosen). `pass` counts from 0.
 */
export function coPublishWaitMs(pass: number): number {
  const base = 3000 * Math.pow(1.5, Math.max(0, pass));
  return Math.min(15_000, Math.round(base));
}
