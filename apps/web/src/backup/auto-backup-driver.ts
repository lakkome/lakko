// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { subscribeAcceptedChanged } from "../contacts/local-store";
import { BackupClient } from "./client";
import { isBackupRestoreOwed } from "./restore-owed";
import {
  knownGroupsChanged,
  markBackupDirty,
  runDueBackup,
  type AutoBackupDeps,
  type AutoBackupOutcome,
} from "./auto-backup";

/**
 * Run the automatic backup: watch for changes worth keeping, and flush them on
 * a slow tick.
 *
 * Two halves of "worth keeping" reach this from single choke points rather
 * than from every mutation site, because a rule spread across call sites is a
 * rule that drifts:
 *
 *   * the address book, through `subscribeAcceptedChanged`, which already
 *     fires for every accept, removal and rename;
 *   * group membership, compared at each tick against the set last reported.
 *
 * Membership is checked HERE rather than hooked into the chat layer's snapshot
 * write, which is where it first went. That hook only fires after an MLS
 * mutation, so a device that simply opened the app - having already joined
 * everything it was going to join - never marked itself dirty and never
 * uploaded, which is precisely the person whose backup is most stale. Asking
 * at the tick also makes the first run after install a change, because nothing
 * on this device can vouch for what the server already holds.
 *
 * The tick itself does nothing while the account is clean - it reads two
 * integers out of `localStorage` and returns - so the interval can be short
 * enough to catch a change soon after it lands without costing anything when
 * nothing has.
 */
export const AUTO_BACKUP_TICK_MS = 60_000;

/** Everything the driver needs from the session, read at each tick because a
 *  session token is rebound and the group list moves. */
export interface AutoBackupDriverInputs {
  readonly accountId: () => Uint8Array | null;
  /** The conversation ids a backup lists - the groups this device holds plus
   *  the restored ones it has not joined yet (`backupKnownGroupIds`) - or
   *  `null` before the chat layer has loaded them, which is not the same as
   *  "no groups" and must not be read as a departure from every one of them. */
  readonly knownGroupIds: () => readonly string[] | null;
  readonly accountIdB64: () => string | null;
  readonly ed25519Seed: () => Uint8Array | null;
  readonly deviceKey: () => Promise<CryptoKey | null>;
  readonly sessionToken: () => string | null;
  /** Injected by tests; production reads `restore-owed.ts`. */
  readonly restoreOwed?: (accountIdB64: string) => boolean;
  /** Injected by tests; production leaves it alone. */
  readonly tickMs?: number;
  readonly onOutcome?: (outcome: AutoBackupOutcome) => void;
}

/** Start the driver. Returns the teardown. */
export function startAutoBackup(inputs: AutoBackupDriverInputs): () => void {
  const deps: AutoBackupDeps = {
    accountId: inputs.accountId,
    accountIdB64: inputs.accountIdB64,
    ed25519Seed: inputs.ed25519Seed,
    deviceKey: inputs.deviceKey,
    sessionToken: inputs.sessionToken,
    restoreOwed: inputs.restoreOwed ?? ((accountIdB64) => isBackupRestoreOwed(accountIdB64)),
    upload: async ({ accountId, ed25519Seed, deviceKey, sessionToken }) => {
      const client = new BackupClient();
      client.setSessionToken(sessionToken);
      const result = await client.uploadFromCache(
        accountId,
        ed25519Seed,
        deviceKey,
        // The account's conversations. Read here rather than passed through
        // the rules, which have no business knowing what a group is.
        inputs.knownGroupIds() ?? [],
      );
      if (result.kind !== "uploaded") {
        // `no-cached-phrase` is the ordinary state for a device that joined by
        // pairing rather than by signup or recover: it holds the account but
        // never saw the phrase, so it cannot seal an envelope. Not an error,
        // and not worth a line on every tick.
        if (result.kind !== "no-cached-phrase") {
          // eslint-disable-next-line no-console
          console.warn("auto backup:", result.kind);
        }
        return false;
      }
      return true;
    },
  };

  let running = false;
  const tick = (): void => {
    // One at a time: a slow upload must not have a second tick start another
    // against the same dirty flag.
    if (running) return;
    const accountIdB64 = inputs.accountIdB64();
    const groups = accountIdB64 === null ? null : inputs.knownGroupIds();
    if (accountIdB64 !== null && groups !== null && knownGroupsChanged(accountIdB64, groups)) {
      markBackupDirty(accountIdB64);
    }
    running = true;
    void runDueBackup(deps)
      .then((outcome) => inputs.onOutcome?.(outcome))
      .catch(() => {
        // `runDueBackup` already absorbs upload failures; this is the last
        // resort so a driver tick can never reject into the host.
      })
      .finally(() => {
        running = false;
      });
  };

  const unsubscribe = subscribeAcceptedChanged((change) => {
    // Another account being bound changes what is read, not what is kept.
    if (change === "account-changed") return;
    const id = inputs.accountIdB64();
    if (id !== null) markBackupDirty(id);
  });
  const timer = setInterval(tick, inputs.tickMs ?? AUTO_BACKUP_TICK_MS);
  // Catch a flag left dirty by a previous session - the tab can close between
  // the change and the tick that would have flushed it.
  tick();

  return (): void => {
    unsubscribe();
    clearInterval(timer);
  };
}
