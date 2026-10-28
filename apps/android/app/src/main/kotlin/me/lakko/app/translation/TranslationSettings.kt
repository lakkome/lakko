// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import android.content.Context
import android.content.SharedPreferences
import uniffi.lakko_ffi_uniffi.canonicalLanguageTag
import uniffi.lakko_ffi_uniffi.sameLanguage

/**
 * UX section 6 — Settings > Language > Translation and transcription. Per
 * device, never sent anywhere (like the interface language). Plain
 * preferences: none of it is a message, and losing it only resets the
 * defaults.
 */
public data class TranslationSettings(
    /** `Show Translate button` (on by default). */
    val showTranslate: Boolean = true,
    /** `Show transcript button` (on by default). */
    val showTranscript: Boolean = true,
    /** `Download packs on mobile data` (off by default). */
    val mobileData: Boolean = false,
    /** `Languages I read` beyond the interface language (canonical tags). */
    val extraReading: List<String> = emptyList(),
    /** `Translate into`; `null` = the interface language. */
    val target: String? = null,
) {
    /** The target language for [interfaceLanguage]. */
    public fun targetFor(interfaceLanguage: String): String = target ?: interfaceLanguage

    /** The reading list the shared rules take: the interface language, the
     *  target (a person reads what they translate into), then the rest. */
    public fun readingFor(interfaceLanguage: String): List<String> {
        val out = mutableListOf(interfaceLanguage)
        val t = targetFor(interfaceLanguage)
        if (out.none { sameLanguage(it, t) }) out += t
        for (r in extraReading) if (out.none { sameLanguage(it, r) }) out += r
        return out
    }
}

/** Load and save [TranslationSettings]. */
public class TranslationSettingsStore(context: Context) {
    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    public fun load(): TranslationSettings =
        TranslationSettings(
            showTranslate = prefs.getBoolean(KEY_SHOW_TRANSLATE, true),
            showTranscript = prefs.getBoolean(KEY_SHOW_TRANSCRIPT, true),
            mobileData = prefs.getBoolean(KEY_MOBILE_DATA, false),
            extraReading = prefs.getString(KEY_READING, null)
                ?.split(',')
                ?.mapNotNull { canonicalLanguageTag(it) }
                ?.distinct()
                .orEmpty(),
            target = prefs.getString(KEY_TARGET, null)?.let { canonicalLanguageTag(it) },
        )

    public fun save(s: TranslationSettings) {
        prefs.edit()
            .putBoolean(KEY_SHOW_TRANSLATE, s.showTranslate)
            .putBoolean(KEY_SHOW_TRANSCRIPT, s.showTranscript)
            .putBoolean(KEY_MOBILE_DATA, s.mobileData)
            .putString(KEY_READING, s.extraReading.joinToString(","))
            .apply {
                if (s.target == null) remove(KEY_TARGET) else putString(KEY_TARGET, s.target)
            }
            .apply()
    }

    private companion object {
        const val PREFS = "lakko.translation"
        const val KEY_SHOW_TRANSLATE = "show_translate"
        const val KEY_SHOW_TRANSCRIPT = "show_transcript"
        const val KEY_MOBILE_DATA = "mobile_data"
        const val KEY_READING = "reading"
        const val KEY_TARGET = "target"
    }
}
