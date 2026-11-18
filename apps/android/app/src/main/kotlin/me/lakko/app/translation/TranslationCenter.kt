// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.ConversationBarInput
import uniffi.lakko_ffi_uniffi.ConversationLanguages
import uniffi.lakko_ffi_uniffi.DetectedLanguage
import uniffi.lakko_ffi_uniffi.LanguageCandidates
import uniffi.lakko_ffi_uniffi.TranslateOffer
import uniffi.lakko_ffi_uniffi.TranslationEngineState
import uniffi.lakko_ffi_uniffi.TranslationSourceResolution
import uniffi.lakko_ffi_uniffi.canonicalLanguageTag
import uniffi.lakko_ffi_uniffi.conversationBarLanguage
import uniffi.lakko_ffi_uniffi.conversationDetection
import uniffi.lakko_ffi_uniffi.conversationLanguagesUsual
import uniffi.lakko_ffi_uniffi.conversationLanguagesWithDetection
import uniffi.lakko_ffi_uniffi.conversationLanguagesWithPick
import uniffi.lakko_ffi_uniffi.orderPickerLanguages
import uniffi.lakko_ffi_uniffi.resolveTranslationSource
import uniffi.lakko_ffi_uniffi.sameLanguage
import uniffi.lakko_ffi_uniffi.storedResultIsCurrent
import uniffi.lakko_ffi_uniffi.transcriptOffered
import uniffi.lakko_ffi_uniffi.translationDetectionConfidentPermille
import uniffi.lakko_ffi_uniffi.translationOffered
import java.io.File

/** The quiet line under a translated (or translatable) bubble, UX 3.2-3.5. */
public sealed interface TranslateLine {
    public data class Translating(val source: String?) : TranslateLine
    public data class Shown(val source: String, val text: String) : TranslateLine
    /** After `Show original`: `Spanish · Translate`. */
    public data class Original(val source: String) : TranslateLine
    public data class Failed(val source: String) : TranslateLine
    public data class SameLanguage(val language: String) : TranslateLine
    public data class Downloading(val source: String, val percent: Int?) : TranslateLine
    public data class WaitingWifi(val source: String) : TranslateLine
    public data class PackFailed(val source: String) : TranslateLine
    /** Reached from the picker: `Lakko can't translate from Tagalog on this device`. */
    public data class Unsupported(val source: String) : TranslateLine
    /**
     * UX 3.2 — before any translation: `Spanish · Translate`, the detected
     * language of a message not in a reading language. Without
     * [canTranslate] (the engine cannot translate from it) the name alone.
     * `Translate` does what the menu row does.
     */
    public data class Detected(val source: String, val canTranslate: Boolean) : TranslateLine
}

/** The transcript of a voice or video message, UX 4.3. */
public sealed interface TranscriptUi {
    public data object Idle : TranscriptUi
    /** Fetching and decrypting the audio. */
    public data object LoadingAudio : TranscriptUi
    public data class Transcribing(val partial: String) : TranscriptUi
    public data class Shown(val text: String, val language: String) : TranscriptUi
    public data object NoSpeech : TranscriptUi
    public data object Failed : TranscriptUi
    public data class Downloading(val language: String, val percent: Int?) : TranscriptUi
    public data class WaitingWifi(val language: String) : TranscriptUi
    public data class PackFailed(val language: String) : TranscriptUi
    public data class Unsupported(val language: String) : TranscriptUi
    /** The file cannot be fetched: no transcript is possible. */
    public data object FileUnavailable : TranscriptUi
}

/** Per message, valid for one [opId]. A state for an older op id is ignored. */
public data class MessageUi(
    val opId: Long,
    val translation: TranslateLine? = null,
    val transcript: TranscriptUi = TranscriptUi.Idle,
    val transcriptOpen: Boolean = false,
)

public enum class PackKind { TRANSLATION, TRANSCRIPTION }

/** UX 3.5 — the pack dialog. */
public data class PackPrompt(
    val kind: PackKind,
    val language: String,
    val sizeBytes: Long?,
    /** On mobile data without permission: `Download on Wi-Fi`. */
    val waitForWifi: Boolean,
    /** Anti-censorship is on: one line instead of the dialog. */
    val blocked: Boolean,
    internal val messageKey: String,
    internal val opId: Long,
)

/** UX 3.4 — `Not Spanish?` / the picker of a message whose language is unknown. */
public data class PickerPrompt(
    val kind: PackKind,
    val options: List<String>,
    /** Checked in the list: the language shown now, or the detector's guess. */
    val marked: String? = null,
    internal val messageKey: String,
    internal val conversation: String,
    internal val opId: Long,
    internal val text: String,
    internal val ref: AttachmentRef?,
    /** The message's close candidates (detection first), listed first. */
    val candidates: List<String> = emptyList(),
)

/** What the device can do, for Settings and for offering controls. */
public data class Capability(
    val translation: Boolean = false,
    val transcription: Boolean = false,
    /** Engine state for the default transcription language (interface language). */
    val transcriptionDefault: TranslationEngineState = TranslationEngineState.UNAVAILABLE,
    /** Cached engine states for translation pairs `source>target`. */
    val pairs: Map<String, TranslationEngineState> = emptyMap(),
    /** Cached engine states for transcription languages. */
    val transcriptionLanguages: Map<String, TranslationEngineState> = emptyMap(),
)

/**
 * ADR-0082 on Android: the one place that runs the on-device engines, keeps
 * their results and decides, through the shared rules
 * (`lakko-ui-rules::translation`, over UniFFI), what the UI offers.
 *
 * Foreground only: nothing here is reached from the push / background
 * decrypt path, and notifications show the original (ADR-0082 section 7).
 * No log line carries text, a transcript, a translation or a language
 * (section 8): only engine states and failure codes.
 */
public object TranslationCenter {
    private const val TAG = "LakkoTranslation"
    private const val SAME_LANGUAGE_MS = 4_000L

    /** Replaceable in a test. */
    @Volatile public var translation: TranslationEngine = NoTranslationEngine
    @Volatile public var transcription: TranscriptionEngine = NoTranscriptionEngine
    @Volatile public var detector: LanguageDetector = NoLanguageDetector
    @Volatile public var antiCensorshipArmed: () -> Boolean =
        { me.lakko.app.transport.AntiCensorshipConfig.current.armed }
    @Volatile public var onUnmeteredNetwork: () -> Boolean = { true }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val storeLock = Mutex()

    @Volatile private var appContext: Context? = null
    @Volatile private var settingsStore: TranslationSettingsStore? = null
    @Volatile private var accountIdB64: String? = null
    @Volatile private var interfaceLanguage: () -> String = { "en" }

    private val mutableSettings = MutableStateFlow(TranslationSettings())
    public val settings: StateFlow<TranslationSettings> = mutableSettings.asStateFlow()

    private val mutableCapability = MutableStateFlow(Capability())
    public val capability: StateFlow<Capability> = mutableCapability.asStateFlow()

    private val mutableStore = MutableStateFlow(TranslationStoreState())
    public val store: StateFlow<TranslationStoreState> = mutableStore.asStateFlow()

    private val mutableUi = MutableStateFlow<Map<String, MessageUi>>(emptyMap())
    public val ui: StateFlow<Map<String, MessageUi>> = mutableUi.asStateFlow()

    private val mutablePack = MutableStateFlow<PackPrompt?>(null)
    public val packPrompt: StateFlow<PackPrompt?> = mutablePack.asStateFlow()

    private val mutablePicker = MutableStateFlow<PickerPrompt?>(null)
    public val picker: StateFlow<PickerPrompt?> = mutablePicker.asStateFlow()

    /** Detections by message key, one per message version (UX 3.1, 3.6). */
    private val messageDetections = MessageDetections()
    public val detections: StateFlow<Map<String, LanguageCandidates>> = messageDetections.detections

    private val jobs = HashMap<String, Job>()

    /** Install the engines of this device; idempotent. */
    public fun install(context: Context, uiLanguage: () -> String) {
        val ctx = context.applicationContext
        interfaceLanguage = uiLanguage
        if (appContext != null) return
        appContext = ctx
        settingsStore = TranslationSettingsStore(ctx).also { mutableSettings.value = it.load() }
        detector = CoreLanguageDetector
        translation = translationEngineFor(ctx) { antiCensorshipArmed() }
        transcription = transcriptionEngineFor(ctx)
        onUnmeteredNetwork = { unmetered(ctx) }
        refreshCapability()
    }

    /** The account whose results are shown; called when it lands on this device. */
    public fun bind(context: Context, accountIdB64: String) {
        install(context, interfaceLanguage)
        if (this.accountIdB64 == accountIdB64) return
        this.accountIdB64 = accountIdB64
        mutableUi.value = emptyMap()
        messageDetections.clear()
        scope.launch {
            val loaded = SealedTranslationStore.load(context, accountIdB64)
            storeLock.withLock { if (this@TranslationCenter.accountIdB64 == accountIdB64) mutableStore.value = loaded }
        }
    }

    /** The session ended: nothing of it stays in memory. */
    public fun unbind() {
        accountIdB64 = null
        synchronized(jobs) {
            jobs.values.forEach { it.cancel() }
            jobs.clear()
        }
        mutableStore.value = TranslationStoreState()
        mutableUi.value = emptyMap()
        messageDetections.clear()
        mutablePack.value = null
        mutablePicker.value = null
    }

    public fun interfaceLanguageNow(): String = interfaceLanguage()

    private fun target(): String = settings.value.targetFor(interfaceLanguage())

    private fun reading(): List<String> = settings.value.readingFor(interfaceLanguage())

    public fun updateSettings(change: (TranslationSettings) -> TranslationSettings) {
        val next = change(mutableSettings.value)
        mutableSettings.value = next
        settingsStore?.save(next)
        refreshCapability()
    }

    /** Ask the engines again (after a setting, a pack, the interface language). */
    public fun refreshCapability() {
        scope.launch {
            val t = translation
            val s = transcription
            val target = target()
            val lang = interfaceLanguage()
            val translationOk = t.available()
            val transcriptionOk = s.available()
            val def = if (transcriptionOk) s.state(lang) else TranslationEngineState.UNAVAILABLE
            mutableCapability.update {
                it.copy(
                    translation = translationOk,
                    transcription = transcriptionOk,
                    transcriptionDefault = def,
                    pairs = emptyMap(),
                    transcriptionLanguages = mapOf(lang to def),
                )
            }
            Log.i(TAG, "engines translation=${if (translationOk) "present" else "unavailable"} transcription=${def.name.lowercase()}")
            if (translationOk) {
                val chosen = mutableStore.value.conversations.values.flatMap { c -> c.languages.picks.map { it.tag } }.distinct()
                for (src in chosen) pairState(src, target)
            }
        }
    }

    private suspend fun pairState(source: String, target: String): TranslationEngineState {
        val key = "$source>$target"
        mutableCapability.value.pairs[key]?.let { return it }
        val st = translation.state(source, target)
        mutableCapability.update { it.copy(pairs = it.pairs + (key to st)) }
        return st
    }

    private suspend fun transcriptionState(language: String): TranslationEngineState {
        mutableCapability.value.transcriptionLanguages[language]?.let { return it }
        val st = transcription.state(language)
        mutableCapability.update { it.copy(transcriptionLanguages = it.transcriptionLanguages + (language to st)) }
        return st
    }

    // ---------------------------------------------------------------------
    // Decisions, through the shared rules
    // ---------------------------------------------------------------------

    public fun choices(conversation: String): ConversationChoices =
        mutableStore.value.conversations[conversation] ?: ConversationChoices()

    /**
     * The source the device would translate [key] from (UX 3.1 / 3.4): a
     * confident detection, else the detector's guess when usable, else the
     * language chosen for this conversation, else the unusable guess or
     * nothing ([SourceRules.known]).
     */
    private fun knownSource(conversation: String, key: String): DetectedLanguage? {
        val detected = effectiveDetection(conversation, key)
        return SourceRules.known(
            detected,
            usual(conversation),
            translationDetectionConfidentPermille().toInt(),
            guessUnusable(detected),
        )
    }

    /**
     * The language a message is taken to be in (ADR-0082 note of
     * 2026-11-15): its detection, unless that is unsure and the
     * conversation's languages name one of its close candidates (a picked
     * one, else the most detected one; shared rule `conversationDetection`,
     * independent of the order messages were detected in).
     */
    private fun effectiveDetection(conversation: String, key: String): DetectedLanguage? {
        val candidates = detections.value[key] ?: return null
        return try {
            conversationDetection(candidates, languages(conversation))
        } catch (_: Throwable) {
            candidates.detected
        }
    }

    /** The conversation's languages as the shared rule takes them. */
    private fun languages(conversation: String): ConversationLanguages =
        StoredLanguagesForm.toCore(choices(conversation).languages)

    /** The conversation's usual language (the latest pick, else the most
     *  detected one); `null` with neither or without the core. */
    private fun usual(conversation: String): String? =
        try {
            conversationLanguagesUsual(languages(conversation))
        } catch (_: Throwable) {
            null
        }

    /**
     * Whether a detector's guess cannot be translated from: it is the target
     * language, or the engine has already said it cannot translate the pair.
     * An engine that has not answered yet is asked again after the tap.
     */
    private fun guessUnusable(detected: DetectedLanguage?): Boolean {
        val d = detected ?: return false
        val tag = canonicalLanguageTag(d.tag) ?: d.tag
        val target = target()
        return sameLanguage(tag, target) ||
            mutableCapability.value.pairs["$tag>$target"] == TranslationEngineState.UNAVAILABLE
    }

    /** UX 3.1 — whether the menu offers `Translate` for this message. */
    public fun translateOffer(conversation: String, messageId: Long, text: String, isOwn: Boolean): TranslateOffer {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        val cap = mutableCapability.value
        val detected = knownSource(conversation, key)
        val target = target()
        val pair = detected?.let { cap.pairs["${canonicalLanguageTag(it.tag) ?: it.tag}>$target"] }
        if (detected != null && pair == null && cap.translation) {
            scope.launch { pairState(canonicalLanguageTag(detected.tag) ?: detected.tag, target) }
        }
        return translationOffered(
            SourceRules.offerInput(settings.value.showTranslate, text, isOwn, detected, pair, cap.translation),
            reading(),
        )
    }

    /**
     * UX 3.2 — the language line shown before any translation, or `null`. It
     * names the language `Translate` would start from ([resolution], the
     * shared rule `resolveTranslationSource`, the same answer [translate]
     * reads), so the line and the tap never disagree; the menu row's shared
     * rule then decides whether that language is one to name
     * ([SourceRules.languageLineInput]). Reads the detection the menu row
     * uses; never detects again.
     */
    public fun languageLine(conversation: String, messageId: Long, text: String, isOwn: Boolean): TranslateLine? {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        val resolution = resolution(conversation, key) ?: return null
        val tag = resolution.lineTag ?: return null
        val offer = translationOffered(
            SourceRules.languageLineInput(
                settings.value.showTranslate, text, isOwn,
                DetectedLanguage(tag, resolution.lineConfidencePermille),
            ),
            reading(),
        )
        return SourceRules.languageLine(offer, resolution)
    }

    /**
     * The tags whose engine answer the shared source rule needs: every close
     * candidate of [key] and the conversation's usual language, less the
     * language translated into.
     */
    private fun askedTags(conversation: String, key: String, target: String): List<String> {
        val out = mutableListOf<String>()
        val all = SourceRules.candidateTags(detections.value[key]) + listOfNotNull(usual(conversation))
        for (raw in all) {
            val tag = canonicalLanguageTag(raw) ?: raw
            if (sameLanguage(tag, target) || out.any { sameLanguage(it, tag) }) continue
            out += tag
        }
        return out
    }

    /** The shared rule over the given engine answers; `null` only if the
     *  core cannot be reached. */
    private fun resolve(conversation: String, key: String, target: String, translatable: List<String>): TranslationSourceResolution? =
        try {
            resolveTranslationSource(detections.value[key], languages(conversation), target, translatable)
        } catch (_: Throwable) {
            null
        }

    /**
     * The line's language and `Translate`'s source from the engine answers
     * already in, or `null` while any is outstanding (asked now).
     */
    private fun resolution(conversation: String, key: String): TranslationSourceResolution? {
        if (detections.value[key] == null) return null
        val target = target()
        val cap = mutableCapability.value
        val tags = askedTags(conversation, key, target)
        val translatable = if (!cap.translation) {
            emptyList()
        } else {
            SourceRules.translatable(tags) { cap.pairs["$it>$target"] } ?: run {
                scope.launch { for (tag in tags) pairState(tag, target) }
                return null
            }
        }
        return resolve(conversation, key, target, translatable)
    }

    /** The same resolution with every engine answer asked now. */
    private suspend fun loadResolution(conversation: String, key: String): TranslationSourceResolution? {
        val target = target()
        val translatable = if (!mutableCapability.value.translation) {
            emptyList()
        } else {
            askedTags(conversation, key, target).filter { pairState(it, target) != TranslationEngineState.UNAVAILABLE }
        }
        return resolve(conversation, key, target, translatable)
    }

    /** UX 4.1 — whether a voice / video message gets the `Aa` button. */
    public fun transcriptButtonOffered(conversation: String): Boolean {
        val cap = mutableCapability.value
        val chosen = choices(conversation).transcriptLanguage
        val state = if (chosen != null) {
            cap.transcriptionLanguages[chosen] ?: run {
                scope.launch { transcriptionState(chosen) }
                cap.transcriptionDefault
            }
        } else {
            cap.transcriptionDefault
        }
        return transcriptOffered(settings.value.showTranscript, true, state)
    }

    /** UX 3.6 — the language the bar names, or `null`. */
    public fun barLanguage(conversation: String, recentIncomingKeys: List<String>): String? {
        val det = detections.value
        val recent = recentIncomingKeys.mapNotNull { det[it]?.detected }
        val c = choices(conversation)
        val lang = conversationBarLanguage(
            ConversationBarInput(
                enabled = settings.value.showTranslate,
                autoTranslateOn = c.autoTranslate,
                recentIncoming = recent,
                dismissed = c.dismissedBar,
            ),
            reading(),
        ) ?: return null
        val st = mutableCapability.value.pairs["$lang>${target()}"]
        if (st == null) {
            scope.launch { pairState(lang, target()) }
            return null
        }
        return if (st == TranslationEngineState.UNAVAILABLE) null else lang
    }

    /**
     * Run the detector on a shown message, once per version: again only when
     * its text changed (a no-op without a detector).
     */
    public fun detect(conversation: String, messageId: Long, text: String) {
        val d = detector
        if (d === NoLanguageDetector || text.isBlank()) return
        val key = TranslationStoreState.messageKey(conversation, messageId)
        scope.launch {
            messageDetections.detect(key, text, d)
            rememberConfident(conversation, messageId, detections.value[key]?.detected)
        }
    }

    /** A confident detection is recorded for its message, once per message
     *  id (with `Not X?` corrections and picker picks, the conversation's
     *  languages; ADR-0082 note of 2026-11-15). */
    private fun rememberConfident(conversation: String, messageId: Long, detected: DetectedLanguage?) {
        val d = detected ?: return
        if (d.confidencePermille.toInt() < translationDetectionConfidentPermille().toInt()) return
        val known = choices(conversation).languages.detections.firstOrNull { it.messageId == messageId }
        if (known != null && sameLanguage(known.tag, d.tag)) return
        setChoices(conversation) {
            val next = conversationLanguagesWithDetection(StoredLanguagesForm.toCore(it.languages), messageId, d.tag)
            it.copy(languages = StoredLanguagesForm.fromCore(next))
        }
    }

    // ---------------------------------------------------------------------
    // Translation
    // ---------------------------------------------------------------------

    private fun setLine(key: String, opId: Long, line: TranslateLine?) {
        mutableUi.update { m ->
            val cur = m[key]?.takeIf { it.opId == opId } ?: MessageUi(opId)
            m + (key to cur.copy(translation = line))
        }
    }

    private fun setTranscript(key: String, opId: Long, t: TranscriptUi, open: Boolean? = null) {
        mutableUi.update { m ->
            val cur = m[key]?.takeIf { it.opId == opId } ?: MessageUi(opId)
            m + (key to cur.copy(transcript = t, transcriptOpen = open ?: cur.transcriptOpen))
        }
    }

    /** A stored translation that still describes this message, if any. */
    public fun storedTranslation(conversation: String, messageId: Long, opId: Long): StoredTranslation? {
        val s = mutableStore.value.translations[TranslationStoreState.messageKey(conversation, messageId)] ?: return null
        return s.takeIf { storedResultIsCurrent(it.lastOpId, opId, it.target, target()) }
    }

    /**
     * The menu's `Translate` (or the line's `Translate` / `Try again`): from
     * [explicitSource] when given, else from the source the language line
     * names ([loadResolution], the shared rule with every engine answer in);
     * the picker, with the guess marked, when the rule names none.
     */
    public fun translate(conversation: String, messageId: Long, opId: Long, text: String, explicitSource: String? = null) {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        if (explicitSource != null) {
            translateFrom(conversation, messageId, opId, text, explicitSource)
            return
        }
        launchFor("s:$key") {
            val chosen = usual(conversation)
            val resolution = loadResolution(conversation, key)
            val action = resolution?.let { SourceRules.tapSource(it) }
                ?: chosen?.let { TapAction.Translate(it) }
                ?: TapAction.Picker(marked = detections.value[key]?.detected?.tag)
            when (action) {
                is TapAction.Translate -> translateFrom(conversation, messageId, opId, text, action.source)
                is TapAction.Picker -> openPicker(PackKind.TRANSLATION, conversation, key, opId, text, null, action.marked)
            }
        }
    }

    private fun translateFrom(conversation: String, messageId: Long, opId: Long, text: String, source: String) {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        val target = target()
        if (sameLanguage(source, target)) {
            setLine(key, opId, TranslateLine.SameLanguage(target))
            scope.launch {
                delay(SAME_LANGUAGE_MS)
                mutableUi.update { m ->
                    val cur = m[key] ?: return@update m
                    if (cur.translation is TranslateLine.SameLanguage) m + (key to cur.copy(translation = null)) else m
                }
            }
            return
        }
        val stored = storedTranslation(conversation, messageId, opId)
        if (stored != null && sameLanguage(stored.source, source)) {
            setLine(key, opId, TranslateLine.Shown(stored.source, stored.text))
            return
        }
        launchFor("t:$key") {
            setLine(key, opId, TranslateLine.Translating(source))
            when (pairState(source, target)) {
                TranslationEngineState.UNAVAILABLE -> setLine(key, opId, TranslateLine.Unsupported(source))
                TranslationEngineState.NEEDS_PACK -> {
                    setLine(key, opId, null)
                    textFor[key] = text
                    promptPack(PackKind.TRANSLATION, source, key, opId) { translation.packSize(source, target) }
                }
                TranslationEngineState.READY -> runTranslation(key, opId, text, source, target)
            }
        }
    }

    private suspend fun runTranslation(key: String, opId: Long, text: String, source: String, target: String) {
        val out = translation.translate(text, source, target)
        if (out == null) {
            Log.w(TAG, "translation failed code=engine")
            setLine(key, opId, TranslateLine.Failed(source))
            return
        }
        setLine(key, opId, TranslateLine.Shown(source, out))
        mutateStore { it.withTranslation(key, StoredTranslation(opId, source, target, out)) }
        // UX 3.4 — only a source picked in the picker is remembered for the
        // conversation (`choosePicked`); a detected one is not, or a guess
        // would decide the next message's language.
    }

    /** UX 3.7 — translate as shown, when the source is known and ready. */
    public fun autoTranslate(conversation: String, messageId: Long, opId: Long, text: String) {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        if (mutableUi.value[key]?.takeIf { it.opId == opId }?.translation != null) return
        val src = knownSource(conversation, key) ?: return
        if (src.confidencePermille < translationDetectionConfidentPermille()) return
        val source = canonicalLanguageTag(src.tag) ?: return
        if (reading().any { sameLanguage(it, source) }) return
        val stored = storedTranslation(conversation, messageId, opId)
        if (stored != null) {
            setLine(key, opId, TranslateLine.Shown(stored.source, stored.text))
            return
        }
        launchFor("t:$key") {
            if (pairState(source, target()) != TranslationEngineState.READY) return@launchFor
            setLine(key, opId, TranslateLine.Translating(source))
            runTranslation(key, opId, text, source, target())
        }
    }

    public fun showOriginal(conversation: String, messageId: Long, opId: Long) {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        val line = mutableUi.value[key]?.translation
        val source = when (line) {
            is TranslateLine.Shown -> line.source
            else -> storedTranslation(conversation, messageId, opId)?.source
        } ?: return setLine(key, opId, null)
        setLine(key, opId, TranslateLine.Original(source))
    }

    /** `Not Spanish?` */
    public fun pickSource(conversation: String, messageId: Long, opId: Long, text: String, ref: AttachmentRef?, kind: PackKind) {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        // `Not Spanish?`: the language shown now is the checked row.
        val shown = when (val line = mutableUi.value[key]?.takeIf { it.opId == opId }?.translation) {
            is TranslateLine.Shown -> line.source
            else -> null
        }
        openPicker(kind, conversation, key, opId, text, ref, if (kind == PackKind.TRANSLATION) shown else null)
    }

    private fun openPicker(
        kind: PackKind,
        conversation: String,
        key: String,
        opId: Long,
        text: String,
        ref: AttachmentRef?,
        marked: String? = null,
    ) {
        scope.launch {
            val options = when (kind) {
                PackKind.TRANSLATION -> translation.sources(target())
                PackKind.TRANSCRIPTION -> transcription.languages()
            }.mapNotNull { canonicalLanguageTag(it) }.distinct()
            // `Not Spanish?`: the message's close candidates first (UX 3.4).
            val candidates = if (kind == PackKind.TRANSLATION) {
                SourceRules.candidateTags(detections.value[key])
            } else {
                emptyList()
            }
            val ordered = if (candidates.isEmpty()) {
                options
            } else {
                try {
                    orderPickerLanguages(
                        options,
                        LanguageCandidates(DetectedLanguage(candidates.first(), 0.toUShort()), candidates.drop(1)),
                    )
                } catch (_: Throwable) {
                    options
                }
            }
            mutablePicker.value = PickerPrompt(
                kind,
                ordered,
                marked?.let { canonicalLanguageTag(it) ?: it },
                key,
                conversation,
                opId,
                text,
                ref,
                candidates,
            )
        }
    }

    public fun dismissPicker() {
        mutablePicker.value = null
    }

    /** A language was chosen in the picker: applies to this message, and is
     *  remembered for the conversation. */
    public fun choosePicked(language: String, hub: me.lakko.app.media.ChatMediaHub?) {
        val p = mutablePicker.value ?: return
        mutablePicker.value = null
        val messageId = p.messageKey.substringAfterLast('/').toLongOrNull() ?: return
        when (p.kind) {
            PackKind.TRANSLATION -> {
                setChoices(p.conversation) {
                    val next = conversationLanguagesWithPick(
                        StoredLanguagesForm.toCore(it.languages),
                        language,
                        System.currentTimeMillis().coerceAtLeast(0).toULong(),
                    )
                    it.copy(languages = StoredLanguagesForm.fromCore(next))
                }
                translate(p.conversation, messageId, p.opId, p.text, explicitSource = language)
            }
            PackKind.TRANSCRIPTION -> {
                setChoices(p.conversation) { it.copy(transcriptLanguage = language) }
                val ref = p.ref ?: return
                if (hub != null) transcribe(p.conversation, messageId, p.opId, ref, hub, force = true)
            }
        }
    }

    // ---------------------------------------------------------------------
    // Transcription
    // ---------------------------------------------------------------------

    public fun storedTranscript(conversation: String, messageId: Long, opId: Long): StoredTranscript? {
        val s = mutableStore.value.transcripts[TranslationStoreState.messageKey(conversation, messageId)] ?: return null
        return s.takeIf { storedResultIsCurrent(it.lastOpId, opId, it.language, it.language) }
    }

    /** The `Aa` button. Opens a kept transcript at once; else makes one. */
    public fun toggleTranscript(conversation: String, messageId: Long, opId: Long, ref: AttachmentRef, hub: me.lakko.app.media.ChatMediaHub) {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        val cur = mutableUi.value[key]?.takeIf { it.opId == opId }
        if (cur != null && cur.transcriptOpen) {
            setTranscript(key, opId, cur.transcript, open = false)
            return
        }
        transcribe(conversation, messageId, opId, ref, hub, force = false)
    }

    public fun hideTranscript(conversation: String, messageId: Long, opId: Long) {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        val cur = mutableUi.value[key]?.takeIf { it.opId == opId } ?: return
        setTranscript(key, opId, cur.transcript, open = false)
    }

    public fun transcribe(conversation: String, messageId: Long, opId: Long, ref: AttachmentRef, hub: me.lakko.app.media.ChatMediaHub, force: Boolean) {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        val language = choices(conversation).transcriptLanguage ?: interfaceLanguage()
        if (!force) {
            val stored = storedTranscript(conversation, messageId, opId)
            if (stored != null) {
                setTranscript(key, opId, if (stored.noSpeech) TranscriptUi.NoSpeech else TranscriptUi.Shown(stored.text, stored.language), open = true)
                return
            }
        }
        launchFor("s:$key") {
            when (transcriptionState(language)) {
                TranslationEngineState.UNAVAILABLE -> {
                    setTranscript(key, opId, TranscriptUi.Unsupported(language), open = true)
                    return@launchFor
                }
                TranslationEngineState.NEEDS_PACK -> {
                    promptPack(PackKind.TRANSCRIPTION, language, key, opId) { null }
                    pendingTranscriptions[key] = PendingTranscription(conversation, messageId, opId, ref, hub)
                    return@launchFor
                }
                TranslationEngineState.READY -> Unit
            }
            runTranscription(conversation, key, messageId, opId, ref, hub, language)
        }
    }

    private suspend fun runTranscription(
        conversation: String,
        key: String,
        messageId: Long,
        opId: Long,
        ref: AttachmentRef,
        hub: me.lakko.app.media.ChatMediaHub,
        language: String,
    ) {
        setTranscript(key, opId, TranscriptUi.LoadingAudio, open = true)
        val file: File? = hub.fileFor(me.lakko.app.media.ChatMediaHub.mediaKey(ref.blobId)) ?: hub.load(ref)
        if (file == null) {
            setTranscript(key, opId, TranscriptUi.FileUnavailable, open = true)
            return
        }
        setTranscript(key, opId, TranscriptUi.Transcribing(""), open = true)
        val outcome = transcription.transcribe(file, language) { partial ->
            setTranscript(key, opId, TranscriptUi.Transcribing(partial))
        }
        when (outcome) {
            is TranscriptOutcome.Text -> {
                setTranscript(key, opId, TranscriptUi.Shown(outcome.text, language), open = true)
                mutateStore { it.withTranscript(key, StoredTranscript(opId, language, outcome.text)) }
            }
            TranscriptOutcome.NoSpeech -> {
                setTranscript(key, opId, TranscriptUi.NoSpeech, open = true)
                mutateStore { it.withTranscript(key, StoredTranscript(opId, language, "", noSpeech = true)) }
            }
            TranscriptOutcome.NeedsPack -> {
                mutableCapability.update { it.copy(transcriptionLanguages = it.transcriptionLanguages - language) }
                setTranscript(key, opId, TranscriptUi.Idle, open = false)
                promptPack(PackKind.TRANSCRIPTION, language, key, opId) { null }
                pendingTranscriptions[key] = PendingTranscription(conversation, messageId, opId, ref, hub)
            }
            TranscriptOutcome.Failed -> setTranscript(key, opId, TranscriptUi.Failed, open = true)
        }
    }

    private data class PendingTranscription(
        val conversation: String,
        val messageId: Long,
        val opId: Long,
        val ref: AttachmentRef,
        val hub: me.lakko.app.media.ChatMediaHub,
    )

    private val pendingTranscriptions = java.util.concurrent.ConcurrentHashMap<String, PendingTranscription>()

    // ---------------------------------------------------------------------
    // Language packs (UX 3.5)
    // ---------------------------------------------------------------------

    private suspend fun promptPack(kind: PackKind, language: String, key: String, opId: Long, size: suspend () -> Long?) {
        val blocked = antiCensorshipArmed()
        mutablePack.value = PackPrompt(
            kind = kind,
            language = language,
            sizeBytes = if (blocked) null else size(),
            waitForWifi = !settings.value.mobileData && !onUnmeteredNetwork(),
            blocked = blocked,
            messageKey = key,
            opId = opId,
        )
    }

    public fun dismissPack() {
        mutablePack.value?.let { pendingTranscriptions.remove(it.messageKey) }
        mutablePack.value = null
    }

    /** `Download` / `Download on Wi-Fi`. Refused while anti-censorship is on. */
    public fun acceptPack() {
        val p = mutablePack.value ?: return
        mutablePack.value = null
        if (antiCensorshipArmed()) return
        val key = p.messageKey
        val messageId = key.substringAfterLast('/').toLongOrNull() ?: return
        val conversation = key.substringBeforeLast('/')
        launchFor("p:$key") {
            fun downloading(percent: Int?) = when (p.kind) {
                PackKind.TRANSLATION -> setLine(key, p.opId, TranslateLine.Downloading(p.language, percent))
                PackKind.TRANSCRIPTION -> setTranscript(key, p.opId, TranscriptUi.Downloading(p.language, percent), open = true)
            }
            while (!settings.value.mobileData && !onUnmeteredNetwork()) {
                when (p.kind) {
                    PackKind.TRANSLATION -> setLine(key, p.opId, TranslateLine.WaitingWifi(p.language))
                    PackKind.TRANSCRIPTION -> setTranscript(key, p.opId, TranscriptUi.WaitingWifi(p.language), open = true)
                }
                delay(5_000)
            }
            // Checked again: anti-censorship may have been armed while waiting.
            if (antiCensorshipArmed()) {
                when (p.kind) {
                    PackKind.TRANSLATION -> setLine(key, p.opId, null)
                    PackKind.TRANSCRIPTION -> setTranscript(key, p.opId, TranscriptUi.Idle, open = false)
                }
                return@launchFor
            }
            downloading(null)
            val target = target()
            val outcome = when (p.kind) {
                PackKind.TRANSLATION -> translation.download(p.language, target) { downloading(it) }
                PackKind.TRANSCRIPTION -> transcription.download(p.language) { downloading(it) }
            }
            Log.i(TAG, "pack download outcome=${outcome.name.lowercase()}")
            mutableCapability.update {
                it.copy(
                    pairs = it.pairs - "${p.language}>$target",
                    transcriptionLanguages = it.transcriptionLanguages - p.language,
                )
            }
            when (afterPack(outcome)) {
                AfterPack.CONTINUE -> when (p.kind) {
                    PackKind.TRANSLATION -> {
                        val text = textFor[key]
                        setLine(key, p.opId, null)
                        if (text != null) translate(conversation, messageId, p.opId, text, explicitSource = p.language)
                    }
                    PackKind.TRANSCRIPTION -> {
                        val pending = pendingTranscriptions.remove(key)
                        if (pending != null) {
                            runTranscription(conversation, key, messageId, p.opId, pending.ref, pending.hub, p.language)
                        }
                    }
                }
                AfterPack.BACK_TO_START -> when (p.kind) {
                    PackKind.TRANSLATION -> setLine(key, p.opId, null)
                    PackKind.TRANSCRIPTION -> {
                        pendingTranscriptions.remove(key)
                        setTranscript(key, p.opId, TranscriptUi.Idle, open = false)
                    }
                }
                AfterPack.FAILED -> when (p.kind) {
                    PackKind.TRANSLATION -> setLine(key, p.opId, TranslateLine.PackFailed(p.language))
                    PackKind.TRANSCRIPTION -> setTranscript(key, p.opId, TranscriptUi.PackFailed(p.language), open = true)
                }
            }
            refreshCapability()
        }
    }

    /** Cancel from the quiet line (`Downloading Spanish… 40%` · `Cancel`). */
    public fun cancelDownload(conversation: String, messageId: Long, opId: Long) {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        synchronized(jobs) { jobs.remove("p:$key")?.cancel() }
        pendingTranscriptions.remove(key)
        mutableUi.update { m ->
            val cur = m[key] ?: return@update m
            val line = cur.translation
            val t = cur.transcript
            m + (
                key to cur.copy(
                    translation = if (line is TranslateLine.Downloading || line is TranslateLine.WaitingWifi) null else line,
                    transcript = if (t is TranscriptUi.Downloading || t is TranscriptUi.WaitingWifi) TranscriptUi.Idle else t,
                    transcriptOpen = if (t is TranscriptUi.Downloading || t is TranscriptUi.WaitingWifi) false else cur.transcriptOpen,
                )
                )
        }
    }

    /** The text a translation pack download was asked for (by message key). */
    private val textFor = java.util.concurrent.ConcurrentHashMap<String, String>()

    /** Remember what a message says while a pack it needs downloads. */
    public fun noteText(conversation: String, messageId: Long, text: String) {
        textFor[TranslationStoreState.messageKey(conversation, messageId)] = text
    }

    // ---------------------------------------------------------------------
    // Conversation choices
    // ---------------------------------------------------------------------

    public fun setChoices(conversation: String, change: (ConversationChoices) -> ConversationChoices) {
        val next = change(choices(conversation))
        mutateStore { it.withChoices(conversation, next) }
    }

    public fun setAutoTranslate(conversation: String, on: Boolean) {
        setChoices(conversation) { it.copy(autoTranslate = on) }
        if (!on) {
            // UX 3.7 — turning it off shows every message in its original.
            val prefix = "$conversation/"
            mutableUi.update { m ->
                m.mapValues { (k, v) -> if (k.startsWith(prefix)) v.copy(translation = null) else v }
            }
        }
    }

    public fun dismissBar(conversation: String, language: String) {
        val tag = canonicalLanguageTag(language) ?: language
        setChoices(conversation) { it.copy(dismissedBar = (it.dismissedBar + tag).distinct()) }
    }

    // ---------------------------------------------------------------------
    // Deletion
    // ---------------------------------------------------------------------

    /** A message was deleted for everyone: its results go too. */
    public fun messageDeleted(conversation: String, messageId: Long) {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        mutableUi.update { it - key }
        textFor.remove(key)
        pendingTranscriptions.remove(key)
        if (mutableStore.value.translations.containsKey(key) || mutableStore.value.transcripts.containsKey(key)) {
            mutateStore { it.withoutMessage(key) }
        }
    }

    /** Settings — `Delete all translations and transcripts`. */
    public fun deleteAllResults() {
        mutableUi.value = emptyMap()
        mutateStore { it.withoutResults() }
    }

    /**
     * Settings - `Remove` on a pack Lakko downloaded. Removes only what
     * Lakko's own engine holds; system packs are never touched.
     */
    public suspend fun removePack(language: String): Boolean {
        val removed = translation.removePack(language)
        if (removed) refreshCapability()
        return removed
    }

    /** Packs on this device, for Settings. */
    public suspend fun packs(): List<PackInfo> {
        val t = translation.installedPacks()
        val s = transcription.installedPacks()
        val merged = LinkedHashMap<String, PackInfo>()
        for (p in t + s) {
            val k = canonicalLanguageTag(p.language) ?: p.language
            val prev = merged[k]
            merged[k] = if (prev == null) {
                p.copy(language = k)
            } else {
                prev.copy(
                    forTranslation = prev.forTranslation || p.forTranslation,
                    forTranscription = prev.forTranscription || p.forTranscription,
                    // A language whose translation model Lakko downloaded can be
                    // removed even when the system also holds a speech pack for it.
                    managedBySystem = prev.managedBySystem && p.managedBySystem,
                )
            }
        }
        return merged.values.toList()
    }

    // ---------------------------------------------------------------------

    private fun mutateStore(change: (TranslationStoreState) -> TranslationStoreState) {
        val ctx = appContext
        val acct = accountIdB64
        scope.launch {
            storeLock.withLock {
                if (acct == null || acct != accountIdB64) return@withLock
                val next = change(mutableStore.value)
                mutableStore.value = next
                if (ctx != null && !SealedTranslationStore.save(ctx, acct, next)) {
                    Log.w(TAG, "store write failed code=not-written")
                }
            }
        }
    }

    private fun launchFor(name: String, block: suspend () -> Unit) {
        synchronized(jobs) {
            jobs.remove(name)?.cancel()
            val job = scope.launch { block() }
            jobs[name] = job
            job.invokeOnCompletion { synchronized(jobs) { if (jobs[name] === job) jobs.remove(name) } }
        }
    }

    private fun unmetered(context: Context): Boolean = try {
        val cm = context.getSystemService(ConnectivityManager::class.java)
        val caps = cm?.getNetworkCapabilities(cm.activeNetwork)
        caps?.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED) == true
    } catch (_: Throwable) {
        false
    }
}

/** What the line does once a pack download has an outcome. */
internal enum class AfterPack {
    /** The pack is here: translate (or transcribe) now. */
    CONTINUE,

    /**
     * Back to the line the message had before (`Spanish · Translate`; the
     * transcript closed). Nothing watches a download the platform only
     * scheduled, so `Downloading…` would never end; a later tap asks the
     * engine again (UX 3.5, ADR-0082 note of 2026-11-15).
     */
    BACK_TO_START,

    /** `Couldn't download the Spanish pack`. */
    FAILED,
}

internal fun afterPack(outcome: PackOutcome): AfterPack = when (outcome) {
    PackOutcome.DONE -> AfterPack.CONTINUE
    PackOutcome.SCHEDULED -> AfterPack.BACK_TO_START
    PackOutcome.FAILED -> AfterPack.FAILED
}
