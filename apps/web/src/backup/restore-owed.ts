// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "This account's backup has not been consulted yet", per account.
 *
 * Every route that restores the identity from the recovery phrase fetches
 * and applies the account backup (`restore.ts`). The flag is set before that
 * fetch starts and cleared once the fetch reached a definitive answer
 * (applied, no backup, or a backup this device will not apply). While it is
 * set:
 *
 *   * the next session landing runs the fetch once more
 *     (`AuthContext.finishCanonicalSession`), which is what recovers from a
 *     network failure or a tab closed mid-restore;
 *   * the automatic backup does not upload (`auto-backup.ts`). The device
 *     holds the phrase, so it can seal an envelope, and an upload composed
 *     before the restore ran would replace the server's blob - the one the
 *     restore still has to read - with this device's nearly empty state.
 *
 * `localStorage`, keyed by the account; one marker byte, no secret. Log out
 * keeps it (the retry belongs to the account's next landing), Forget device
 * and Delete account drop it (`auth/wipe-account.ts`).
 */

/** Prefix of the per-account row; the account id (standard base64) follows. */
const RESTORE_OWED_PREFIX = "lakko.backup.restore-owed.v1.";

/** The slice of `Storage` this module uses; injectable for tests. */
export interface RestoreOwedStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function defaultStorage(): RestoreOwedStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/** Record that `accountIdB64`'s backup still has to be fetched and applied. */
export function markBackupRestoreOwed(
  accountIdB64: string,
  storage: RestoreOwedStorage | null = defaultStorage(),
): void {
  if (storage === null || accountIdB64.length === 0) return;
  try {
    storage.setItem(RESTORE_OWED_PREFIX + accountIdB64, "1");
  } catch {
    // Storage unavailable: the restore still runs on this landing; only the
    // retry after a failure is lost.
  }
}

/** Whether a fetch of `accountIdB64`'s backup is still owed. */
export function isBackupRestoreOwed(
  accountIdB64: string,
  storage: RestoreOwedStorage | null = defaultStorage(),
): boolean {
  if (storage === null || accountIdB64.length === 0) return false;
  try {
    return storage.getItem(RESTORE_OWED_PREFIX + accountIdB64) !== null;
  } catch {
    return false;
  }
}

/** The fetch reached a definitive answer (or the account is being wiped). */
export function clearBackupRestoreOwed(
  accountIdB64: string,
  storage: RestoreOwedStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  try {
    storage.removeItem(RESTORE_OWED_PREFIX + accountIdB64);
  } catch {
    // Nothing to clear.
  }
}
