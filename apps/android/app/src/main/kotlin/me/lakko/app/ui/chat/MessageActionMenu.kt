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
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.ui.draw.alpha
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
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
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

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
internal const val CLOSE_MENU_LABEL: String = "chat.message.close_actions"

/** Label on the reaction row's trailing control. */
internal const val MORE_REACTIONS_LABEL: String = "chat.message.more_reactions"

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
    val pinLabel: String = Localization.strings.value["chat.message.pin"],
    /** Section 8 — Select enters the multi-select mode with this message
     *  already selected. Offered where the row could be selected at all:
     *  a completed send that is not deleted. */
    val canSelect: Boolean = false,
    /** docs/ux/saved-messages-and-file-lifetime.md section 2 — Save sits
     *  directly under Pin on any message that is not deleted. */
    val canSave: Boolean = false,
    /** `Remove from saved` instead of `Save`. */
    val saved: Boolean = false,
    /** ADR-0082 / translation UX 3.1 — `Translate` after Copy, decided by the
     *  shared `translationOffered` rule; reads `Show original` on a message
     *  already showing its translation. */
    val canTranslate: Boolean = false,
    val translateLabel: String = Localization.strings.value["chat.message.translate"],
    /** ADR-0087 section 13 — a publisher's `Ban from comments` on someone
     *  else's comment, in the destructive group under Delete. */
    val canBan: Boolean = false,
) {
    /** Whether the card has anything to draw. */
    val any: Boolean get() = canCopy || canEdit || canDelete || canReply || canForward || canPin || canSelect || canSave || canTranslate || canBan
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
    pinLabel: String = Localization.strings.value["chat.message.pin"],
    /** Saved section 2 — the message is in the account's saved list. */
    saved: Boolean = false,
    /** ADR-0087 section 14 — the bubble is drawn as a channel invitation
     *  card. Its text is the `/g/` link with its secret, so it is never
     *  edited (Edit would put that link in the composer) and never forwarded
     *  (maintainer, 2026-11-13). Delete stays. */
    invitationCard: Boolean = false,
): MessageActions = MessageActions(
    // Copy reads a string this device already holds and sends nothing, so it is
    // the one action that does NOT die with the membership gate. It is still
    // gated on the menu being available because the menu is how it is reached.
    canCopy = menuAvailable && text.isNotEmpty(),
    canEdit = menuAvailable && isOwn && !deleted && !invitationCard,
    canDelete = menuAvailable && isOwn && !deleted,
    // Section 3.1 — "any message with an id". `menuAvailable` already carries
    // the id gate (a row with no DS id has nothing to point at), the deleted
    // gate and the membership gate: a reply composes an application message,
    // which the core refuses once membership has ended, like Send.
    canReply = menuAvailable,
    // Section 5.1 — "A message that cannot be forwarded yet (its attachments
    // still uploading, or a failed send) does not offer Forward." The id gate
    // and the deleted gate ride in `menuAvailable`, as for Reply.
    // Maintainer, 2026-11-13 — a message carrying a channel invitation card
    // is never forwarded.
    canForward = menuAvailable && sendComplete && !invitationCard,
    // Section 4.1 — "on any message whose send is complete and that is not
    // deleted": the same gates as Forward. A personal pin sends nothing, so
    // the membership gate inside `menuAvailable` is stricter than a pin
    // strictly needs; it is kept because the menu is how the row is reached.
    canPin = menuAvailable && sendComplete,
    pinLabel = pinLabel,
    // Section 8 — the same gates a selectable row has (`canSelectMessage`):
    // the id and deleted gates ride in `menuAvailable`, the send gate here.
    canSelect = menuAvailable && sendComplete,
    // Saved section 2 — "any message that is not deleted, in any chat the
    // person can read". It sends nothing to the chat, but the menu is how the
    // row is reached, so the same id and deleted gates apply; a row still
    // sending has no id to save under.
    canSave = !deleted && sendComplete,
    saved = saved,
)

/**
 * Put [text] on the system clipboard. Section 3: "Nothing leaves the device."
 *
 * The clip label is what the OS shows in its own paste UI; the message text is
 * the clip itself.
 */
/**
 * ADR-0073 R.5 — the card of a row of an OLDER segment of the conversation:
 * the `Unpin` row alone when the row is pinned, and nothing otherwise
 * ([me.lakko.app.chat.historyRowOffersUnpin]). A history row takes no edit,
 * delete, reaction, reply, forward, selection or new pin.
 */
internal fun historyRowActions(rowIsPinned: Boolean, pinLabel: String): MessageActions = MessageActions(
    canCopy = false,
    canEdit = false,
    canDelete = false,
    canPin = me.lakko.app.chat.historyRowOffersUnpin(isHistoryRow = true, rowIsPinned = rowIsPinned),
    pinLabel = pinLabel,
)

internal fun copyMessageText(context: Context, text: String) {
    val clipboard =
        context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
            ?: return
    clipboard.setPrimaryClip(ClipData.newPlainText(Localization.strings.value["common.clipboard.message"], text))
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
    /** Section 8 — Select was chosen: enter selection mode on this message. */
    onSelect: () -> Unit = {},
    /** Saved section 2 — Save / Remove from saved was chosen. */
    onSave: () -> Unit = {},
    /** ADR-0082 — Translate / Show original was chosen. */
    onTranslate: () -> Unit = {},
    /** ADR-0087 section 13 — Ban from comments was chosen. */
    onBan: () -> Unit = {},
    /** reactions-and-emoji section 5 — the emoji this person already gave
     *  this message (drawn pressed; pressing one takes it off). */
    ownReactions: Set<String> = emptySet(),
    /** Section 5 — this person holds 3 here: the others are dimmed. */
    capReached: Boolean = false,
    /** Section 5 — the trailing expand control (not with six or fewer
     *  allowed). */
    showExpand: Boolean = true,
    /** Section 9 — `No reactions`: no row at all. */
    showRow: Boolean = true,
) {
    // The chat window's own height, read OUTSIDE the popup. Inside it,
    // `constraints.maxHeight` is the popup window's height, which the system
    // hands out without subtracting the insets the popup content is placed
    // under - so a card clamped against it could still run off the bottom of
    // the screen (seen live 2026-09-27: on a message near the composer the
    // card showed Reply..Select and Pin, Edit and Delete were below the
    // display edge). The bubble is measured in this window's space, so its
    // height is the visible bottom in that same space.
    // The display's height less the navigation bar, read from the chat
    // window: `LocalView.current.height` read here still let the card run one
    // row past the display edge on the emulator, so the display metrics are
    // the reference and the navigation bar is taken off explicitly.
    val hostHeight = LocalContext.current.resources.displayMetrics.heightPixels -
        WindowInsets.navigationBars.getBottom(LocalDensity.current)
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
            val originNow = origin ?: return@Box
            val bubble = bubbleBounds.translate(-originNow)
            val closeLabel = t(CLOSE_MENU_LABEL)
            Box(
                Modifier
                    .fillMaxSize()
                    .semantics { contentDescription = closeLabel }
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
                // The chat window's bottom edge, in the popup content's space
                // (the same translation the bubble got).
                visibleBottom = (hostHeight - originNow.y).toInt(),
                row = {
                    if (showRow) {
                        MessageReactionRow(
                            reactions = reactions,
                            onReact = onReact,
                            onOpenPicker = onOpenReactionPicker,
                            own = ownReactions,
                            capReached = capReached,
                            showExpand = showExpand,
                        )
                    }
                },
                card = {
                    // The card is a menu: system text size, not the chat's.
                    if (actions.any) me.lakko.app.ui.theme.SystemTextScale {
                        MessageActionCard(
                            actions = actions,
                            onReply = onReply,
                            onForward = onForward,
                            onPin = onPin,
                            onCopy = onCopy,
                            onSelect = onSelect,
                            onSave = onSave,
                            onEdit = onEdit,
                            onDelete = onDelete,
                            onTranslate = onTranslate,
                            onBan = onBan,
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
    own: Set<String> = emptySet(),
    capReached: Boolean = false,
    showExpand: Boolean = true,
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
        val strings = LocalStrings.current
        val reactLabel = { emoji: String -> strings.format("chat.message.react_with", mapOf("emoji" to emoji)) }
        val moreReactions = strings[MORE_REACTIONS_LABEL]
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Row(
            // A narrow window (or a longer emoji set than the six defaults)
            // scrolls rather than clipping the picker control off the end: the
            // trailing control is the only route to any other emoji.
            modifier = Modifier
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = 6.dp, vertical = 4.dp),
            horizontalArrangement = Arrangement.spacedBy(REACTION_BAR_SPACING),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            for (emoji in reactions) {
                val mine = emoji in own
                // Section 5 — with 3 held, the emoji not given are dimmed and
                // do nothing; the person's own 3 stay pressable.
                val enabled = mine || !capReached
                Box(
                    modifier = Modifier
                        .size(reactionTapSize())
                        .then(
                            if (mine) {
                                Modifier.background(MaterialTheme.colorScheme.primaryContainer, CircleShape)
                            } else {
                                Modifier
                            },
                        )
                        .clickable(enabled = enabled, onClickLabel = reactLabel(emoji)) { onReact(emoji) },
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        text = emoji,
                        style = MaterialTheme.typography.titleMedium.copy(
                            fontSize = REACTION_BAR_EMOJI_SP,
                            lineHeight = REACTION_BAR_EMOJI_LINE_SP,
                        ),
                        modifier = Modifier.alpha(if (enabled) 1f else 0.35f),
                    )
                }
            }
            if (showExpand) {
                Box(
                    modifier = Modifier
                        .size(reactionTapSize())
                        .semantics { contentDescription = moreReactions }
                        .clickable { onOpenPicker() },
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        text = "+",
                        style = MaterialTheme.typography.titleMedium.copy(
                            fontSize = REACTION_BAR_EMOJI_SP,
                            lineHeight = REACTION_BAR_EMOJI_LINE_SP,
                        ),
                    )
                }
            }
        }
        if (capReached) {
            Text(
                text = strings["chat.reaction.cap_reached"],
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(start = 12.dp, end = 12.dp, bottom = 6.dp),
            )
        }
        }
    }
}

/** design-system.md "Reaction chip", quick reaction bar: each emoji 28 in a
 *  44 square tap target, 4 apart. Six emoji plus the picker take 344dp with
 *  the padding, inside a 360dp phone less the window margins; a narrower
 *  window scrolls the row rather than clipping the picker. */
private val REACTION_TAP_SIZE: Dp = 44.dp

/** docs/ux/text-size.md — the quick bar's emoji follow the text size (the
 *  `sp` scale), the tap target grows with them and never goes below 44. */
@Composable
private fun reactionTapSize(): Dp =
    REACTION_TAP_SIZE * maxOf(1f, me.lakko.app.ui.theme.LocalChatTextFactor.current)
private val REACTION_BAR_SPACING: Dp = 4.dp
private val REACTION_BAR_EMOJI_SP = 28.sp
private val REACTION_BAR_EMOJI_LINE_SP = 34.sp

/**
 * Section 2 — the action card, sized by its own content and nothing else.
 *
 * Section 2's order, of what exists today: the actions that produce a new
 * message first (Reply, then Forward), then the ones that only read (Copy,
 * then Select), then the ones that change this message (Pin, Edit), then the
 * destructive one, separated and in the platform's destructive colour. Read
 * by is the one section 3 row nothing has built yet; it slots into that order
 * when it lands.
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
    onSelect: () -> Unit = {},
    onSave: () -> Unit = {},
    onTranslate: () -> Unit = {},
    onBan: () -> Unit = {},
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
                    label = t("chat.message.reply"),
                    icon = R.drawable.ic_lk_reply,
                    onClick = onReply,
                )
            }
            if (actions.canForward) {
                // Section 5.1 — second: it also produces a new message, in
                // another conversation.
                ActionRow(
                    label = t("chat.message.forward"),
                    icon = R.drawable.ic_lk_forward,
                    onClick = onForward,
                )
            }
            if (actions.canCopy) {
                ActionRow(
                    label = t("common.copy"),
                    icon = R.drawable.ic_lk_copy,
                    onClick = onCopy,
                )
            }
            if (actions.canTranslate) {
                // Translation UX 3.1 — with the actions that only read,
                // after Copy. Nothing leaves the device.
                ActionRow(
                    label = actions.translateLabel,
                    // Show original reads as its own glyph, as on iOS and web.
                    icon = if (actions.translateLabel == t("chat.message.show_original")) {
                        R.drawable.ic_lk_text_bubble
                    } else {
                        R.drawable.ic_lk_translate
                    },
                    onClick = onTranslate,
                )
            }
            if (actions.canSelect) {
                // Section 8 — after Copy, with the actions that only read:
                // entering the mode changes nothing until an action fires.
                ActionRow(
                    label = me.lakko.app.chat.SELECT_ACTION_LABEL.text(),
                    icon = R.drawable.ic_lk_checkmark_circle,
                    onClick = onSelect,
                )
            }
            if (actions.canPin) {
                // Section 4.1 — the keep-message action, with the ones that
                // change this message. The word is decided once by
                // `pinMenuFor` and rendered here.
                ActionRow(
                    label = actions.pinLabel,
                    // Unpin draws the slashed pin, as on iOS and web.
                    icon = if (actions.pinLabel == t("chat.message.unpin")) {
                        R.drawable.ic_lk_pin_slash
                    } else {
                        R.drawable.ic_lk_pin
                    },
                    onClick = onPin,
                )
            }
            if (actions.canSave) {
                // Saved section 2 — directly under Pin.
                ActionRow(
                    label = t(if (actions.saved) "chat.message.unsave" else "chat.message.save"),
                    icon = if (actions.saved) R.drawable.ic_lk_bookmark_slash else R.drawable.ic_lk_bookmark,
                    onClick = onSave,
                )
            }
            if (actions.canEdit) {
                ActionRow(
                    label = t("chat.message.edit"),
                    icon = R.drawable.ic_lk_pencil,
                    onClick = onEdit,
                )
            }
            if (actions.canDelete || actions.canBan) {
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
                    if (actions.canReply || actions.canForward || actions.canCopy || actions.canTranslate || actions.canSelect || actions.canPin || actions.canSave || actions.canEdit) {
                        HorizontalDivider(
                            thickness = CARD_BORDER_WIDTH,
                            color = MaterialTheme.colorScheme.outlineVariant,
                        )
                        // Web's `padding-top: 2px` between the rule and the row.
                        Spacer(Modifier.height(DESTRUCTIVE_GAP))
                    }
                    if (actions.canDelete) {
                        ActionRow(
                            label = t("common.delete"),
                            icon = R.drawable.ic_lk_trash,
                            onClick = onDelete,
                            tint = MaterialTheme.colorScheme.error,
                        )
                    }
                    if (actions.canBan) {
                        ActionRow(
                            label = t("channels.moderation.ban"),
                            icon = R.drawable.ic_lk_nosign,
                            onClick = onBan,
                            tint = MaterialTheme.colorScheme.error,
                        )
                    }
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

/** A menu row's icon box: the design system's band for settings and menu
 *  rows (section "Icons"). */
private val ROW_ICON_SIZE: Dp = LkIconSize.Row

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
    @androidx.annotation.DrawableRes icon: Int,
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
        LkIcon(
            icon = icon,
            contentDescription = null,
            tint = if (tint == Color.Unspecified) MaterialTheme.colorScheme.onSurfaceVariant else tint,
            size = ROW_ICON_SIZE,
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
    /** The lowest y (in this layout's coordinates) that is on screen; the
     *  popup's own `maxHeight` overstates it, see the caller. `0` or less
     *  means unknown, and the layout falls back to `maxHeight`. */
    visibleBottom: Int = 0,
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
        val height = if (visibleBottom > 0) minOf(constraints.maxHeight, visibleBottom) else constraints.maxHeight

        fun x(itemWidth: Int): Int {
            val wanted = if (alignEnd) bubble.right.toInt() - itemWidth else bubble.left.toInt()
            return wanted.coerceIn(margin, (width - itemWidth - margin).coerceAtLeast(margin))
        }

        val placement = messageMenuPlacement(
            bubbleTop = bubble.top.toInt(),
            bubbleBottom = bubble.bottom.toInt(),
            rowHeight = rowPlaceable.height,
            cardHeight = cardPlaceable.height,
            height = height,
            gap = gap,
            margin = margin,
        )

        layout(width, height) {
            // `place`, not `placeRelative`: these coordinates are already window
            // coordinates read off the bubble, so mirroring them under RTL would
            // move both surfaces away from the bubble they belong to.
            rowPlaceable.place(x = x(rowPlaceable.width), y = placement.rowY)
            cardPlaceable.place(x = x(cardPlaceable.width), y = placement.cardY)
        }
    }
}

/** Where the reaction row and the action card go, top edges. */
internal data class MenuPlacement(val rowY: Int, val cardY: Int)

/**
 * Section 2 — the reaction row above the bubble and the card below it, and
 * the two NEVER on top of each other. The bubble is not moved here (the scrim
 * leaves it legible in place), so where one surface has no room the pair is
 * placed together instead, in this order of preference:
 *
 *  1. row above the bubble, card below (the ordinary case);
 *  2. both above, the row next to the bubble and the card above the row (a
 *     message near the bottom: seen on a Pixel 2026-11-04, the card was
 *     pushed up over the first two reactions);
 *  3. both below, the row next to the bubble and the card under the row (a
 *     message near the top);
 *  4. otherwise the row and the card as one block, the row on top, moved in
 *     one piece inside the screen; it may then cover the bubble, never itself.
 *
 * A height of 0 is a surface that is not drawn, and its gap goes with it.
 * Pure arithmetic on pixels, so it is tested without a screen.
 */
internal fun messageMenuPlacement(
    bubbleTop: Int,
    bubbleBottom: Int,
    rowHeight: Int,
    cardHeight: Int,
    height: Int,
    gap: Int,
    margin: Int,
): MenuPlacement {
    val hasRow = rowHeight > 0
    val hasCard = cardHeight > 0
    val low = margin
    val high = height - margin
    fun fits(top: Int, bottom: Int): Boolean = top >= low && bottom <= high

    // 1. Row above, card below.
    val rowAbove = bubbleTop - gap - rowHeight
    val cardBelow = bubbleBottom + gap
    if ((!hasRow || fits(rowAbove, rowAbove + rowHeight)) && (!hasCard || fits(cardBelow, cardBelow + cardHeight))) {
        return MenuPlacement(rowAbove, cardBelow)
    }
    // 2. Both above: the card over the row.
    val cardAbove = if (hasRow) rowAbove - gap - cardHeight else bubbleTop - gap - cardHeight
    val topOfAbove = if (hasCard) cardAbove else rowAbove
    if (fits(topOfAbove, bubbleTop - gap)) return MenuPlacement(rowAbove, cardAbove)
    // 3. Both below: the card under the row.
    val rowBelow = bubbleBottom + gap
    val cardUnderRow = if (hasRow) rowBelow + rowHeight + gap else bubbleBottom + gap
    val bottomOfBelow = if (hasCard) cardUnderRow + cardHeight else rowBelow + rowHeight
    if (fits(bubbleBottom + gap, bottomOfBelow)) return MenuPlacement(rowBelow, cardUnderRow)
    // 4. One block, row on top, moved in one piece.
    val blockHeight = (if (hasRow) rowHeight else 0) + (if (hasRow && hasCard) gap else 0) +
        (if (hasCard) cardHeight else 0)
    val wanted = if (hasRow) rowAbove else bubbleBottom + gap
    val ceiling = high - blockHeight
    val top = if (ceiling < low) low else wanted.coerceIn(low, ceiling)
    val rowY = top
    val cardY = if (hasRow) top + rowHeight + gap else top
    return MenuPlacement(rowY, cardY)
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
