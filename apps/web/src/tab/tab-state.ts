// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether this page may still write the account's state (docs/ux/one-tab.md).
 *
 * One Lakko tab per browser runs at a time (`active-tab.ts`). When another tab
 * takes over, this page FREEZES before it reloads: from that moment nothing it
 * still has in flight may save the MLS provider or anything that is published
 * after such a save. The freeze is a flag read synchronously by the writers
 * (`chat/mls-store.ts` checks it on the line before it opens a transaction),
 * so a write already past that line completes and every later one is refused.
 *
 * No DOM, no imports: the stores and the tests read it without loading the app.
 */

/** A write refused because this page no longer runs Lakko. */
export class TabInactiveError extends Error {
  override name = "TabInactiveError";
  constructor() {
    super("Lakko runs in another tab: this page no longer writes");
  }
}

let frozen = false;
const listeners = new Set<() => void>();

/** Has this page stopped writing? */
export function isTabFrozen(): boolean {
  return frozen;
}

/** Throw {@link TabInactiveError} when this page stopped writing. */
export function requireTabActive(): void {
  if (frozen) throw new TabInactiveError();
}

/**
 * Stop every write from this page, for good (the page reloads next). Runs the
 * listeners once; a second call is a no-op.
 */
export function freezeTab(): void {
  if (frozen) return;
  frozen = true;
  for (const fn of [...listeners]) {
    try {
      fn();
    } catch {
      /* a listener's failure must not stop the freeze */
    }
  }
}

/** Observe the freeze (the page reload is wired this way). */
export function onTabFrozen(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** Tests only: thaw. */
export function resetTabStateForTests(): void {
  frozen = false;
  listeners.clear();
}
