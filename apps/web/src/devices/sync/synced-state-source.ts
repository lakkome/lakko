// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where the backup finds the account's synced state (ADR-0058 section 5.2).
 *
 * The backup is composed outside the sync runtime's component tree, so the
 * runtime registers a reader here for the life of a session and the backup
 * asks it. No reader (no session, or the runtime has not started) means an
 * empty value, which the backup payload carries as "none held".
 */

let reader: (() => Uint8Array | null) | null = null;

/** Register the reader; returns the unregister. */
export function registerSyncedStateSource(read: () => Uint8Array | null): () => void {
  reader = read;
  return () => {
    if (reader === read) reader = null;
  };
}

/** `SyncedState.toBytes()` of the running session, or `null`. */
export function currentSyncedStateBytes(): Uint8Array | null {
  try {
    return reader === null ? null : reader();
  } catch {
    return null;
  }
}

// ---------- a restore landing while the runtime is already up -------------

type RestoreListener = (accountIdB64: string, synced: Uint8Array) => void;
const restoreListeners = new Set<RestoreListener>();

/**
 * A backup restore produced the account's synced state (ADR-0058 section
 * 5.2). Every route that restores the identity from the phrase lands the
 * session before the restore runs (`backup/restore.ts`), so the sync runtime
 * may already be running; it merges the restored fold into its own.
 */
export function announceRestoredSyncedState(accountIdB64: string, synced: Uint8Array): void {
  for (const l of restoreListeners) l(accountIdB64, synced);
}

/** Subscribe; returns the unsubscribe. */
export function onRestoredSyncedState(listener: RestoreListener): () => void {
  restoreListeners.add(listener);
  return () => restoreListeners.delete(listener);
}
