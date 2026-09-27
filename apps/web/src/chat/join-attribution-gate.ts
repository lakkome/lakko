// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * When the members panel may re-read a group's join attributions
 * (ADR-0028 OQ2, read from the knock queue since ADR-0053).
 *
 * An attribution only matters for a PENDING external-join request, and the
 * read pages the whole knock queue from the start, so it is taken only when
 * the set of pending joiner keys differs from the set the current attribution
 * map was fetched for, and never more than once per 10 s per group - the
 * same floor as the knock poll (`knock-cursor.ts`). An empty pending set
 * clears the map with no read. Found on iOS: the panel re-read the queue on
 * every poll, some 45 times a minute while it was open; the rule is shared by
 * all three clients.
 *
 * Pure: the clock is handed in.
 */
import { KNOCK_READ_MIN_INTERVAL_MS } from "./knock-cursor";

/** What the caller does now. */
export type AttributionDecision =
  /** Nothing pending: drop the map, read nothing. */
  | { readonly kind: "clear" }
  /** The map already answers this pending set. */
  | { readonly kind: "keep" }
  /** Read now; report the outcome with {@link JoinAttributionGate.fetched}. */
  | { readonly kind: "fetch" }
  /** The set changed but a read happened less than the floor ago: decide
   *  again after `retryInMs`. */
  | { readonly kind: "wait"; readonly retryInMs: number };

function setKey(keysB64: readonly string[]): string {
  return [...new Set(keysB64)].sort().join(",");
}

export class JoinAttributionGate {
  private readonly fetchedFor = new Map<string, string>();
  private readonly lastFetchMs = new Map<string, number>();

  constructor(private readonly minIntervalMs: number = KNOCK_READ_MIN_INTERVAL_MS) {}

  /** Decide for `groupIdB64` given the pending joiner keys now. A `fetch`
   *  decision records the read as taken. */
  decide(groupIdB64: string, pendingKeysB64: readonly string[], nowMs: number): AttributionDecision {
    const wanted = setKey(pendingKeysB64);
    if (wanted === "") {
      this.fetchedFor.set(groupIdB64, "");
      return { kind: "clear" };
    }
    if (this.fetchedFor.get(groupIdB64) === wanted) return { kind: "keep" };
    const last = this.lastFetchMs.get(groupIdB64);
    // A clock that went backwards reads as due, so it can never stop reads.
    if (last !== undefined && nowMs >= last && nowMs - last < this.minIntervalMs) {
      return { kind: "wait", retryInMs: this.minIntervalMs - (nowMs - last) };
    }
    this.lastFetchMs.set(groupIdB64, nowMs);
    return { kind: "fetch" };
  }

  /** Forget which set the map was fetched for (a fresh view holds no map);
   *  the floor on the last read is kept. */
  forget(groupIdB64: string): void {
    this.fetchedFor.delete(groupIdB64);
  }

  /** Record that a read for `pendingKeysB64` succeeded, so the same set is
   *  not read again. A failed read records nothing: the set stays "changed"
   *  and is read again once the floor allows. */
  fetched(groupIdB64: string, pendingKeysB64: readonly string[]): void {
    this.fetchedFor.set(groupIdB64, setKey(pendingKeysB64));
  }
}
