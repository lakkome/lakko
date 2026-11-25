// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.PressInteraction
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.indication
import androidx.compose.foundation.layout.Box
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.State
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.runtime.remember
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.layout.LocalPinnableContainer
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.onLongClick
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.zIndex
import me.lakko.app.l10n.t
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.changedToUp
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import me.lakko.app.chat.PinSlot
import me.lakko.app.chat.pinDropIndex
import me.lakko.app.chat.pinDropIndexMeasured
import me.lakko.app.chat.pinShift
import me.lakko.app.chat.pinShiftPx

/**
 * Dragging a pinned conversation to another place among the pins
 * (`docs/ux/navigation.md` section 2.2.1): the Compose half. Where a dropped
 * row lands and which rows step aside are the pure [pinDropIndex] and
 * [pinShift]; this holds the drag while the finger is down and scrolls the
 * list when the lifted row nears an edge.
 *
 * The lifted row stays in its own slot of the list while it is dragged and is
 * drawn translated by [displayOffset]; the other pins are drawn translated by
 * [shiftPx]. Nothing in the list's order changes until the drop, when the new
 * order is written and drawn at once (the caller holds it optimistically) and
 * every translation returns to 0 in the same frame.
 */
@Stable
internal class PinDragController(
    private val listState: LazyListState,
    private val scope: CoroutineScope,
    /** Within this distance of the list's top or bottom edge the list scrolls. */
    private val edgePx: Float,
    /** The most the list scrolls in one frame, at the very edge. */
    private val maxScrollPerFramePx: Float,
) {
    /** The list keys of the pins in the order the list draws them. Set by the
     *  list on every composition. */
    var pinKeys: List<String> = emptyList()

    /** The key of the row being dragged, or `null`. */
    var draggedKey: String? by mutableStateOf(null)
        private set

    /** The dragged row's index among the pins when the drag started. */
    var from: Int by mutableIntStateOf(-1)
        private set

    /** Where the dragged row would land if lifted now. */
    var target: Int by mutableIntStateOf(-1)
        private set

    private var draggedHeight by mutableIntStateOf(0)

    /** How far the finger has taken the row from its slot, scroll included. */
    private var rawOffset by mutableFloatStateOf(0f)

    /** The dragged row's slot top in list coordinates, kept up to date as
     *  the list scrolls under it. */
    private var slotTop = 0f
    private var autoscroll: Job? = null

    val active: Boolean get() = draggedKey != null

    /** Start dragging [key]. `false` when it is not a pin on screen. */
    fun start(key: String): Boolean {
        val index = pinKeys.indexOf(key)
        if (index < 0) return false
        val info = listState.layoutInfo.visibleItemsInfo.firstOrNull { it.key == key } ?: return false
        slotTop = info.offset.toFloat()
        draggedHeight = info.size
        rawOffset = 0f
        from = index
        target = index
        draggedKey = key
        autoscroll?.cancel()
        autoscroll = scope.launch { autoscrollLoop() }
        return true
    }

    /** The finger moved by [dy]. */
    fun drag(dy: Float) {
        if (!active) return
        rawOffset += dy
        retarget()
    }

    /**
     * The finger lifted: the dragged key and where it lands, or `null` when
     * it lands where it started (nothing to write). The drag ends either way.
     */
    fun end(): Pair<String, Int>? {
        val key = draggedKey ?: return null
        retarget()
        val moved = if (target != from && target >= 0) key to target else null
        reset()
        return moved
    }

    /** The touch was taken away (the system cancelled it): nothing moves. */
    fun cancel() {
        reset()
    }

    private fun reset() {
        autoscroll?.cancel()
        autoscroll = null
        draggedKey = null
        from = -1
        target = -1
        rawOffset = 0f
        draggedHeight = 0
    }

    /** The dragged row's translation, held within the pins where their
     *  edges are on screen, so it is never drawn among the unpinned rows. */
    val displayOffset: Float
        get() {
            if (!active) return 0f
            val (lo, hi) = blockBounds()
            val top = slotTop + rawOffset
            val clampedTop = top.coerceIn(lo, (hi - draggedHeight).coerceAtLeast(lo))
            return clampedTop - slotTop
        }

    /** How far the row under [key] is drawn moved aside, in pixels. */
    fun shiftPx(key: String): Float {
        if (!active) return 0f
        val index = pinKeys.indexOf(key)
        if (index < 0) return 0f
        return pinShiftPx(index, from, target, draggedHeight)
    }

    /** The top of the first pin and the bottom of the last, where on screen;
     *  unbounded where off it. */
    private fun blockBounds(): Pair<Float, Float> {
        val visible = listState.layoutInfo.visibleItemsInfo
        val first = pinKeys.firstOrNull()?.let { k -> visible.firstOrNull { it.key == k } }
        val last = pinKeys.lastOrNull()?.let { k -> visible.firstOrNull { it.key == k } }
        val lo = when {
            first == null -> Float.NEGATIVE_INFINITY
            first.key == draggedKey -> slotTop
            else -> first.offset.toFloat()
        }
        val hi = when {
            last == null -> Float.POSITIVE_INFINITY
            last.key == draggedKey -> slotTop + draggedHeight
            else -> (last.offset + last.size).toFloat()
        }
        return lo to hi
    }

    /** Every pin's slot as measured now; the dragged one's own slot, which
     *  it keeps while lifted, at its index. Rows differ in height. */
    private fun measuredSlots(key: String): List<PinSlot?> {
        val visible = listState.layoutInfo.visibleItemsInfo
        return pinKeys.map { k ->
            if (k == key) {
                PinSlot(slotTop, draggedHeight)
            } else {
                visible.firstOrNull { it.key == k }?.let { PinSlot(it.offset.toFloat(), it.size) }
            }
        }
    }

    private fun retarget() {
        val key = draggedKey ?: return
        // The lifted row's own height, re-measured while it is on screen (a
        // call row can open or close under it).
        listState.layoutInfo.visibleItemsInfo.firstOrNull { it.key == key }?.let { draggedHeight = it.size }
        val dragged = pinKeys.indexOf(key).takeIf { it >= 0 } ?: from
        target = pinDropIndexMeasured(slotTop + displayOffset, draggedHeight, measuredSlots(key), dragged)
    }

    private suspend fun autoscrollLoop() {
        while (scope.isActive && active) {
            val info = listState.layoutInfo
            val top = slotTop + rawOffset
            val bottom = top + draggedHeight
            val start = info.viewportStartOffset.toFloat()
            val end = info.viewportEndOffset.toFloat()
            val step = when {
                top < start + edgePx -> -maxScrollPerFramePx * ((start + edgePx - top) / edgePx).coerceIn(0f, 1f)
                bottom > end - edgePx -> maxScrollPerFramePx * ((bottom - (end - edgePx)) / edgePx).coerceIn(0f, 1f)
                else -> 0f
            }
            if (step != 0f && scrollAllowed(step > 0f)) {
                val consumed = listState.scrollBy(step)
                // The slot moved with the content; the row stays under the finger.
                slotTop -= consumed
                rawOffset += consumed
                retarget()
            }
            withFrameNanos { }
        }
    }

    /** No scrolling past the pin block: down only while the last pin is not
     *  yet on screen, up only while the first is not. */
    private fun scrollAllowed(down: Boolean): Boolean {
        val visible = listState.layoutInfo.visibleItemsInfo
        val info = listState.layoutInfo
        return if (down) {
            val last = pinKeys.lastOrNull()?.let { k -> visible.firstOrNull { it.key == k } }
            last == null || last.key == draggedKey || last.offset + last.size > info.viewportEndOffset
        } else {
            val first = pinKeys.firstOrNull()?.let { k -> visible.firstOrNull { it.key == k } }
            first == null || first.key == draggedKey || first.offset < info.viewportStartOffset
        }
    }
}

/**
 * The gesture of a pinned row that can be dragged (navigation 2.2.1, iOS and
 * Android row of the table): a tap opens the conversation; press and hold,
 * then move, drags; press and hold and lift without moving opens the row's
 * menu. Before the hold completes the row gives the touch up at once to a
 * scroll or a swipe that takes it; after it, every move is the drag's, so the
 * list, the pager and the swipe do not move under it.
 */
internal fun Modifier.pinDragGesture(
    key: String,
    controller: PinDragController,
    interactionSource: MutableInteractionSource,
    onClick: () -> Unit,
    onHold: () -> Unit,
    onOpenMenu: () -> Unit,
    onDrop: (key: String, toIndex: Int) -> Unit,
): Modifier = pointerInput(key, controller) {
    awaitEachGesture {
        val down = awaitFirstDown(requireUnconsumed = false)
        val press = PressInteraction.Press(down.position)
        interactionSource.tryEmit(press)
        val slop = viewConfiguration.touchSlop
        var phase = HoldPhase.Held
        val beforeHold = withTimeoutOrNull(viewConfiguration.longPressTimeoutMillis) {
            while (true) {
                val event = awaitPointerEvent()
                val change = event.changes.firstOrNull { it.id == down.id }
                if (change == null || change.isConsumed) return@withTimeoutOrNull HoldPhase.Cancelled
                if (change.changedToUp()) return@withTimeoutOrNull HoldPhase.Tapped
                if ((change.position - down.position).getDistance() > slop) {
                    return@withTimeoutOrNull HoldPhase.Cancelled
                }
            }
            @Suppress("UNREACHABLE_CODE")
            HoldPhase.Cancelled
        }
        if (beforeHold != null) phase = beforeHold
        when (phase) {
            HoldPhase.Tapped -> {
                interactionSource.tryEmit(PressInteraction.Release(press))
                onClick()
                return@awaitEachGesture
            }
            HoldPhase.Cancelled -> {
                interactionSource.tryEmit(PressInteraction.Cancel(press))
                return@awaitEachGesture
            }
            HoldPhase.Held -> Unit
        }
        onHold()
        var dragging = false
        try {
            while (true) {
                val event = awaitPointerEvent(PointerEventPass.Main)
                val change = event.changes.firstOrNull { it.id == down.id }
                if (change == null) {
                    if (dragging) controller.cancel()
                    break
                }
                if (change.changedToUp()) {
                    change.consume()
                    if (dragging) {
                        controller.end()?.let { (k, to) -> onDrop(k, to) }
                    } else {
                        onOpenMenu()
                    }
                    break
                }
                if (!dragging && (change.position - down.position).getDistance() > slop) {
                    dragging = controller.start(key)
                    if (!dragging) {
                        // Not on screen as a pin any more: the hold ends here.
                        break
                    }
                }
                if (dragging) controller.drag(change.positionChange().y)
                change.consume()
            }
        } catch (cancelled: CancellationException) {
            if (dragging) controller.cancel()
            throw cancelled
        } finally {
            interactionSource.tryEmit(PressInteraction.Release(press))
        }
    }
}

private enum class HoldPhase { Tapped, Cancelled, Held }

/** One pinned row that can be dragged and moved (navigation 2.2.1). */
internal class RowReorder(
    /** The row's key in the list. */
    val key: String,
    val controller: PinDragController,
    /** Move up, or `null` when the shared rule offers none (the first pin). */
    val moveUp: (() -> Unit)?,
    /** Move down, or `null` (the last pin). */
    val moveDown: (() -> Unit)?,
    /** The row was dropped at this index among the pins. */
    val onDrop: (Int) -> Unit,
)

/**
 * The list item of a pin that can be dragged: lifted above the others and
 * following the finger while it is the one dragged, stepping aside while
 * another is, at rest otherwise. Kept composed while it is dragged, so a drag
 * that scrolls its own slot off screen does not lose the finger.
 */
@Composable
internal fun PinReorderItem(reorder: RowReorder, content: @Composable () -> Unit) {
    val controller = reorder.controller
    val dragged = controller.draggedKey == reorder.key
    val pinnable = LocalPinnableContainer.current
    DisposableEffect(dragged, pinnable) {
        val handle = if (dragged) pinnable?.pin() else null
        onDispose { handle?.release() }
    }
    val shift: State<Float> = animateFloatAsState(
        targetValue = if (controller.active && !dragged) controller.shiftPx(reorder.key) else 0f,
        label = "pin-shift",
    )
    val modifier = when {
        dragged -> Modifier
            .zIndex(1f)
            .graphicsLayer { translationY = controller.displayOffset }
            // Design system shadow-2 on the row's own surface (the dialogs'
            // elevation, LakkoDialog).
            .shadow(LIFTED_ELEVATION)
        // At the drop the new order is drawn in the same frame: no shift
        // left over to animate back from.
        controller.active -> Modifier.graphicsLayer { translationY = shift.value }
        else -> Modifier
    }
    Box(modifier) { content() }
}

/** The lifted row's elevation: shadow-2, as `LakkoDialog` draws it. */
private val LIFTED_ELEVATION = 8.dp

/**
 * The tap, the menu, the drag and the TalkBack actions of a pin that can be
 * reordered, in place of the row's `combinedClickable`: the same tap and the
 * same long-press menu for TalkBack, plus "Move up" / "Move down" where the
 * shared rule offers them.
 */
@Composable
internal fun Modifier.pinReorderable(
    reorder: RowReorder,
    onClick: () -> Unit,
    onOpenMenu: () -> Unit,
): Modifier {
    val interaction = remember { MutableInteractionSource() }
    val haptic = LocalHapticFeedback.current
    val moveUpLabel = t("chats.action.move_up")
    val moveDownLabel = t("chats.action.move_down")
    val currentClick by rememberUpdatedState(onClick)
    val currentMenu by rememberUpdatedState(onOpenMenu)
    val currentDrop by rememberUpdatedState(reorder.onDrop)
    val actions = buildList {
        reorder.moveUp?.let { up -> add(CustomAccessibilityAction(moveUpLabel) { up(); true }) }
        reorder.moveDown?.let { down -> add(CustomAccessibilityAction(moveDownLabel) { down(); true }) }
    }
    return this
        .semantics(mergeDescendants = true) {
            onClick { currentClick(); true }
            onLongClick { currentMenu(); true }
            if (actions.isNotEmpty()) customActions = actions
        }
        .indication(interaction, LocalIndication.current)
        .pinDragGesture(
            key = reorder.key,
            controller = reorder.controller,
            interactionSource = interaction,
            onClick = { currentClick() },
            onHold = { haptic.performHapticFeedback(HapticFeedbackType.LongPress) },
            onOpenMenu = { currentMenu() },
            onDrop = { _, to -> currentDrop(to) },
        )
}
