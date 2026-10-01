// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.l10n

import android.os.Build
import android.util.Log
import androidx.compose.runtime.Composable
import java.util.Locale

/**
 * A user-visible sentence held as its catalog key and arguments (ADR-0061
 * section 6: errors render from their code, never from core prose).
 *
 * View models put a [UiText] into their state; the screen renders it with
 * [text], so a language switch re-renders a message that is already showing.
 * Arguments follow the catalog: [texts] are strings passed as they are,
 * [counts] are numbers the formatter pluralises, and [parts] are other
 * catalog messages rendered in the same language first (a sentence built
 * from two sentences, a list of names joined pair by pair), so a switch
 * re-renders the whole of it.
 */
public data class UiText(
    val key: String,
    val texts: Map<String, String> = emptyMap(),
    val counts: Map<String, ULong> = emptyMap(),
    val parts: Map<String, UiText> = emptyMap(),
) {
    /** The text in the current language, for code outside composition. */
    public fun render(): String = renderWith(Localization.strings.value)

    internal fun renderWith(strings: Strings): String {
        if (texts.isEmpty() && counts.isEmpty() && parts.isEmpty()) return strings[key]
        val all = if (parts.isEmpty()) texts else texts + parts.mapValues { it.value.renderWith(strings) }
        return strings.format(key, all, counts)
    }
}

/** [this] in the language of the current composition. */
@Composable
public fun UiText.text(): String = renderWith(LocalStrings.current)

/** [items] joined pair by pair through `common.list_pair` in the current composition's language. */
@Composable
public fun joinList(items: List<String>): String = joinList(items, LocalStrings.current)

/** [items] joined pair by pair through `common.list_pair` in [strings]. */
public fun joinList(items: List<String>, strings: Strings): String =
    items.fold("") { acc, item ->
        if (acc.isEmpty()) item else strings.format("common.list_pair", mapOf("first" to acc, "second" to item))
    }

/** Short facts on one line ("Created 16 Sep · Used up") joined pair by pair through `common.fact_pair`. */
@Composable
public fun joinFacts(items: List<String>): String = joinFacts(items, LocalStrings.current)

/** [items] joined pair by pair through `common.fact_pair` in [strings]. */
public fun joinFacts(items: List<String>, strings: Strings): String =
    items.fold("") { acc, item ->
        if (acc.isEmpty()) item else strings.format("common.fact_pair", mapOf("first" to acc, "second" to item))
    }

/**
 * A list of word positions ("3, 7 and 15") for the `{positions}` argument of
 * the recovery messages: the locale's list formatter for the language the
 * interface shows, digits kept Latin (ADR-0061 open question 2). English
 * uses the catalog's own form, without a serial comma, as web does.
 */
public fun listPositions(positions: List<Int>, language: String): String {
    val items = positions.map { it.toString() }
    if (items.size <= 1) return items.joinToString("")
    if (language != "en" && Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
        try {
            return android.icu.text.ListFormatter
                .getInstance(Locale.forLanguageTag(language))
                .format(items)
        } catch (e: Throwable) {
            Log.w("LakkoL10n", "WARN list format failed for $language: ${e.javaClass.simpleName}")
        }
    }
    return "${items.dropLast(1).joinToString(", ")} and ${items.last()}"
}

/** [listPositions] in the language of the current composition. */
@Composable
public fun listPositions(positions: List<Int>): String = listPositions(positions, LocalStrings.current.language)
