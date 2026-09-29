// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.l10n

import android.os.Build
import android.util.Log
import androidx.compose.runtime.Composable
import java.util.Locale

/**
 * A user-visible sentence held as its catalog key and arguments (ADR-0058
 * section 6: errors render from their code, never from core prose).
 *
 * View models put a [UiText] into their state; the screen renders it with
 * [text], so a language switch re-renders a message that is already showing.
 * Arguments follow the catalog: [texts] are strings passed as they are,
 * [counts] are numbers the formatter pluralises.
 */
public data class UiText(
    val key: String,
    val texts: Map<String, String> = emptyMap(),
    val counts: Map<String, ULong> = emptyMap(),
) {
    /** The text in the current language, for code outside composition. */
    public fun render(): String = renderWith(Localization.strings.value)

    internal fun renderWith(strings: Strings): String =
        if (texts.isEmpty() && counts.isEmpty()) strings[key] else strings.format(key, texts, counts)
}

/** [this] in the language of the current composition. */
@Composable
public fun UiText.text(): String = renderWith(LocalStrings.current)

/**
 * A list of word positions ("3, 7 and 15") for the `{positions}` argument of
 * the recovery messages: the locale's list formatter for the language the
 * interface shows, digits kept Latin (ADR-0058 open question 2). English
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
