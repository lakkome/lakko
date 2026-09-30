// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

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

public object DiscoverCopy {
    public const val PICK_TITLE: String = "Pick a few topics."
    public const val PICK_BODY: String =
        "Lakko downloads suggestions for them, mixed with a few random topics so Lakko's server can't tell exactly which ones you picked. It never sees which channels you open."
    public const val MIX_LABEL: String = "Mix in random topics"
    public const val MIX_OFF_LINE: String = "Uses less data. Lakko's server will see exactly which topics you picked."
    public const val SEARCH_HELPER: String = "Searches are sent to Lakko's server."
    public const val UNAVAILABLE: String = "Suggestions aren't available right now."
    public const val NO_RESULTS: String = "No channels found."
    public const val GONE: String = "This channel is no longer available."
    public const val SEARCH_TOO_SHORT: String = "Type at least 2 characters."
    public const val NOTHING_YET: String =
        "No suggestions yet. Pick topics, or ask people in your groups to share their channels."

    public const val LISTING_REFUSED: String = "Couldn't list this channel. Try again."
    public const val LISTING_CONFIRM: String =
        "Anyone can find this channel's name, description and subscriber count. Posts stay encrypted."
    public const val LISTING_NEEDS_HANDLE: String = "A channel needs an @handle to be listed in Discover."

    public const val SHARE_CHANNELS_LABEL: String = "Share my channels with this group"
    public const val SHARE_CHANNELS_LINE: String =
        "People in this group can see which channels you follow. Lakko's server cannot."
}

/** A stamp strictly after the stored one (the listing's `updated_at_ms`). */
public fun nextListingStamp(nowMs: Long, stored: ULong?): ULong {
    val now = nowMs.coerceAtLeast(0L).toULong()
    if (stored == null || now > stored) return now
    return stored + 1uL
}
