// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Fetch and apply the account backup: the ONE function every route that
 * restores the identity from the recovery phrase runs (2026-10-02).
 *
 * The routes that end with the phrase in hand are the restore screen
 * (`AuthContext.recover`: the full phrase, or the phrase a mailed code
 * opened from the email vault) and the phrase prompt after an ordinary
 * sign-in on a device with no cached identity seed
 * (`AuthContext.provideRecoveryPhrase`: the full phrase, or a passkey that
 * carries the keys). Until this existed only the first
 * fetched the backup, so a person who signed in and then typed the phrase
 * landed with no contacts, no pins and no folders. The another-device route
 * is not here: its state arrives in the pairing bundle
 * (`devices/state-bundle.ts`).
 *
 * What a restore applies, and what it never does:
 *
 *   * the accepted contacts, merged into the account's list (nothing already
 *     there is dropped);
 *   * the ADR-0058 synced state (pins, mutes, archive, drafts, read cursors,
 *     folders), merged register by register;
 *   * the ids of the account's conversations, kept so this device's own
 *     uploads still list them (`restored-known-groups.ts`);
 *   * the account's own predecessors (ADR-0074 section 3.7), stored where a
 *     kept device holds them. Every conversation leaf stays under the oldest
 *     own key, so a device restored with the newest key alone would hold a
 *     key no conversation knows. A landing with the phrase in hand fetches
 *     first ({@link prefetchAccountBackup}), so the device mints its binding
 *     and names its DeviceGroup under that key from the start; a restore that
 *     runs after the landing takes the pairing bundle's path (the session
 *     mints the binding again while the device holds no conversation);
 *   * NO MLS state. The backup carries none (payload v6): a provider
 *     snapshot belongs to the device that made it, and loading one here
 *     would clone a leaf (ADR-0050), roll a reinstalled device back behind
 *     commits it already published (ADR-0040) and re-use ratchet positions
 *     (G-34). After a restore the device's MLS state is whatever it already
 *     held locally for this account, possibly nothing, and it is admitted to
 *     its chats by the other members (ADR-0050 section 2.5a).
 *
 * The ADR-0037 head gate runs inside `BackupClient.restore` exactly as it
 * always did on the restore screen: a tampered head refuses with an alert, a
 * stale one asks through the app's confirm dialog (which answers "no" when no
 * dialog host is mounted), and neither is applied without that.
 *
 * A missing backup is not an error. A fetch failure never fails the sign-in:
 * it is logged, the "restore owed" flag (`restore-owed.ts`) stays set, and
 * the next session landing runs this once more
 * ({@link restoreOwedAccountBackup}). While the flag is set the automatic
 * backup does not upload, so the blob this still has to read is not replaced
 * by this device's nearly empty state.
 */
import { registerAccountMemory } from "../auth/account-memory";
import type { DeviceCipher } from "../auth/device-cipher";
import { announceRestoredSyncedState } from "../devices/sync/synced-state-source";
import { mergeSyncStateFromBackup } from "../devices/sync/sync-state-idb";
import { mergeRestoredFoldBytes } from "../devices/sync/synced-projection";
import {
  accountKeyHexOfSeed,
  applyOwnPredecessors,
  type CarriedOwnPredecessor,
} from "../devices/state-bundle";
import { t } from "../l10n";
import { requestConfirm } from "../ui/confirm-dialog";
import { loadWasm } from "../wasm";
import {
  applyRestoredContacts,
  BackupClient,
  type RestoreGateContext,
  type RestoreResult,
} from "./client";
import type { AcceptedContact } from "../contacts/local-store";
import { getPhrase } from "./phrase-store";
import {
  clearBackupRestoreOwed,
  isBackupRestoreOwed,
  markBackupRestoreOwed,
} from "./restore-owed";
import { addRestoredKnownGroups } from "./restored-known-groups";

/** What a run of {@link restoreAccountBackup} ended as. */
export type AccountBackupRestoreOutcome =
  /** The backup was applied. */
  | { readonly kind: "applied"; readonly contacts: number; readonly knownGroups: number }
  /** The account has no backup on the server (404). Not an error. */
  | { readonly kind: "no-backup" }
  /** A backup was served and this device will not apply it. Definitive: a
   *  retry would get the same answer. */
  | {
      readonly kind: "not-applied";
      readonly reason:
        | "rollback-refused"
        | "stale-declined"
        | "wrong-phrase-or-tampered"
        | "unsupported-payload";
    }
  /** The fetch failed. The owed flag stays set; the next landing retries. */
  | { readonly kind: "deferred" };

/** What the restore needs from the session. */
export interface AccountBackupRestoreInputs {
  /** The 16-byte account id the session authenticated as. */
  readonly accountIdBytes: Uint8Array;
  /** The same id, standard base64. */
  readonly accountIdB64: string;
  /** The device root key (the head-gate floor store and the synced-state
   *  row are wrapped under it). */
  readonly deviceKey: DeviceCipher;
  /** Bearer token for `GET /backup`. */
  readonly sessionToken: string;
  /** The recovery phrase, canonical form. */
  readonly phrase: string;
  /** ADR-0074 section 3.7: the backup carried own predecessors and they are
   *  stored. The session mints the device binding again while the device
   *  holds no conversation, and publishes again, as after a pairing bundle. */
  readonly onOwnPredecessorsStored?: () => Promise<unknown>;
}

/** The effects, injectable so the rule can be tested without a network, a
 *  wasm core or IndexedDB. Production passes none. */
export interface AccountBackupRestoreDeps {
  /** Fetch, gate, open and parse the backup. */
  readonly fetch: (
    phraseBytes: Uint8Array,
    sessionToken: string,
    gate: RestoreGateContext,
  ) => Promise<RestoreResult>;
  /** Merge the restored contacts into the account's list. */
  readonly applyContacts: (contacts: AcceptedContact[], accountIdB64: string) => void;
  /** Merge the restored ADR-0058 synced state. */
  readonly applySyncedState: (
    accountIdB64: string,
    deviceKey: DeviceCipher,
    synced: Uint8Array,
  ) => Promise<void>;
  /** Keep the conversation ids the backup named. */
  readonly rememberKnownGroups: (accountIdB64: string, idsB64: readonly string[]) => void;
  /** Store the own predecessors under the device key. */
  readonly applyOwnPredecessors: (
    accountIdB64: string,
    deviceKey: DeviceCipher,
    ownPredecessors: readonly CarriedOwnPredecessor[],
  ) => Promise<unknown>;
  readonly markOwed: (accountIdB64: string) => void;
  readonly clearOwed: (accountIdB64: string) => void;
  /** Tell the person a rolled-back / tampered backup was refused. */
  readonly alertRollbackRefused: () => void;
}

/** ADR-0058 section 5.2 — merge the backup's fold into the stored one (never
 *  "take one side"), then tell a sync runtime that is already running.
 *
 *  A backup whose synced state this build refuses (an older encoding
 *  version: the core reads exactly one) is a backup that holds none: nothing
 *  is written, nothing is announced, and the rest of the restore goes on. */
async function mergeRestoredSyncedState(
  accountIdB64: string,
  deviceKey: DeviceCipher,
  synced: Uint8Array,
): Promise<void> {
  if (synced.length === 0) return;
  const wasm = await loadWasm();
  let readable = true;
  await mergeSyncStateFromBackup(accountIdB64, deviceKey, synced, (localBytes, backupBytes) => {
    const merged = mergeRestoredFoldBytes(wasm, localBytes, backupBytes);
    if (merged === null) readable = false;
    return merged;
  });
  if (!readable) return;
  // The session may have landed before this restore, so the sync runtime
  // may already be running on an empty fold.
  announceRestoredSyncedState(accountIdB64, synced);
}

const DEFAULT_DEPS: AccountBackupRestoreDeps = {
  fetch: (phraseBytes, sessionToken, gate) => {
    const client = new BackupClient();
    client.setSessionToken(sessionToken);
    return client.restore(phraseBytes, gate);
  },
  applyContacts: applyRestoredContacts,
  applySyncedState: mergeRestoredSyncedState,
  rememberKnownGroups: (accountIdB64, idsB64) => addRestoredKnownGroups(accountIdB64, idsB64),
  applyOwnPredecessors: async (accountIdB64, deviceKey, ownPredecessors) => {
    const wasm = await loadWasm();
    return applyOwnPredecessors({ ownPredecessors }, accountIdB64, deviceKey, (seed) =>
      accountKeyHexOfSeed(wasm, seed),
    );
  },
  markOwed: (accountIdB64) => markBackupRestoreOwed(accountIdB64),
  clearOwed: (accountIdB64) => clearBackupRestoreOwed(accountIdB64),
  alertRollbackRefused: () => {
    // The app's own notice, never `window.alert` (design system section 6).
    void requestConfirm({
      title: t("backup.rollback_refused.title"),
      body: t("backup.rollback_refused.body"),
      confirmLabel: t("common.ok"),
      cancelLabel: t("common.close"),
      tone: "neutral",
      notice: true,
    });
  },
};

/** One run per account at a time: a landing's retry and a route's own run
 *  must not both fetch and apply. */
const inFlight = new Map<string, Promise<AccountBackupRestoreOutcome>>();

/** What {@link prefetchAccountBackup} fetched, for the restore of the same
 *  landing to apply. Holds seeds: it goes with the account's memory. */
const prefetched = new Map<string, RestoreResult>();

registerAccountMemory(
  "backup-prefetch",
  (ids) => {
    for (const id of ids) prefetched.delete(id);
  },
  { signOut: true },
);

/** Store the backup's own predecessors. Never throws: a device that could
 *  not store them is one that knows only the current key. Answers whether
 *  there were any to store. */
async function storeOwnPredecessors(
  inputs: AccountBackupRestoreInputs,
  deps: AccountBackupRestoreDeps,
  ownPredecessors: readonly CarriedOwnPredecessor[],
): Promise<boolean> {
  if (ownPredecessors.length === 0) return false;
  try {
    await deps.applyOwnPredecessors(inputs.accountIdB64, inputs.deviceKey, ownPredecessors);
    return true;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(
      "account backup restore: the earlier account keys were not stored",
      err instanceof Error ? err.name : "unknown",
    );
    return false;
  }
}

/**
 * The first half of a restore on a landing that holds the phrase, run BEFORE
 * the session's identities are installed: fetch the backup and store its own
 * predecessors, so the device mints its binding under the oldest key and
 * derives its DeviceGroup id from it the first time (ADR-0074 section 3.7).
 * What was fetched waits for {@link restoreAccountBackup} of the same landing,
 * which applies it and fetches nothing. A failed fetch leaves nothing: that
 * run fetches again. Never throws.
 */
export async function prefetchAccountBackup(
  inputs: AccountBackupRestoreInputs,
  overrides: Partial<AccountBackupRestoreDeps> = {},
): Promise<void> {
  const deps = { ...DEFAULT_DEPS, ...overrides };
  prefetched.delete(inputs.accountIdB64);
  const phraseBytes = new TextEncoder().encode(inputs.phrase);
  try {
    const result = await deps.fetch(phraseBytes, inputs.sessionToken, {
      accountId: inputs.accountIdBytes,
      accountIdB64: inputs.accountIdB64,
      deviceKey: inputs.deviceKey,
    });
    if (result.kind === "transport-failed") return;
    prefetched.set(inputs.accountIdB64, result);
    if (result.kind === "restored") {
      await storeOwnPredecessors(inputs, deps, result.ownPredecessors);
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("account backup restore: fetch ahead of the landing threw", err);
  } finally {
    phraseBytes.fill(0);
  }
}

/**
 * Fetch the account's backup, run the ADR-0037 head gate, and apply the
 * contacts, the synced state and the known conversation ids. Never throws,
 * and never touches MLS state.
 *
 * Call it once the session token and the device key exist. The caller is
 * expected to have set the owed flag before the session landed
 * (`markBackupRestoreOwed`), so the automatic backup cannot upload ahead of
 * it; it is set again here in case the caller could not.
 */
export function restoreAccountBackup(
  inputs: AccountBackupRestoreInputs,
  overrides: Partial<AccountBackupRestoreDeps> = {},
): Promise<AccountBackupRestoreOutcome> {
  const running = inFlight.get(inputs.accountIdB64);
  if (running !== undefined) return running;
  const run = runRestore(inputs, { ...DEFAULT_DEPS, ...overrides }).finally(() => {
    inFlight.delete(inputs.accountIdB64);
  });
  inFlight.set(inputs.accountIdB64, run);
  return run;
}

async function runRestore(
  inputs: AccountBackupRestoreInputs,
  deps: AccountBackupRestoreDeps,
): Promise<AccountBackupRestoreOutcome> {
  const { accountIdB64 } = inputs;
  deps.markOwed(accountIdB64);

  let result: RestoreResult;
  const fetchedAhead = prefetched.get(accountIdB64);
  if (fetchedAhead !== undefined) {
    prefetched.delete(accountIdB64);
    result = fetchedAhead;
  } else {
    const phraseBytes = new TextEncoder().encode(inputs.phrase);
    try {
      result = await deps.fetch(phraseBytes, inputs.sessionToken, {
        accountId: inputs.accountIdBytes,
        accountIdB64,
        deviceKey: inputs.deviceKey,
      });
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("account backup restore: fetch threw; retried on the next landing", err);
      return { kind: "deferred" };
    } finally {
      phraseBytes.fill(0);
    }
  }

  switch (result.kind) {
    case "transport-failed":
      // eslint-disable-next-line no-console
      console.warn(
        "account backup restore: fetch failed; retried on the next landing",
        result.cause,
      );
      return { kind: "deferred" };

    case "no-backup":
      deps.clearOwed(accountIdB64);
      return { kind: "no-backup" };

    case "rollback-blocked":
      // ADR-0037 section 6. `hard` = an unambiguous tamper / corruption;
      // otherwise the person was offered the stale override and declined (or
      // no dialog could be shown, which answers "no"). Nothing is applied.
      deps.clearOwed(accountIdB64);
      if (result.hard) {
        // eslint-disable-next-line no-console
        console.error(
          "account backup restore: BLOCKED a rolled-back / tampered backup:",
          result.reason,
        );
        deps.alertRollbackRefused();
        return { kind: "not-applied", reason: "rollback-refused" };
      }
      // eslint-disable-next-line no-console
      console.warn("account backup restore: a stale backup was not applied:", result.reason);
      return { kind: "not-applied", reason: "stale-declined" };

    case "wrong-phrase-or-tampered":
    case "unsupported-payload":
      deps.clearOwed(accountIdB64);
      // eslint-disable-next-line no-console
      console.warn("account backup restore: not applied:", result.kind);
      return { kind: "not-applied", reason: result.kind };

    case "restored": {
      // The own predecessors first, as a pairing bundle does: nothing below
      // may run on the idea that the current key is the account's only one.
      if (await storeOwnPredecessors(inputs, deps, result.ownPredecessors)) {
        try {
          await inputs.onOwnPredecessorsStored?.();
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(
            "account backup restore: re-binding failed",
            err instanceof Error ? err.name : "unknown",
          );
        }
      }
      // Contacts first: the next upload reads the account's list, so the
      // merged set is what it carries.
      deps.applyContacts(result.acceptedContacts, accountIdB64);
      try {
        await deps.applySyncedState(accountIdB64, inputs.deviceKey, result.syncedState);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("account backup restore: synced-state merge failed", err);
      }
      // The conversations the account has. Recorded BEFORE the owed flag is
      // cleared: once it is, the automatic backup may upload, and that
      // upload must already list them.
      deps.rememberKnownGroups(accountIdB64, result.knownGroupIds);
      deps.clearOwed(accountIdB64);
      return {
        kind: "applied",
        contacts: result.acceptedContacts.length,
        knownGroups: result.knownGroupIds.length,
      };
    }
  }
}

/** What {@link restoreOwedAccountBackup} needs from a session landing. */
export interface OwedRestoreInputs {
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  readonly sessionToken: string;
  /** The account's device root key, or `null` when this device has none. */
  readonly deviceKey: () => Promise<DeviceCipher | null>;
  /** The phrase, when the landing itself has it in hand (the phrase prompt).
   *  Absent on every other landing, which reads the device's phrase cache. */
  readonly phrase?: string;
  /** As in {@link AccountBackupRestoreInputs}. */
  readonly onOwnPredecessorsStored?: () => Promise<unknown>;
}

/** The effects of the landing step, injectable for tests. */
export interface OwedRestoreDeps {
  readonly isOwed: (accountIdB64: string) => boolean;
  readonly clearOwed: (accountIdB64: string) => void;
  /** The phrase cached on this device, or `null`. */
  readonly cachedPhrase: (accountIdB64: string, deviceKey: DeviceCipher) => Promise<string | null>;
  readonly restore: (inputs: AccountBackupRestoreInputs) => Promise<AccountBackupRestoreOutcome>;
}

const DEFAULT_OWED_DEPS: OwedRestoreDeps = {
  isOwed: (accountIdB64) => isBackupRestoreOwed(accountIdB64),
  clearOwed: (accountIdB64) => clearBackupRestoreOwed(accountIdB64),
  cachedPhrase: async (accountIdB64, deviceKey) => {
    const cached = await getPhrase(accountIdB64, deviceKey);
    if (cached === null) return null;
    try {
      return new TextDecoder().decode(cached.phrase);
    } finally {
      cached.phrase.fill(0);
    }
  },
  restore: (inputs) => restoreAccountBackup(inputs),
};

/**
 * The session-landing step: when a fetch of this account's backup is still
 * owed, or the landing itself holds the phrase, run it ONCE for this landing.
 * A no-op (resolving `null`) when nothing is owed, which is every ordinary
 * landing. Never throws.
 *
 * A device that holds neither the phrase in hand nor a cached one cannot
 * open the backup and cannot upload one either, so the flag protects nothing
 * there and is dropped.
 */
export async function restoreOwedAccountBackup(
  inputs: OwedRestoreInputs,
  overrides: Partial<OwedRestoreDeps> = {},
): Promise<AccountBackupRestoreOutcome | null> {
  const deps: OwedRestoreDeps = { ...DEFAULT_OWED_DEPS, ...overrides };
  try {
    // A landing that holds the phrase (the phrase prompt) always runs: its
    // route marked the restore owed a moment ago, and a browser that could
    // not store the flag must not skip the restore for that. Every other
    // landing runs only when one is owed.
    if (inputs.phrase === undefined && !deps.isOwed(inputs.accountIdB64)) return null;
    const deviceKey = await inputs.deviceKey();
    if (deviceKey === null) {
      // eslint-disable-next-line no-console
      console.warn("account backup restore: owed, but this device has no device key yet");
      return null;
    }
    let phrase = inputs.phrase ?? null;
    if (phrase === null) {
      try {
        phrase = await deps.cachedPhrase(inputs.accountIdB64, deviceKey);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("account backup restore: the cached phrase is unreadable", err);
        phrase = null;
      }
    }
    if (phrase === null) {
      // eslint-disable-next-line no-console
      console.warn("account backup restore: owed, but no phrase is held on this device");
      deps.clearOwed(inputs.accountIdB64);
      return null;
    }
    return await deps.restore({
      accountIdBytes: inputs.accountIdBytes,
      accountIdB64: inputs.accountIdB64,
      deviceKey,
      sessionToken: inputs.sessionToken,
      phrase,
      ...(inputs.onOwnPredecessorsStored !== undefined
        ? { onOwnPredecessorsStored: inputs.onOwnPredecessorsStored }
        : {}),
    });
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("account backup restore (landing) failed", err);
    return null;
  }
}
