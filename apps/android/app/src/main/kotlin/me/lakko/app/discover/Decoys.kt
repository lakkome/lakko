// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import java.time.Instant
import java.time.ZoneOffset

/**
 * Which directory buckets a Discover refresh fetches (ADR-0059 section 5,
 * open question 1 as decided on 2026-09-29). Mirror of
 * `apps/web/src/discover/decoys.ts`.
 *
 * A bucket is one (topic, language) pair. The person's picked topics are
 * fetched in rounds of [ROUND_SIZE]; unless "Mix in random topics" is off,
 * every round is padded with decoy topics in the same language and shuffled,
 * so the server sees eight buckets from an address and cannot tell which were
 * picked.
 *
 * The decoys are a **stable random set per device per month**: one random
 * order of every topic is drawn when the month (or the language) changes and
 * reused on every refresh of that month, so the picked buckets and the decoys
 * both repeat every day. A change of the picked topics stays visible.
 *
 * Pure: the caller supplies the clock and the random source.
 */
public object Decoys {
    /** Buckets per round (ADR-0059 section 5, a chosen number). */
    public const val ROUND_SIZE: Int = 8

    /** A refresh at most this often (once a day). */
    public const val REFRESH_INTERVAL_MS: Long = 24L * 60L * 60L * 1000L
}

/** The decoy order this device uses for one month and one language. */
public data class DecoyDraw(
    /** `YYYY-MM`, UTC. */
    val month: String,
    val language: String,
    /** Every topic id, in a random order drawn once for the month. */
    val order: List<Int>,
)

/** `YYYY-MM` of [nowMs], UTC. */
public fun monthKey(nowMs: Long): String {
    val d = Instant.ofEpochMilli(nowMs).atZone(ZoneOffset.UTC)
    val m = d.monthValue
    return "${d.year}-${if (m < 10) "0" else ""}$m"
}

/** Fisher-Yates over a copy; [random] returns values in [0, 1). */
public fun <T> shuffled(items: List<T>, random: () -> Double): List<T> {
    val out = items.toMutableList()
    for (i in out.size - 1 downTo 1) {
        val j = minOf(i, kotlin.math.floor(random() * (i + 1)).toInt().coerceAtLeast(0))
        val a = out[i]
        out[i] = out[j]
        out[j] = a
    }
    return out
}

/** The decoy draw to use now, and whether a new draw was made. */
public data class CurrentDecoys(val draw: DecoyDraw, val fresh: Boolean)

/**
 * [held] when it is for this month and language, else a fresh random order of
 * [allTopics]. `fresh` says whether a new draw was made (the caller persists it).
 */
public fun currentDecoys(
    held: DecoyDraw?,
    allTopics: List<Int>,
    language: String,
    nowMs: Long,
    random: () -> Double,
): CurrentDecoys {
    val month = monthKey(nowMs)
    if (held != null && held.month == month && held.language == language) return CurrentDecoys(held, false)
    return CurrentDecoys(DecoyDraw(month, language, shuffled(allTopics, random)), true)
}

/**
 * The rounds of topic ids to fetch. Picked topics are split into rounds of
 * [Decoys.ROUND_SIZE]; with [mixRandom], each round is filled up to the size
 * with the next decoys of the month's order (never a picked topic; the order
 * wraps when a day needs more decoys than there are unpicked topics) and
 * shuffled. Without [mixRandom] a round holds only picked topics. No picked
 * topics, no rounds: the shortlist is not downloaded at all.
 */
public fun planBucketRounds(
    picked: List<Int>,
    decoys: DecoyDraw,
    mixRandom: Boolean,
    random: () -> Double,
): List<List<Int>> {
    val unique = picked.distinct()
    if (unique.isEmpty()) return emptyList()
    val pickedSet = unique.toSet()
    val pool = decoys.order.filter { it !in pickedSet }
    val rounds = mutableListOf<List<Int>>()
    var next = 0
    var i = 0
    while (i < unique.size) {
        val round = unique.subList(i, minOf(i + Decoys.ROUND_SIZE, unique.size)).toMutableList()
        if (mixRandom && pool.isNotEmpty()) {
            val want = minOf(Decoys.ROUND_SIZE - round.size, pool.size)
            val chosen = LinkedHashSet<Int>()
            while (chosen.size < want) {
                chosen += pool[next % pool.size]
                next += 1
            }
            round += chosen
        }
        rounds += shuffled(round, random)
        i += Decoys.ROUND_SIZE
    }
    return rounds
}

/** Whether the shortlist is due: never fetched, a day old, a clock that went
 *  backwards, or a different set of picked buckets than the one last fetched. */
public fun refreshDue(lastRefreshMs: Long?, lastFetchedKey: String?, pickedKey: String, nowMs: Long): Boolean {
    if (lastRefreshMs == null || lastFetchedKey != pickedKey) return true
    return nowMs - lastRefreshMs >= Decoys.REFRESH_INTERVAL_MS || nowMs < lastRefreshMs
}

/** A stable key for a set of picked topics in a language. */
public fun pickedKey(picked: List<Int>, language: String): String =
    "$language:${picked.distinct().sorted().joinToString(",")}"
