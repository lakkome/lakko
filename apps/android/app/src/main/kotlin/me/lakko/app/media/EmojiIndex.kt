// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import java.text.BreakIterator

/**
 * ADR-0047 §3 / UX spec §4 — the emoji picker's data model, search, the
 * per-device Recent row and skin-tone defaults, and the one-grapheme check a
 * reaction must pass before it is sent.
 *
 * Port of `apps/web/src/media/emoji/emoji-index.ts`. The dataset is the SAME
 * file the web client bundles (`apps/web/src/media/emoji/emoji-data.json`,
 * generated offline from pinned Unicode 15.1 + CLDR 45 sources), copied into
 * this app's assets at build time by the `copyEmojiData` Gradle task — so the
 * three clients share one emoji set and nothing here touches the network.
 */

/** Row shape written by the generator: `[emoji, name, "kw|kw", group]`,
 *  optionally followed by the five single-tone variants. */
public data class EmojiEntry(
    val emoji: String,
    val name: String,
    val keywords: List<String>,
    val group: Int,
    /** light, medium-light, medium, medium-dark, dark — present only when all
     *  five exist. */
    val tones: List<String>?,
)

public class EmojiDataset(
    public val groups: List<String>,
    public val entries: List<EmojiEntry>,
) {
    /** Every base emoji AND every tone variant, mapped to its base entry. */
    public val byEmoji: Map<String, EmojiEntry> = buildMap {
        for (entry in this@EmojiDataset.entries) {
            put(entry.emoji, entry)
            for (t in entry.tones.orEmpty()) put(t, entry)
        }
    }

    public fun entriesInGroup(group: Int): List<EmojiEntry> = entries.filter { it.group == group }

    public companion object {
        public val EMPTY: EmojiDataset = EmojiDataset(emptyList(), emptyList())
    }
}

private val datasetJson = Json { ignoreUnknownKeys = true }

/** Build the dataset, skipping any row that does not have the expected shape
 *  rather than failing the picker. A body that is not the dataset at all is an
 *  empty dataset (the picker then shows `No emoji found`). */
public fun parseEmojiDataset(text: String): EmojiDataset {
    val root = try {
        datasetJson.parseToJsonElement(text) as? JsonObject
    } catch (_: Throwable) {
        null
    } ?: return EmojiDataset.EMPTY
    val groups = (root["groups"] as? JsonArray)
        ?.mapNotNull { (it as? JsonPrimitive)?.takeIf { p -> p.isString }?.contentOrNull }
        ?: return EmojiDataset.EMPTY
    val rows = root["emoji"] as? JsonArray ?: return EmojiDataset.EMPTY
    val entries = rows.mapNotNull { row ->
        val arr = row as? JsonArray ?: return@mapNotNull null
        if (arr.size < 4) return@mapNotNull null
        val emoji = (arr[0] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull
        val name = (arr[1] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull
        val keywords = (arr[2] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull
        val group = (arr[3] as? JsonPrimitive)?.takeIf { !it.isString }?.intOrNull
        if (emoji == null || name == null || keywords == null || group == null ||
            group < 0 || group >= groups.size
        ) {
            return@mapNotNull null
        }
        val tones = (arr.getOrNull(4) as? JsonArray)
            ?.map { (it as? JsonPrimitive)?.takeIf { p -> p.isString }?.contentOrNull }
            ?.takeIf { list -> list.size == 5 && list.all { it != null } }
            ?.map { it!! }
        EmojiEntry(
            emoji = emoji,
            name = name,
            keywords = if (keywords.isEmpty()) emptyList() else keywords.split('|'),
            group = group,
            tones = tones,
        )
    }
    return EmojiDataset(groups, entries)
}

private val WORD_SPLIT = Regex("[\\s:,.()\\-_'’]+")

private fun words(text: String): List<String> =
    text.lowercase().split(WORD_SPLIT).filter { it.isNotEmpty() }

/**
 * Entries matching [query], best first. Every query token must be a prefix of
 * a word in the name or of a keyword. Ranking: exact name, then a name that
 * starts with the query, then a name-word match, then keyword-only matches;
 * ties keep the dataset's CLDR order. An empty query matches nothing (the
 * picker shows categories instead).
 */
public fun searchEmoji(dataset: EmojiDataset, query: String, limit: Int = 200): List<EmojiEntry> {
    val q = query.trim().lowercase()
    if (q.isEmpty()) return emptyList()
    val tokens = words(q)
    if (tokens.isEmpty()) return emptyList()
    data class Scored(val entry: EmojiEntry, val score: Int, val order: Int)
    val scored = ArrayList<Scored>()
    dataset.entries.forEachIndexed { order, entry ->
        val name = entry.name.lowercase()
        val nameWords = words(name)
        val keywordWords = entry.keywords.flatMap(::words)
        var allInName = true
        for (t in tokens) {
            val inName = nameWords.any { it.startsWith(t) }
            if (!inName) allInName = false
            if (!inName && keywordWords.none { it.startsWith(t) }) return@forEachIndexed
        }
        val score = when {
            name == q -> 0
            name.startsWith(q) -> 1
            allInName -> 2
            else -> 3
        }
        scored.add(Scored(entry, score, order))
    }
    return scored
        .sortedWith(compareBy<Scored> { it.score }.thenBy { it.order })
        .take(maxOf(0, limit))
        .map { it.entry }
}

// ----- per-account preferences (pure half; storage is MediaPrefs) ------------

/** §4 — the Recent row holds up to this many. */
public const val MAX_RECENT_EMOJI: Int = 24

/** Move [emoji] to the front, de-duplicated, capped at [max]. */
public fun pushRecent(list: List<String>, emoji: String, max: Int = MAX_RECENT_EMOJI): List<String> =
    (listOf(emoji) + list.filter { it != emoji }).take(maxOf(0, max))

public fun encodeRecent(list: List<String>): String =
    JsonArray(list.take(MAX_RECENT_EMOJI).map { JsonPrimitive(it) }).toString()

/** Decode a stored Recent row. Anything that is not a single emoji grapheme is
 *  dropped, so a corrupted store can never smuggle text into a reaction. */
public fun decodeRecent(stored: String?): List<String> {
    if (stored == null) return emptyList()
    val arr = try {
        datasetJson.parseToJsonElement(stored) as? JsonArray
    } catch (_: Throwable) {
        null
    } ?: return emptyList()
    return arr
        .mapNotNull { (it as? JsonPrimitive)?.takeIf { p -> p.isString }?.contentOrNull }
        .filter(::isSingleEmojiGrapheme)
        .take(MAX_RECENT_EMOJI)
}

/** Base emoji -> the tone variant the signed-in account chose for it. */
public typealias TonePrefs = Map<String, String>

/** Record [variant] as the default for [entry]. Choosing the base emoji itself
 *  (or anything that is not one of its tones) clears the preference. */
public fun withTonePref(prefs: TonePrefs, entry: EmojiEntry, variant: String): TonePrefs {
    val next = prefs.toMutableMap()
    if (variant == entry.emoji || variant !in entry.tones.orEmpty()) {
        next.remove(entry.emoji)
    } else {
        next[entry.emoji] = variant
    }
    return next
}

public fun encodeTonePrefs(prefs: TonePrefs): String =
    JsonObject(prefs.mapValues { JsonPrimitive(it.value) }).toString()

public fun decodeTonePrefs(stored: String?): TonePrefs {
    if (stored == null) return emptyMap()
    val obj = try {
        datasetJson.parseToJsonElement(stored) as? JsonObject
    } catch (_: Throwable) {
        null
    } ?: return emptyMap()
    return obj.mapNotNull { (k, v) ->
        (v as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull?.let { k to it }
    }.toMap()
}

/** What the grid shows (and inserts) for [entry] on this device. */
public fun displayEmoji(entry: EmojiEntry, prefs: TonePrefs): String {
    val pref = prefs[entry.emoji]
    return if (pref != null && pref in entry.tones.orEmpty()) pref else entry.emoji
}

// ----- reaction validation ---------------------------------------------------

/** `lakko-attachments::MAX_EMOJI_BYTES` — the wire bound on a reaction. */
public const val MAX_EMOJI_BYTES: Int = 64

/**
 * UX spec §4 — "A reaction is exactly one emoji grapheme". True when [text] is
 * one extended grapheme cluster that is an emoji (a pictographic sequence, a
 * regional-indicator flag or a keycap) and fits the wire bound. A letter, two
 * emoji, an emoji with trailing text, or an empty string are all refused.
 *
 * Grapheme segmentation is `java.text.BreakIterator`, which is ICU-backed on
 * Android and follows the extended grapheme cluster rules (emoji ZWJ
 * sequences, flags, modifiers) on the host JDK the unit tests run on.
 * Pictographic classification uses [isExtendedPictographic], a table of the
 * Unicode 15.1 `Extended_Pictographic` ranges, because neither
 * `Character.isExtendedPictographic` (JDK 21) nor the ICU `UProperty` constant
 * exists at this app's minimum API level.
 */
public fun isSingleEmojiGrapheme(text: String): Boolean {
    if (text.isEmpty()) return false
    if (text.toByteArray(Charsets.UTF_8).size > MAX_EMOJI_BYTES) return false
    if (graphemeCount(text) != 1) return false
    val cps = text.codePoints().toArray()
    if (isFlag(cps) || isKeycap(cps)) return true
    if (cps.none(::isExtendedPictographic)) return false
    // A pictograph glued to letters or digits in one cluster (possible with a
    // combining sequence) is not an emoji the picker could produce.
    return cps.none { !isExtendedPictographic(it) && Character.isLetterOrDigit(it) }
}

internal fun graphemeCount(text: String): Int {
    val it = BreakIterator.getCharacterInstance()
    it.setText(text)
    var count = 0
    var end = it.next()
    while (end != BreakIterator.DONE) {
        count += 1
        end = it.next()
    }
    return count
}

private fun isRegionalIndicator(cp: Int): Boolean = cp in 0x1F1E6..0x1F1FF

private fun isFlag(cps: IntArray): Boolean =
    cps.size == 2 && isRegionalIndicator(cps[0]) && isRegionalIndicator(cps[1])

private fun isKeycap(cps: IntArray): Boolean {
    if (cps.isEmpty()) return false
    val base = cps[0]
    val baseOk = base == '#'.code || base == '*'.code || base in '0'.code..'9'.code
    return baseOk && when (cps.size) {
        2 -> cps[1] == 0x20E3
        3 -> cps[1] == 0xFE0F && cps[2] == 0x20E3
        else -> false
    }
}

/** Unicode 15.1 `emoji-data.txt` `Extended_Pictographic`, as sorted inclusive
 *  ranges. */
private val EXTENDED_PICTOGRAPHIC: IntArray = intArrayOf(
    0x00A9, 0x00A9, 0x00AE, 0x00AE, 0x203C, 0x203C, 0x2049, 0x2049,
    0x2122, 0x2122, 0x2139, 0x2139, 0x2194, 0x2199, 0x21A9, 0x21AA,
    0x231A, 0x231B, 0x2328, 0x2328, 0x2388, 0x2388, 0x23CF, 0x23CF,
    0x23E9, 0x23F3, 0x23F8, 0x23FA, 0x24C2, 0x24C2, 0x25AA, 0x25AB,
    0x25B6, 0x25B6, 0x25C0, 0x25C0, 0x25FB, 0x25FE, 0x2600, 0x2605,
    0x2607, 0x2612, 0x2614, 0x2685, 0x2690, 0x2705, 0x2708, 0x2712,
    0x2714, 0x2714, 0x2716, 0x2716, 0x271D, 0x271D, 0x2721, 0x2721,
    0x2728, 0x2728, 0x2733, 0x2734, 0x2744, 0x2744, 0x2747, 0x2747,
    0x274C, 0x274C, 0x274E, 0x274E, 0x2753, 0x2755, 0x2757, 0x2757,
    0x2763, 0x2767, 0x2795, 0x2797, 0x27A1, 0x27A1, 0x27B0, 0x27B0,
    0x27BF, 0x27BF, 0x2934, 0x2935, 0x2B05, 0x2B07, 0x2B1B, 0x2B1C,
    0x2B50, 0x2B50, 0x2B55, 0x2B55, 0x3030, 0x3030, 0x303D, 0x303D,
    0x3297, 0x3297, 0x3299, 0x3299,
    0x1F000, 0x1F0FF, 0x1F10D, 0x1F10F, 0x1F12F, 0x1F12F, 0x1F16C, 0x1F171,
    0x1F17E, 0x1F17F, 0x1F18E, 0x1F18E, 0x1F191, 0x1F19A, 0x1F1AD, 0x1F1E5,
    0x1F201, 0x1F20F, 0x1F21A, 0x1F21A, 0x1F22F, 0x1F22F, 0x1F232, 0x1F23A,
    0x1F23C, 0x1F23F, 0x1F249, 0x1F3FA, 0x1F400, 0x1F53D, 0x1F546, 0x1F64F,
    0x1F680, 0x1F6FF, 0x1F774, 0x1F77F, 0x1F7D5, 0x1F7FF, 0x1F80C, 0x1F80F,
    0x1F848, 0x1F84F, 0x1F85A, 0x1F85F, 0x1F888, 0x1F88F, 0x1F8AE, 0x1F8FF,
    0x1F90C, 0x1F93A, 0x1F93C, 0x1F945, 0x1F947, 0x1FAFF, 0x1FC00, 0x1FFFD,
)

internal fun isExtendedPictographic(cp: Int): Boolean {
    var lo = 0
    var hi = EXTENDED_PICTOGRAPHIC.size / 2 - 1
    while (lo <= hi) {
        val mid = (lo + hi) ushr 1
        val start = EXTENDED_PICTOGRAPHIC[mid * 2]
        val end = EXTENDED_PICTOGRAPHIC[mid * 2 + 1]
        when {
            cp < start -> hi = mid - 1
            cp > end -> lo = mid + 1
            else -> return true
        }
    }
    return false
}
