// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Forward
import androidx.compose.material.icons.automirrored.filled.Reply
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.RoundRect
import androidx.compose.ui.graphics.ClipOp
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.layout.layoutId
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Popup
import androidx.compose.ui.window.PopupPositionProvider
import androidx.compose.ui.window.PopupProperties
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionOnScreen
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntRect
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.LayoutDirection

/**
 * The per-message action menu — `docs/ux/message-actions.md` sections 2, 3
 * and 7.
 *
 * Section 7 says the platform's own dropdown cannot express this shape, and
 * that is why this file exists rather than another `DropdownMenu`: a
 * `DropdownMenu` is one vertical column of rows, so a reaction row inside it
 * can only be a row that opens another row of things — which is the defect
 * section 2 forbids in as many words, and which is what Android shipped (the
 * commonest action on a message cost two taps).
 *
 * Three pieces, and they are deliberately separate composables rather than one:
 *
 *  - [MessageReactionRow] sits ABOVE the bubble. One tap per emoji.
 *  - [MessageActionCard] sits BELOW it, and is a surface of its own so that it
 *    is sized by its own content. Section 2: "It is a separate surface from the
 *    reaction row and must not inherit its width." Putting both in one card is
 *    what stretches the action rows to the width of the emoji row, which is the
 *    web defect the same sentence was written against.
 *  - [MessageActionMenuOverlay] is the presentation: one window-sized popup
 *    carrying a scrim with a hole where the bubble is, so the conversation
 *    recedes and the acted-on message stays fully legible IN PLACE. It is
 *    literally in place: nothing is drawn over the bubble and the bubble is not
 *    moved or copied, the dim is simply not painted there.
 */

/** Radius of the undimmed hole in the scrim. UX section 4 — 18 for bubbles, so
 *  the hole follows the bubble's own corners rather than squaring them off. */
private val HOLE_RADIUS: Dp = 18.dp

/** Gap between the bubble and each of the two surfaces around it. */
private val BUBBLE_GAP: Dp = 8.dp

/** Keep both surfaces this far from the window edges. */
private val WINDOW_MARGIN: Dp = 8.dp

/** The scrim's opacity. Enough that the conversation reads as behind the menu,
 *  little enough that a person still sees where in the conversation they are. */
private const val SCRIM_ALPHA: Float = 0.45f

/** Label on the scrim, which dismisses the menu. Also the one node that says
 *  "the menu is open" whatever actions this particular message offers, which is
 *  what the UI tests anchor on. */
internal const val CLOSE_MENU_LABEL: String = "Close message actions"

/** Label on the reaction row's trailing control. */
internal const val MORE_REACTIONS_LABEL: String = "More reactions"

/**
 * Which rows [MessageActionCard] offers. Computed by the caller from the
 * message, so the card stays a pure renderer of a decision taken once.
 *
 * React is NOT here: section 2 takes the reactions out of the card entirely, so
 * a card with no rows at all is a real state (someone else's message with no
 * text) and the reaction row above it is still the whole point of the menu.
 */
internal data class MessageActions(
    /** Section 3 — Copy is offered on "any message with text". */
    val canCopy: Boolean,
    /** Section 3 — Edit and Delete on "own, not deleted". */
    val canEdit: Boolean,
    val canDelete: Boolean,
    /** Section 3.1 (ADR-0052) — Reply is offered on any message with an id. */
    val canReply: Boolean = false,
    /** Section 5.1 (ADR-0052 decision 2) — Forward is offered on any message
     *  with an id that is not deleted and whose send is complete. */
    val canForward: Boolean = false,
    /** Section 4.1 (ADR-0052 decision 3) — Pin sits after Forward on any
     *  message whose send is complete and that is not deleted. */
    val canPin: Boolean = false,
    /** Section 4.1 — what the row reads: `Pin`, or `Unpin` when this device
     *  can undo a pin in at least one scope. Decided by `pinMenuFor`. */
    val pinLabel: String = "Pin",
) {
    /** Whether the card has anything to draw. */
    val any: Boolean get() = canCopy || canEdit || canDelete || canReply || canForward || canPin
}

/**
 * Section 3's actions for [msg] as seen by this device. [menuAvailable] is the
 * caller's ADR-0045 §6.2.5 gate (no membership, no ops).
 */
internal fun messageActionsFor(
    isOwn: Boolean,
    deleted: Boolean,
    text: String,
    menuAvailable: Boolean,
    /** Section 5.1 — whether the row's send has landed. A row still sending,
     *  or one that failed, has nothing a forward could point at and may hold
     *  attachments that never reached storage; it does not offer Forward. */
    sendComplete: Boolean = true,
    /** Section 4.1 — the row's word for this message (`Pin` / `Unpin`). */
    pinLabel: String = "Pin",
): MessageActions = MessageActions(
    // Copy reads a string this device already holds and sends nothing, so it is
    // the one action that does NOT die with the membership gate. It is still
    // gated on the menu being available because the menu is how it is reached.
    canCopy = menuAvailable && text.isNotEmpty(),
    canEdit = menuAvailable && isOwn && !deleted,
    canDelete = menuAvailable && isOwn && !deleted,
    // Section 3.1 — "any message with an id". `menuAvailable` already carries
    // the id gate (a row with no DS id has nothing to point at), the deleted
    // gate and the membership gate: a reply composes an application message,
    // which the core refuses once membership has ended, like Send.
    canReply = menuAvailable,
    // Section 5.1 — "A message that cannot be forwarded yet (its attachments
    // still uploading, or a failed send) does not offer Forward." The id gate
    // and the deleted gate ride in `menuAvailable`, as for Reply.
    canForward = menuAvailable && sendComplete,
    // Section 4.1 — "on any message whose send is complete and that is not
    // deleted": the same gates as Forward. A personal pin sends nothing, so
    // the membership gate inside `menuAvailable` is stricter than a pin
    // strictly needs; it is kept because the menu is how the row is reached.
    canPin = menuAvailable && sendComplete,
    pinLabel = pinLabel,
)

/**
 * Put [text] on the system clipboard. Section 3: "Nothing leaves the device."
 *
 * The clip label is what the OS shows in its own paste UI; the message text is
 * the clip itself.
 */
internal fun copyMessageText(context: Context, text: String) {
    val clipboard =
        context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
            ?: return
    clipboard.setPrimaryClip(ClipData.newPlainText("Lakko message", text))
}

/**
 * The whole presentation: a window-sized popup holding the scrim, the reaction
 * row above [bubbleBounds] and the action card below it.
 *
 * [bubbleBounds] is the bubble's rectangle in WINDOW coordinates, which is the
 * same space the popup's own content is placed in (see [WindowOriginProvider]),
 * so the two need no translation between them.
 *
 * [alignEnd] follows the bubble's side of the conversation — section 2: "on a
 * phone it sits under the bubble, aligned to the bubble's own side".
 */
@Composable
internal fun MessageActionMenuOverlay(
    bubbleBounds: Rect,
    alignEnd: Boolean,
    actions: MessageActions,
    reactions: List<String>,
    onReact: (String) -> Unit,
    onOpenReactionPicker: () -> Unit,
    onReply: () -> Unit,
    onCopy: () -> Unit,
    onEdit: () -> Unit,
    onDelete: () -> Unit,
    onDismiss: () -> Unit,
    /** Section 5.1 (ADR-0052 decision 2) — Forward was chosen. */
    onForward: () -> Unit = {},
    /** Section 4.1 (ADR-0052 decision 3) — Pin / Unpin was chosen. */
    onPin: () -> Unit = {},
) {
    Popup(
        popupPositionProvider = WindowOriginProvider,
        onDismissRequest = onDismiss,
        // Focusable so the system back gesture reaches `onDismissRequest`
        // rather than leaving the chat while the menu is up.
        properties = PopupProperties(focusable = true),
    ) {
        val holeRadius = with(LocalDensity.current) { HOLE_RADIUS.toPx() }
        // A popup is its OWN WINDOW, and that is the whole difficulty here.
        // The bubble measures itself in the chat window's space; this content
        // lives in the popup's, which the system lays out inside its insets,
        // so the status bar's height sits between the two origins. Measured on
        // a phone: the popup's content began at screen y=128 while the bubble
        // reported its position from y=0, so a hole drawn at the bubble's raw
        // coordinates landed 128px low - over the message BELOW the one being
        // acted on, which from the outside reads as "the area around the tap
        // is lit rather than the message".
        //
        // `positionInWindow` cannot bridge it: inside a popup it reports a
        // position in the POPUP's window and so answers zero. Screen space is
        // the one coordinate system both windows share, so both sides are
        // measured there and the difference is the offset.
        var origin by remember { mutableStateOf<Offset?>(null) }
        Box(
            Modifier
                .fillMaxSize()
                .onGloballyPositioned { origin = it.positionOnScreen() },
        ) {
            // Nothing is placed until the origin is known - one frame - because
            // a hole drawn at the wrong offset is worse than no hole at all.
            val bubble = origin?.let { bubbleBounds.translate(-it) } ?: return@Box
            Box(
                Modifier
                    .fillMaxSize()
                    .semantics { contentDescription = CLOSE_MENU_LABEL }
                    .clickable(
                        interactionSource = remember { MutableInteractionSource() },
                        // No ripple: a ripple over the whole conversation reads
                        // as a control, and the scrim is a way out, not a row.
                        indication = null,
                        onClick = onDismiss,
                    )
                    .drawBehind {
                        val hole = Path().apply {
                            addRoundRect(
                                RoundRect(
                                    bubble,
                                    CornerRadius(holeRadius, holeRadius),
                                ),
                            )
                        }
                        // Everything BUT the bubble is dimmed, so the message
                        // being acted on is untouched rather than redrawn.
                        clipPath(hole, ClipOp.Difference) {
                            drawRect(Color.Black.copy(alpha = SCRIM_ALPHA))
                        }
                    },
            )
            MenuLayout(
                bubble = bubble,
                alignEnd = alignEnd,
                row = {
                    MessageReactionRow(
                        reactions = reactions,
                        onReact = onReact,
                        onOpenPicker = onOpenReactionPicker,
                    )
                },
                card = {
                    if (actions.any) {
                        MessageActionCard(
                            actions = actions,
                            onReply = onReply,
                            onForward = onForward,
                            onPin = onPin,
                            onCopy = onCopy,
                            onEdit = onEdit,
                            onDelete = onDelete,
                        )
                    }
                },
            )
        }
    }
}

/**
 * Section 2 — the reaction row: a horizontal row of emoji, each one tap, with a
 * trailing control that opens the full picker.
 *
 * It is a pill of its own rather than the first row of the card, and that is the
 * width rule in section 2: two surfaces, each sized by what is in it.
 */
@Composable
internal fun MessageReactionRow(
    reactions: List<String>,
    onReact: (String) -> Unit,
    onOpenPicker: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier,
        shape = RoundedCornerShape(percent = 50),
        color = MaterialTheme.colorScheme.surface,
        contentColor = MaterialTheme.colorScheme.onSurface,
        // Section 2: each of the two surfaces carries its own background,
        // border and shadow, so the pill takes the card's border and elevation
        // and keeps only its own shape.
        border = menuSurfaceBorder(),
        shadowElevation = MENU_SURFACE_ELEVATION,
    ) {
        Row(
            // A narrow window (or a longer emoji set than the six defaults)
            // scrolls rather than clipping the picker control off the end: the
            // trailing control is the only route to any other emoji.
            modifier = Modifier
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = 6.dp, vertical = 4.dp),
            horizontalArrangement = Arrangement.spacedBy(2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            for (emoji in reactions) {
                Box(
                    modifier = Modifier
                        .size(REACTION_TAP_SIZE)
                        .clickable(onClickLabel = "React $emoji") { onReact(emoji) },
                    contentAlignment = Alignment.Center,
                ) {
                    Text(text = emoji, style = MaterialTheme.typography.titleMedium)
                }
            }
            Box(
                modifier = Modifier
                    .size(REACTION_TAP_SIZE)
                    .semantics { contentDescription = MORE_REACTIONS_LABEL }
                    .clickable { onOpenPicker() },
                contentAlignment = Alignment.Center,
            ) {
                Text(text = "+", style = MaterialTheme.typography.titleMedium)
            }
        }
    }
}

/** Each emoji's own tap target. 40dp keeps six of them plus the picker within a
 *  narrow phone while clearing the 40dp minimum in `docs/ux/navigation.md`. */
private val REACTION_TAP_SIZE: Dp = 40.dp

/**
 * Section 2 — the action card, sized by its own content and nothing else.
 *
 * Section 2's order, of what exists today: the actions that produce a new
 * message first (Reply, then Forward), then the reading action (Copy), then
 * the one that changes this message (Edit), then the destructive one,
 * separated and in the platform's destructive colour. Pin, Select and Read by
 * are section 3 rows that nothing has built yet; they slot into that order
 * when they land.
 *
 * The look is section 2's shared card, whose reference is web's
 * `.lk-msg-menu-card` / `.lk-msg-menu-items` (`apps/web/src/styles.css`), not
 * Material's dropdown chrome: radius 12, 4 of padding inside a 1dp border in the
 * border colour, rows 7 by 8 with a 6 radius and the icon 8 from the label, and
 * the destructive group set off by a rule.
 */
@Composable
internal fun MessageActionCard(
    actions: MessageActions,
    onReply: () -> Unit,
    onCopy: () -> Unit,
    onEdit: () -> Unit,
    onDelete: () -> Unit,
    modifier: Modifier = Modifier,
    onForward: () -> Unit = {},
    onPin: () -> Unit = {},
) {
    Surface(
        modifier = modifier.testTag(ACTION_CARD_TAG),
        // `medium` is Lakko's "12 for cards and rows" (`LakkoShapes`).
        shape = MaterialTheme.shapes.medium,
        color = MaterialTheme.colorScheme.surface,
        contentColor = MaterialTheme.colorScheme.onSurface,
        border = menuSurfaceBorder(),
        shadowElevation = MENU_SURFACE_ELEVATION,
    ) {
        Column(
            modifier = Modifier
                // Width of the widest row, and every row stretched to it — the
                // shape web gets from `width: max-content` on the card and the
                // default `stretch` of its flex column. Without this each row
                // would be as wide as its own label and the pressed highlight
                // would stop short of the card's edge.
                .width(IntrinsicSize.Max)
                // CSS padding sits INSIDE the border, so web's rows start 1 + 4
                // in from the card's edge. A Compose `Surface` draws its border
                // over its content rather than insetting it, so the border's
                // width is added here to land the rows at the same place.
                .padding(CARD_BORDER_WIDTH + CARD_INNER_PADDING),
            // Web's `gap: 2px` between rows.
            verticalArrangement = Arrangement.spacedBy(ROW_GAP),
        ) {
            if (actions.canReply) {
                ActionRow(
                    label = "Reply",
                    icon = Icons.AutoMirrored.Filled.Reply,
                    onClick = onReply,
                )
            }
            if (actions.canForward) {
                // Section 5.1 — second: it also produces a new message, in
                // another conversation.
                ActionRow(
                    label = "Forward",
                    icon = Icons.AutoMirrored.Filled.Forward,
                    onClick = onForward,
                )
            }
            if (actions.canPin) {
                // Section 4.1 — after Forward: the keep-message action. The
                // word is decided once by `pinMenuFor` and rendered here.
                ActionRow(
                    label = actions.pinLabel,
                    icon = Icons.Filled.PushPin,
                    onClick = onPin,
                )
            }
            if (actions.canCopy) {
                ActionRow(
                    label = "Copy",
                    icon = Icons.Filled.ContentCopy,
                    onClick = onCopy,
                )
            }
            if (actions.canEdit) {
                ActionRow(
                    label = "Edit",
                    icon = Icons.Filled.Edit,
                    onClick = onEdit,
                )
            }
            if (actions.canDelete) {
                // Section 2 — the destructive action is separated and in the
                // platform's destructive colour. The rule belongs to the
                // destructive GROUP rather than to the Delete row, as on web,
                // so a second destructive action lands under the same rule.
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        // Web's `margin-top: 2px` on the group, on top of the
                        // row gap above it.
                        .padding(top = DESTRUCTIVE_GAP),
                ) {
                    if (actions.canReply || actions.canForward || actions.canPin || actions.canCopy || actions.canEdit) {
                        HorizontalDivider(
                            thickness = CARD_BORDER_WIDTH,
                            color = MaterialTheme.colorScheme.outlineVariant,
                        )
                        // Web's `padding-top: 2px` between the rule and the row.
                        Spacer(Modifier.height(DESTRUCTIVE_GAP))
                    }
                    ActionRow(
                        label = "Delete",
                        icon = Icons.Filled.Delete,
                        onClick = onDelete,
                        tint = MaterialTheme.colorScheme.error,
                    )
                }
            }
        }
    }
}

/** Test tag on the card's surface: the one handle a UI test has on the card's
 *  own bounds, which carry no semantics otherwise. */
internal const val ACTION_CARD_TAG: String = "message-action-card"

/**
 * Section 2's shared numbers, in dp. The reference is web's card; each is named
 * after the rule it mirrors so a divergence is a diff against a sentence.
 */
internal val CARD_BORDER_WIDTH: Dp = 1.dp
internal val CARD_INNER_PADDING: Dp = 4.dp
internal val ROW_GAP: Dp = 2.dp
internal val DESTRUCTIVE_GAP: Dp = 2.dp
private val ROW_PADDING_VERTICAL: Dp = 7.dp
private val ROW_PADDING_HORIZONTAL: Dp = 8.dp
private val ROW_RADIUS: Dp = 6.dp
private val ROW_ICON_GAP: Dp = 8.dp

/** Web's icons are 16 in a 14px label; the same pair here (`bodyMedium` is
 *  14sp). Section 2 does not fix either, so this follows the reference. */
private val ROW_ICON_SIZE: Dp = 16.dp

/**
 * Web's `--shadow-2` is `0 8px 24px` at 12 percent. Compose has no CSS
 * box-shadow, and a `Surface` shadow is a Material elevation, so this is the
 * nearest Material 3 level rather than a translation: level 4, 8dp — the same 8
 * as the CSS y-offset, and a blur that reads soft rather than the tighter edge of
 * the 6dp the card used before.
 */
private val MENU_SURFACE_ELEVATION: Dp = 8.dp

/** The 1dp border in Lakko's border colour (`LakkoTokens.border`, which the
 *  scheme carries as `outlineVariant`). */
@Composable
private fun menuSurfaceBorder(): BorderStroke =
    BorderStroke(CARD_BORDER_WIDTH, MaterialTheme.colorScheme.outlineVariant)

@Composable
private fun ActionRow(
    label: String,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    onClick: () -> Unit,
    tint: Color = Color.Unspecified,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            // The pressed highlight follows the row's own 6 radius, as web's
            // hover background does, rather than squaring off inside the card.
            .clip(RoundedCornerShape(ROW_RADIUS))
            .clickable(onClick = onClick)
            .padding(horizontal = ROW_PADDING_HORIZONTAL, vertical = ROW_PADDING_VERTICAL),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = if (tint == Color.Unspecified) MaterialTheme.colorScheme.onSurfaceVariant else tint,
            modifier = Modifier.size(ROW_ICON_SIZE),
        )
        Box(Modifier.width(ROW_ICON_GAP))
        Text(
            text = label,
            style = MaterialTheme.typography.bodyMedium,
            color = if (tint == Color.Unspecified) MaterialTheme.colorScheme.onSurface else tint,
        )
    }
}

/**
 * Places the reaction row above [bubble] and the card below it, both clamped
 * into the window.
 *
 * A `Layout` rather than two `offset` modifiers because both offsets depend on
 * the measured size of the thing being offset — the row's own width decides
 * where its right edge can sit, and the card's own height decides whether it
 * still fits under the bubble.
 */
@Composable
private fun MenuLayout(
    bubble: Rect,
    alignEnd: Boolean,
    row: @Composable () -> Unit,
    card: @Composable () -> Unit,
) {
    val density = LocalDensity.current
    val gap = with(density) { BUBBLE_GAP.roundToPx() }
    val margin = with(density) { WINDOW_MARGIN.roundToPx() }
    Layout(
        content = {
            Box(Modifier.layoutId(SLOT_ROW)) { row() }
            Box(Modifier.layoutId(SLOT_CARD)) { card() }
        },
    ) { measurables, constraints ->
        val loose = constraints.copy(minWidth = 0, minHeight = 0)
        val rowPlaceable = measurables.first { it.layoutId == SLOT_ROW }.measure(loose)
        val cardPlaceable = measurables.first { it.layoutId == SLOT_CARD }.measure(loose)
        val width = constraints.maxWidth
        val height = constraints.maxHeight

        fun x(itemWidth: Int): Int {
            val wanted = if (alignEnd) bubble.right.toInt() - itemWidth else bubble.left.toInt()
            return wanted.coerceIn(margin, (width - itemWidth - margin).coerceAtLeast(margin))
        }

        fun clampY(wanted: Int, itemHeight: Int): Int =
            wanted.coerceIn(margin, (height - itemHeight - margin).coerceAtLeast(margin))

        layout(width, height) {
            // `place`, not `placeRelative`: these coordinates are already window
            // coordinates read off the bubble, so mirroring them under RTL would
            // move both surfaces away from the bubble they belong to.
            rowPlaceable.place(
                x = x(rowPlaceable.width),
                y = clampY(bubble.top.toInt() - gap - rowPlaceable.height, rowPlaceable.height),
            )
            cardPlaceable.place(
                x = x(cardPlaceable.width),
                y = clampY(bubble.bottom.toInt() + gap, cardPlaceable.height),
            )
        }
    }
}

private const val SLOT_ROW = "reaction-row"
private const val SLOT_CARD = "action-card"

/**
 * Puts the popup's content at the window's own origin, so the content's
 * coordinate space IS the window's and a bubble rectangle read with
 * `boundsInWindow()` can be used verbatim.
 */
private object WindowOriginProvider : PopupPositionProvider {
    override fun calculatePosition(
        anchorBounds: IntRect,
        windowSize: IntSize,
        layoutDirection: LayoutDirection,
        popupContentSize: IntSize,
    ): IntOffset = IntOffset.Zero
}
