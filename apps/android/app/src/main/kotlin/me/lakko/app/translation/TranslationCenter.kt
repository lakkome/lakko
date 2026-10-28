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
import uniffi.lakko_ffi_uniffi.DetectedLanguage
import uniffi.lakko_ffi_uniffi.TranslateOffer
import uniffi.lakko_ffi_uniffi.TranslateOfferInput
import uniffi.lakko_ffi_uniffi.TranslationEngineState
import uniffi.lakko_ffi_uniffi.canonicalLanguageTag
import uniffi.lakko_ffi_uniffi.conversationBarLanguage
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
    internal val messageKey: String,
    internal val conversation: String,
    internal val opId: Long,
    internal val text: String,
    internal val ref: AttachmentRef?,
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

    /** Detections by message key (empty while there is no detector). */
    private val mutableDetections = MutableStateFlow<Map<String, DetectedLanguage>>(emptyMap())
    public val detections: StateFlow<Map<String, DetectedLanguage>> = mutableDetections.asStateFlow()

    private val jobs = HashMap<String, Job>()

    /** Install the engines of this device; idempotent. */
    public fun install(context: Context, uiLanguage: () -> String) {
        val ctx = context.applicationContext
        interfaceLanguage = uiLanguage
        if (appContext != null) return
        appContext = ctx
        settingsStore = TranslationSettingsStore(ctx).also { mutableSettings.value = it.load() }
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
        mutableDetections.value = emptyMap()
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
        mutableDetections.value = emptyMap()
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
                val chosen = mutableStore.value.conversations.values.mapNotNull { it.translationSource }.distinct()
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
     * The source the device would translate [key] from: a confident
     * detection, else the language chosen for this conversation (UX 3.4:
     * "remembered for the conversation"), else nothing.
     */
    private fun knownSource(conversation: String, key: String): DetectedLanguage? {
        val d = mutableDetections.value[key]
        if (d != null && d.confidencePermille >= translationDetectionConfidentPermille()) return d
        val chosen = choices(conversation).translationSource ?: return d
        return DetectedLanguage(chosen, 1000.toUShort())
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
            TranslateOfferInput(
                enabled = settings.value.showTranslate,
                hasText = text.isNotBlank(),
                isOwn = isOwn,
                detected = detected,
                engineForDetected = pair ?: TranslationEngineState.UNAVAILABLE,
                anyEngine = cap.translation,
            ),
            reading(),
        )
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
        val det = mutableDetections.value
        val recent = recentIncomingKeys.mapNotNull { det[it] }
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

    /** Run the detector on a message once (a no-op without a detector). */
    public fun detect(conversation: String, messageId: Long, text: String) {
        if (detector === NoLanguageDetector || text.isBlank()) return
        val key = TranslationStoreState.messageKey(conversation, messageId)
        if (mutableDetections.value.containsKey(key)) return
        scope.launch {
            val d = detector.detect(text) ?: return@launch
            mutableDetections.update { it + (key to d) }
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

    /** The menu's `Translate` (or the line's `Translate` / `Try again`). */
    public fun translate(conversation: String, messageId: Long, opId: Long, text: String, explicitSource: String? = null) {
        val key = TranslationStoreState.messageKey(conversation, messageId)
        val target = target()
        val source = explicitSource
            ?: knownSource(conversation, key)?.tag?.let { canonicalLanguageTag(it) ?: it }
        if (source == null) {
            openPicker(PackKind.TRANSLATION, conversation, key, opId, text, null)
            return
        }
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
                TranslationEngineState.READY -> runTranslation(conversation, key, opId, text, source, target)
            }
        }
    }

    private suspend fun runTranslation(conversation: String, key: String, opId: Long, text: String, source: String, target: String) {
        val out = translation.translate(text, source, target)
        if (out == null) {
            Log.w(TAG, "translation failed code=engine")
            setLine(key, opId, TranslateLine.Failed(source))
            return
        }
        setLine(key, opId, TranslateLine.Shown(source, out))
        mutateStore { it.withTranslation(key, StoredTranslation(opId, source, target, out)) }
        // UX 3.4 — a picked source is remembered for the conversation.
        if (choices(conversation).translationSource == null) {
            setChoices(conversation) { it.copy(translationSource = source) }
        }
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
            runTranslation(conversation, key, opId, text, source, target())
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
        openPicker(kind, conversation, TranslationStoreState.messageKey(conversation, messageId), opId, text, ref)
    }

    private fun openPicker(kind: PackKind, conversation: String, key: String, opId: Long, text: String, ref: AttachmentRef?) {
        scope.launch {
            val options = when (kind) {
                PackKind.TRANSLATION -> translation.sources(target())
                PackKind.TRANSCRIPTION -> transcription.languages()
            }.mapNotNull { canonicalLanguageTag(it) }.distinct()
            mutablePicker.value = PickerPrompt(kind, options, key, conversation, opId, text, ref)
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
                setChoices(p.conversation) { it.copy(translationSource = language) }
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
            when (outcome) {
                PackOutcome.DONE -> when (p.kind) {
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
                PackOutcome.SCHEDULED -> when (p.kind) {
                    PackKind.TRANSLATION -> setLine(key, p.opId, TranslateLine.Downloading(p.language, null))
                    PackKind.TRANSCRIPTION -> setTranscript(key, p.opId, TranscriptUi.Downloading(p.language, null), open = true)
                }
                PackOutcome.FAILED -> when (p.kind) {
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
