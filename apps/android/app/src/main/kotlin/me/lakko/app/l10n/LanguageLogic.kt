// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.l10n

import java.text.Collator
import java.text.Normalizer
import java.util.Locale
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/*
 * ADR-0061 / docs/ux/localization.md — the Android side of the interface
 * language, kept free of the FFI and of Android types so the host JVM suite
 * can run it. The resolution rule itself (`l10nResolveLanguage`) lives in
 * `lakko-l10n` and is injected here as a function; this file only decides
 * WHAT to ask it and WHERE the choice is kept.
 */

/** Where the person's choice is kept on this device; `null` = same as system. */
public interface LanguageChoiceStore {
    public fun read(): String?
    public fun write(code: String?)
}

/**
 * The system per-app language (Android 13+ `LocaleManager`), which the UX
 * spec makes the authority: a change in system Settings > Apps > Lakko >
 * Language is reflected in Lakko. `null` below Android 13.
 */
public interface PlatformAppLanguage {
    /** The app locale tag the system holds for Lakko, `null` when it follows the system. */
    public fun read(): String?
    public fun write(code: String?)
}

/** What the UI renders: the stored choice and the language it resolves to. */
public data class LanguageState(
    /** Lakko language code the person picked, `null` = same as system. */
    val selected: String?,
    /** The language the interface is in now. */
    val resolved: String,
    /** The language "Same as system" would give; the first row's detail. */
    val systemResolved: String,
)

/**
 * Keeps the choice and resolves it: stored choice, else the first supported
 * system language, else English (all three decided by [resolve]).
 *
 * On Android 13+ the system per-app language wins over the local copy, so a
 * change made in system Settings while Lakko was not looking is picked up on
 * the next [refresh]; the local copy is then rewritten to match. The choice
 * never leaves the device.
 */
public class LanguageController(
    private val store: LanguageChoiceStore,
    private val platform: PlatformAppLanguage?,
    private val systemPreferred: () -> List<String>,
    private val resolve: (selected: String?, systemPreferred: List<String>) -> String,
    /** Maps a platform locale tag to a Lakko code, `null` when unsupported. */
    private val matchTag: (tag: String) -> String?,
) {
    private val flow = MutableStateFlow(compute())

    public val state: StateFlow<LanguageState> = flow.asStateFlow()

    /** Re-read the choice and the system languages (start, configuration change). */
    public fun refresh(): LanguageState {
        val next = compute()
        flow.value = next
        return next
    }

    /** Pick a language; `null` returns to following the system. */
    public fun select(code: String?): LanguageState {
        store.write(code)
        platform?.write(code)
        val system = systemPreferred()
        val next = LanguageState(
            selected = code,
            resolved = resolve(code, system),
            systemResolved = resolve(null, system),
        )
        flow.value = next
        return next
    }

    private fun compute(): LanguageState {
        val choice = currentChoice()
        val system = systemPreferred()
        return LanguageState(
            selected = choice,
            resolved = resolve(choice, system),
            systemResolved = resolve(null, system),
        )
    }

    private fun currentChoice(): String? {
        if (platform == null) return store.read()
        val fromPlatform = platform.read()?.let(matchTag)
        if (fromPlatform != store.read()) store.write(fromPlatform)
        return fromPlatform
    }
}

/** One row of the language list; the title is [ownName], never translated. */
public data class LanguageEntry(
    val code: String,
    val ownName: String,
    val rtl: Boolean,
)

/** Catalog key of a language's name in the current UI language. */
public fun languageNameKey(code: String): String = "language.name.${code.lowercase(Locale.ROOT)}"

/**
 * Case- and diacritic-insensitive form used by the search field
 * (docs/ux/localization.md section 3).
 */
public fun searchFold(text: String): String =
    Normalizer.normalize(text, Normalizer.Form.NFD)
        .replace(COMBINING_MARKS, "")
        .lowercase(Locale.ROOT)
        .trim()

private val COMBINING_MARKS = Regex("\\p{Mn}+")

/**
 * The rows the list shows for [query]: those whose own name or translated
 * name contains it, sorted by the translated name in [uiLanguage].
 */
public fun visibleLanguages(
    entries: List<LanguageEntry>,
    translatedName: (LanguageEntry) -> String,
    uiLanguage: String,
    query: String,
): List<LanguageEntry> {
    val folded = searchFold(query)
    val collator = Collator.getInstance(Locale.forLanguageTag(uiLanguage))
    return entries
        .filter { entry ->
            folded.isEmpty() ||
                searchFold(entry.ownName).contains(folded) ||
                searchFold(translatedName(entry)).contains(folded)
        }
        .sortedWith { a, b -> collator.compare(translatedName(a), translatedName(b)) }
}
