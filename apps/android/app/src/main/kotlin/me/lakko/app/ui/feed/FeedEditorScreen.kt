// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.feed

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import me.lakko.app.feed.FeedCore
import me.lakko.app.feed.FeedSource
import me.lakko.app.feed.Membership
import me.lakko.app.feed.blankDraft
import me.lakko.app.feed.draftProblem
import me.lakko.app.feed.membershipOf
import me.lakko.app.feed.parseWords
import me.lakko.app.feed.presetDraft
import me.lakko.app.feed.withMembership
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import uniffi.lakko_ffi_uniffi.FeedHalfLife
import uniffi.lakko_ffi_uniffi.FeedPreset
import uniffi.lakko_ffi_uniffi.FeedRule
import uniffi.lakko_ffi_uniffi.FeedSuggestions
import uniffi.lakko_ffi_uniffi.FeedWeights

/** One line of the editor's preview. */
public data class PreviewLine(val key: String, val source: String, val text: String)

private data class WeightField(
    /** Catalog key of the slider label. */
    val labelKey: String,
    val get: (FeedWeights) -> UByte,
    val set: (FeedWeights, UByte) -> FeedWeights,
)

private val WEIGHTS: List<WeightField> = listOf(
    WeightField("feed.editor.weight.recency", { it.recency }, { w, v -> w.copy(recency = v) }),
    WeightField("feed.editor.weight.reactions", { it.reactions }, { w, v -> w.copy(reactions = v) }),
    WeightField("feed.editor.weight.replies", { it.replies }, { w, v -> w.copy(replies = v) }),
    WeightField("feed.editor.weight.from_contacts", { it.fromContacts }, { w, v -> w.copy(fromContacts = v) }),
    WeightField("feed.editor.weight.mentions", { it.mentionsMe }, { w, v -> w.copy(mentionsMe = v) }),
    WeightField("feed.editor.weight.media", { it.hasMedia }, { w, v -> w.copy(hasMedia = v) }),
)

private val HALF_LIVES = listOf(
    FeedHalfLife.FEW_HOURS to "feed.editor.fade.hours",
    FeedHalfLife.DAY to "feed.editor.fade.day",
    FeedHalfLife.WEEK to "feed.editor.fade.week",
)

private val SUGGESTIONS = listOf(
    FeedSuggestions.NEVER to "feed.editor.suggestions.never",
    FeedSuggestions.RARELY to "feed.editor.suggestions.rarely",
    FeedSuggestions.SOMETIMES to "feed.editor.suggestions.sometimes",
)

/**
 * The feed editor (`docs/ux/feed.md` section 4.1): one screen, four sections,
 * and a live preview under it that re-ranks as the person changes things.
 * Everything is optional; a blank feed is "Latest". Mirror of
 * `apps/web/src/feed/FeedEditor.tsx`.
 *
 * Validity is the core's: Save is disabled with the core's refusal turned into
 * one sentence ([draftProblem]), never a check written here.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun FeedEditorScreen(
    core: FeedCore,
    initial: FeedRule,
    isNew: Boolean,
    sources: List<FeedSource>,
    preview: (FeedRule) -> List<PreviewLine>?,
    error: UiText?,
    onSave: (FeedRule) -> Unit,
    onCancel: () -> Unit,
    onDelete: (() -> Unit)?,
) {
    var draft by remember(initial) { mutableStateOf(initial) }
    var hideText by remember(initial) { mutableStateOf(initial.hideWords.joinToString("\n")) }
    var onlyText by remember(initial) { mutableStateOf(initial.onlyWords.joinToString("\n")) }
    var windowText by remember(initial) { mutableStateOf(initial.windowDays.toString()) }
    var minReactionsText by remember(initial) { mutableStateOf(initial.minReactions.toString()) }
    val maxWeight = remember(core) { core.maxWeight().coerceAtLeast(1) }

    val problem = remember(draft) { draftProblem(core, draft) }
    val lines = remember(draft, problem) { if (problem == null) preview(draft) else null }

    fun startFrom(value: FeedPreset?) {
        val name = draft.name
        val icon = draft.icon
        val next = (if (value == null) blankDraft(core, name) else presetDraft(core, value, name)).copy(icon = icon)
        draft = next
        hideText = next.hideWords.joinToString("\n")
        onlyText = next.onlyWords.joinToString("\n")
        windowText = next.windowDays.toString()
        minReactionsText = next.minReactions.toString()
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(if (isNew) t("feed.new") else t("feed.edit_feed")) },
                navigationIcon = { TextButton(onClick = onCancel) { Text(t("common.cancel")) } },
                actions = {
                    TextButton(onClick = { onSave(draft) }, enabled = problem == null) { Text(t("common.save")) }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedTextField(
                value = draft.name,
                onValueChange = { draft = draft.copy(name = it.take(64)) },
                label = { Text(t("feed.editor.name")) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            // UX spec section 4.1 — the icon, chosen from a grid under the name.
            FeedIconPicker(selected = draft.icon, onSelect = { draft = draft.copy(icon = it) })

            if (isNew) {
                SectionTitle(t("feed.editor.start_from"))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(onClick = { startFrom(null) }) { Text(t("feed.editor.start.blank")) }
                    TextButton(onClick = { startFrom(FeedPreset.LATEST) }) { Text(t("feed.preset.latest")) }
                    TextButton(onClick = { startFrom(FeedPreset.POPULAR) }) { Text(t("feed.preset.popular")) }
                    TextButton(onClick = { startFrom(FeedPreset.FOR_ME) }) { Text(t("feed.preset.for_me")) }
                }
            }

            SectionTitle(t("feed.editor.sources"))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                val both = draft.groups && draft.channels
                FilterChip(
                    selected = both,
                    onClick = { draft = draft.copy(groups = true, channels = true) },
                    label = { Text(t("feed.editor.sources.both")) },
                )
                FilterChip(
                    selected = draft.groups && !draft.channels,
                    onClick = { draft = draft.copy(groups = true, channels = false) },
                    label = { Text(t("feed.editor.sources.groups")) },
                )
                FilterChip(
                    selected = draft.channels && !draft.groups,
                    onClick = { draft = draft.copy(groups = false, channels = true) },
                    label = { Text(t("feed.editor.sources.channels")) },
                )
            }
            CheckRow(t("feed.editor.include_muted"), draft.includeMuted) { draft = draft.copy(includeMuted = it) }
            if (draft.sourcesStripped) {
                Hint(t("feed.editor.stripped_hint"))
            }
            if (sources.isNotEmpty()) {
                Hint(t("feed.editor.membership_hint"))
                for (s in sources) {
                    ConversationMembershipRow(
                        title = s.title,
                        value = membershipOf(draft, s.idHex),
                        onChange = { draft = withMembership(draft, s.idHex, it) },
                    )
                }
            }

            HorizontalDivider()
            SectionTitle(t("feed.editor.what_counts"))
            for (w in WEIGHTS) {
                Text(t(w.labelKey), style = MaterialTheme.typography.bodyMedium)
                Slider(
                    value = w.get(draft.weights).toFloat(),
                    onValueChange = { v ->
                        draft = draft.copy(weights = w.set(draft.weights, v.toInt().coerceIn(0, maxWeight).toUByte()))
                    },
                    valueRange = 0f..maxWeight.toFloat(),
                )
                Row(modifier = Modifier.fillMaxWidth()) {
                    Text(t("feed.editor.slider.min"), style = MaterialTheme.typography.labelSmall, modifier = Modifier.weight(1f))
                    Text(t("feed.editor.slider.max"), style = MaterialTheme.typography.labelSmall)
                }
            }
            Text(t("feed.editor.fade"), style = MaterialTheme.typography.bodyMedium)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                for ((value, label) in HALF_LIVES) {
                    FilterChip(
                        selected = draft.halfLife == value,
                        onClick = { draft = draft.copy(halfLife = value) },
                        label = { Text(t(label)) },
                    )
                }
            }
            OutlinedTextField(
                value = windowText,
                onValueChange = { raw ->
                    windowText = raw.filter(Char::isDigit).take(3)
                    windowText.toIntOrNull()?.let { draft = draft.copy(windowDays = it.coerceIn(0, 255).toUByte()) }
                },
                label = { Text(t("feed.editor.look_back")) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.fillMaxWidth(),
            )

            HorizontalDivider()
            SectionTitle(t("feed.editor.filters"))
            CheckRow(t("feed.editor.for_me"), draft.forMe) {
                draft = draft.copy(forMe = it)
            }
            CheckRow(t("feed.editor.media_only"), draft.mediaOnly) { draft = draft.copy(mediaOnly = it) }
            CheckRow(t("feed.editor.hide_own"), draft.hideOwn) { draft = draft.copy(hideOwn = it) }
            OutlinedTextField(
                value = minReactionsText,
                onValueChange = { raw ->
                    minReactionsText = raw.filter(Char::isDigit).take(3)
                    val n = minReactionsText.toIntOrNull() ?: 0
                    draft = draft.copy(minReactions = n.coerceIn(0, 999).toUShort())
                },
                label = { Text(t("feed.editor.min_reactions")) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = hideText,
                onValueChange = {
                    hideText = it
                    draft = draft.copy(hideWords = parseWords(core, it))
                },
                label = { Text(t("feed.editor.hide_words")) },
                minLines = 3,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = onlyText,
                onValueChange = {
                    onlyText = it
                    draft = draft.copy(onlyWords = parseWords(core, it))
                },
                label = { Text(t("feed.editor.only_words")) },
                minLines = 3,
                modifier = Modifier.fillMaxWidth(),
            )

            HorizontalDivider()
            SectionTitle(t("feed.editor.suggestions"))
            Text(t("feed.editor.suggestions_rate"), style = MaterialTheme.typography.bodyMedium)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                for ((value, label) in SUGGESTIONS) {
                    FilterChip(
                        selected = draft.suggestions == value,
                        onClick = { draft = draft.copy(suggestions = value) },
                        label = { Text(t(label)) },
                    )
                }
            }

            (problem ?: error)?.let {
                Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
            }
            if (!isNew && onDelete != null) {
                TextButton(onClick = onDelete) {
                    Text(t("feed.editor.delete"), color = MaterialTheme.colorScheme.error)
                }
            }

            HorizontalDivider()
            SectionTitle(t("feed.editor.preview"))
            when {
                lines == null -> Hint(t("feed.editor.preview_pending"))
                lines.isEmpty() -> Hint(t("feed.empty.no_match"))
                else -> for (l in lines) {
                    Column(modifier = Modifier.padding(vertical = 4.dp)) {
                        Text(
                            l.source,
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Text(l.text, style = MaterialTheme.typography.bodySmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
                    }
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
private fun SectionTitle(text: String) {
    Text(text, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
}

@Composable
private fun Hint(text: String) {
    Text(text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

@Composable
private fun CheckRow(label: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Checkbox(checked = checked, onCheckedChange = onChange)
        Text(label, style = MaterialTheme.typography.bodyMedium)
    }
}

@Composable
private fun ConversationMembershipRow(title: String, value: Membership, onChange: (Membership) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
        Text(
            title,
            style = MaterialTheme.typography.bodyMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        Box {
            TextButton(onClick = { open = true }) { Text(membershipLabel(value)) }
            DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                for (m in Membership.entries) {
                    DropdownMenuItem(
                        text = { Text(membershipLabel(m)) },
                        onClick = {
                            open = false
                            onChange(m)
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun membershipLabel(m: Membership): String = when (m) {
    Membership.ANY -> "\u2014"
    Membership.ONLY -> t("feed.editor.membership.only")
    Membership.NEVER -> t("feed.editor.membership.never")
}
