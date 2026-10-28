// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Keep the encrypted backup current.
 *
 * Until this existed the backup was written exactly twice in an account's
 * life - an empty baseline at signup and one more at recover - so everything
 * a person did afterwards (contacts accepted, groups joined, renames) never
 * reached the server blob, and a restore on a new device landed them back at
 * registration day. `BackupClient.uploadFromCache` was built for precisely
 * this and had no caller anywhere.
 *
 * **Why a dirty flag rather than a plain timer.** What the payload carries -
 * the address book, the list of conversations, the synced state - changes
 * rarely, so a timer would upload the same thing over and over. What makes a
 * backup materially better is a membership or address-book change, so those
 * mark it dirty and the timer only decides *when* a dirty backup goes out.
 * (The payload carries no MLS state: a provider snapshot belongs to the
 * device that made it and is never backed up, 2026-10-02.)
 *
 * **Why the flag is persisted.** The throttle means the upload usually does
 * not happen at the moment of the change, and a tab can close in between. A
 * flag kept only in memory would lose exactly the backups that matter - the
 * ones taken right after the change that made them worth taking.
 */

import type { DeviceCipher } from "../auth/device-cipher";

/** No more than one upload per this interval, however much changes. */
export const MIN_BACKUP_INTERVAL_MS = 5 * 60_000;

/** What the scheduler remembers between runs, and across reloads. */
export interface AutoBackupState {
  /** When the last upload succeeded. `0` means never. */
  readonly lastUploadAtMs: number;
  /** When the account first became worth backing up again, or `null` when
   *  nothing has changed since the last upload. */
  readonly dirtySinceMs: number | null;
  /** The group set this device last reported, as a sorted join. `null` before
   *  the first report - which counts as a change, because nothing here can
   *  vouch for what the server already holds. */
  readonly lastGroupsKey: string | null;
}

export const CLEAN_STATE: AutoBackupState = {
  lastUploadAtMs: 0,
  dirtySinceMs: null,
  lastGroupsKey: null,
};

export type AutoBackupVerdict = "clean" | "throttled" | "due";

/**
 * Decide whether a backup should go out now.
 *
 * Pure and clock-free (the caller supplies `nowMs`) so the rule can be tested
 * rather than observed.
 *
 * A clock that has moved BACKWARDS reads as due, which is the opposite of the
 * choice `should_request_profiles` makes for its own throttle, and deliberately
 * so: there, suppressing costs a display name; here, suppressing costs the
 * user's data, and a stored `lastUploadAtMs` in the future would otherwise wedge
 * backups off forever.
 */
export function backupVerdict(
  state: AutoBackupState,
  nowMs: number,
  minIntervalMs: number = MIN_BACKUP_INTERVAL_MS,
): AutoBackupVerdict {
  if (state.dirtySinceMs === null) return "clean";
  if (state.lastUploadAtMs === 0) return "due";
  const elapsed = nowMs - state.lastUploadAtMs;
  if (elapsed < 0) return "due";
  return elapsed < minIntervalMs ? "throttled" : "due";
}

/** The state after a change worth backing up. Idempotent: the FIRST such
 *  change sets the clock, so the throttle is measured from the last upload
 *  and not pushed out by every later edit. */
export function markedDirty(
  state: AutoBackupState,
  nowMs: number,
): AutoBackupState {
  if (state.dirtySinceMs !== null) return state;
  return { ...state, dirtySinceMs: nowMs };
}

/** The state after a successful upload: the clock restarts and the account is
 *  clean again. Takes no prior state - an upload supersedes whatever was
 *  recorded, and accepting one would invite carrying a stale `dirtySinceMs`
 *  forward. */
export function markedUploaded(state: AutoBackupState, nowMs: number): AutoBackupState {
  return { ...state, lastUploadAtMs: nowMs, dirtySinceMs: null };
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/**
 * `localStorage`, keyed per account.
 *
 * Two integers and nothing else - no phrase, no key, no envelope - so this
 * needs neither the device key nor IndexedDB. A read that throws (private
 * mode, blocked site data) answers `CLEAN_STATE`, which costs at most one
 * delayed backup and never a failed one.
 */
function storageKey(accountIdB64: string): string {
  return `lakko.backup.auto.${accountIdB64}`;
}

export function loadAutoBackupState(accountIdB64: string): AutoBackupState {
  try {
    const raw = localStorage.getItem(storageKey(accountIdB64));
    if (raw === null) return CLEAN_STATE;
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return CLEAN_STATE;
    const p = parsed as Partial<AutoBackupState>;
    const last = typeof p.lastUploadAtMs === "number" ? p.lastUploadAtMs : 0;
    const dirty = typeof p.dirtySinceMs === "number" ? p.dirtySinceMs : null;
    const groups = typeof p.lastGroupsKey === "string" ? p.lastGroupsKey : null;
    return { lastUploadAtMs: last, dirtySinceMs: dirty, lastGroupsKey: groups };
  } catch {
    return CLEAN_STATE;
  }
}

export function saveAutoBackupState(
  accountIdB64: string,
  state: AutoBackupState,
): void {
  try {
    localStorage.setItem(storageKey(accountIdB64), JSON.stringify(state));
  } catch {
    // A backup that cannot remember it is due still runs on the next change
    // or the next load; failing the caller here would be worse.
  }
}

export function clearAutoBackupState(accountIdB64: string): void {
  try {
    localStorage.removeItem(storageKey(accountIdB64));
  } catch {
    // Nothing to do: the row is per-account and a stale one is harmless.
  }
}

/**
 * Record that something worth backing up has changed.
 *
 * Cheap enough to call from any mutation site: it reads and writes two
 * integers and never touches the network.
 */
export function markBackupDirty(
  accountIdB64: string,
  nowMs: number = Date.now(),
): void {
  const state = loadAutoBackupState(accountIdB64);
  const next = markedDirty(state, nowMs);
  if (next !== state) saveAutoBackupState(accountIdB64, next);
}

// ---------------------------------------------------------------------------
// The runner
// ---------------------------------------------------------------------------

/** What a run needs. Supplied as accessors rather than values because a run
 *  happens on a timer, long after the caller wired it, and the session token
 *  moves. */
export interface AutoBackupDeps {
  readonly accountId: () => Uint8Array | null;
  readonly accountIdB64: () => string | null;
  readonly ed25519Seed: () => Uint8Array | null;
  readonly deviceKey: () => Promise<DeviceCipher | null>;
  readonly sessionToken: () => string | null;
  /** Whether a fetch of this account's backup is still owed
   *  (`restore-owed.ts`). While it is, nothing is uploaded: the blob on the
   *  server is the one the restore still has to read, and this device's
   *  state before that restore is nearly empty. Absent means "never owed". */
  readonly restoreOwed?: (accountIdB64: string) => boolean;
  /** Injected for tests. */
  readonly upload: (args: {
    accountId: Uint8Array;
    ed25519Seed: Uint8Array;
    deviceKey: DeviceCipher;
    sessionToken: string;
  }) => Promise<boolean>;
  readonly now?: () => number;
}

export type AutoBackupOutcome =
  | "clean"
  | "throttled"
  | "not-ready"
  | "uploaded"
  | "failed";

/**
 * Upload if one is due, and record the result.
 *
 * `not-ready` covers every reason the session cannot produce an envelope right
 * now - no identity, no device key, no token - and the one reason it must not:
 * a restore of the account's backup is still owed. It leaves the dirty flag
 * alone, so the backup simply goes out at the next tick once the session is
 * whole; clearing it there would silently drop the change that made it dirty.
 */
export async function runDueBackup(
  deps: AutoBackupDeps,
): Promise<AutoBackupOutcome> {
  const nowMs = (deps.now ?? Date.now)();
  const accountIdB64 = deps.accountIdB64();
  if (accountIdB64 === null) return "not-ready";
  // Before the verdict: an owed restore holds the upload back however due
  // it is.
  if (deps.restoreOwed?.(accountIdB64) === true) return "not-ready";
  const state = loadAutoBackupState(accountIdB64);
  const verdict = backupVerdict(state, nowMs);
  if (verdict !== "due") return verdict;

  const accountId = deps.accountId();
  const ed25519Seed = deps.ed25519Seed();
  const sessionToken = deps.sessionToken();
  if (
    accountId === null ||
    ed25519Seed === null ||
    sessionToken === null ||
    sessionToken.length === 0
  ) {
    return "not-ready";
  }
  const deviceKey = await deps.deviceKey();
  if (deviceKey === null) return "not-ready";

  let ok = false;
  try {
    ok = await deps.upload({
      accountId,
      ed25519Seed,
      deviceKey,
      sessionToken,
    });
  } catch {
    ok = false;
  }
  if (!ok) return "failed";
  saveAutoBackupState(accountIdB64, markedUploaded(state, nowMs));
  return "uploaded";
}

// ---------------------------------------------------------------------------
// Group-membership change detection
// ---------------------------------------------------------------------------

/**
 * Whether the set of groups this device knows about has changed since the last
 * time this was asked, recording the new set as it goes.
 *
 * Persisted rather than held in memory, because an in-memory memo starts empty
 * on every load and would therefore report a change at every launch - turning
 * "back up when membership moves" into "back up once per session", which is a
 * different rule wearing the same name.
 *
 * The set is compared as a sorted join. Order from `groups()` is not
 * guaranteed stable, and an unsorted comparison would report a change whenever
 * the list was merely reordered.
 */
export function knownGroupsChanged(
  accountIdB64: string,
  groupIdsB64: readonly string[],
): boolean {
  const key = [...groupIdsB64].sort().join(",");
  const state = loadAutoBackupState(accountIdB64);
  if (state.lastGroupsKey === key) return false;
  saveAutoBackupState(accountIdB64, { ...state, lastGroupsKey: key });
  return true;
}
