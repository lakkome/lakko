// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import uniffi.lakko_ffi_uniffi.DetectedLanguage
import uniffi.lakko_ffi_uniffi.TranslateOfferInput
import uniffi.lakko_ffi_uniffi.TranslationEngineState
import uniffi.lakko_ffi_uniffi.detectTextLanguage

/**
 * The language of a message, found by the shared core (`lakko-ui-rules`
 * `detect_text_language`, ADR-0082 note of 2026-11-15): trigram profiles
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
}

/**
 * Detections of the shown messages, one per message version: a message is
 * detected again only when its text changed (an edit), never per redraw.
 */
internal class MessageDetections {
    private val mutable = MutableStateFlow<Map<String, DetectedLanguage>>(emptyMap())
    val detections: StateFlow<Map<String, DetectedLanguage>> = mutable.asStateFlow()

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
        val detected = detector.detect(text)
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
     * The source the device would translate a message from (UX 3.4):
     * a confident detection, else the language chosen for this conversation,
     * else the detector's best guess even when it is unsure, else nothing,
     * which is the only case the tap opens the picker. A guess translated from
     * shows `Translated from Dutch · Not Dutch?`, which corrects it.
     */
    fun known(detected: DetectedLanguage?, chosen: String?, confidentPermille: Int): DetectedLanguage? {
        if (detected != null && detected.confidencePermille.toInt() >= confidentPermille) return detected
        if (chosen != null) return DetectedLanguage(chosen, 1000.toUShort())
        return detected
    }

    /**
     * Whether the source [known] would pick is the detector's unsure guess
     * (no confident detection, no language picked for the conversation).
     */
    fun isUnsureGuess(detected: DetectedLanguage?, chosen: String?, confidentPermille: Int): Boolean =
        detected != null && detected.confidencePermille.toInt() < confidentPermille && chosen == null

    /**
     * What a tap on `Translate` does. A guess is not used when it is the
     * target language or the engine cannot translate from it: both would end
     * on a line with no `Not X?` to correct it ("already in English",
     * "can't translate from"), so the picker opens with the guess marked.
     * [engine] is `null` while the engine has not answered for the pair; the
     * caller asks again once it has.
     */
    fun tap(source: String?, isGuess: Boolean, sameAsTarget: Boolean, engine: TranslationEngineState?): TapAction =
        when {
            source == null -> TapAction.Picker(marked = null)
            sameAsTarget && isGuess -> TapAction.Picker(marked = source)
            sameAsTarget -> TapAction.AlreadyIn(source)
            isGuess && engine == TranslationEngineState.UNAVAILABLE -> TapAction.Picker(marked = source)
            else -> TapAction.Translate(source)
        }

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

/** The result of [SourceRules.tap]. */
internal sealed interface TapAction {
    /** Translate from [source]. */
    data class Translate(val source: String) : TapAction

    /** The message is in the target language already (UX 3.3). */
    data class AlreadyIn(val language: String) : TapAction

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
