// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 section 6 — delete this account's blobs from the server: recorded
 * in the durable owed list first (`blob-delete-queue.ts`), then tried. The
 * glue between the queue and `StorageClient.deleteBlob`.
 */

import { hexToBytes32 } from "../chat/blob-ids";
import { blobAccess } from "./blob-access";
import { drainOwedDeletes, owe, type DeleteOutcome } from "./blob-delete-queue";
import { StorageClient, StorageHttpError } from "./client";

let draining: Promise<void> | null = null;

async function attempt(idHex: string): Promise<DeleteOutcome> {
  const id = hexToBytes32(idHex);
  if (id === null) return "refused";
  try {
    await new StorageClient().deleteBlob(id);
    return "deleted";
  } catch (err) {
    // 403: not this account's to delete (or never uploaded with its key).
    if (err instanceof StorageHttpError && err.status === 403) return "refused";
    return "retry";
  }
}

/** Try every owed delete of `accountId` once. Never throws. */
export function drainBlobDeletes(accountId: string): Promise<void> {
  if (blobAccess() === null) return Promise.resolve();
  if (draining !== null) return draining;
  draining = drainOwedDeletes(accountId, attempt)
    .then(() => undefined, () => undefined)
    .finally(() => {
      draining = null;
    });
  return draining;
}

/** Owe the deletes of `idsHex` and try them now. Never throws. */
export function deleteBlobs(accountId: string, idsHex: readonly string[]): Promise<void> {
  if (idsHex.length === 0) return Promise.resolve();
  owe(accountId, idsHex);
  const running = draining;
  // A drain already running read the list before these were added: run
  // again after it.
  return running === null ? drainBlobDeletes(accountId) : running.then(() => drainBlobDeletes(accountId));
}
