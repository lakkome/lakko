// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Last-seen transparency-log root store (KT-log S3c, no-rollback gate).
 *
 * The transparency log is GLOBAL and append-only — its `log_size`
 * only ever grows. We persist the highest `log_size` this client has
 * ever observed (one global value, not per-account) so that a root
 * whose `log_size` is SMALLER than what we already saw is caught as an
 * unambiguous rollback attack: the log appears to have shrunk, which a
 * correct append-only log can never do.
 *
 * This is the gossip/consistency half of the equivocation defence — a
 * server that tries to show an old, smaller tree (to hide a leaf it
 * later appended) trips the rollback check. The store NEVER advances
 * on a shrink; it only advances on `newLogSize >= seen`.
 *
 * The value lives in `localStorage` and is not secret (a public tree
 * size). Stored as a decimal string so an arbitrarily large `u64`
 * survives a `JSON`-free round-trip without `Number` precision loss —
 * `log_size` is a `bigint` end to end.
 */

const STORAGE_KEY = "lakko.transparency.last-seen-root.v1";

/** Outcome of [`checkAndAdvance`]. */
export type RootCheckResult =
  /** `newLogSize >= seen`: the value was persisted (advanced or equal).
   *  The append-only invariant holds; proceed with the inclusion check. */
  | "ok"
  /** `newLogSize < seen`: the log shrank — an unambiguous rollback
   *  attack. The store was NOT advanced. The caller treats this as a
   *  hard failure. */
  | "rollback";

/** Read the highest `log_size` ever seen, or `0n` when none persisted.
 *  Exposed for tests + diagnostics. */
export function loadLastSeenLogSize(): bigint {
  if (typeof localStorage === "undefined") return 0n;
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw === null) return 0n;
  try {
    const v = BigInt(raw);
    return v >= 0n ? v : 0n;
  } catch {
    // Malformed row — treat as "never seen" rather than mis-trusting it.
    return 0n;
  }
}

/**
 * Compare a freshly-fetched root's `log_size` against the persisted
 * high-water mark, advancing it on a non-shrinking value.
 *
 * Semantics:
 *   * `newLogSize >= seen` → persist `newLogSize` (a `==` is an
 *     idempotent re-observation; a `>` is normal growth). Returns
 *     `"ok"`.
 *   * `newLogSize < seen`  → the log shrank. **Refuse** — leave the
 *     stored value untouched and return `"rollback"`. NEVER advance
 *     (or regress) on a shrink.
 *
 * @param newLogSize  the `log_size` from the just-fetched signed root.
 */
export function checkAndAdvance(newLogSize: bigint): RootCheckResult {
  const seen = loadLastSeenLogSize();
  if (newLogSize < seen) {
    return "rollback";
  }
  if (typeof localStorage !== "undefined") {
    // Persist only when it actually moves forward to avoid a redundant
    // write on every steady-state observation.
    if (newLogSize > seen) {
      localStorage.setItem(STORAGE_KEY, newLogSize.toString());
    }
  }
  return "ok";
}

/** Wipe the stored marker. For the "forget device" flow + tests. */
export function clearLastSeenRoot(): void {
  if (typeof localStorage === "undefined") return;
  localStorage.removeItem(STORAGE_KEY);
}
