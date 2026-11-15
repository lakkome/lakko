// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import uniffi.lakko_ffi_uniffi.ConversationLanguages
import uniffi.lakko_ffi_uniffi.DetectedLanguage
import uniffi.lakko_ffi_uniffi.LanguageCandidates
import uniffi.lakko_ffi_uniffi.LanguagePick
import uniffi.lakko_ffi_uniffi.MessageLanguage
import uniffi.lakko_ffi_uniffi.TranslateOffer
import uniffi.lakko_ffi_uniffi.TranslateOfferInput
import uniffi.lakko_ffi_uniffi.TranslationEngineState
import uniffi.lakko_ffi_uniffi.TranslationSourceResolution
import uniffi.lakko_ffi_uniffi.detectTextLanguage
import uniffi.lakko_ffi_uniffi.detectTextLanguageCandidates

/**
 * The language of a message, found by the shared core (`lakko-ui-rules`
 * `detect_text_language_candidates`, ADR-0082 note of 2026-11-15: the
 * detection and the languages it was a near tie with): trigram profiles
 * compiled into the native library, no network, no file access. The text
 * never leaves the call and nothing of it is kept or logged.
 *
 * Android has no platform detector Lakko may use (ML Kit Language ID reports
 * the languages it identifies to Google; `TextClassifier` may run in an
 * OEM process), so this is the Android detector.
 */
public object CoreLanguageDetector : LanguageDetector {
    override suspend fun detect(text: String): DetectedLanguage? =
        try {
            detectTextLanguage(text)
        } catch (_: Throwable) {
            // No native library (host tests) or a core failure: could not tell.
            null
        }

    /** `detect_text_language_candidates`: the detection and its near ties. */
    override suspend fun detectCandidates(text: String): LanguageCandidates? =
        try {
            detectTextLanguageCandidates(text)
        } catch (_: Throwable) {
            null
        }
}

/**
 * Detections of the shown messages, one per message version: a message is
 * detected again only when its text changed (an edit), never per redraw.
 */
internal class MessageDetections {
    private val mutable = MutableStateFlow<Map<String, LanguageCandidates>>(emptyMap())
    val detections: StateFlow<Map<String, LanguageCandidates>> = mutable.asStateFlow()

    /** The text each key was last detected (or is being detected) for. */
    private val texts = HashMap<String, String>()

    /**
     * Detect [text] of the message [key] unless this version was already
     * detected. A result for an older version that arrives late is dropped.
     */
    suspend fun detect(key: String, text: String, detector: LanguageDetector) {
        if (text.isBlank()) return
        val fresh = synchronized(texts) {
            if (texts[key] == text) {
                false
            } else {
                texts[key] = text
                true
            }
        }
        if (!fresh) return
        // An edited message no longer reads as its old language.
        mutable.update { it - key }
        val detected = detector.detectCandidates(text)
        synchronized(texts) { if (texts[key] != text) return }
        if (detected != null) mutable.update { it + (key to detected) }
    }

    fun clear() {
        synchronized(texts) { texts.clear() }
        mutable.value = emptyMap()
    }
}

/**
 * Which language a message is translated from, and what the menu is asked
 * about. Pure: the confidence line is passed in (the shared rules give it,
 * `translationDetectionConfidentPermille`).
 */
internal object SourceRules {
    /**
     * The source the device would translate a message from (UX 3.1 / 3.4):
     * a confident detection, else the detector's best guess even when it is
     * unsure, else the language chosen for this conversation, else nothing,
     * which is the only case the tap opens the picker. A guess translated from
     * shows `Translated from Dutch · Not Dutch?`, which corrects it.
     *
     * [guessUnusable] says the guess is the target language or one the engine
     * reports it cannot translate from; such a guess yields to the
     * conversation's language, and without one it is still returned so the
     * tap opens the picker with it marked. The conversation's language comes
     * after an unsure guess so that one pick in a chat (Dutch for one
     * message) does not decide every later message the detector is unsure
     * about (a Ukrainian one read from Dutch, found live 2026-11-15).
     */
    fun known(
        detected: DetectedLanguage?,
        chosen: String?,
        confidentPermille: Int,
        guessUnusable: Boolean = false,
    ): DetectedLanguage? {
        if (detected != null && detected.confidencePermille.toInt() >= confidentPermille) return detected
        if (detected != null && !guessUnusable) return detected
        if (chosen != null) return DetectedLanguage(chosen, 1000.toUShort())
        return detected
    }

    /**
     * The input of the shared offer rule for the language line (UX 3.2): the
     * menu row's facts with the engine set to "can translate", so the rule
     * answers only whether the language is one to name (not own, has text,
     * setting on, not a reading language, an unsure guess included). The
     * engine's answer decides the button in [languageLine].
     */
    fun languageLineInput(enabled: Boolean, text: String, isOwn: Boolean, detected: DetectedLanguage): TranslateOfferInput =
        TranslateOfferInput(
            enabled = enabled,
            hasText = text.isNotBlank(),
            isOwn = isOwn,
            detected = detected,
            engineForDetected = TranslationEngineState.READY,
            anyEngine = true,
        )

    /**
     * The language line from the rule's answer for [languageLineInput] and
     * the shared source rule's [resolution] (`resolveTranslationSource`, the
     * same answer `Translate` starts from): none when the rule offers
     * nothing or names no language, else that language, with `Translate`
     * when the device can translate from it ("hola, qué tal?" read as an
     * unsure Catalan on a device without Catalan names Spanish).
     */
    fun languageLine(offer: TranslateOffer, resolution: TranslationSourceResolution?): TranslateLine? {
        val tag = resolution?.lineTag ?: return null
        if (offer == TranslateOffer.NONE) return null
        return TranslateLine.Detected(tag, resolution.lineCanTranslate)
    }

    /**
     * The languages the engine answered it can translate from among
     * [tags], or `null` while any of them has no answer yet (the line would
     * otherwise change a moment later).
     */
    fun translatable(tags: List<String>, states: (String) -> TranslationEngineState?): List<String>? {
        val out = mutableListOf<String>()
        for (tag in tags) {
            when (states(tag)) {
                null -> return null
                TranslationEngineState.UNAVAILABLE -> Unit
                else -> out += tag
            }
        }
        return out
    }

    /**
     * What a tap on `Translate` does with the shared rule's [resolution]:
     * the picker, with the guess marked, when it names no source; otherwise
     * translate from it.
     */
    fun tapSource(resolution: TranslationSourceResolution): TapAction =
        resolution.translateSource?.let { TapAction.Translate(it) } ?: TapAction.Picker(marked = resolution.guess)

    /** The candidate tags of a detection: the detected language, then the others. */
    fun candidateTags(candidates: LanguageCandidates?): List<String> =
        candidates?.let { listOf(it.detected.tag) + it.alternatives }.orEmpty()

    /** The input of the shared offer rule (UX 3.1); the rule itself is unchanged. */
    fun offerInput(
        enabled: Boolean,
        text: String,
        isOwn: Boolean,
        known: DetectedLanguage?,
        engineForKnown: TranslationEngineState?,
        anyEngine: Boolean,
    ): TranslateOfferInput =
        TranslateOfferInput(
            enabled = enabled,
            hasText = text.isNotBlank(),
            isOwn = isOwn,
            detected = known,
            engineForDetected = engineForKnown ?: TranslationEngineState.UNAVAILABLE,
            anyEngine = anyEngine,
        )
}

/** The stored record as the shared rule takes it, and back. */
internal object StoredLanguagesForm {
    fun toCore(s: StoredLanguages): ConversationLanguages =
        ConversationLanguages(
            picks = s.picks.map { LanguagePick(it.tag, it.atMs.coerceAtLeast(0).toULong()) },
            detections = s.detections.map { MessageLanguage(it.messageId, it.tag) },
        )

    fun fromCore(c: ConversationLanguages): StoredLanguages =
        StoredLanguages(
            picks = c.picks.map { StoredLanguagePick(it.tag, it.atMs.coerceAtMost(Long.MAX_VALUE.toULong()).toLong()) },
            detections = c.detections.map { StoredMessageLanguage(it.messageId, it.tag) },
        )
}

/** The result of [SourceRules.tapSource]. */
internal sealed interface TapAction {
    /** Translate from [source]. */
    data class Translate(val source: String) : TapAction

    /** Open the language picker, with [marked] checked when there is a guess. */
    data class Picker(val marked: String?) : TapAction
}

/**
 * Tags as the platform engines name them, against the tags of the core
 * detector and the shared rules. The core names Norwegian Bokmal `nb` and
 * Tagalog `tl`; ML Kit Translate names them `no` and `tl`, and ICU-based
 * system services may name Tagalog `fil` and Norwegian `no`. The rules treat
 * `nb` / `no` and `tl` / `fil` as different languages, so every tag that
 * crosses into or out of an engine goes through here.
 */
internal object PlatformLanguageTags {
    /** A tag an engine reported, in the form the core and the rules use. */
    fun fromPlatform(tag: String): String {
        val trimmed = tag.trim()
        val cut = trimmed.indexOfFirst { it == '-' || it == '_' }
        val primary = (if (cut < 0) trimmed else trimmed.substring(0, cut)).lowercase()
        val rest = if (cut < 0) "" else trimmed.substring(cut)
        val mapped = when (primary) {
            "no" -> "nb"
            "fil" -> "tl"
            else -> return trimmed
        }
        return mapped + rest
    }

    /** The primary language code ML Kit Translate takes for a core tag. */
    fun toMlKit(tag: String): String {
        val primary = fromPlatform(tag).substringBefore('-').substringBefore('_').lowercase()
        return when (primary) {
            "nb" -> "no"
            else -> primary
        }
    }
}
