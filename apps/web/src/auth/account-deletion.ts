// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The order of an account deletion on this device, and the bounded retries of
 * the server-side cleanups that follow it.
 *
 * 0. Before anything is deleted, the tokens the cleanups will spend are
 *    bought ({@link AccountDeletionDeps.beforeAuth}): each push unsubscribe
 *    spends a `push-register` anonymous token, and auth issues none once the
 *    account is gone. Buying tokens deletes nothing; a failure is logged and
 *    the deletion goes on.
 * 1. The account is deleted at auth. Auth can refuse (the ADR-0074 recall
 *    window, a missing proof): then nothing else runs and the error reaches
 *    the caller, so nothing is removed for an account that still exists.
 *    Right after a deletion succeeded the session provider is told
 *    ({@link AccountDeletionDeps.afterAuth}): from then on no refresh runs
 *    and no refusal signs the device out, since a sign-out in the middle of
 *    the cleanups would stop the account-deleted statements (ADR-0098).
 * 2. The cleanups run: what only this device can remove from the servers
 *    (its push-gw subscriptions, ADR-0078 phase 3, which push-gw cannot tie
 *    to an account), plus the backup blob and the directory entry, which the
 *    servers also remove on their own. Each is best-effort: up to
 *    {@link DELETION_CLEANUP_RETRIES} retries with a short backoff, all of
 *    them within {@link DELETION_CLEANUP_BUDGET_MS}, so the person is not
 *    kept waiting. A cleanup that still fails is logged by its label only
 *    (no identifier) and does not stop the wipe.
 * 3. This device is wiped.
 *
 * Kept free of IndexedDB, wasm and network imports so it runs under vitest.
 */

/** Retries after the first attempt of one cleanup. Chosen, not measured. */
export const DELETION_CLEANUP_RETRIES = 2;
/** Waits before the first and the second retry. Chosen, not measured. */
export const DELETION_CLEANUP_BACKOFF_MS: readonly number[] = [300, 900];
/** The whole cleanup stage stops waiting after this. Chosen, not measured. */
export const DELETION_CLEANUP_BUDGET_MS = 10_000;
/** Cleanups run at most this many at a time (one per push subscription can
 *  be many). Chosen, not measured. */
export const DELETION_CLEANUP_CONCURRENCY = 4;

/** One server-side removal that follows the auth deletion. */
export interface DeletionCleanup {
  /** A fixed label for the log; never an identifier. */
  readonly label: string;
  readonly run: () => Promise<void>;
  /** `false` for an error a retry cannot change (the default retries all). */
  readonly retryable?: (err: unknown) => boolean;
}

export interface AccountDeletionDeps {
  /** Runs before the auth deletion; best-effort, never stops it. */
  readonly beforeAuth?: () => Promise<void>;
  /** The auth deletion. A throw stops everything and is rethrown. */
  readonly deleteAtAuth: () => Promise<void>;
  /** Runs once the auth deletion succeeded, before any cleanup (ADR-0098):
   *  tells the session provider the account is gone, so a cleanup's `401`
   *  cannot sign the device out while the conversations are still being
   *  told. Never throws into the deletion. */
  readonly afterAuth?: () => void;
  /** ADR-0098 (walk 10): the account-deleted statements. Run to the end
   *  before any other cleanup starts: removing the directory entry at the
   *  same time can rebuild this device's conversation identity and stop the
   *  statements. Built after the auth deletion succeeded. */
  readonly firstCleanups?: () => Promise<readonly DeletionCleanup[]>;
  /** Built only after the auth deletion succeeded, before the wipe. */
  readonly cleanups: () => Promise<readonly DeletionCleanup[]>;
  /** Wipes this device. Its error is rethrown. */
  readonly wipe: () => Promise<void>;
  readonly sleep?: (ms: number) => Promise<void>;
  readonly now?: () => number;
  readonly log?: (message: string, label: string, errorName: string) => void;
  readonly budgetMs?: number;
}

const defaultSleep = (ms: number): Promise<void> =>
  new Promise((resolve) => {
    setTimeout(resolve, ms);
  });

const defaultLog = (message: string, label: string, errorName: string): void => {
  console.warn(message, label, errorName);
};

function errorName(err: unknown): string {
  return err instanceof Error ? err.name : "unknown";
}

/** `false` for an HTTP refusal a retry cannot change: a 4xx other than
 *  408 / 425 / 429. Network failures and 5xx are retried. */
export function retryableHttpFailure(err: unknown): boolean {
  if (typeof err !== "object" || err === null) return true;
  const status = (err as { status?: unknown }).status;
  if (typeof status !== "number") return true;
  if (status >= 400 && status < 500) return status === 408 || status === 425 || status === 429;
  return true;
}

/** Run one cleanup with its retries; never throws. `true` when it succeeded. */
export async function runCleanupWithRetry(
  cleanup: DeletionCleanup,
  deadline: number,
  deps: Pick<AccountDeletionDeps, "sleep" | "now" | "log"> = {},
): Promise<boolean> {
  const sleep = deps.sleep ?? defaultSleep;
  const now = deps.now ?? Date.now;
  const log = deps.log ?? defaultLog;
  for (let attempt = 0; ; attempt += 1) {
    try {
      await cleanup.run();
      return true;
    } catch (err) {
      const retryable = cleanup.retryable?.(err) ?? true;
      const wait = DELETION_CLEANUP_BACKOFF_MS[attempt] ?? 0;
      if (!retryable || attempt >= DELETION_CLEANUP_RETRIES || now() + wait >= deadline) {
        log("[account] delete cleanup failed", cleanup.label, errorName(err));
        return false;
      }
      await sleep(wait);
    }
  }
}

/** Run every cleanup, a few at a time, waiting at most until `deadline`.
 *  Never throws. A cleanup still running at the deadline is left to finish
 *  on its own. */
export async function runCleanups(
  cleanups: readonly DeletionCleanup[],
  deadline: number,
  deps: Pick<AccountDeletionDeps, "sleep" | "now" | "log"> = {},
): Promise<void> {
  const now = deps.now ?? Date.now;
  let next = 0;
  const worker = async (): Promise<void> => {
    while (next < cleanups.length) {
      const cleanup = cleanups[next];
      next += 1;
      if (cleanup === undefined) return;
      if (now() >= deadline) {
        (deps.log ?? defaultLog)("[account] delete cleanup skipped", cleanup.label, "deadline");
        continue;
      }
      await runCleanupWithRetry(cleanup, deadline, deps);
    }
  };
  const workers = Array.from(
    { length: Math.min(DELETION_CLEANUP_CONCURRENCY, cleanups.length) },
    () => worker(),
  );
  const remaining = Math.max(0, deadline - now());
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<void>((resolve) => {
    timer = setTimeout(resolve, remaining);
  });
  try {
    await Promise.race([Promise.all(workers), timeout]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

/** The whole deletion: auth first, then the cleanups, then the wipe. */
export async function runAccountDeletion(deps: AccountDeletionDeps): Promise<void> {
  if (deps.beforeAuth !== undefined) {
    try {
      await deps.beforeAuth();
    } catch (err) {
      (deps.log ?? defaultLog)("[account] delete preparation failed", "tokens", errorName(err));
    }
  }
  await deps.deleteAtAuth();
  try {
    deps.afterAuth?.();
  } catch (err) {
    (deps.log ?? defaultLog)("[account] delete after-auth step failed", "session", errorName(err));
  }
  const now = deps.now ?? Date.now;
  const deadline = now() + (deps.budgetMs ?? DELETION_CLEANUP_BUDGET_MS);
  if (deps.firstCleanups !== undefined) {
    let first: readonly DeletionCleanup[] = [];
    try {
      first = await deps.firstCleanups();
    } catch (err) {
      (deps.log ?? defaultLog)("[account] delete cleanup list failed", "first", errorName(err));
    }
    await runCleanups(first, deadline, deps);
  }
  let cleanups: readonly DeletionCleanup[] = [];
  try {
    cleanups = await deps.cleanups();
  } catch (err) {
    (deps.log ?? defaultLog)("[account] delete cleanup list failed", "list", errorName(err));
  }
  await runCleanups(cleanups, deadline, deps);
  await deps.wipe();
}
