// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowDownward
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import me.lakko.app.l10n.t
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import me.lakko.app.ui.components.LakkoPicker

/**
 * A full-screen Lakko editor: a top bar with Close, a title and the primary
 * action, then scrolling content. System back is Close.
 */
@Composable
internal fun ContentEditorScaffold(
    title: String,
    actionLabel: String,
    actionEnabled: Boolean,
    onAction: () -> Unit,
    onClose: () -> Unit,
    content: @Composable () -> Unit,
) {
    Dialog(
        onDismissRequest = onClose,
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Surface(color = MaterialTheme.colorScheme.background, modifier = Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize().safeDrawingPadding().imePadding()) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(horizontal = 4.dp),
                ) {
                    IconButton(onClick = onClose) {
                        Icon(Icons.Filled.Close, contentDescription = t("common.close"))
                    }
                    Text(
                        title,
                        fontSize = 17.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.weight(1f),
                    )
                    Button(onClick = onAction, enabled = actionEnabled, modifier = Modifier.padding(end = 8.dp)) {
                        Text(actionLabel)
                    }
                }
                Column(
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f)
                        .verticalScroll(rememberScrollState())
                        .padding(horizontal = 16.dp, vertical = 8.dp),
                ) { content() }
            }
        }
    }
}

@Composable
private fun SwitchRow(label: String, checked: Boolean, onChange: (Boolean) -> Unit, note: String? = null) {
    Column(Modifier.fillMaxWidth()) {
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)) {
            Text(label, fontSize = 15.sp, modifier = Modifier.weight(1f))
            Switch(checked = checked, onCheckedChange = onChange)
        }
        if (note != null) {
            Text(note, fontSize = 13.sp, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

/**
 * UX 7.1 / 8.1 — a list of 1..[max] text fields: a new empty field appears
 * when the last one is typed into, an emptied field that is not the last is
 * removed, and the arrows reorder.
 */
@Composable
private fun GrowingFieldList(
    items: MutableList<String>,
    max: Int,
    placeholder: @Composable (Int) -> String,
    maxChars: Int,
    trailing: @Composable (Int) -> Unit = {},
) {
    for (i in items.indices) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            trailing(i)
            OutlinedTextField(
                value = items[i],
                onValueChange = { v ->
                    items[i] = v.take(maxChars)
                    if (i == items.lastIndex && v.isNotBlank() && items.size < max) items.add("")
                },
                placeholder = { Text(placeholder(i)) },
                singleLine = true,
                modifier = Modifier
                    .weight(1f)
                    .onFocusLost { if (items[i].isBlank() && i != items.lastIndex && items.size > 2) items.removeAt(i) },
            )
            IconButton(onClick = { if (i > 0) items.add(i - 1, items.removeAt(i)) }, enabled = i > 0) {
                Icon(Icons.Filled.ArrowUpward, contentDescription = t("poll.editor.move_up"))
            }
            IconButton(
                onClick = { if (i < items.lastIndex) items.add(i + 1, items.removeAt(i)) },
                enabled = i < items.lastIndex,
            ) {
                Icon(Icons.Filled.ArrowDownward, contentDescription = t("poll.editor.move_down"))
            }
        }
    }
}

private fun Modifier.onFocusLost(action: () -> Unit): Modifier = composed {
    var had by remember { mutableStateOf(false) }
    onFocusChanged { state ->
        if (had && !state.isFocused) action()
        had = state.isFocused
    }
}

/** The result of the poll editor, before signing. */
internal data class PollDraft(
    val question: String,
    val options: List<String>,
    val anonymous: Boolean,
    val multiple: Boolean,
    val quizCorrect: Int?,
    val explanation: String,
    val closeAfterMs: Long?,
)

/** UX 7.1 — whether Create is enabled. */
internal fun pollDraftReady(question: String, options: List<String>, quiz: Boolean, correct: Int?): Boolean {
    val filled = options.map { it.trim() }.filter { it.isNotEmpty() }
    if (question.isBlank() || filled.size < 2) return false
    if (quiz) {
        val c = correct ?: return false
        if (c !in options.indices || options[c].isBlank()) return false
    }
    return true
}

/** UX 7.1 — the poll editor. */
@Composable
internal fun PollEditor(
    anonymity: uniffi.lakko_ffi_uniffi.PollAnonymity,
    onCreate: (PollDraft) -> Unit,
    onClose: () -> Unit,
) {
    var question by remember { mutableStateOf("") }
    val options = remember { mutableStateListOf("", "") }
    var anonymous by remember { mutableStateOf(anonymity != uniffi.lakko_ffi_uniffi.PollAnonymity.NOT_OFFERED) }
    var multiple by remember { mutableStateOf(false) }
    var quiz by remember { mutableStateOf(false) }
    var correct by remember { mutableStateOf<Int?>(null) }
    var explanation by remember { mutableStateOf("") }
    var closeAuto by remember { mutableStateOf(false) }
    var closeAfter by remember { mutableStateOf(DAY_MS) }
    var confirmDiscard by remember { mutableStateOf(false) }
    val dirty = question.isNotBlank() || options.any { it.isNotBlank() }
    val ready = pollDraftReady(question, options, quiz, correct)
    ContentEditorScaffold(
        title = t("poll.editor.title"),
        actionLabel = t("poll.editor.create"),
        actionEnabled = ready,
        onAction = {
            val kept = options.withIndex().filter { it.value.isNotBlank() }
            onCreate(
                PollDraft(
                    question = question.trim(),
                    options = kept.map { it.value.trim() },
                    anonymous = when (anonymity) {
                        uniffi.lakko_ffi_uniffi.PollAnonymity.NOT_OFFERED -> false
                        uniffi.lakko_ffi_uniffi.PollAnonymity.ALWAYS_ANONYMOUS -> true
                        else -> anonymous
                    },
                    multiple = multiple && !quiz,
                    quizCorrect = if (quiz) kept.indexOfFirst { it.index == correct }.takeIf { it >= 0 } else null,
                    explanation = explanation.trim(),
                    closeAfterMs = if (closeAuto) closeAfter else null,
                ),
            )
        },
        onClose = { if (dirty) confirmDiscard = true else onClose() },
    ) {
        OutlinedTextField(
            value = question,
            onValueChange = { question = it.take(255) },
            label = { Text(t("poll.editor.question")) },
            modifier = Modifier.fillMaxWidth(),
        )
        Text(t("poll.editor.options"), fontSize = 13.sp, color = MaterialTheme.colorScheme.onSurfaceVariant)
        GrowingFieldList(
            items = options,
            max = 10,
            placeholder = { i -> t("poll.editor.option", mapOf("n" to (i + 1).toString())) },
            maxChars = 100,
            trailing = { i ->
                if (quiz) RadioButton(selected = correct == i, onClick = { correct = i })
            },
        )
        if (anonymity == uniffi.lakko_ffi_uniffi.PollAnonymity.SWITCH_DEFAULT_ON) {
            SwitchRow(
                t("poll.editor.anonymous"),
                anonymous,
                { anonymous = it },
                note = t(if (anonymous) "poll.editor.anonymous_on" else "poll.editor.anonymous_off"),
            )
        }
        if (!quiz) SwitchRow(t("poll.editor.multiple"), multiple, { multiple = it })
        SwitchRow(t("poll.editor.quiz"), quiz, { quiz = it; if (it) multiple = false })
        if (quiz) {
            OutlinedTextField(
                value = explanation,
                onValueChange = { explanation = it.take(200) },
                label = { Text(t("poll.editor.explanation")) },
                modifier = Modifier.fillMaxWidth(),
            )
        }
        SwitchRow(t("poll.editor.close_auto"), closeAuto, { closeAuto = it })
        if (closeAuto) {
            LakkoPicker(
                selected = closeAfter,
                options = listOf(
                    HOUR_MS to t("poll.editor.close_1h"),
                    DAY_MS to t("poll.editor.close_1d"),
                    WEEK_MS to t("poll.editor.close_1w"),
                ),
                onPick = { closeAfter = it },
            )
        }
    }
    if (confirmDiscard) {
        LakkoDialog(
            onDismissRequest = { confirmDiscard = false },
            title = t("poll.editor.discard"),
            confirm = LakkoDialogAction(t("editor.discard"), LakkoDialogTone.Danger) { onClose() },
            cancel = LakkoDialogAction(t("editor.keep_editing"), LakkoDialogTone.Secondary) {
                confirmDiscard = false
            },
        )
    }
}

internal const val HOUR_MS: Long = 3_600_000L
internal const val DAY_MS: Long = 24 * HOUR_MS
internal const val WEEK_MS: Long = 7 * DAY_MS

/** The result of the checklist editor. */
internal data class ChecklistDraft(
    val title: String,
    val items: List<String>,
    val othersAdd: Boolean,
    val othersMark: Boolean,
)

/** UX 8.1 — the checklist editor (also the author's Edit checklist). */
@Composable
internal fun ChecklistEditor(
    initial: ChecklistDraft?,
    onSave: (ChecklistDraft) -> Unit,
    onClose: () -> Unit,
) {
    var title by remember { mutableStateOf(initial?.title ?: "") }
    val items = remember { mutableStateListOf<String>().apply { addAll(initial?.items ?: listOf("")); add("") } }
    var othersAdd by remember { mutableStateOf(initial?.othersAdd ?: false) }
    var othersMark by remember { mutableStateOf(initial?.othersMark ?: true) }
    val filled = items.map { it.trim() }.filter { it.isNotEmpty() }
    ContentEditorScaffold(
        title = t(if (initial == null) "checklist.editor.title" else "checklist.editor.edit_title"),
        actionLabel = t(if (initial == null) "checklist.editor.create" else "checklist.editor.save"),
        actionEnabled = title.isNotBlank() && filled.isNotEmpty(),
        onAction = { onSave(ChecklistDraft(title.trim(), filled.take(30), othersAdd, othersMark)) },
        onClose = onClose,
    ) {
        OutlinedTextField(
            value = title,
            onValueChange = { title = it.take(255) },
            label = { Text(t("checklist.editor.name")) },
            modifier = Modifier.fillMaxWidth(),
        )
        GrowingFieldList(
            items = items,
            max = 30,
            placeholder = { i -> t("checklist.editor.item", mapOf("n" to (i + 1).toString())) },
            maxChars = 100,
        )
        SwitchRow(t("checklist.others_add"), othersAdd, { othersAdd = it })
        SwitchRow(t("checklist.others_mark"), othersMark, { othersMark = it })
    }
}

@Composable
internal fun EditorSpacer() = Spacer(Modifier.width(8.dp))
