// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package me.lakko.app.ui.chat

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import me.lakko.app.chat.PIN_LIST_LABEL
import me.lakko.app.chat.PinAction
import me.lakko.app.chat.PinBarEntry
import me.lakko.app.chat.PinMenu
import me.lakko.app.chat.pinActionLabel
import me.lakko.app.chat.pinActionSubline

/*
 * ADR-0052 decision 3 + `docs/ux/message-actions.md` section 4.1 — the pinned
 * bar under the header, the list of every pin, and the sheet that asks which
 * scope a pin is for. Every rule these render (which pin is shown, in what
 * order, what the row says) is decided in `me.lakko.app.chat.Pin`; nothing
 * here re-derives one.
 */

/** How many bars the stacked indicator draws at most; a longer list slides a
 *  window over the position so the current one stays visible. */
private const val INDICATOR_MAX_SEGMENTS: Int = 5

/**
 * Section 4.1 — ONE pinned message under the header: author (or `For you`)
 * and excerpt on one line each, the stacked indicator beside them saying how
 * many and which, and the list control at the far end. Tapping the row is
 * the caller's jump-then-advance.
 */
@Composable
internal fun PinnedBar(
    author: String,
    excerpt: String,
    position: Int,
    count: Int,
    onTap: () -> Unit,
    onOpenList: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier.fillMaxWidth(),
        color = MaterialTheme.colorScheme.surface,
        contentColor = MaterialTheme.colorScheme.onSurface,
    ) {
        Column {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable(onClick = onTap)
                    .padding(start = 12.dp, end = 4.dp, top = 6.dp, bottom = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                StackedIndicator(position = position, count = count)
                Spacer(Modifier.width(10.dp))
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = author,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.primary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        text = excerpt,
                        style = MaterialTheme.typography.bodyMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                IconButton(onClick = onOpenList) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.List,
                        contentDescription = PIN_LIST_LABEL,
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            HorizontalDivider()
        }
    }
}

/**
 * The thin stacked indicator: one short bar per pin (a window of at most
 * [INDICATOR_MAX_SEGMENTS] when there are more), the shown one in the accent
 * and the rest in the outline colour. Its semantics say the same in words.
 */
@Composable
private fun StackedIndicator(position: Int, count: Int) {
    val shown = count.coerceAtLeast(1)
    val segments = minOf(shown, INDICATOR_MAX_SEGMENTS)
    val windowStart = (position - segments / 2).coerceIn(0, (shown - segments).coerceAtLeast(0))
    val lit = (position - windowStart).coerceIn(0, segments - 1)
    val active = MaterialTheme.colorScheme.primary
    val idle = MaterialTheme.colorScheme.outlineVariant
    Canvas(
        modifier = Modifier
            .width(3.dp)
            .height(32.dp)
            .semantics { contentDescription = "Pinned message ${position + 1} of $shown" },
    ) {
        val gap = 2.dp.toPx()
        val segmentHeight = (size.height - gap * (segments - 1)) / segments
        for (i in 0 until segments) {
            drawRoundRect(
                color = if (i == lit) active else idle,
                topLeft = Offset(0f, i * (segmentHeight + gap)),
                size = Size(size.width, segmentHeight),
                cornerRadius = CornerRadius(size.width / 2, size.width / 2),
            )
        }
    }
}

/**
 * Section 4.1 — every pin as rows, shared before personal, each row jumping
 * to its message and closing the list. [authorOf] and [excerptOf] are the
 * screen's own resolutions, so a name here is the name the timeline shows.
 */
@Composable
internal fun PinListSheet(
    entries: List<PinBarEntry>,
    authorOf: (PinBarEntry) -> String,
    excerptOf: (PinBarEntry) -> String,
    onPick: (PinBarEntry) -> Unit,
    onDismiss: () -> Unit,
) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState) {
        Text(
            text = PIN_LIST_LABEL,
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp),
        )
        LazyColumn(modifier = Modifier.fillMaxWidth()) {
            items(items = entries, key = { it.messageId }) { entry ->
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { onPick(entry) }
                        .padding(horizontal = 20.dp, vertical = 10.dp),
                ) {
                    Text(
                        text = authorOf(entry),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.primary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        text = excerptOf(entry),
                        style = MaterialTheme.typography.bodyMedium,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
        Spacer(Modifier.height(16.dp))
    }
}

/**
 * Section 4.1 — the two-row choice: which scope this pin (or unpin) is for,
 * one line under each row saying what it does. Rendered only for a menu
 * with more than one action; a single action acts at once without it.
 */
@Composable
internal fun PinScopeSheet(
    menu: PinMenu,
    oneToOne: Boolean,
    onPick: (PinAction) -> Unit,
    onDismiss: () -> Unit,
) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState) {
        for (action in menu.actions) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable { onPick(action) }
                    .padding(horizontal = 20.dp, vertical = 12.dp),
            ) {
                Text(
                    text = pinActionLabel(action, oneToOne),
                    style = MaterialTheme.typography.bodyLarge,
                )
                Text(
                    text = pinActionSubline(action),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        Spacer(Modifier.height(16.dp))
    }
}
