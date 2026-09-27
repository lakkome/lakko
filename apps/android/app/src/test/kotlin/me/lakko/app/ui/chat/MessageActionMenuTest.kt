// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import me.lakko.app.chat.Ack
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.InlineKind
import me.lakko.app.media.MediaAllowlist
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * `docs/ux/message-actions.md` sections 2 and 3 — the shape of the per-message
 * action menu, and which actions it offers to whom.
 *
 * What was here before this suite was a `DropdownMenu` of three rows, one of
 * which was `React` and led to another row of things. So the four properties
 * asserted here are the four sentences that menu broke:
 *
 *  1. the reactions are a row, one tap each, not a row that opens a submenu;
 *  2. the action card is sized by its own content and does not inherit the
 *     reaction row's width;
 *  3. Copy puts the text on the clipboard and nothing leaves the device;
 *  4. Edit and Delete are own-message-only.
 *
 * Existence rather than display throughout: the menu lives in a `Popup`, which
 * Robolectric never places on a real display — see `VoiceBubbleUiTest`. Sizes
 * ARE readable, which is what makes property 2 measurable.
 */
@RunWith(RobolectricTestRunner::class)
class MessageActionMenuTest {

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private lateinit var scope: CoroutineScope
    private lateinit var hub: ChatMediaHub

    /** The production allowlist's shape without the native core — see
     *  `MediaBubbleActionMenuTest`. No message here carries an attachment, so
     *  nothing consults it; the hub is a constructor argument, not a subject. */
    private val allowlist: MediaAllowlist = { mime ->
        if (mime.startsWith("image/")) InlineKind.IMAGE else null
    }

    private val reacted = mutableListOf<String>()
    private var edits = 0
    private var deletes = 0
    private var replies = 0
    private var forwards = 0
    private var pickerOpens = 0

    @Before
    fun setUp() {
        scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        hub = ChatMediaHub(compose.activity, scope, allowlist)
        // A sentinel rather than an empty clipboard, so the Copy test proves
        // the clipboard CHANGED rather than that it happened to hold something.
        clipboard()?.setPrimaryClip(ClipData.newPlainText("sentinel", SENTINEL))
    }

    @After
    fun tearDown() {
        hub.dispose()
        scope.cancel()
    }

    // ----- 1. the reaction row is one tap, not a row that opens a row -------

    @Test
    fun anEmojiIsOneTapAwayFromTheOpenMenu() {
        compose.setContent { BubbleUnderTest(incoming("Hello there")) }

        openMenu()

        // §2: "A row of emoji sits above the bubble and each one is one tap
        // away." Between the gesture and the reaction there is nothing to
        // navigate — no disclosure row, no submenu, no second surface.
        compose.onNodeWithText(FIRST_EMOJI).performClick()

        assertEquals(listOf(FIRST_EMOJI), reacted)
    }

    @Test
    fun thereIsNoReactRowThatOpensASubmenu() {
        compose.setContent { BubbleUnderTest(incoming("Hello there")) }

        openMenu()

        // The defect §2 exists to forbid, named: "Putting them behind a row that
        // opens a submenu is the defect this rule exists to forbid." This is the
        // row the menu shipped with, asserted absent rather than merely
        // unreferenced, so restoring it fails here rather than passing quietly.
        compose.onNodeWithText("React").assertDoesNotExist()
        // And the trailing control §2 asks for, which is how any OTHER emoji is
        // reached — the one thing that is allowed to lead somewhere.
        compose.onNodeWithContentDescription(MORE_REACTIONS_LABEL).performClick()
        assertEquals(1, pickerOpens)
    }

    // ----- 3.1 Reply is offered on any message with an id ------------------
    @Test
    fun replyIsOfferedOnSomeoneElsesMessageAndFiresOnce() {
        compose.setContent { BubbleUnderTest(incoming("Hello there")) }
        openMenu()
        // ADR-0052 / §3.1: Reply on any message with an id, own or not; it is
        // the first row of the card (§2: the action that produces a new
        // message comes first).
        compose.onNodeWithText("Reply").performClick()
        assertEquals(1, replies)
    }

    // ----- 5.1 Forward is second, on any sent message with an id -----------
    @Test
    fun forwardIsTheSecondRowAndFiresOnce() {
        // ADR-0052 decision 2 / §5.1 — Forward is offered on any message with
        // an id that is not deleted and whose send is complete, own or not.
        // §2's order: the actions that produce a new message first (Reply,
        // then Forward), then the ones that only read (Copy, then Select —
        // §8), then the ones that change this message (Pin, Edit), then
        // Delete.
        compose.setContent { BubbleUnderTest(own("Hello there")) }
        openMenu()

        val top = { label: String -> compose.onNodeWithText(label).fetchSemanticsNode().boundsInWindow.top }
        val reply = top("Reply")
        val forward = top("Forward")
        val copy = top("Copy")
        val select = top("Select")
        val pin = top("Pin")
        val edit = top("Edit")
        val delete = top("Delete")
        assertTrue("Forward ($forward) must sit below Reply ($reply)", reply < forward)
        assertTrue("Copy ($copy) must sit below Forward ($forward)", forward < copy)
        assertTrue("Select ($select) must sit below Copy ($copy)", copy < select)
        assertTrue("Pin ($pin) must sit below Select ($select)", select < pin)
        assertTrue("Edit ($edit) must sit below Pin ($pin)", pin < edit)
        assertTrue("Delete ($delete) must sit below Edit ($edit)", edit < delete)

        compose.onNodeWithText("Forward").performClick()
        assertEquals(1, forwards)
        assertEquals(0, replies)
    }

    @Test
    fun forwardIsOfferedOnSomeoneElsesMessageToo() {
        compose.setContent { BubbleUnderTest(incoming("Hello there")) }
        openMenu()
        compose.onNodeWithText("Forward").performClick()
        assertEquals(1, forwards)
    }

    // ----- 2. the card is sized by its own content -------------------------

    @Test
    fun theActionCardDoesNotInheritTheReactionRowsWidth() {
        // §2: "It is a separate surface from the reaction row and must not
        // inherit its width." The measurable form of that sentence is the one
        // the web defect took: action rows stretched to the width of the emoji
        // row above them, because both lived in one card.
        compose.setContent { BubbleUnderTest(own("Hello there")) }

        openMenu()

        val reactionRow = spanOfReactionRow()
        val copyRow = compose.onNodeWithText("Copy").fetchSemanticsNode().size.width
        val deleteRow = compose.onNodeWithText("Delete").fetchSemanticsNode().size.width

        assertTrue("the reaction row measured nothing at all", reactionRow > 0)
        assertTrue(
            "the Copy row is $copyRow wide and the reaction row $reactionRow — " +
                "the card is being stretched to the row above it",
            copyRow < reactionRow,
        )
        assertTrue(
            "the Delete row is $deleteRow wide and the reaction row $reactionRow",
            deleteRow < reactionRow,
        )
    }

    @Test
    fun theCardCarriesTheSharedCardNumbers() {
        // §2: "The card's look is shared, and web's is the reference" — and the
        // numbers are written down "so that 'the same card' is checkable rather
        // than felt". Radius, shadow and colour are not readable here; where the
        // rows sit inside the card, and how far apart, are.
        compose.setContent { BubbleUnderTest(own("Hello there")) }

        openMenu()

        val card = compose.onNodeWithTag(ACTION_CARD_TAG).fetchSemanticsNode().boundsInWindow
        val copy = compose.onNodeWithText("Copy").fetchSemanticsNode().boundsInWindow
        // Copy and Select are neighbours (§2's order); Edit is the row the
        // destructive rule sits under.
        val select = compose.onNodeWithText("Select").fetchSemanticsNode().boundsInWindow
        val edit = compose.onNodeWithText("Edit").fetchSemanticsNode().boundsInWindow
        val delete = compose.onNodeWithText("Delete").fetchSemanticsNode().boundsInWindow
        val px = { dp: androidx.compose.ui.unit.Dp -> with(compose.density) { dp.toPx() } }
        val tolerance = 1f

        // Every row stretched to the card, as web's flex column does, so the
        // pressed highlight reaches the card's edge on every row.
        assertEquals(copy.width, edit.width, tolerance)
        assertEquals(copy.width, delete.width, tolerance)
        // Rows inset by the 4 of padding inside the 1dp border, on each side.
        val inset = px(CARD_BORDER_WIDTH + CARD_INNER_PADDING)
        assertEquals("row inset from the card's leading edge", inset, copy.left - card.left, tolerance)
        assertEquals("row inset from the card's trailing edge", inset, card.right - copy.right, tolerance)
        // Rows 2 apart; the destructive group a further 2, then the 1dp rule,
        // then 2 more before Delete — web's gap, margin, border and padding.
        assertEquals("gap between two ordinary rows", px(ROW_GAP), select.top - copy.bottom, tolerance)
        assertEquals(
            "gap across the destructive rule",
            px(ROW_GAP + DESTRUCTIVE_GAP + CARD_BORDER_WIDTH + DESTRUCTIVE_GAP),
            delete.top - edit.bottom,
            tolerance,
        )
    }

    // ----- 3. Copy -------------------------------------------------------—-

    @Test
    fun copyPutsTheMessageTextOnTheClipboard() {
        compose.setContent { BubbleUnderTest(incoming(TEXT)) }

        openMenu()
        compose.onNodeWithText("Copy").performClick()

        // §3: "Puts the text on the clipboard. Nothing leaves the device."
        assertEquals(TEXT, clipboardText())
    }

    @Test
    fun copyIsNotOfferedOnAMessageWithNoText() {
        // §3 offers Copy on "any message with text". A voice message or a photo
        // with no caption has none, and a row that copied an empty string would
        // silently wipe whatever the person had on their clipboard.
        compose.setContent { BubbleUnderTest(incoming("")) }

        openMenu()

        compose.onNodeWithText("Copy").assertDoesNotExist()
        // The reaction row is still the point of the menu on such a message.
        compose.onNodeWithText(FIRST_EMOJI).assertExists()
    }

    // ----- 4. Edit / Delete are own-message-only ---------------------------

    @Test
    fun deleteIsNotOfferedOnSomeoneElsesMessage() {
        compose.setContent { BubbleUnderTest(incoming(TEXT)) }

        openMenu()

        // §3 — Edit and Delete are offered on "own, not deleted".
        compose.onNodeWithText("Delete").assertDoesNotExist()
        compose.onNodeWithText("Edit").assertDoesNotExist()
    }

    @Test
    fun deleteAndEditAreOfferedOnOurOwn() {
        // The control for the test above: the same finders, on a message that
        // does offer both, so an absence there means own-message-only rather
        // than a card that never draws a row at all.
        compose.setContent { BubbleUnderTest(own(TEXT)) }

        openMenu()

        compose.onNodeWithText("Edit").performClick()
        assertEquals(1, edits)

        openMenu()
        compose.onNodeWithText("Delete").performClick()
        assertEquals(1, deletes)
    }

    // ----- the receding backdrop ------------------------------------------—

    @Test
    fun theMessageStaysInPlaceBehindTheScrim() {
        compose.setContent { BubbleUnderTest(incoming(TEXT)) }

        openMenu()

        // §2: "the message being acted on stays fully legible in place." In
        // place, literally: the bubble is neither moved nor copied into the
        // overlay — the scrim simply is not painted over it — so the message's
        // text exists exactly once in the tree while the menu is up.
        compose.onNodeWithContentDescription(CLOSE_MENU_LABEL).assertExists()
        assertEquals(1, compose.onAllNodesWithText(TEXT).fetchSemanticsNodes().size)
    }

    @Test
    fun tappingTheScrimClosesTheMenuAndActsOnNothing() {
        compose.setContent { BubbleUnderTest(own(TEXT)) }

        openMenu()

        // Far down the dimmed conversation rather than at the node's geometric
        // centre: the scrim is window-sized, so its centre is under the action
        // card, and a tap there is a tap on the CARD (whose `Surface` swallows
        // it, which is why the card has no holes that dismiss). The gesture
        // being asserted is the one a person makes — a tap on the dimmed part.
        compose.onNodeWithContentDescription(CLOSE_MENU_LABEL).performTouchInput {
            down(percentOffset(0.5f, 0.9f))
            up()
        }

        compose.onNodeWithContentDescription(CLOSE_MENU_LABEL).assertDoesNotExist()
        assertEquals(emptyList<String>(), reacted)
        assertEquals("the way out of the menu deleted the message", 0, deletes)
    }

    // ----- helpers ---------------------------------------------------------

    private fun openMenu() {
        compose.onNodeWithTag(BUBBLE).performTouchInput { longClick() }
        compose.onNodeWithContentDescription(CLOSE_MENU_LABEL).assertExists()
    }

    /** The reaction row's width, read as the span from the first emoji's left
     *  edge to the trailing picker control's right edge. The row's `Surface`
     *  carries no semantics of its own, so its two ends are what can be
     *  measured — and they are what the card would be stretched to. */
    private fun spanOfReactionRow(): Int {
        val first = compose.onNodeWithText(FIRST_EMOJI).fetchSemanticsNode().boundsInWindow
        val plus = compose
            .onNodeWithContentDescription(MORE_REACTIONS_LABEL)
            .fetchSemanticsNode()
            .boundsInWindow
        return (plus.right - first.left).toInt()
    }

    private fun clipboard(): ClipboardManager? =
        compose.activity.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager

    private fun clipboardText(): String? =
        clipboard()?.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.text?.toString()

    @Composable
    private fun BubbleUnderTest(msg: ChatMessage) {
        MaterialTheme {
            Surface {
                Box(Modifier.testTag(BUBBLE)) {
                    Bubble(
                        msg = msg,
                        time = TIME,
                        maxBubbleWidth = 320.dp,
                        mediaHub = hub,
                        onOpenReactionPicker = { pickerOpens += 1 },
                        ownIdentity = null,
                        isOwn = msg.direction == Direction.Outgoing,
                        actionsAvailable = true,
                        readState = OwnReadState.None,
                        background = MaterialTheme.colorScheme.surfaceVariant,
                        contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                        onPreview = {},
                        onSave = {},
                        onEdit = { edits += 1 },
                        onDelete = { deletes += 1 },
                        onReact = { reacted += it },
                        onRetrySend = {},
                        onReply = { replies += 1 },
                        onForward = { forwards += 1 },
                    )
                }
            }
        }
    }

    private companion object {
        const val BUBBLE = "text-bubble"
        const val TIME = "09:00"
        const val TEXT = "the quick brown fox"

        /** What the clipboard holds before each test. */
        const val SENTINEL = "clipboard untouched"

        /** `REACTION_EMOJIS.first()` — the thumbs up. */
        const val FIRST_EMOJI = "👍"

        private fun incoming(text: String) = ChatMessage(
            messageId = 7L,
            direction = Direction.Incoming,
            text = text,
            createdAt = "2026-09-25T09:00:00Z",
            authorKey = "peer-key",
            ack = Ack.SENT,
        )

        private fun own(text: String) = ChatMessage(
            messageId = 8L,
            direction = Direction.Outgoing,
            text = text,
            createdAt = "2026-09-25T09:00:00Z",
            authorKey = "own-key",
            ack = Ack.SENT,
        )
    }
}
