// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.input.InputMode
import androidx.compose.ui.platform.LocalInputModeManager
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
import me.lakko.app.l10n.t

/** How a dialog action is drawn (design system section 3, Button). */
public enum class LakkoDialogTone {
    /** The confirming action: accent fill. */
    Primary,

    /** A confirming action that destroys something: danger fill. */
    Danger,

    /** A further choice beside the confirming one: bordered, elevated surface. */
    Secondary,

    /** Cancel and its stand-ins: no border, muted text. */
    Ghost,
}

/** One button of a [LakkoDialog]. */
@Immutable
public class LakkoDialogAction(
    public val label: String,
    public val tone: LakkoDialogTone = LakkoDialogTone.Primary,
    public val enabled: Boolean = true,
    public val onClick: () -> Unit,
)

/** Where a dialog's actions go: one right-aligned row, or a full-width stack. */
internal enum class LakkoDialogActionLayout { Row, Stacked }

/** Two actions or fewer share one row; three or more stack (design system section 3). */
internal fun lakkoDialogActionLayout(actionCount: Int): LakkoDialogActionLayout =
    if (actionCount <= 2) LakkoDialogActionLayout.Row else LakkoDialogActionLayout.Stacked

/**
 * The order the actions are drawn in. A row reads Cancel first and the
 * confirming action last (the right edge); a stack puts the confirming action
 * on top, the other choices under it and Cancel last.
 */
internal fun <A> lakkoDialogActionOrder(confirm: A?, extras: List<A>, cancel: A?): List<A> {
    val count = listOfNotNull(confirm, cancel).size + extras.size
    return when (lakkoDialogActionLayout(count)) {
        LakkoDialogActionLayout.Row -> listOfNotNull(cancel) + extras + listOfNotNull(confirm)
        LakkoDialogActionLayout.Stacked -> listOfNotNull(confirm) + extras + listOfNotNull(cancel)
    }
}

/** The dim behind a dialog: black at 35%. */
private const val SCRIM_ALPHA: Float = 0.35f

/**
 * Lakko's one dialog shape (`.claude/skills/ux-ui/design-system.md`, Dialog):
 * a centred card over a dimmed screen, elevated surface, radius 12, at most
 * 440dp wide with a 24dp side margin. In order: the title, an optional body
 * sentence, an optional [content] slot (one text field, or a form or an image
 * where a call site needs one), then the actions.
 *
 * Tapping the dim and system back call [onDismissRequest], which means Cancel.
 * A notice that must be answered by a button passes a no-op. [cancel] is the
 * ghost button in Cancel's place; pass `null` for a notice with a single
 * button. The stock Material `AlertDialog` is never used.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
public fun LakkoDialog(
    onDismissRequest: () -> Unit,
    title: String?,
    confirm: LakkoDialogAction?,
    modifier: Modifier = Modifier,
    body: String? = null,
    cancel: LakkoDialogAction? = LakkoDialogAction(
        label = t("common.cancel"),
        tone = LakkoDialogTone.Ghost,
        onClick = onDismissRequest,
    ),
    extraActions: List<LakkoDialogAction> = emptyList(),
    content: (@Composable ColumnScope.() -> Unit)? = null,
) {
    Dialog(
        onDismissRequest = onDismissRequest,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        val window = (LocalView.current.parent as? DialogWindowProvider)?.window
        SideEffect { window?.setDimAmount(SCRIM_ALPHA) }
        val ordered = lakkoDialogActionOrder(confirm, extraActions, cancel)
        val layout = lakkoDialogActionLayout(ordered.size)
        val cancelFocus = remember { FocusRequester() }
        val keyboardMode = LocalInputModeManager.current.inputMode == InputMode.Keyboard
        Surface(
            shape = RoundedCornerShape(12.dp),
            color = MaterialTheme.colorScheme.surface,
            contentColor = MaterialTheme.colorScheme.onSurface,
            shadowElevation = 8.dp,
            modifier = modifier
                .padding(horizontal = 24.dp)
                .widthIn(max = 440.dp)
                .fillMaxWidth()
                .imePadding(),
        ) {
            Column(modifier = Modifier.padding(20.dp)) {
                val scrolling = body != null || content != null
                if (title != null) {
                    Text(
                        title,
                        fontSize = 17.sp,
                        lineHeight = 24.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.semantics { heading() },
                    )
                }
                if (scrolling) {
                    if (title != null) Spacer(Modifier.height(8.dp))
                    Column(
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                        modifier = Modifier
                            .weight(1f, fill = false)
                            .verticalScroll(rememberScrollState()),
                    ) {
                        if (body != null) {
                            Text(
                                body,
                                fontSize = 15.sp,
                                lineHeight = 22.sp,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        content?.invoke(this)
                    }
                }
                if (ordered.isNotEmpty()) {
                    Spacer(Modifier.height(20.dp))
                    when (layout) {
                        LakkoDialogActionLayout.Row -> FlowRow(
                            horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
                            verticalArrangement = Arrangement.spacedBy(8.dp),
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            for (action in ordered) {
                                DialogButton(action, fill = false, focus = cancelFocus.takeIf { action === cancel })
                            }
                        }
                        LakkoDialogActionLayout.Stacked -> Column(
                            verticalArrangement = Arrangement.spacedBy(8.dp),
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            for (action in ordered) {
                                DialogButton(action, fill = true, focus = cancelFocus.takeIf { action === cancel })
                            }
                        }
                    }
                }
            }
        }
        // Initial focus on Cancel, for a keyboard; a touch screen shows no
        // focus ring and is left alone.
        LaunchedEffect(cancel, keyboardMode) {
            if (cancel != null && keyboardMode) runCatching { cancelFocus.requestFocus() }
        }
    }
}

@Composable
private fun DialogButton(action: LakkoDialogAction, fill: Boolean, focus: FocusRequester?) {
    val shape = RoundedCornerShape(8.dp)
    var modifier = Modifier.heightIn(min = 44.dp)
    if (fill) modifier = modifier.fillMaxWidth()
    if (focus != null) modifier = modifier.focusRequester(focus)
    val scheme = MaterialTheme.colorScheme
    val label: @Composable () -> Unit = { Text(action.label, fontSize = 15.sp, fontWeight = FontWeight.Medium) }
    when (action.tone) {
        LakkoDialogTone.Primary -> Button(
            onClick = action.onClick,
            enabled = action.enabled,
            shape = shape,
            colors = ButtonDefaults.buttonColors(containerColor = scheme.primary, contentColor = scheme.onPrimary),
            modifier = modifier,
        ) { label() }
        LakkoDialogTone.Danger -> Button(
            onClick = action.onClick,
            enabled = action.enabled,
            shape = shape,
            colors = ButtonDefaults.buttonColors(containerColor = scheme.error, contentColor = scheme.onError),
            modifier = modifier,
        ) { label() }
        LakkoDialogTone.Secondary -> OutlinedButton(
            onClick = action.onClick,
            enabled = action.enabled,
            shape = shape,
            border = BorderStroke(1.dp, scheme.outline),
            colors = ButtonDefaults.outlinedButtonColors(containerColor = scheme.surface, contentColor = scheme.onSurface),
            modifier = modifier,
        ) { label() }
        LakkoDialogTone.Ghost -> TextButton(
            onClick = action.onClick,
            enabled = action.enabled,
            shape = shape,
            colors = ButtonDefaults.textButtonColors(contentColor = scheme.onSurfaceVariant),
            modifier = modifier,
        ) { label() }
    }
}
