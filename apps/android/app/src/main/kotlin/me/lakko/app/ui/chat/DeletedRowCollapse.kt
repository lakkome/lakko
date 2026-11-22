// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.EnterTransition
import androidx.compose.animation.core.MutableTransitionState
import androidx.compose.animation.core.tween
import androidx.compose.animation.shrinkVertically
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.foundation.layout.padding
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import me.lakko.app.chat.DeletedRows

/**
 * `docs/ux/message-actions.md` section 9 — the row of a deleted message in its
 * hold. [content] is drawn as it is ("Message deleted", the bubble's own
 * shape) until [collapsing], then its height animates to zero over
 * [DeletedRows.COLLAPSE_MS]; the view model removes the row when the collapse
 * ends. With the system's animations turned off (the animator duration scale
 * at 0, Android's reduce-motion setting) a collapsing row is not drawn at all:
 * it goes at the end of the hold, without the collapse.
 *
 * A row that is not [deleted] is drawn directly, with no wrapper.
 */
@Composable
internal fun DeletedRowCollapse(
    deleted: Boolean,
    collapsing: Boolean,
    content: @Composable () -> Unit,
) {
    if (!deleted) {
        content()
        return
    }
    val context = LocalContext.current
    val reducedMotion = remember(context) { animationsOff(context) }
    if (collapsing && reducedMotion) return
    val visible = remember { MutableTransitionState(true) }
    visible.targetState = !collapsing
    AnimatedVisibility(
        visibleState = visible,
        enter = EnterTransition.None,
        exit = shrinkVertically(animationSpec = tween(DeletedRows.COLLAPSE_MS.toInt())),
    ) {
        content()
    }
}

/** `true` when the system's animator duration scale is 0 (animations off). */
private fun animationsOff(context: android.content.Context): Boolean =
    try {
        android.provider.Settings.Global.getFloat(
            context.contentResolver,
            android.provider.Settings.Global.ANIMATOR_DURATION_SCALE,
            1f,
        ) == 0f
    } catch (_: Throwable) {
        false
    }

/**
 * Section 9 — the bubble of a message in its hold outside a chat timeline (a
 * channel post, a comment): "Message deleted" in the bubble's own shape, no
 * chips, no quote, no attachments, no menu.
 */
@Composable
internal fun DeletedMessageBubble(modifier: androidx.compose.ui.Modifier = androidx.compose.ui.Modifier) {
    androidx.compose.material3.Surface(
        shape = androidx.compose.foundation.shape.RoundedCornerShape(18.dp),
        color = androidx.compose.material3.MaterialTheme.colorScheme.surfaceVariant,
        modifier = modifier,
    ) {
        androidx.compose.material3.Text(
            text = me.lakko.app.l10n.t("chat.message.deleted"),
            style = androidx.compose.material3.MaterialTheme.typography.bodyMedium,
            color = androidx.compose.material3.MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.6f),
            fontStyle = androidx.compose.ui.text.font.FontStyle.Italic,
            modifier = androidx.compose.ui.Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
        )
    }
}
