// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

/**
 * `docs/ux/who-can-find-me.md` section 6.1 — a text asked in the last 60
 * seconds is not asked again. A small process-wide memory of answers by text,
 * each kept for [ttlMs] and at most [capacity] of them, the oldest dropped
 * first. Only answers are kept: a failed question is asked again.
 *
 * Used for the `@name` fields (group and channel creation, the @name change):
 * the taken check while typing and the OPRF evaluation a claim needs share it,
 * so the Create after a check spends no second lookup token.
 */
public class RecentAnswers<V>(
    private val ttlMs: Long = RECENT_ANSWER_TTL_MS,
    private val capacity: Int = 64,
    private val now: () -> Long = System::currentTimeMillis,
) {
    private val entries = LinkedHashMap<String, Pair<Long, V>>()

    /** The answer kept for [key], or `null` when none is younger than [ttlMs]. */
    @Synchronized
    public fun get(key: String): V? {
        val (at, value) = entries[key] ?: return null
        if (now() - at >= ttlMs || now() < at) {
            entries.remove(key)
            return null
        }
        return value
    }

    @Synchronized
    public fun put(key: String, value: V) {
        entries.remove(key)
        entries[key] = now() to value
        while (entries.size > capacity) entries.remove(entries.keys.first())
    }

    @Synchronized
    public fun forget(key: String) {
        entries.remove(key)
    }

    /** The kept answer, else [ask]'s, kept when it returns. */
    public suspend fun getOrAsk(key: String, ask: suspend () -> V): V {
        get(key)?.let { return it }
        val value = ask()
        put(key, value)
        return value
    }
}

/** Section 6.1 — how long an answer for a text is reused. Chosen, not measured. */
public const val RECENT_ANSWER_TTL_MS: Long = 60_000L
