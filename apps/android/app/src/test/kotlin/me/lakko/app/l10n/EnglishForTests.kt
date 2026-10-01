// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.l10n

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.io.File

/**
 * ADR-0061 — the English source catalog for host JVM tests, which have no
 * native core: read from the compiled `assets/l10n/source.json`, rendered by
 * the core-less fallback (arguments and the English plural rule).
 *
 * [install] makes it the current language, so [UiText.render] and a
 * composition's `t(...)` read English, and a test can assert the words a
 * person sees rather than a key.
 */
internal object EnglishForTests {
    val strings: Strings by lazy {
        val candidates = listOf(
            File("src/main/assets/l10n/source.json"),
            File("app/src/main/assets/l10n/source.json"),
        )
        val file = candidates.first { it.isFile }
        val messages = Json.parseToJsonElement(file.readText()).jsonObject.getValue("messages").jsonObject
        val plain = messages.mapValues { (_, v) -> v.jsonObject.getValue("text").jsonPrimitive.content }
        Strings("en", false, plain, null)
    }

    /** Make English the current language for this process. Idempotent. */
    fun install(): Strings {
        Localization.installForTests(strings)
        return strings
    }
}

/** [this] in English, for assertions on the words a person sees. */
internal val UiText.en: String get() = renderWith(EnglishForTests.strings)

/** The English text of [key]. */
internal fun en(key: String): String = EnglishForTests.strings[key]
