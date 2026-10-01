// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Folder edits that have not reached the journal yet (UX spec section 6).
 *
 * An edit applies on this device at once and travels later: offline, the
 * person still sees their new folder. Each edit is kept as the batch of
 * stamped registers it was, and is sent as that one message with the stamps
 * it was given - never re-stamped, or an edit that waited an hour would
 * become newer than a change another device made in that hour.
 *
 * Pure, with the send injected, so the ordering and the stop-at-first-failure
 * rule are testable without a network.
 */
import type { StampedEdit } from "../../chat/folders/folder-model";

export type OutboxBatch = readonly StampedEdit[];

/** Whether Settings shows "Changes will reach your other devices when this
 *  device is back online." */
export function hasPendingFolderSync(queue: readonly OutboxBatch[]): boolean {
  return queue.length > 0;
}

export interface DrainResult {
  /** What is still waiting, oldest first. */
  readonly remaining: OutboxBatch[];
  readonly sent: number;
  /** A send failed; the rest wait for the next attempt. */
  readonly failed: boolean;
}

/**
 * Send the waiting batches oldest first, stopping at the first failure so the
 * order the person made the edits in is the order they arrive in. A batch the
 * send refuses as malformed (`isPermanent` says so) is dropped rather than
 * retried forever: it already applied here, and nothing can make it valid.
 */
export async function drainOutbox(
  queue: readonly OutboxBatch[],
  send: (batch: OutboxBatch) => Promise<void>,
  isPermanent: (err: unknown) => boolean = () => false,
): Promise<DrainResult> {
  const remaining = [...queue];
  let sent = 0;
  while (remaining.length > 0) {
    const next = remaining[0] as OutboxBatch;
    try {
      await send(next);
      sent += 1;
      remaining.shift();
    } catch (err) {
      if (isPermanent(err)) {
        remaining.shift();
        continue;
      }
      return { remaining, sent, failed: true };
    }
  }
  return { remaining, sent, failed: false };
}
