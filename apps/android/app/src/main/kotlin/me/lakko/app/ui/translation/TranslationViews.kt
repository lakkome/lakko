// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.translation

import androidx.compose.animation.core.animateFloat
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.translation.PackKind
import me.lakko.app.ui.chat.bubbleControl
import me.lakko.app.translation.TranscriptUi
import me.lakko.app.translation.TranslateLine
import me.lakko.app.translation.TranslationCenter
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import java.util.Locale
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/**
 * The conversation (the hex id of the group a message is stored in) whose
 * bubbles are being drawn, for the translation and transcript controls. `null`
 * outside a conversation: then no control is offered.
 */
public val LocalTranslationConversation: androidx.compose.runtime.ProvidableCompositionLocal<String?> =
    compositionLocalOf { null }

/** A language's name in the interface language ("Spanish"). */
public fun languageName(tag: String, uiLanguage: String): String {
    val ui = Locale.forLanguageTag(uiLanguage)
    val name = Locale.forLanguageTag(tag).getDisplayName(ui)
    return if (name.isBlank()) tag else name.replaceFirstChar { it.titlecase(ui) }
}

/** A language's own name ("Español"), the picker's titles (localization.md section 2). */
public fun ownLanguageName(tag: String): String {
    val l = Locale.forLanguageTag(tag)
    val name = l.getDisplayName(l)
    return if (name.isBlank()) tag else name.replaceFirstChar { it.titlecase(l) }
}

@Composable
private fun langName(tag: String): String = languageName(tag, LocalStrings.current.language)

/** A word of the quiet line that is a button. */
@Composable
private fun LineButton(label: String, color: Color, onClick: () -> Unit) {
    Text(
        label,
        style = MaterialTheme.typography.labelSmall,
        color = color,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .clickable(role = Role.Button, onClick = onClick)
            .padding(vertical = 4.dp),
    )
}

@Composable
private fun Dot(color: Color) {
    Text(" · ", style = MaterialTheme.typography.labelSmall, color = color)
}

/**
 * UX 3.2-3.5 — the one quiet line under a translated or translatable bubble,
 * in the bubble's secondary text colour.
 */
@Composable
public fun TranslateQuietLine(
    line: TranslateLine,
    contentColor: Color,
    onShowOriginal: () -> Unit,
    onTranslate: () -> Unit,
    onNotLanguage: (String) -> Unit,
    onCancel: () -> Unit,
    /** UX 4.2 — the line of a translated transcript leads with `Transcript`. */
    transcript: Boolean = false,
) {
    val quiet = contentColor.copy(alpha = 0.7f)
    Row(verticalAlignment = Alignment.CenterVertically) {
        val label: @Composable (String) -> Unit = { Text(it, style = MaterialTheme.typography.labelSmall, color = quiet) }
        when (line) {
            is TranslateLine.Translating -> {
                label(t("translate.line.translating"))
                line.source?.let { src ->
                    Dot(quiet)
                    LineButton(t("translate.line.not_language", mapOf("language" to langName(src))), quiet) { onNotLanguage(src) }
                }
            }
            is TranslateLine.Shown -> {
                label(
                    if (transcript) {
                        t("transcript.line.translated", mapOf("language" to langName(line.source)))
                    } else {
                        t("translate.line.translated", mapOf("language" to langName(line.source)))
                    },
                )
                Dot(quiet)
                LineButton(t("translate.line.show_original"), quiet, onShowOriginal)
                Dot(quiet)
                LineButton(t("translate.line.not_language", mapOf("language" to langName(line.source))), quiet) { onNotLanguage(line.source) }
            }
            is TranslateLine.Original -> {
                label(langName(line.source))
                Dot(quiet)
                LineButton(t("translate.line.translate"), quiet, onTranslate)
            }
            is TranslateLine.Detected -> {
                label(langName(line.source))
                if (line.canTranslate) {
                    Dot(quiet)
                    LineButton(t("translate.line.translate"), quiet, onTranslate)
                }
            }
            is TranslateLine.Failed -> {
                label(t("translate.line.failed"))
                Dot(quiet)
                LineButton(t("translate.line.try_again"), quiet, onTranslate)
            }
            is TranslateLine.SameLanguage -> label(t("translate.line.same_language", mapOf("language" to langName(line.language))))
            is TranslateLine.Downloading -> {
                // The platform reports no percentage for some packs: then the
                // line names the download without one.
                if (line.percent != null) {
                    label(
                        t(
                            "translate.pack.downloading",
                            mapOf("language" to langName(line.source), "percent" to line.percent.toString()),
                        ),
                    )
                } else {
                    label(t("translate.pack.downloading_unsized", mapOf("language" to langName(line.source))))
                }
                Dot(quiet)
                LineButton(t("translate.pack.cancel"), quiet, onCancel)
            }
            is TranslateLine.WaitingWifi -> {
                label(t("translate.pack.waiting_wifi", mapOf("language" to langName(line.source))))
                Dot(quiet)
                LineButton(t("translate.pack.cancel"), quiet, onCancel)
            }
            is TranslateLine.PackFailed -> {
                label(t("translate.pack.failed", mapOf("language" to langName(line.source))))
                Dot(quiet)
                LineButton(t("translate.line.try_again"), quiet, onTranslate)
            }
            is TranslateLine.Unsupported -> label(t("translate.unsupported_source", mapOf("language" to langName(line.source))))
        }
    }
}

/**
 * The pack dialog (UX 3.5) and the language picker (UX 3.4), drawn once per
 * conversation screen.
 */
@Composable
public fun TranslationDialogs(hub: me.lakko.app.media.ChatMediaHub?) {
    val pack by TranslationCenter.packPrompt.collectAsStateWithLifecycle()
    val picker by TranslationCenter.picker.collectAsStateWithLifecycle()
    val context = LocalContext.current
    pack?.let { p ->
        val language = langName(p.language)
        val title = t(
            if (p.kind == PackKind.TRANSLATION) "translate.pack.title.translation" else "translate.pack.title.transcription",
            mapOf("language" to language),
        )
        val size = p.sizeBytes?.let { android.text.format.Formatter.formatShortFileSize(context, it) }
        val bodyKey = when (p.kind) {
            PackKind.TRANSLATION -> if (size != null) "translate.pack.body.translation_sized" else "translate.pack.body.translation"
            PackKind.TRANSCRIPTION -> if (size != null) "translate.pack.body.transcription_sized" else "translate.pack.body.transcription"
        }
        LakkoDialog(
            onDismissRequest = TranslationCenter::dismissPack,
            title = title,
            body = t(bodyKey, buildMap { put("language", language); size?.let { put("size", it) } }),
            confirm = LakkoDialogAction(
                label = t(if (p.waitForWifi) "translate.pack.download_wifi" else "translate.pack.download"),
                onClick = TranslationCenter::acceptPack,
            ),
            cancel = LakkoDialogAction(
                label = t("translate.pack.not_now"),
                tone = LakkoDialogTone.Ghost,
                onClick = TranslationCenter::dismissPack,
            ),
        ) {
            Text(
                t("translate.pack.source.android"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    picker?.let { p ->
        LanguageSheet(
            heading = t("translate.picker.title"),
            options = p.options,
            onPick = { TranslationCenter.choosePicked(it, hub) },
            onDismiss = TranslationCenter::dismissPicker,
            selected = p.marked,
        )
    }
}

/**
 * A choice of one language from a list, as the design system's action sheet:
 * an in-app bottom sheet of 52dp rows with an optional heading, own names as
 * titles (localization.md section 2). Dismissing it changes nothing.
 */
@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
@Composable
internal fun LanguageSheet(
    heading: String?,
    options: List<String>,
    onPick: (String) -> Unit,
    onDismiss: () -> Unit,
    selected: String? = null,
) {
    val sheetState = androidx.compose.material3.rememberModalBottomSheetState(skipPartiallyExpanded = true)
    androidx.compose.material3.ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .navigationBarsPadding(),
        ) {
            if (heading != null) {
                Text(
                    heading,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.Medium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                )
            }
            for (tag in options) {
                val isCurrent = selected != null && tag == selected
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = 52.dp)
                        .semantics { this.selected = isCurrent }
                        .clickable(role = Role.Button) { onPick(tag) }
                        .padding(horizontal = 16.dp),
                ) {
                    Text(ownLanguageName(tag), fontSize = 15.sp, modifier = Modifier.weight(1f))
                    if (isCurrent) {
                        LkIcon(
                            R.drawable.ic_lk_checkmark,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.primary,
                            size = LkIconSize.Row,
                        )
                    }
                }
            }
            Spacer(Modifier.height(8.dp))
        }
    }
}

/**
 * UX 3.6 — `Messages here are in Spanish.` `Translate` · `Not now` · close.
 * Draws nothing unless the shared rule names a language and the engine can
 * translate it.
 */
@Composable
public fun ConversationTranslateBar(conversation: String, recentIncomingKeys: List<String>) {
    val settings by TranslationCenter.settings.collectAsStateWithLifecycle()
    val cap by TranslationCenter.capability.collectAsStateWithLifecycle()
    val store by TranslationCenter.store.collectAsStateWithLifecycle()
    val det by TranslationCenter.detections.collectAsStateWithLifecycle()
    val language = remember(settings, cap, store, det, recentIncomingKeys, conversation) {
        if (!cap.translation) null else TranslationCenter.barLanguage(conversation, recentIncomingKeys)
    } ?: return
    Surface(color = MaterialTheme.colorScheme.surfaceVariant, modifier = Modifier.fillMaxWidth()) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(start = 16.dp, end = 4.dp),
        ) {
            Text(
                t("translate.bar.body", mapOf("language" to langName(language))),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = { TranslationCenter.setAutoTranslate(conversation, true) }) {
                Text(t("translate.bar.translate"))
            }
            TextButton(onClick = { TranslationCenter.dismissBar(conversation, language) }) {
                Text(t("translate.bar.not_now"))
            }
            IconButton(onClick = { TranslationCenter.dismissBar(conversation, language) }) {
                LkIcon(R.drawable.ic_lk_xmark, contentDescription = t("translate.bar.close"), size = LkIconSize.Row)
            }
        }
    }
}

/** UX 3.7 — `Translate automatically`, in the conversation's info screen.
 *  Absent when this device cannot translate or the setting is off. */
@Composable
public fun AutoTranslateRow(conversation: String?) {
    if (conversation == null) return
    val settings by TranslationCenter.settings.collectAsStateWithLifecycle()
    val cap by TranslationCenter.capability.collectAsStateWithLifecycle()
    val store by TranslationCenter.store.collectAsStateWithLifecycle()
    if (!settings.showTranslate || !cap.translation) return
    val on = store.conversations[conversation]?.autoTranslate == true
    ListItem(
        headlineContent = { Text(t("translate.auto")) },
        trailingContent = {
            Switch(checked = on, onCheckedChange = null)
        },
        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
        modifier = Modifier
            .fillMaxWidth()
            .clickable(role = Role.Switch) { TranslationCenter.setAutoTranslate(conversation, !on) },
    )
}

/** The width the `Aa` button's slot keeps on every voice row (media-messages
 *  section 2: the row never changes geometry). */
public val TRANSCRIPT_BUTTON_SLOT: androidx.compose.ui.unit.Dp = 32.dp

/**
 * UX 4.1 / 4.3 — the `Aa` button. [offered] false leaves the slot reserved and
 * empty. Its size is the same in every state.
 */
@Composable
public fun TranscriptButton(
    offered: Boolean,
    state: TranscriptUi,
    open: Boolean,
    contentColor: Color,
    onClick: () -> Unit,
    onLongPress: (() -> Unit)?,
) {
    Box(Modifier.size(TRANSCRIPT_BUTTON_SLOT), contentAlignment = Alignment.Center) {
        if (!offered) return@Box
        val disabled = state is TranscriptUi.FileUnavailable
        val selected = open && (state is TranscriptUi.Shown || state is TranscriptUi.NoSpeech)
        val busy = state is TranscriptUi.LoadingAudio || state is TranscriptUi.Transcribing
        val label = t(if (open) "transcript.button.hide" else "transcript.button.show")
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .size(TRANSCRIPT_BUTTON_SLOT)
                .clip(CircleShape)
                .then(
                    if (selected) {
                        Modifier.background(contentColor.copy(alpha = 0.18f))
                    } else {
                        Modifier
                    },
                )
                .semantics {
                    contentDescription = label
                    this.selected = selected
                }
                .then(
                    if (disabled) {
                        Modifier.alpha(0.4f)
                    } else {
                        Modifier.bubbleControl(onLongPress = onLongPress, onClickLabel = label, onClick = onClick)
                    },
                ),
        ) {
            if (state is TranscriptUi.LoadingAudio) {
                CircularProgressIndicator(Modifier.size(TRANSCRIPT_BUTTON_SLOT), strokeWidth = 2.dp, color = contentColor)
            }
            val pulse = if (state is TranscriptUi.Transcribing) {
                val transition = androidx.compose.animation.core.rememberInfiniteTransition(label = "aa")
                transition.animateFloat(
                    initialValue = 1f,
                    targetValue = 0.35f,
                    animationSpec = androidx.compose.animation.core.infiniteRepeatable(
                        androidx.compose.animation.core.tween(700),
                        androidx.compose.animation.core.RepeatMode.Reverse,
                    ),
                    label = "aa-alpha",
                ).value
            } else {
                1f
            }
            Text(
                "Aa",
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                color = contentColor,
                modifier = Modifier.alpha(if (busy && state !is TranscriptUi.LoadingAudio) pulse else 1f),
            )
        }
    }
}

/** Lines a long transcript shows before `Show more` (UX 4.3). */
private const val TRANSCRIPT_PREVIEW_LINES = 6

/**
 * UX 4.2 / 4.3 — the transcript under the row, inside the same bubble. Draws
 * nothing when closed.
 */
@Composable
public fun TranscriptBlock(
    state: TranscriptUi,
    open: Boolean,
    translation: TranslateLine?,
    contentColor: Color,
    onHide: () -> Unit,
    onRetry: () -> Unit,
    onNotLanguage: (String) -> Unit,
    onShowOriginal: () -> Unit,
    onTranslate: () -> Unit,
    onCancel: () -> Unit,
    /** `Not Spanish?` of a translated transcript: the translation's picker. */
    onNotTranslationLanguage: (String) -> Unit = onNotLanguage,
) {
    if (!open) return
    val quiet = contentColor.copy(alpha = 0.7f)
    var expanded by rememberSaveable { mutableStateOf(false) }
    Column(verticalArrangement = Arrangement.spacedBy(2.dp), modifier = Modifier.padding(top = 4.dp)) {
        when (state) {
            TranscriptUi.Idle, TranscriptUi.LoadingAudio -> Unit
            is TranscriptUi.Transcribing -> {
                if (state.partial.isNotEmpty()) Text(state.partial, style = MaterialTheme.typography.bodyMedium)
                Text(t("transcript.transcribing"), style = MaterialTheme.typography.labelSmall, color = quiet)
            }
            is TranscriptUi.Shown -> {
                val shown = (translation as? TranslateLine.Shown)?.text ?: state.text
                var overflow by remember(shown) { mutableStateOf(false) }
                Text(
                    shown,
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = if (expanded) Int.MAX_VALUE else TRANSCRIPT_PREVIEW_LINES,
                    onTextLayout = { if (!expanded) overflow = it.hasVisualOverflow },
                )
                if (overflow && !expanded) LineButton(t("transcript.show_more"), quiet) { expanded = true }
                if (translation != null && translation !is TranslateLine.Original) {
                    TranslateQuietLine(
                        line = translation,
                        contentColor = contentColor,
                        onShowOriginal = onShowOriginal,
                        onTranslate = onTranslate,
                        onNotLanguage = onNotTranslationLanguage,
                        onCancel = onCancel,
                        transcript = true,
                    )
                } else {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(t("transcript.line"), style = MaterialTheme.typography.labelSmall, color = quiet)
                        Dot(quiet)
                        LineButton(t("transcript.hide"), quiet, onHide)
                        Dot(quiet)
                        LineButton(t("translate.line.not_language", mapOf("language" to langName(state.language))), quiet) {
                            onNotLanguage(state.language)
                        }
                    }
                }
            }
            TranscriptUi.NoSpeech -> Row(verticalAlignment = Alignment.CenterVertically) {
                Text(t("transcript.no_speech"), style = MaterialTheme.typography.labelSmall, color = quiet)
                Dot(quiet)
                LineButton(t("transcript.hide"), quiet, onHide)
            }
            TranscriptUi.Failed -> Row(verticalAlignment = Alignment.CenterVertically) {
                Text(t("transcript.failed"), style = MaterialTheme.typography.labelSmall, color = quiet)
                Dot(quiet)
                LineButton(t("translate.line.try_again"), quiet, onRetry)
            }
            is TranscriptUi.Downloading -> Row(verticalAlignment = Alignment.CenterVertically) {
                if (state.percent != null) {
                    Text(
                        t("translate.pack.downloading", mapOf("language" to langName(state.language), "percent" to state.percent.toString())),
                        style = MaterialTheme.typography.labelSmall,
                        color = quiet,
                    )
                } else {
                    Text(
                        t("translate.pack.downloading_unsized", mapOf("language" to langName(state.language))),
                        style = MaterialTheme.typography.labelSmall,
                        color = quiet,
                    )
                }
                Dot(quiet)
                LineButton(t("translate.pack.cancel"), quiet, onCancel)
            }
            is TranscriptUi.WaitingWifi -> Row(verticalAlignment = Alignment.CenterVertically) {
                Text(t("translate.pack.waiting_wifi", mapOf("language" to langName(state.language))), style = MaterialTheme.typography.labelSmall, color = quiet)
                Dot(quiet)
                LineButton(t("translate.pack.cancel"), quiet, onCancel)
            }
            is TranscriptUi.PackFailed -> Row(verticalAlignment = Alignment.CenterVertically) {
                Text(t("translate.pack.failed", mapOf("language" to langName(state.language))), style = MaterialTheme.typography.labelSmall, color = quiet)
                Dot(quiet)
                LineButton(t("translate.line.try_again"), quiet, onRetry)
            }
            is TranscriptUi.Unsupported -> Row(verticalAlignment = Alignment.CenterVertically) {
                Text(t("transcript.unavailable_source", mapOf("language" to langName(state.language))), style = MaterialTheme.typography.labelSmall, color = quiet)
                Dot(quiet)
                LineButton(t("translate.line.not_language", mapOf("language" to langName(state.language))), quiet) {
                    onNotLanguage(state.language)
                }
            }
            TranscriptUi.FileUnavailable -> Unit
        }
    }
}

/** The message a voice or video bubble belongs to, for its transcript. */
public data class TranscriptTarget(
    val conversation: String,
    val messageId: Long,
    val opId: Long,
    /** The translation line of the transcript (a message with no caption). */
    val translation: TranslateLine?,
)

/** Provided by the bubble around its attachments; `null` = no transcript here. */
public val LocalTranscriptTarget: androidx.compose.runtime.ProvidableCompositionLocal<TranscriptTarget?> =
    compositionLocalOf { null }

/**
 * UX 4.1 — the `Aa` button of the voice row / video bubble, in its reserved
 * slot. Empty (and still taking its space) when not offered.
 */
@Composable
public fun TranscriptSlot(
    ref: uniffi.lakko_ffi_uniffi.AttachmentRef,
    hub: me.lakko.app.media.ChatMediaHub,
    contentColor: Color,
    onLongPress: (() -> Unit)?,
) {
    val target = LocalTranscriptTarget.current
    val ui by TranslationCenter.ui.collectAsStateWithLifecycle()
    val settings by TranslationCenter.settings.collectAsStateWithLifecycle()
    val cap by TranslationCenter.capability.collectAsStateWithLifecycle()
    val store by TranslationCenter.store.collectAsStateWithLifecycle()
    val offered = remember(target, settings, cap, store) {
        target != null && TranslationCenter.transcriptButtonOffered(target.conversation)
    }
    val state = target?.let { ui[me.lakko.app.translation.TranslationStoreState.messageKey(it.conversation, it.messageId)] }
        ?.takeIf { it.opId == target.opId }
    TranscriptButton(
        offered = offered,
        state = state?.transcript ?: TranscriptUi.Idle,
        open = state?.transcriptOpen == true,
        contentColor = contentColor,
        onClick = {
            if (target != null) TranslationCenter.toggleTranscript(target.conversation, target.messageId, target.opId, ref, hub)
        },
        onLongPress = onLongPress,
    )
}

/** UX 4.2 — the transcript block under the row, inside the same bubble. */
@Composable
public fun TranscriptUnder(
    ref: uniffi.lakko_ffi_uniffi.AttachmentRef,
    hub: me.lakko.app.media.ChatMediaHub,
    contentColor: Color,
) {
    val target = LocalTranscriptTarget.current ?: return
    val ui by TranslationCenter.ui.collectAsStateWithLifecycle()
    val state = ui[me.lakko.app.translation.TranslationStoreState.messageKey(target.conversation, target.messageId)]
        ?.takeIf { it.opId == target.opId } ?: return
    val text = (state.transcript as? TranscriptUi.Shown)?.text.orEmpty()
    TranscriptBlock(
        state = state.transcript,
        open = state.transcriptOpen,
        translation = target.translation,
        contentColor = contentColor,
        onHide = { TranslationCenter.hideTranscript(target.conversation, target.messageId, target.opId) },
        onRetry = { TranslationCenter.transcribe(target.conversation, target.messageId, target.opId, ref, hub, force = true) },
        onNotLanguage = { _ ->
            TranslationCenter.pickSource(target.conversation, target.messageId, target.opId, text, ref, PackKind.TRANSCRIPTION)
        },
        onShowOriginal = { TranslationCenter.showOriginal(target.conversation, target.messageId, target.opId) },
        onTranslate = { TranslationCenter.translate(target.conversation, target.messageId, target.opId, text) },
        onCancel = { TranslationCenter.cancelDownload(target.conversation, target.messageId, target.opId) },
        onNotTranslationLanguage = { _ ->
            TranslationCenter.pickSource(target.conversation, target.messageId, target.opId, text, null, PackKind.TRANSLATION)
        },
    )
}
