// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * ADR-0082 section 5 — what this device keeps: translations, transcripts and
 * the per-conversation choices, for one account.
 *
 * One AES-GCM sealed file per account under `noBackupFilesDir`, the key in
 * the Android Keystore ([me.lakko.app.feed.FeedSealedStore], whose writes go
 * through the ADR-0077 [me.lakko.app.account.AccountStoreGate]). Never in the
 * backup, the conversation vault or the DeviceGroup synced state; wiped by
 * Forget device (the `translations` step of `AccountLocalWipe`).
 *
 * A result is keyed on (conversation, message id) and carries the message's
 * `last_op_id` and, for a translation, the target language; whether it still
 * describes the message is `storedResultIsCurrent`, the shared rule. The
 * conversation is the group the message is stored in: DS message ids are
 * per group journal.
 */
@Serializable
public data class StoredTranslation(
    val lastOpId: Long,
    val source: String,
    val target: String,
    val text: String,
)

@Serializable
public data class StoredTranscript(
    val lastOpId: Long,
    val language: String,
    val text: String,
    val noSpeech: Boolean = false,
)

/** A language picked for a conversation (`Not X?` or the picker), and when. */
@Serializable
public data class StoredLanguagePick(val tag: String, val atMs: Long)

/** One message's confidently detected language. */
@Serializable
public data class StoredMessageLanguage(val messageId: Long, val tag: String)

/**
 * What this device knows of a conversation's languages (ADR-0082 note of
 * 2026-11-15), in the canonical bounded form of the shared rule
 * (`ConversationLanguages`), changed only through it.
 */
@Serializable
public data class StoredLanguages(
    val picks: List<StoredLanguagePick> = emptyList(),
    val detections: List<StoredMessageLanguage> = emptyList(),
)

@Serializable
public data class ConversationChoices(
    /** UX 3.7 — `Translate automatically`. */
    val autoTranslate: Boolean = false,
    /** UX 3.4 — the languages picked and confidently detected here. */
    val languages: StoredLanguages = StoredLanguages(),
    /** UX 4.4 — the language last chosen for transcription here. */
    val transcriptLanguage: String? = null,
    /** UX 3.6 — languages whose bar was dismissed here (canonical tags). */
    val dismissedBar: List<String> = emptyList(),
) {
    public val isDefault: Boolean get() = this == ConversationChoices()
}

@Serializable
public data class TranslationStoreState(
    val translations: Map<String, StoredTranslation> = emptyMap(),
    val transcripts: Map<String, StoredTranscript> = emptyMap(),
    val conversations: Map<String, ConversationChoices> = emptyMap(),
) {
    /** How many results are kept (the `Delete all` dialog's count). */
    public val resultCount: Int get() = translations.size + transcripts.size

    public fun withTranslation(key: String, value: StoredTranslation): TranslationStoreState =
        copy(translations = translations + (key to value))

    public fun withTranscript(key: String, value: StoredTranscript): TranslationStoreState =
        copy(transcripts = transcripts + (key to value))

    /** UX section 2 — a message deleted for everyone takes its results with it. */
    public fun withoutMessage(key: String): TranslationStoreState =
        copy(translations = translations - key, transcripts = transcripts - key)

    public fun withChoices(conversation: String, choices: ConversationChoices): TranslationStoreState =
        copy(
            conversations = if (choices.isDefault) {
                conversations - conversation
            } else {
                conversations + (conversation to choices)
            },
        )

    /** Settings — `Delete all translations and transcripts`. The
     *  per-conversation choices are settings, not results, and stay. */
    public fun withoutResults(): TranslationStoreState = copy(translations = emptyMap(), transcripts = emptyMap())

    public companion object {
        private val json = Json { ignoreUnknownKeys = true }

        public fun messageKey(conversationHex: String, messageId: Long): String = "$conversationHex/$messageId"

        public fun encode(state: TranslationStoreState): String =
            json.encodeToString(serializer(), state)

        /** An unreadable file is an empty store, never an error. */
        public fun decode(text: String?): TranslationStoreState =
            if (text == null) {
                TranslationStoreState()
            } else {
                try {
                    json.decodeFromString(serializer(), text)
                } catch (_: Throwable) {
                    TranslationStoreState()
                }
            }
    }
}

/** The sealed file of [TranslationStoreState]. */
public object SealedTranslationStore {
    private const val DIR = "translations"

    public fun load(context: Context, accountIdB64: String): TranslationStoreState =
        try {
            TranslationStoreState.decode(me.lakko.app.feed.FeedSealedStore.read(context.applicationContext, DIR, accountIdB64))
        } catch (_: Throwable) {
            TranslationStoreState()
        }

    /** `false` when it was not written (sealed account, Keystore failure). */
    public fun save(context: Context, accountIdB64: String, state: TranslationStoreState): Boolean =
        try {
            me.lakko.app.feed.FeedSealedStore.write(
                context.applicationContext,
                DIR,
                accountIdB64,
                TranslationStoreState.encode(state),
            )
        } catch (_: Throwable) {
            false
        }

    /** Forget device. */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        try {
            me.lakko.app.feed.FeedSealedStore.delete(context.applicationContext, DIR, accountIdB64)
        } catch (_: Throwable) {
            // best-effort
        }
    }
}
