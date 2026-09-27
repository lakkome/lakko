// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0053 / ADR-0028 OQ2 — when the members sheet may re-read the join
 * attributions.
 *
 * An attribution arrives in the knock queue, and reading it means reading the
 * WHOLE queue from its start (`ChatRepository.fetchJoinAttributions`). Doing
 * that on every refresh bypasses the knock poll's own floor. The rule, the
 * same on web, iOS and Android:
 *
 *   - an EMPTY pending set clears the map with no fetch;
 *   - a pending set equal to the one the current map was fetched for keeps
 *     the map with no fetch;
 *   - otherwise fetch, but never more than once per
 *     [JOIN_ATTRIBUTION_MIN_INTERVAL_MS] per group; inside that window the
 *     fetch is deferred to the window's end.
 */

/** At most one attribution read per group in this window. */
public const val JOIN_ATTRIBUTION_MIN_INTERVAL_MS: Long = 10_000L

/** What a members-sheet refresh does about the attribution map. */
public sealed interface JoinAttributionDecision {
    /** No pending request: empty the map, fetch nothing. */
    public data object Clear : JoinAttributionDecision

    /** The map already describes these requests. */
    public data object Keep : JoinAttributionDecision

    /** Read the attributions now. */
    public data object Fetch : JoinAttributionDecision

    /** The set changed but the group was read less than the floor ago:
     *  re-evaluate after [waitMs]. */
    public data class Defer(val waitMs: Long) : JoinAttributionDecision
}

/**
 * Decide for one refresh. [pending] is the current set of pending joiner keys
 * (hex), [fetchedFor] the set the current map was fetched for (`null` when
 * none was), [lastFetchMs] when this group's attributions were last read
 * (`null` when never).
 */
public fun joinAttributionDecision(
    pending: Set<String>,
    fetchedFor: Set<String>?,
    lastFetchMs: Long?,
    nowMs: Long,
): JoinAttributionDecision {
    if (pending.isEmpty()) return JoinAttributionDecision.Clear
    if (pending == fetchedFor) return JoinAttributionDecision.Keep
    if (lastFetchMs != null) {
        val elapsed = nowMs - lastFetchMs
        // A clock that moved backwards reads as "just fetched": wait a full
        // window rather than read in a loop.
        if (elapsed < 0) return JoinAttributionDecision.Defer(JOIN_ATTRIBUTION_MIN_INTERVAL_MS)
        if (elapsed < JOIN_ATTRIBUTION_MIN_INTERVAL_MS) {
            return JoinAttributionDecision.Defer(JOIN_ATTRIBUTION_MIN_INTERVAL_MS - elapsed)
        }
    }
    return JoinAttributionDecision.Fetch
}

/** Per-group time of the last attribution read, process-wide, so two
 *  screens on one group share the floor. */
internal object JoinAttributionReads {
    private val last = java.util.concurrent.ConcurrentHashMap<String, Long>()

    fun lastFetchMs(groupIdHex: String): Long? = last[groupIdHex.lowercase()]

    fun markFetched(groupIdHex: String, nowMs: Long) {
        last[groupIdHex.lowercase()] = nowMs
    }
}
