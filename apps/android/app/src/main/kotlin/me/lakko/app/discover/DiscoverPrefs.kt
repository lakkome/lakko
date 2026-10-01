// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import me.lakko.app.l10n.UiText
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.util.Locale

/**
 * Discover's state on this device (ADR-0059, docs/ux/feed.md section 6).
 * Mirror of `apps/web/src/discover/discover-store.ts`: the picked topics and
 * language, the "Mix in random topics" switch, the not-interested channels and
 * hidden topics, the month's decoy draw and the cache of fetched (picked)
 * buckets. Sealed per account by [DiscoverStore]; nothing here leaves the
 * device. Pure codec, host-testable.
 */
public data class DiscoverPrefs(
    val topics: List<Int> = emptyList(),
    /** `null` = not chosen yet; the app language is proposed. */
    val language: String? = null,
    val mixRandom: Boolean = true,
    val notInterested: List<String> = emptyList(),
    val hiddenTopics: List<Int> = emptyList(),
    val decoys: DecoyDraw? = null,
    val lastRefreshMs: Long? = null,
    /** [pickedKey] of the last refresh. */
    val lastFetchedKey: String? = null,
    /** `<topic>:<language>` -> bucket. */
    val buckets: Map<String, CachedBucket> = emptyMap(),
    /** The pick-topics card was answered with "Not now". */
    val introDismissed: Boolean = false,
)

/** One cached bucket (only picked buckets are kept; decoys are discarded). */
public data class CachedBucket(val fetchedAtMs: Long, val items: List<DirectoryItemWire>)

public const val DISCOVER_PREFS_VERSION: Int = 1

/** Most not-interested channels kept (chosen). */
public const val MAX_NOT_INTERESTED: Int = 500

/** The bucket cache key. */
public fun bucketKey(topic: Int, language: String): String = "$topic:$language"

@Serializable
private data class WireDecoys(val month: String = "", val language: String = "", val order: List<Int> = emptyList())

@Serializable
private data class WireBucket(val fetchedAtMs: Long = -1L, val items: List<DirectoryItemWire> = emptyList())

@Serializable
private data class WirePrefs(
    val version: Int = 0,
    val topics: List<Int> = emptyList(),
    val language: String? = null,
    val mixRandom: Boolean = true,
    val notInterested: List<String> = emptyList(),
    val hiddenTopics: List<Int> = emptyList(),
    val decoys: WireDecoys? = null,
    val lastRefreshMs: Long? = null,
    val lastFetchedKey: String? = null,
    val buckets: Map<String, WireBucket> = emptyMap(),
    val introDismissed: Boolean = false,
)

private val prefsJson = Json {
    ignoreUnknownKeys = true
    coerceInputValues = true
}
private val LANG_RE = Regex("^[a-z]{2,3}$")
private val HEX_ID = Regex("^[0-9a-f]{32}$")
private val BUCKET_KEY = Regex("^\\d{1,3}:[a-z]{2,3}$")
private val MONTH = Regex("^\\d{4}-\\d{2}$")

private fun topicOk(t: Int): Boolean = t in 0..255

/** Lenient per field: a malformed field falls back to its default alone. */
public fun decodeDiscoverPrefs(json: String?): DiscoverPrefs {
    if (json == null) return DiscoverPrefs()
    val w = try {
        prefsJson.decodeFromString(WirePrefs.serializer(), json)
    } catch (_: Throwable) {
        return DiscoverPrefs()
    }
    if (w.version != DISCOVER_PREFS_VERSION) return DiscoverPrefs()
    val decoys = w.decoys?.takeIf { d -> MONTH.matches(d.month) && d.order.all(::topicOk) }
        ?.let { DecoyDraw(it.month, it.language, it.order) }
    return DiscoverPrefs(
        topics = w.topics.filter(::topicOk).distinct(),
        language = w.language?.takeIf { LANG_RE.matches(it) },
        mixRandom = w.mixRandom,
        notInterested = w.notInterested.filter { HEX_ID.matches(it) }.takeLast(MAX_NOT_INTERESTED),
        hiddenTopics = w.hiddenTopics.filter(::topicOk).distinct(),
        decoys = decoys,
        lastRefreshMs = w.lastRefreshMs,
        lastFetchedKey = w.lastFetchedKey,
        buckets = w.buckets
            .filter { (k, v) -> BUCKET_KEY.matches(k) && v.fetchedAtMs >= 0 }
            .mapValues { (_, v) ->
                CachedBucket(v.fetchedAtMs, v.items.filter { HEX_ID.matches(it.channel_id) }.take(100))
            },
        introDismissed = w.introDismissed,
    )
}

public fun encodeDiscoverPrefs(p: DiscoverPrefs): String =
    prefsJson.encodeToString(
        WirePrefs.serializer(),
        WirePrefs(
            version = DISCOVER_PREFS_VERSION,
            topics = p.topics,
            language = p.language,
            mixRandom = p.mixRandom,
            notInterested = p.notInterested,
            hiddenTopics = p.hiddenTopics,
            decoys = p.decoys?.let { WireDecoys(it.month, it.language, it.order) },
            lastRefreshMs = p.lastRefreshMs,
            lastFetchedKey = p.lastFetchedKey,
            buckets = p.buckets.mapValues { WireBucket(it.value.fetchedAtMs, it.value.items) },
            introDismissed = p.introDismissed,
        ),
    )

// ----- Languages (mirror of apps/web/src/discover/languages.ts) --------------

/** The languages Discover offers (ISO 639, a chosen set of common ones). */
public val LANGUAGE_CODES: List<String> = listOf(
    "en", "es", "pt", "fr", "de", "it", "nl", "pl", "uk", "ru", "tr", "ar",
    "fa", "hi", "id", "zh", "ja", "ko",
)

/** The app language as an ISO 639 code, `en` when nothing usable. */
public fun appLanguage(tag: String?): String {
    val code = (tag ?: "").lowercase().split('-', '_').firstOrNull().orEmpty()
    return if (LANG_RE.matches(code)) code else "en"
}

/** The offered list, the given languages first when missing from it. */
public fun languageOptions(vararg extra: String?): List<String> {
    val out = LANGUAGE_CODES.toMutableList()
    for (e in extra) if (e != null && LANG_RE.matches(e) && e !in out) out.add(0, e)
    return out
}

/** "English", "Deutsch", ... in the language itself; the code as a fallback. */
public fun languageLabel(code: String): String {
    val name = try {
        Locale.forLanguageTag(code).let { it.getDisplayLanguage(it) }
    } catch (_: Throwable) {
        ""
    }
    if (name.isBlank() || name == code) return code
    return name.replaceFirstChar { it.titlecase(Locale.forLanguageTag(code)) }
}

// ----- Copy (docs/ux/feed.md sections 6 and 7; identical to web) --------------

/**
 * The Discover sentences, as catalog references (ADR-0061): each renders at
 * composition, so a language switch applies live.
 */
public object DiscoverCopy {
    public val PICK_TITLE: UiText = UiText("discover.pick.title")
    public val PICK_BODY: UiText = UiText("discover.pick.body")
    public val MIX_LABEL: UiText = UiText("discover.mix.label")
    public val MIX_OFF_LINE: UiText = UiText("discover.mix.off_line")
    public val SEARCH_HELPER: UiText = UiText("discover.search.helper")
    public val UNAVAILABLE: UiText = UiText("discover.unavailable")
    public val NO_RESULTS: UiText = UiText("discover.search.no_results")
    public val GONE: UiText = UiText("discover.gone")
    public val SEARCH_TOO_SHORT: UiText = UiText("discover.search.too_short")
    public val NOTHING_YET: UiText = UiText("discover.nothing_yet")

    public val LISTING_REFUSED: UiText = UiText("discover.listing.refused")
    public val LISTING_CONFIRM: UiText = UiText("discover.listing.confirm_body")
    public val LISTING_NEEDS_HANDLE: UiText = UiText("discover.listing.needs_handle")

    public val SHARE_CHANNELS_LABEL: UiText = UiText("discover.share.label")
    public val SHARE_CHANNELS_LINE: UiText = UiText("discover.share.line")
}

/**
 * The catalog key of a directory topic's name, by its id (ADR-0059). The
 * core's topic list is English and only supplies the ids; an id this table
 * does not know reads "Other".
 */
public fun topicKey(id: Int): String = when (id) {
    1 -> "discover.topic.news"
    2 -> "discover.topic.technology"
    3 -> "discover.topic.science"
    4 -> "discover.topic.business"
    5 -> "discover.topic.finance"
    6 -> "discover.topic.politics"
    7 -> "discover.topic.sports"
    8 -> "discover.topic.gaming"
    9 -> "discover.topic.music"
    10 -> "discover.topic.film_tv"
    11 -> "discover.topic.books"
    12 -> "discover.topic.art_design"
    13 -> "discover.topic.photography"
    14 -> "discover.topic.food"
    15 -> "discover.topic.travel"
    16 -> "discover.topic.health"
    17 -> "discover.topic.education"
    18 -> "discover.topic.humour"
    19 -> "discover.topic.lifestyle"
    20 -> "discover.topic.nature"
    21 -> "discover.topic.religion"
    22 -> "discover.topic.local"
    23 -> "discover.topic.software_development"
    else -> "discover.topic.other"
}

/** A directory topic's name, by its id ([topicKey]). */
public fun topicName(id: Int): UiText = UiText(topicKey(id))

/** A stamp strictly after the stored one (the listing's `updated_at_ms`). */
public fun nextListingStamp(nowMs: Long, stored: ULong?): ULong {
    val now = nowMs.coerceAtLeast(0L).toULong()
    if (stored == null || now > stored) return now
    return stored + 1uL
}
