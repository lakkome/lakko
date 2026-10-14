// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The in-memory half of Forget device: one registry of every page-lifetime
 * cache that holds an account's data and later writes it to a store.
 *
 * ## The defect this closes
 *
 * The store gate (`account-store-gate.ts`) refuses a write of a wiped
 * account's data until that account lands again. It cannot see memory. A
 * module that keeps an account's rows in memory (a cache read once and saved
 * back after an edit, a pending write waiting for its port, records merged
 * into the next save) still holds the pre-wipe rows after the wipe, and when
 * the same account signs in again on the same page the gate is open and the
 * next save writes them back: a forgotten account's data on the device again.
 * Work suspended on an `await` across the wipe can also refill such a cache
 * after the wipe step that cleared it.
 *
 * ## The rule
 *
 * A module with such a cache registers a drop here, at module load, once.
 * The gate calls {@link dropAccountMemory} at two points:
 *
 * - when the wipe seals the account (before its first delete), so nothing in
 *   memory outlives the deletes;
 * - when an account that was sealed by a wipe (here or, through the marker,
 *   in another tab) lands again, before its first write, so whatever in-flight
 *   work put back in memory in between is gone too.
 *
 * A drop receives every form the account's rows are filed under (the standard
 * base64 id, and the base64url form where it differs) and drops only what
 * belongs to one of them. A cache that is not keyed by account (it holds the
 * session's data) checks the account it was filled for where it records one,
 * and otherwise drops when it has no owner at all.
 *
 * A plain sign-out keeps the account's local data, so it calls only the drops
 * registered with `signOut: true`: caches that are cheap to re-read and would
 * otherwise outlive the session. A pending write of the account's own intent
 * (a rename, a recovery-address register) is not one of them: dropping it at
 * sign-out would lose a change the server already holds.
 *
 * A drop must be synchronous and must not throw; one that throws is logged by
 * name and the others still run.
 */

/** Why the memory is dropped. */
export type AccountMemoryReason = "wipe" | "sign-out";

/** Drop whatever the cache holds for any of `ids` (the forms of one account). */
export type AccountMemoryDrop = (ids: readonly string[]) => void;

interface Registration {
  readonly drop: AccountMemoryDrop;
  readonly signOut: boolean;
}

const registry = new Map<string, Registration>();

/**
 * Register the drop of one cache. `name` is plain ASCII (logged on failure);
 * registering the same name again replaces the earlier drop. Returns the
 * unregister, for tests.
 */
export function registerAccountMemory(
  name: string,
  drop: AccountMemoryDrop,
  options: { readonly signOut?: boolean } = {},
): () => void {
  const registration: Registration = { drop, signOut: options.signOut === true };
  registry.set(name, registration);
  return () => {
    if (registry.get(name) === registration) registry.delete(name);
  };
}

/** Run the registered drops for the account whose forms are `ids`. Never
 *  throws. Returns the names of the drops that threw. */
export function dropAccountMemory(
  ids: readonly string[],
  reason: AccountMemoryReason = "wipe",
): string[] {
  const forms = ids.filter((id) => id.length > 0);
  const failed: string[] = [];
  if (forms.length === 0) return failed;
  for (const [name, registration] of registry) {
    if (reason === "sign-out" && !registration.signOut) continue;
    try {
      registration.drop(forms);
    } catch (err) {
      failed.push(name);
      // eslint-disable-next-line no-console
      console.warn(
        `[account-memory] ${name} failed: ${err instanceof Error ? err.name : "unknown error"}`,
      );
    }
  }
  return failed;
}

/** The names registered now (for the inventory test). */
export function registeredAccountMemory(): string[] {
  return [...registry.keys()].sort();
}
