// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a running tab saves before it hands Lakko to another tab
 * (docs/ux/one-tab.md section 4): the open composer's draft, then the
 * account's synced state that holds it, so nothing typed is lost.
 *
 * Savers register under a phase; phases run in order (a draft is folded into
 * the synced state in phase 0 and written to disk in phase 1), the savers of
 * one phase side by side. The whole run is bounded: past the deadline the tab
 * reloads anyway, and a saver that throws does not stop the others.
 */

/** Longest a takeover waits for the savers. Chosen, not measured. */
export const BEFORE_YIELD_DEADLINE_MS = 1_000;

/** The composer drafts. */
export const PHASE_DRAFTS = 0;
/** The synced state that holds them, written to disk. */
export const PHASE_SYNCED_STATE = 1;

const savers = new Map<number, Set<() => Promise<void>>>();

/** Register a saver; returns its unregister. */
export function registerBeforeYield(phase: number, fn: () => Promise<void>): () => void {
  let set = savers.get(phase);
  if (set === undefined) {
    set = new Set();
    savers.set(phase, set);
  }
  set.add(fn);
  return () => {
    savers.get(phase)?.delete(fn);
  };
}

/**
 * Run every saver, phase by phase, for at most `deadlineMs`. Resolves `true`
 * when all finished in time, `false` on the deadline. Never rejects.
 */
export async function runBeforeYield(
  deadlineMs: number = BEFORE_YIELD_DEADLINE_MS,
  setTimer: (fn: () => void, ms: number) => unknown = (fn, ms) => setTimeout(fn, ms),
): Promise<boolean> {
  const all = (async (): Promise<boolean> => {
    for (const phase of [...savers.keys()].sort((a, b) => a - b)) {
      const fns = [...(savers.get(phase) ?? [])];
      await Promise.all(
        fns.map(async (fn) => {
          try {
            await fn();
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("one tab: a save before the takeover failed", err instanceof Error ? err.name : "unknown");
          }
        }),
      );
    }
    return true;
  })();
  const late = new Promise<boolean>((resolve) => {
    setTimer(() => resolve(false), deadlineMs);
  });
  return Promise.race([all, late]);
}

/** Tests only. */
export function resetBeforeYieldForTests(): void {
  savers.clear();
}
