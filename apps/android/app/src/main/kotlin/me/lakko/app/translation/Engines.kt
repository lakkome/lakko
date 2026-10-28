// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import uniffi.lakko_ffi_uniffi.DetectedLanguage
import uniffi.lakko_ffi_uniffi.TranslationEngineState
import java.io.File

/**
 * The three on-device engines of ADR-0082, as interfaces, so the rules and
 * the UI do not depend on which platform engine (if any) answers.
 *
 * Every implementation must process on this device and nowhere else
 * (ADR-0082 sections 1-2). An implementation that cannot confirm that through
 * its own API answers [TranslationEngineState.UNAVAILABLE] for everything;
 * there is no remote implementation and none may be added.
 */

/** A language pack the platform reports. */
public data class PackInfo(
    /** The language, as the platform names it. */
    val language: String,
    /** Bytes, when the platform reports a size; never guessed. */
    val sizeBytes: Long?,
    /** For translation, transcription, or both. */
    val forTranslation: Boolean,
    val forTranscription: Boolean,
    /** `true` when the operating system manages it (Lakko offers no Remove). */
    val managedBySystem: Boolean,
)

/** How a pack download ended. */
public enum class PackOutcome {
    /** The pack is on the device. */
    DONE,

    /** The platform accepted the request but will fetch it later. */
    SCHEDULED,

    /** It did not happen. */
    FAILED,
}

/** Translation of text, on the device. */
public interface TranslationEngine {
    /** Whether this device can translate at all (some pair, now or after a pack). */
    public suspend fun available(): Boolean

    /** What the device can do for translating [source] into [target]. */
    public suspend fun state(source: String, target: String): TranslationEngineState

    /** The languages this device can translate from into [target]. */
    public suspend fun sources(target: String): List<String>

    /** The translation, or `null` when it failed. */
    public suspend fun translate(text: String, source: String, target: String): String?

    /** Ask the platform to put the pack for [source] -> [target] on the device. */
    public suspend fun download(source: String, target: String, progress: (Int?) -> Unit): PackOutcome

    /** The size the platform reports for that pack, `null` when it reports none. */
    public suspend fun packSize(source: String, target: String): Long? = null

    /** Packs on the device that this engine uses. */
    public suspend fun installedPacks(): List<PackInfo> = emptyList()

    /**
     * Delete a pack this engine downloaded itself ([PackInfo.managedBySystem]
     * false). `false` when there is none or the platform refused.
     */
    public suspend fun removePack(language: String): Boolean = false
}

/** What a transcription produced. */
public sealed interface TranscriptOutcome {
    public data class Text(val text: String) : TranscriptOutcome

    public data object NoSpeech : TranscriptOutcome

    public data object NeedsPack : TranscriptOutcome

    public data object Failed : TranscriptOutcome
}

/** Transcription of an audio file, on the device. */
public interface TranscriptionEngine {
    /** Whether this device can transcribe at all. */
    public suspend fun available(): Boolean

    public suspend fun state(language: String): TranslationEngineState

    /** The languages this device can transcribe (now or after a pack). */
    public suspend fun languages(): List<String>

    /**
     * Transcribe [file] (an authenticated, decrypted media file) in
     * [language]. [partial] receives the text as far as it has got.
     */
    public suspend fun transcribe(file: File, language: String, partial: (String) -> Unit): TranscriptOutcome

    public suspend fun download(language: String, progress: (Int?) -> Unit): PackOutcome

    public suspend fun installedPacks(): List<PackInfo> = emptyList()
}

/** Which language some text is in, on the device. */
public interface LanguageDetector {
    /** `null` when it cannot tell, or when there is no detector. */
    public suspend fun detect(text: String): DetectedLanguage?
}

/** The translation engine of a device that has none it can trust to stay local. */
public object NoTranslationEngine : TranslationEngine {
    override suspend fun available(): Boolean = false

    override suspend fun state(source: String, target: String): TranslationEngineState =
        TranslationEngineState.UNAVAILABLE

    override suspend fun sources(target: String): List<String> = emptyList()

    override suspend fun translate(text: String, source: String, target: String): String? = null

    override suspend fun download(source: String, target: String, progress: (Int?) -> Unit): PackOutcome =
        PackOutcome.FAILED
}

/** The transcription engine of a device that has none. */
public object NoTranscriptionEngine : TranscriptionEngine {
    override suspend fun available(): Boolean = false

    override suspend fun state(language: String): TranslationEngineState = TranslationEngineState.UNAVAILABLE

    override suspend fun languages(): List<String> = emptyList()

    override suspend fun transcribe(file: File, language: String, partial: (String) -> Unit): TranscriptOutcome =
        TranscriptOutcome.Failed

    override suspend fun download(language: String, progress: (Int?) -> Unit): PackOutcome = PackOutcome.FAILED
}

/**
 * No detector. Android's `TextClassifier.detectLanguage` is the only platform
 * detector, and its own documentation says requests "may be handled in an
 * OEM-provided process": nothing in its API says the text stays on the device,
 * so by ADR-0082 section 1 it is not used. ML Kit Language Identification
 * reports the identified languages to Google (its data disclosure), so it is
 * not used either. Without a detector every message is "could not tell": the
 * menu opens the language picker, and the conversation bar never appears.
 */
public object NoLanguageDetector : LanguageDetector {
    override suspend fun detect(text: String): DetectedLanguage? = null
}
