// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.feed

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.getUnclippedBoundsInRoot
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import me.lakko.app.feed.FeedAttachment
import me.lakko.app.feed.FeedCandidateRef
import me.lakko.app.feed.FeedMessage
import me.lakko.app.feed.FeedSource
import me.lakko.app.feed.FeedSourceKind
import me.lakko.app.feed.RankedItem
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.InlineKind
import me.lakko.app.media.MediaAllowlist
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.GraphicsMode
import uniffi.lakko_ffi_uniffi.AttachmentRef

/**
 * `docs/ux/feed.md` section 3 — the shape of one item: the source avatar's
 * column, the text limit, the order of text, media and why line, and that a
 * bubble keeps its own gestures.
 */
@RunWith(RobolectricTestRunner::class)
class FeedItemCardTest {
    init {
        EnglishForTests.install()
    }

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private lateinit var scope: CoroutineScope
    private lateinit var hub: ChatMediaHub
    private var opened = 0
    private var menuOpened = false

    private val allowlist: MediaAllowlist = { mime ->
        when {
            mime.startsWith("image/") -> InlineKind.IMAGE
            else -> null
        }
    }

    @Before
    fun setUp() {
        scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        hub = ChatMediaHub(compose.activity, scope, allowlist)
        // Filed locally so the bubble finds a file and no test touches the
        // network. The bytes decode as nothing: the bubble stays, relabelled.
        hub.fileLocal(ChatMediaHub.mediaKey(PHOTO.blobId), "photo.jpg", ByteArray(64))
    }

    @After
    fun tearDown() {
        hub.dispose()
        scope.cancel()
    }

    private fun show(message: FeedMessage, why: String? = null) {
        val source = FeedSource(FeedSourceKind.GROUP, "aa".repeat(32), SOURCE, muted = false, audience = 4)
        val ref = FeedCandidateRef(source, message, mentionsMe = false, repliesToMe = false)
        val noAction: (FeedCandidateRef) -> Unit = {}
        compose.setContent {
            var menu by remember { mutableStateOf(false) }
            MaterialTheme {
                Surface {
                    Box(Modifier.width(360.dp).testTag(CARD)) {
                        FeedItemCard(
                            item = RankedItem(ref, emptyList()),
                            why = why,
                            author = AUTHOR,
                            time = "14:32",
                            expanded = false,
                            lastReplyLine = null,
                            onOpen = { opened += 1 },
                            onToggleReplies = {},
                            hub = hub,
                            onSaveMedia = {},
                            menu = ItemMenuActions(noAction, noAction, noAction, noAction, noAction),
                            menuOpen = menu,
                            onMenuOpenChange = {
                                menu = it
                                if (it) menuOpened = true
                            },
                        )
                    }
                }
            }
        }
    }

    private fun message(text: String, vararg refs: AttachmentRef) = FeedMessage(
        messageId = 1,
        direction = "incoming",
        text = text,
        attachments = refs.map { FeedAttachment(it.kind, it.mime, it) },
        createdAt = "2026-10-31T10:00:00Z",
        authorKeyB64 = null,
    )

    private fun photoBubble() = compose.onNode(
        SemanticsMatcher("the photo's control") { node ->
            node.config.getOrNull(SemanticsActions.OnClick)?.label in
                setOf(en("chat.photo.open"), en("common.retry"))
        },
    )

    @Test
    fun theFirstRowAndTheTextOpenTheItem() {
        show(message("hello"))
        compose.onNodeWithText(SOURCE).performClick()
        assertEquals(1, opened)
        compose.onNodeWithText("hello").performClick()
        assertEquals(2, opened)
    }

    @Test
    fun aTapOnTheMediaDoesNotOpenTheItem() {
        show(message("look", PHOTO))
        photoBubble().performClick()
        compose.waitForIdle()
        assertEquals(0, opened)
    }

    @Test
    fun aLongPressOnTheMediaOpensTheItemsMenu() {
        show(message("look", PHOTO))
        photoBubble().performTouchInput { longClick() }
        compose.waitForIdle()
        assertTrue(menuOpened)
        assertEquals(0, opened)
    }

    // Native graphics: the default mode lays every text out on one line.
    @Test
    @GraphicsMode(GraphicsMode.Mode.NATIVE)
    fun theTextStopsAtEightLinesWithAnEllipsis() {
        val long = (1..40).joinToString("\n") { "line $it" }
        show(message(long))
        val layouts = mutableListOf<TextLayoutResult>()
        // Unmerged: the merged node answers with its first text, the source.
        val node = compose.onNodeWithText(long, useUnmergedTree = true).fetchSemanticsNode()
        node.config[SemanticsActions.GetTextLayoutResult].action!!.invoke(layouts)
        assertEquals(8, layouts.single().lineCount)
        assertTrue(layouts.single().isLineEllipsized(7))
    }

    @Test
    fun aMediaOnlyMessageHasNoTextLine() {
        show(message("", PHOTO))
        compose.onNodeWithText(en("chat.media.photo")).assertDoesNotExist()
    }

    @Test
    fun aFileStaysATextLineAndDrawsNoBubble() {
        show(message("", FILE))
        compose.onNodeWithText(en("chat.media.file")).assertExists()
        assertFalse(
            compose.onAllNodes(
                SemanticsMatcher("a media control") { it.config.getOrNull(SemanticsActions.OnClick)?.label == en("chat.photo.open") },
            ).fetchSemanticsNodes().isNotEmpty(),
        )
    }

    @Test
    fun theColumnsAndTheOrderAreTheSpecs() {
        show(message("look", PHOTO), why = WHY)
        val density = compose.density
        val source = compose.onNodeWithText(SOURCE).getUnclippedBoundsInRoot()
        val text = compose.onNodeWithText("look").getUnclippedBoundsInRoot()
        val media = photoBubble().getUnclippedBoundsInRoot()
        val why = compose.onNodeWithText(WHY).getUnclippedBoundsInRoot()
        val card = compose.onNodeWithTag(CARD).getUnclippedBoundsInRoot()

        // 16 of padding, the 40 avatar, 12 of gap: everything is beside it.
        assertEquals(68f, with(density) { source.left.toPx() / density.density }, 0.5f)
        assertEquals(source.left, text.left)
        assertEquals(source.left, media.left)
        assertEquals(source.left, why.left)
        // Text, then media, then the why line.
        assertTrue(text.bottom <= media.top)
        assertTrue(media.bottom <= why.top)
        // Nothing runs past the card's own edge.
        assertTrue(why.bottom <= card.bottom)
    }

    private companion object {
        const val CARD = "feed-card"
        const val SOURCE = "Team"
        const val AUTHOR = "Anna"
        const val WHY = "Mentions you"

        private fun ref(seed: Byte, mime: String, name: String) = AttachmentRef(
            blobId = ByteArray(32) { seed },
            key = ByteArray(32) { 22 },
            nonce = ByteArray(12) { 33 },
            mime = mime,
            size = 4_096u,
            name = name,
            width = 240u,
            height = 240u,
        )

        val PHOTO = ref(1, "image/jpeg", "photo.jpg")
        val FILE = ref(2, "application/pdf", "notes.pdf")
    }
}
