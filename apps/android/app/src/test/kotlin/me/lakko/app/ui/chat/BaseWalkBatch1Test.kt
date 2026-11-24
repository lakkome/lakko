// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.ComponentActivity
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsFocused
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performKeyInput
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.pressKey
import androidx.compose.ui.test.withKeyDown
import androidx.compose.ui.text.TextRange
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import me.lakko.app.feed.FeedSource
import me.lakko.app.feed.FeedSourceKind
import me.lakko.app.feed.feedSourceAvatarId
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.InlineKind
import me.lakko.app.media.KIND_GIF
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import uniffi.lakko_ffi_uniffi.AttachmentRef

/** Base walk 2026-11-04, batch 1: no GIF label on a GIF (2026-11-23), the edit
 *  field focused with the caret at the end, the feed card's group photo. */
@RunWith(RobolectricTestRunner::class)
class BaseWalkBatch1Test {
    init {
        EnglishForTests.install()
    }

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private lateinit var scope: CoroutineScope
    private lateinit var hub: ChatMediaHub

    private val gif = AttachmentRef(
        blobId = ByteArray(32) { 3 },
        key = ByteArray(32) { 22 },
        nonce = ByteArray(12) { 33 },
        mime = "image/gif",
        size = 4_096u,
        name = "loop.gif",
        kind = KIND_GIF,
        width = 240u,
        height = 160u,
    )

    @Before
    fun setUp() {
        scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        hub = ChatMediaHub(compose.activity, scope) { mime -> if (mime.startsWith("image/")) InlineKind.IMAGE else null }
        hub.fileLocal(ChatMediaHub.mediaKey(gif.blobId), "loop.gif", ByteArray(64))
    }

    @After
    fun tearDown() {
        hub.dispose()
        scope.cancel()
    }

    /** 2026-11-23 — a GIF shows no `GIF` label, on the picture or under it. */
    @Test
    fun `a GIF bubble draws the picture alone, with no GIF label`() {
        compose.setContent {
            MaterialTheme { Surface { GifBubble(gif, hub, InlineKind.IMAGE) } }
        }
        compose.onNodeWithTag(GIF_IMAGE_TAG, useUnmergedTree = true).assertExists()
        compose.onAllNodesWithText("GIF", useUnmergedTree = true).assertCountEquals(0)
    }

    @Test
    fun `choosing Edit focuses the field with the caret at the end`() {
        compose.setContent {
            MaterialTheme { EditMessageDialog(initialText = "hello there", onCancel = {}, onSave = {}) }
        }
        compose.waitForIdle()
        val field = compose.onNodeWithTag(EDIT_FIELD_TAG)
        field.assertIsFocused()
        val selection = field.fetchSemanticsNode().config.getOrNull(SemanticsProperties.TextSelectionRange)
        assertEquals(TextRange("hello there".length), selection)
    }

    @Test
    fun `a feed card looks the group photo up by the group id the Chats row uses`() {
        val group = FeedSource(FeedSourceKind.GROUP, "AB12", "Hiking", muted = false, audience = 3, conversationHex = "cd34")
        assertEquals("ab12", feedSourceAvatarId(group))
        val channel = FeedSource(FeedSourceKind.CHANNEL, "ef56", "News", muted = false, audience = 0)
        assertEquals("ef56", feedSourceAvatarId(channel))
    }

    @Test
    fun `Enter decides by the keyboard and Shift`() {
        assertEquals(me.lakko.app.ui.components.EnterKey.SUBMIT, me.lakko.app.ui.components.enterKeyAction(true, true, false, true))
        assertEquals(me.lakko.app.ui.components.EnterKey.NEWLINE, me.lakko.app.ui.components.enterKeyAction(true, true, true, true))
        // The on-screen keyboard's Enter is a new line, as on every phone.
        assertEquals(me.lakko.app.ui.components.EnterKey.PASS, me.lakko.app.ui.components.enterKeyAction(true, true, false, false))
        assertEquals(me.lakko.app.ui.components.EnterKey.PASS, me.lakko.app.ui.components.enterKeyAction(false, true, false, true))
    }

    @OptIn(androidx.compose.ui.test.ExperimentalTestApi::class)
    @Test
    fun `in the edit dialog a hardware Enter saves and Shift+Enter does not`() {
        val saved = mutableListOf<String>()
        compose.setContent {
            MaterialTheme { EditMessageDialog(initialText = "hello", onCancel = {}, onSave = { saved += it }) }
        }
        compose.waitForIdle()
        val field = compose.onNodeWithTag(EDIT_FIELD_TAG)
        field.assertIsFocused()
        field.performTextInput(" there")
        // Shift+Enter is left to the field (its line break); nothing is saved.
        field.performKeyInput { withKeyDown(androidx.compose.ui.input.key.Key.ShiftLeft) { pressKey(androidx.compose.ui.input.key.Key.Enter) } }
        compose.waitForIdle()
        assertTrue(saved.isEmpty())
        field.performKeyInput { pressKey(androidx.compose.ui.input.key.Key.Enter) }
        compose.waitForIdle()
        assertEquals(1, saved.size)
        assertTrue(saved.single().startsWith("hello there"))
    }
}
