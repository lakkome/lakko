// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusDirection
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.zIndex
import me.lakko.app.R
import me.lakko.app.l10n.t
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import me.lakko.app.ui.components.LakkoPicker
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

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
                        me.lakko.app.ui.components.LkIcon(me.lakko.app.R.drawable.ic_lk_xmark, contentDescription = t("common.close"))
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

/**
 * UX 7.1a — the poll and checklist editors' frame: a top bar with Close at
 * the start and the title centred, scrolling content on the screen
 * background, and one full-width primary button at the bottom. System back is
 * Close.
 */
@Composable
private fun CompactEditorScaffold(
    title: String,
    actionLabel: String,
    actionEnabled: Boolean,
    onAction: () -> Unit,
    onClose: () -> Unit,
    content: @Composable ColumnScope.() -> Unit,
) {
    Dialog(
        onDismissRequest = onClose,
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        // Edge to edge, so the footer button clears the navigation bar
        // through the safe-drawing padding below (see EdgeToEdgeDialogWindow).
        me.lakko.app.ui.call.EdgeToEdgeDialogWindow()
        EditorSystemBarIcons(light = MaterialTheme.colorScheme.background.luminance() > 0.5f)
        Surface(color = MaterialTheme.colorScheme.background, modifier = Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize().safeDrawingPadding().imePadding()) {
                Box(Modifier.fillMaxWidth().height(56.dp)) {
                    IconButton(onClick = onClose, modifier = Modifier.align(Alignment.CenterStart).padding(start = 4.dp)) {
                        LkIcon(R.drawable.ic_lk_xmark, contentDescription = t("common.close"))
                    }
                    Text(
                        title,
                        fontSize = 17.sp,
                        fontWeight = FontWeight.SemiBold,
                        textAlign = TextAlign.Center,
                        maxLines = 1,
                        modifier = Modifier.align(Alignment.Center).padding(horizontal = 56.dp),
                    )
                }
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f)
                        .verticalScroll(rememberScrollState())
                        .padding(horizontal = 16.dp, vertical = 4.dp),
                    content = content,
                )
                Button(
                    onClick = onAction,
                    enabled = actionEnabled,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 8.dp)
                        .heightIn(min = 48.dp)
                        .testTag("editor-primary"),
                ) {
                    Text(actionLabel, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
                }
            }
        }
    }
}

/**
 * The status and navigation bar icons of the editor's edge-to-edge dialog
 * window follow the screen background: dark icons on the light theme, light
 * icons on the dark one.
 */
@Composable
private fun EditorSystemBarIcons(light: Boolean) {
    val view = LocalView.current
    SideEffect {
        var parent: android.view.ViewParent? = view.parent
        while (parent != null && parent !is androidx.compose.ui.window.DialogWindowProvider) parent = parent.parent
        val window = (parent as? androidx.compose.ui.window.DialogWindowProvider)?.window ?: return@SideEffect
        val controller = androidx.core.view.WindowCompat.getInsetsController(window, window.decorView)
        controller.isAppearanceLightStatusBars = light
        controller.isAppearanceLightNavigationBars = light
    }
}

/** A card of grouped rows on the screen background (design system: radius 12, surface). */
@Composable
private fun EditorCard(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    Surface(
        color = MaterialTheme.colorScheme.surface,
        shape = MaterialTheme.shapes.medium,
        modifier = modifier.fillMaxWidth(),
    ) { Column(content = content) }
}

/** The small muted label above a group of rows. */
@Composable
private fun EditorSectionLabel(text: String) {
    Text(
        text,
        fontSize = 13.sp,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 6.dp),
    )
}

/** A borderless field with a muted placeholder, for a row inside an [EditorCard]. */
@Composable
private fun EditorField(
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
    modifier: Modifier = Modifier,
    singleLine: Boolean = true,
    imeAction: ImeAction = ImeAction.Next,
    onIme: () -> Unit = {},
) {
    val scheme = MaterialTheme.colorScheme
    BasicTextField(
        value = value,
        onValueChange = onValueChange,
        singleLine = singleLine,
        maxLines = if (singleLine) 1 else 4,
        textStyle = TextStyle(fontSize = 15.sp, color = scheme.onSurface),
        cursorBrush = SolidColor(scheme.primary),
        keyboardOptions = KeyboardOptions(imeAction = imeAction),
        keyboardActions = KeyboardActions(onNext = { onIme() }, onDone = { onIme() }),
        modifier = modifier,
        decorationBox = { inner ->
            Box(contentAlignment = Alignment.CenterStart) {
                if (value.isEmpty()) {
                    Text(placeholder, fontSize = 15.sp, color = scheme.onSurfaceVariant, maxLines = 1)
                }
                inner()
            }
        },
    )
}

/** A switch row inside an [EditorCard]: label at the start, switch at the end, an optional note under it. */
@Composable
private fun EditorSwitchRow(label: String, checked: Boolean, onChange: (Boolean) -> Unit, note: String? = null) {
    Column(Modifier.fillMaxWidth()) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = 48.dp)
                .toggleable(value = checked, role = Role.Switch, onValueChange = onChange)
                .padding(horizontal = 16.dp),
        ) {
            Text(label, fontSize = 15.sp, color = MaterialTheme.colorScheme.onSurface, modifier = Modifier.weight(1f))
            Switch(checked = checked, onCheckedChange = null)
        }
        if (note != null) {
            Text(
                note,
                fontSize = 13.sp,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(start = 16.dp, end = 16.dp, bottom = 10.dp),
            )
        }
    }
}

@Composable
private fun EditorDivider(start: Int = 16) {
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant, modifier = Modifier.padding(start = start.dp))
}

/**
 * UX 7.1a — the options / items card: one single-line field per row with a
 * drag handle at its start and a remove icon at its end (above the minimum),
 * then `Add option` / `Add item` (below the maximum). IME Next moves to the
 * next row and adds one on the last. TalkBack offers Move up / Move down on
 * each row and announces the new position.
 */
@Composable
private fun EditorRowList(
    rows: EditorRows,
    onRows: (EditorRows) -> Unit,
    placeholder: @Composable (Int) -> String,
    removeLabel: @Composable (Int) -> String,
    addLabel: String,
    testTagPrefix: String,
    leading: @Composable (EditorRow) -> Unit = {},
    onRemoved: (Long) -> Unit = {},
) {
    val scheme = MaterialTheme.colorScheme
    val view = LocalView.current
    val focusManager = LocalFocusManager.current
    val requesters = remember { HashMap<Long, FocusRequester>() }
    var focusTarget by remember { mutableStateOf<Long?>(null) }
    val heights = remember { mutableStateMapOf<Long, Int>() }
    var dragging by remember { mutableStateOf<Long?>(null) }
    var dragOffset by remember { mutableFloatStateOf(0f) }
    // The latest rows, read by gesture and accessibility callbacks between
    // recompositions (a drag can move a row twice before the parent recomposes).
    val latest = remember { LatestRows(rows) }
    latest.value = rows

    val movedLabels = (1..rows.max).map { t("editor.moved", mapOf("n" to it.toString())) }
    val moveUp = t("poll.editor.move_up")
    val moveDown = t("poll.editor.move_down")
    fun announceMove(position: Int) {
        movedLabels.getOrNull(position - 1)?.let { view.announceForAccessibility(it) }
    }
    fun apply(next: EditorRows) {
        latest.value = next
        onRows(next)
    }

    LaunchedEffect(focusTarget) {
        val id = focusTarget ?: return@LaunchedEffect
        runCatching { requesters[id]?.requestFocus() }
        focusTarget = null
    }

    EditorCard {
        rows.rows.forEachIndexed { index, row -> key(row.id) {
            if (index > 0) EditorDivider(start = 52)
            val requester = requesters.getOrPut(row.id) { FocusRequester() }
            val isDragged = dragging == row.id
            val isLast = index == rows.rows.lastIndex
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = 48.dp)
                    .zIndex(if (isDragged) 1f else 0f)
                    .graphicsLayer { translationY = if (isDragged) dragOffset else 0f }
                    .onGloballyPositioned { heights[row.id] = it.size.height }
                    .semantics {
                        customActions = listOfNotNull(
                            if (index > 0) {
                                CustomAccessibilityAction(moveUp) {
                                    apply(latest.value.move(index, index - 1))
                                    announceMove(index)
                                    true
                                }
                            } else {
                                null
                            },
                            if (!isLast) {
                                CustomAccessibilityAction(moveDown) {
                                    apply(latest.value.move(index, index + 1))
                                    announceMove(index + 2)
                                    true
                                }
                            } else {
                                null
                            },
                        )
                    }
                    .testTag("$testTagPrefix-row-$index"),
            ) {
                Box(
                    contentAlignment = Alignment.Center,
                    modifier = Modifier
                        .padding(start = 4.dp)
                        .size(44.dp)
                        .pointerInput(row.id) {
                            detectDragGestures(
                                onDragStart = {
                                    dragging = row.id
                                    dragOffset = 0f
                                },
                                onDrag = { change, amount ->
                                    change.consume()
                                    dragOffset += amount.y
                                    val list = latest.value
                                    val i = list.indexOf(row.id)
                                    if (dragOffset > 0 && i in 0 until list.rows.lastIndex) {
                                        val below = heights[list.rows[i + 1].id] ?: 0
                                        if (below > 0 && dragOffset > below / 2f) {
                                            val moved = list.move(i, i + 1)
                                            apply(moved)
                                            dragOffset -= below
                                        }
                                    } else if (dragOffset < 0 && i > 0) {
                                        val above = heights[list.rows[i - 1].id] ?: 0
                                        if (above > 0 && -dragOffset > above / 2f) {
                                            val moved = list.move(i, i - 1)
                                            apply(moved)
                                            dragOffset += above
                                        }
                                    }
                                },
                                onDragEnd = {
                                    dragging = null
                                    dragOffset = 0f
                                },
                                onDragCancel = {
                                    dragging = null
                                    dragOffset = 0f
                                },
                            )
                        },
                ) {
                    LkIcon(R.drawable.ic_lk_drag_handle, contentDescription = null, size = LkIconSize.Row, tint = scheme.onSurfaceVariant)
                }
                leading(row)
                EditorField(
                    value = row.text,
                    onValueChange = { apply(latest.value.setText(row.id, it, EDITOR_ROW_MAX_CHARS)) },
                    placeholder = placeholder(index),
                    imeAction = if (isLast && !rows.canAdd) ImeAction.Done else ImeAction.Next,
                    onIme = {
                        val next = latest.value.next(row.id)
                        if (next == null) {
                            focusManager.clearFocus()
                        } else {
                            apply(next.first)
                            focusTarget = next.second
                        }
                    },
                    modifier = Modifier
                        .weight(1f)
                        .padding(start = 4.dp, end = if (rows.canRemove) 0.dp else 16.dp)
                        .focusRequester(requester)
                        .testTag("$testTagPrefix-field-$index"),
                )
                if (rows.canRemove) {
                    IconButton(
                        onClick = {
                            apply(latest.value.remove(row.id))
                            requesters.remove(row.id)
                            heights.remove(row.id)
                            onRemoved(row.id)
                        },
                        modifier = Modifier.testTag("$testTagPrefix-remove-$index"),
                    ) {
                        LkIcon(
                            R.drawable.ic_lk_xmark,
                            contentDescription = removeLabel(index),
                            size = LkIconSize.Row,
                            tint = scheme.onSurfaceVariant,
                        )
                    }
                }
            }
        } }
        if (rows.canAdd) {
            EditorDivider(start = 52)
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = 48.dp)
                    .clickable(role = Role.Button) {
                        latest.value.add()?.let { (added, id) ->
                            apply(added)
                            focusTarget = id
                        }
                    }
                    .testTag("$testTagPrefix-add"),
            ) {
                Box(contentAlignment = Alignment.Center, modifier = Modifier.padding(start = 4.dp).size(44.dp)) {
                    LkIcon(R.drawable.ic_lk_plus, contentDescription = null, size = LkIconSize.Row, tint = scheme.primary)
                }
                Text(
                    addLabel,
                    fontSize = 15.sp,
                    color = scheme.primary,
                    modifier = Modifier.padding(start = 4.dp),
                )
            }
        }
    }
}

/** A plain holder of the latest rows (not snapshot state: written from callbacks too). */
private class LatestRows(var value: EditorRows)

/** The single field at the top of an editor (question / title), in its own card. */
@Composable
private fun EditorHeadField(value: String, onValueChange: (String) -> Unit, placeholder: String, testTag: String) {
    val focusManager = LocalFocusManager.current
    EditorCard(Modifier.padding(top = 8.dp)) {
        EditorField(
            value = value,
            // The field wraps but holds one line: Enter (a hardware key, or a
            // keyboard that sends a line break) moves to the first row instead.
            onValueChange = { v ->
                if (v.contains('\n')) {
                    onValueChange(v.replace("\n", ""))
                    focusManager.moveFocus(FocusDirection.Next)
                } else {
                    onValueChange(v)
                }
            },
            onIme = { focusManager.moveFocus(FocusDirection.Next) },
            placeholder = placeholder,
            singleLine = false,
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = 48.dp)
                .padding(horizontal = 16.dp, vertical = 13.dp)
                .testTag(testTag),
        )
    }
}

/** UX 7.1 / 7.1a — the poll editor. */
@Composable
internal fun PollEditor(
    anonymity: uniffi.lakko_ffi_uniffi.PollAnonymity,
    onCreate: (PollDraft) -> Unit,
    onClose: () -> Unit,
) {
    var question by remember { mutableStateOf("") }
    var options by remember { mutableStateOf(EditorRows.of(emptyList(), POLL_OPTIONS_MIN, POLL_OPTIONS_MAX)) }
    var anonymous by remember { mutableStateOf(anonymity != uniffi.lakko_ffi_uniffi.PollAnonymity.NOT_OFFERED) }
    var multiple by remember { mutableStateOf(false) }
    var quiz by remember { mutableStateOf(false) }
    var correctId by remember { mutableStateOf<Long?>(null) }
    var explanation by remember { mutableStateOf("") }
    var closeAuto by remember { mutableStateOf(false) }
    var closeAfter by remember { mutableStateOf(DAY_MS) }
    var confirmDiscard by remember { mutableStateOf(false) }
    val focusManager = LocalFocusManager.current
    val dirty = question.isNotBlank() || options.anyText
    val texts = options.rows.map { it.text }
    val correctIndex = correctId?.let { options.indexOf(it) }?.takeIf { it >= 0 }
    val ready = pollDraftReady(question, texts, quiz, correctIndex)
    CompactEditorScaffold(
        title = t("poll.editor.title"),
        actionLabel = t("poll.editor.create"),
        actionEnabled = ready,
        onAction = {
            onCreate(
                PollDraft(
                    question = question.trim(),
                    options = options.filled,
                    anonymous = when (anonymity) {
                        uniffi.lakko_ffi_uniffi.PollAnonymity.NOT_OFFERED -> false
                        uniffi.lakko_ffi_uniffi.PollAnonymity.ALWAYS_ANONYMOUS -> true
                        else -> anonymous
                    },
                    multiple = multiple && !quiz,
                    quizCorrect = if (quiz) pollCorrectIndex(options, correctId) else null,
                    explanation = explanation.trim(),
                    closeAfterMs = if (closeAuto) closeAfter else null,
                ),
            )
        },
        onClose = { if (dirty) confirmDiscard = true else onClose() },
    ) {
        EditorHeadField(question, { question = it.take(255) }, t("poll.editor.question"), "poll-question")
        EditorSectionLabel(t("poll.editor.options"))
        EditorRowList(
            rows = options,
            onRows = { options = it },
            placeholder = { i -> t("poll.editor.option", mapOf("n" to (i + 1).toString())) },
            removeLabel = { i -> t("poll.editor.remove_option", mapOf("n" to (i + 1).toString())) },
            addLabel = t("poll.editor.add_option"),
            testTagPrefix = "poll-option",
            leading = { row ->
                if (quiz) {
                    RadioButton(
                        selected = correctId == row.id,
                        onClick = { correctId = row.id },
                        modifier = Modifier.size(40.dp),
                    )
                }
            },
            onRemoved = { id -> if (correctId == id) correctId = null },
        )
        if (quiz) {
            EditorCard(Modifier.padding(top = 12.dp)) {
                EditorField(
                    value = explanation,
                    onValueChange = { explanation = it.take(200) },
                    placeholder = t("poll.editor.explanation"),
                    singleLine = false,
                    imeAction = ImeAction.Done,
                    onIme = { focusManager.clearFocus() },
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = 48.dp)
                        .padding(horizontal = 16.dp, vertical = 13.dp),
                )
            }
        }
        EditorCard(Modifier.padding(top = 16.dp)) {
            var first = true
            if (anonymity == uniffi.lakko_ffi_uniffi.PollAnonymity.SWITCH_DEFAULT_ON) {
                EditorSwitchRow(
                    t("poll.editor.anonymous"),
                    anonymous,
                    { anonymous = it },
                    note = t(if (anonymous) "poll.editor.anonymous_on" else "poll.editor.anonymous_off"),
                )
                first = false
            }
            if (!quiz) {
                if (!first) EditorDivider()
                EditorSwitchRow(t("poll.editor.multiple"), multiple, { multiple = it })
                first = false
            }
            if (!first) EditorDivider()
            EditorSwitchRow(t("poll.editor.quiz"), quiz, { quiz = it; if (it) multiple = false })
            EditorDivider()
            EditorSwitchRow(t("poll.editor.close_auto"), closeAuto, { closeAuto = it })
            if (closeAuto) {
                LakkoPicker(
                    selected = closeAfter,
                    options = listOf(
                        HOUR_MS to t("poll.editor.close_1h"),
                        DAY_MS to t("poll.editor.close_1d"),
                        WEEK_MS to t("poll.editor.close_1w"),
                    ),
                    onPick = { closeAfter = it },
                    modifier = Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, bottom = 12.dp),
                )
            }
        }
        Spacer(Modifier.height(8.dp))
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

/** UX 8.1 — whether Create / Save is enabled. */
internal fun checklistDraftReady(title: String, items: EditorRows): Boolean =
    title.isNotBlank() && items.filled.isNotEmpty()

/** UX 8.1 / 7.1a — the checklist editor (also the author's Edit checklist). */
@Composable
internal fun ChecklistEditor(
    initial: ChecklistDraft?,
    onSave: (ChecklistDraft) -> Unit,
    onClose: () -> Unit,
) {
    var title by remember { mutableStateOf(initial?.title ?: "") }
    var items by remember {
        mutableStateOf(EditorRows.of(initial?.items ?: emptyList(), CHECKLIST_ITEMS_MIN, CHECKLIST_ITEMS_MAX))
    }
    var othersAdd by remember { mutableStateOf(initial?.othersAdd ?: false) }
    var othersMark by remember { mutableStateOf(initial?.othersMark ?: true) }
    var confirmDiscard by remember { mutableStateOf(false) }
    val dirty = if (initial == null) {
        title.isNotBlank() || items.anyText
    } else {
        title.trim() != initial.title || items.filled != initial.items ||
            othersAdd != initial.othersAdd || othersMark != initial.othersMark
    }
    CompactEditorScaffold(
        title = t(if (initial == null) "checklist.editor.title" else "checklist.editor.edit_title"),
        actionLabel = t(if (initial == null) "checklist.editor.create" else "checklist.editor.save"),
        actionEnabled = checklistDraftReady(title, items),
        onAction = { onSave(ChecklistDraft(title.trim(), items.filled, othersAdd, othersMark)) },
        onClose = { if (dirty) confirmDiscard = true else onClose() },
    ) {
        EditorHeadField(title, { title = it.take(255) }, t("checklist.editor.name"), "checklist-title")
        EditorSectionLabel(t("checklist.editor.items"))
        EditorRowList(
            rows = items,
            onRows = { items = it },
            placeholder = { i -> t("checklist.editor.item", mapOf("n" to (i + 1).toString())) },
            removeLabel = { i -> t("checklist.editor.remove_item", mapOf("n" to (i + 1).toString())) },
            addLabel = t("checklist.editor.add_item"),
            testTagPrefix = "checklist-item",
        )
        EditorCard(Modifier.padding(top = 16.dp)) {
            EditorSwitchRow(t("checklist.others_add"), othersAdd, { othersAdd = it })
            EditorDivider()
            EditorSwitchRow(t("checklist.others_mark"), othersMark, { othersMark = it })
        }
        Spacer(Modifier.height(8.dp))
    }
    if (confirmDiscard) {
        LakkoDialog(
            onDismissRequest = { confirmDiscard = false },
            title = t("checklist.editor.discard"),
            confirm = LakkoDialogAction(t("editor.discard"), LakkoDialogTone.Danger) { onClose() },
            cancel = LakkoDialogAction(t("editor.keep_editing"), LakkoDialogTone.Secondary) {
                confirmDiscard = false
            },
        )
    }
}

@Composable
internal fun EditorSpacer() = Spacer(Modifier.width(8.dp))
