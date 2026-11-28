// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.chat.ChatMessage
import me.lakko.app.l10n.t
import me.lakko.app.translation.PackKind
import me.lakko.app.translation.TranscriptUi
import me.lakko.app.translation.TranslateLine
import me.lakko.app.translation.TranslationCenter
import me.lakko.app.translation.TranslationStoreState
import me.lakko.app.ui.translation.LocalTranslationConversation
import me.lakko.app.ui.translation.TranscriptTarget
import uniffi.lakko_ffi_uniffi.TranslateOffer

/** What one bubble draws and offers for ADR-0082 (translation UX 3.1-3.7). */
internal data class BubbleTranslation(
    /** The text in the bubble: the translation while it is shown. */
    val shownText: String,
    /** The quiet line under a caption, `null` when there is none. */
    val captionLine: TranslateLine?,
    /** What Copy copies (UX 3.2: the text shown). */
    val copyText: String,
    /** Whether the menu offers its Translate row, and what it reads. */
    val menuOffered: Boolean,
    val menuLabel: String,
    val onMenuTranslate: () -> Unit,
    val onShowOriginal: () -> Unit,
    val onTranslate: () -> Unit,
    val onNotLanguage: (String) -> Unit,
    val onCancel: () -> Unit,
    /** Handed to the voice / video bubble for its transcript button. */
    val transcriptTarget: TranscriptTarget?,
)

/**
 * Decide [BubbleTranslation] for [msg]. The decisions are the shared rules
 * (`TranslationCenter` calls them over UniFFI); this only reads the state they
 * produce. Reply, Forward, Save and Select keep acting on [ChatMessage.text],
 * the original (UX 3.2); only Copy follows what is shown.
 */
@Composable
internal fun rememberBubbleTranslation(msg: ChatMessage, isOwn: Boolean): BubbleTranslation {
    val conversation = LocalTranslationConversation.current?.takeIf {
        !msg.isHistory && !msg.deleted && msg.unsupportedVersion == null && msg.messageId > 0L
    }
    val ui by TranslationCenter.ui.collectAsStateWithLifecycle()
    val settings by TranslationCenter.settings.collectAsStateWithLifecycle()
    val cap by TranslationCenter.capability.collectAsStateWithLifecycle()
    val store by TranslationCenter.store.collectAsStateWithLifecycle()
    val opId = msg.effectiveOpId
    val state = conversation?.let { ui[TranslationStoreState.messageKey(it, msg.messageId)] }?.takeIf { it.opId == opId }
    val line = state?.translation
    val transcript = (state?.transcript as? TranscriptUi.Shown)?.takeIf { state.transcriptOpen }?.text
    // The text a translation works on: the caption, else an open transcript.
    val readable = msg.text.ifEmpty { transcript.orEmpty() }
    val shown = (line as? TranslateLine.Shown)?.text
    val detections by TranslationCenter.detections.collectAsStateWithLifecycle()
    // The menu's answer changes when this message's detection arrives.
    val detected = conversation?.let { detections[TranslationStoreState.messageKey(it, msg.messageId)] }
    val offer = remember(conversation, readable, settings, cap, store, detected) {
        if (conversation == null) {
            TranslateOffer.NONE
        } else {
            TranslationCenter.translateOffer(conversation, msg.messageId, readable)
        }
    }
    // UX 3.2 — before any translation, the message's language up front.
    val languageLine = remember(conversation, readable, settings, cap, detected) {
        if (conversation == null || msg.text.isEmpty()) {
            null
        } else {
            TranslationCenter.languageLine(conversation, msg.messageId, msg.text)
        }
    }
    val autoOn = conversation != null && store.conversations[conversation]?.autoTranslate == true
    LaunchedEffect(conversation, readable) {
        if (conversation != null) TranslationCenter.detect(conversation, msg.messageId, readable, incoming = !isOwn)
    }
    LaunchedEffect(conversation, opId, autoOn, readable) {
        if (conversation != null && autoOn && readable.isNotBlank()) {
            TranslationCenter.autoTranslate(conversation, msg.messageId, opId, readable)
        }
    }
    val translate: () -> Unit = {
        if (conversation != null) TranslationCenter.translate(conversation, msg.messageId, opId, readable)
    }
    return BubbleTranslation(
        shownText = if (msg.text.isNotEmpty() && shown != null) shown else msg.text,
        captionLine = bubbleCaptionLine(msg.text, line, languageLine),
        copyText = shown ?: readable,
        menuOffered = offer != TranslateOffer.NONE || line is TranslateLine.Shown,
        menuLabel = if (line is TranslateLine.Shown) t("chat.message.show_original") else t("chat.message.translate"),
        onMenuTranslate = {
            if (conversation != null) {
                if (line is TranslateLine.Shown) {
                    TranslationCenter.showOriginal(conversation, msg.messageId, opId)
                } else {
                    translate()
                }
            }
        },
        onShowOriginal = {
            if (conversation != null) TranslationCenter.showOriginal(conversation, msg.messageId, opId)
        },
        onTranslate = translate,
        onNotLanguage = { _ ->
            if (conversation != null) {
                TranslationCenter.pickSource(conversation, msg.messageId, opId, readable, null, PackKind.TRANSLATION)
            }
        },
        onCancel = {
            if (conversation != null) TranslationCenter.cancelDownload(conversation, msg.messageId, opId)
        },
        transcriptTarget = conversation?.let { TranscriptTarget(it, msg.messageId, opId, line.takeIf { msg.text.isEmpty() }) },
    )
}

/**
 * UX 3.2 — the quiet line under a caption: the translation's own line when
 * there is one (manual or automatic), else the language line up front; none
 * without a caption.
 */
internal fun bubbleCaptionLine(caption: String, line: TranslateLine?, languageLine: TranslateLine?): TranslateLine? =
    if (caption.isEmpty()) null else line ?: languageLine
