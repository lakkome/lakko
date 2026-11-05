// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.getUnclippedBoundsInRoot
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import me.lakko.app.chat.Ack
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.chat.Reaction
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.ui.theme.TextSize
import me.lakko.app.ui.theme.TextSizePreference
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/** docs/ux/text-size.md — the stored step, and the factor reaching the
 *  bubble's text and the reaction chips, applied at once. */
@RunWith(RobolectricTestRunner::class)
class TextSizeTest {
    init {
        EnglishForTests.install()
    }

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private lateinit var scope: CoroutineScope
    private lateinit var hub: ChatMediaHub

    @Before
    fun setUp() {
        TextSizePreference.resetForTests()
        scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        hub = ChatMediaHub(compose.activity, scope) { null }
    }

    @After
    fun tearDown() {
        hub.dispose()
        scope.cancel()
        TextSizePreference.resetForTests()
    }

    @Test
    fun `the default is Medium and an unknown stored value reads as Medium`() {
        assertEquals(TextSize.Medium, TextSize.fromStored(null))
        assertEquals(TextSize.Medium, TextSize.fromStored("Huge"))
        assertEquals(TextSize.Large, TextSize.fromStored("Large"))
        assertEquals(listOf(0.87f, 1.0f, 1.2f), TextSize.entries.map { it.factor })

        val context = RuntimeEnvironment.getApplication()
        val prefs = context.getSharedPreferences(TextSizePreference.PREFS, android.content.Context.MODE_PRIVATE)
        prefs.edit().clear().commit()
        TextSizePreference.init(context)
        assertEquals(TextSize.Medium, TextSizePreference.size.value)
        TextSizePreference.set(TextSize.Small)
        assertEquals("Small", prefs.getString(TextSizePreference.KEY, null))
        TextSizePreference.resetForTests()
        TextSizePreference.init(context)
        assertEquals(TextSize.Small, TextSizePreference.size.value)
        prefs.edit().putString(TextSizePreference.KEY, "Gigantic").commit()
        TextSizePreference.resetForTests()
        TextSizePreference.init(context)
        assertEquals(TextSize.Medium, TextSizePreference.size.value)
    }

    @Test
    fun `the factor reaches the bubble text and the chips, at once`() {
        val reactor = java.util.Base64.getEncoder().encodeToString(ByteArray(32) { 5 })
        val msg = ChatMessage(
            messageId = 7L,
            direction = Direction.Incoming,
            text = "Is this size easy to read?",
            createdAt = "2026-11-04T09:00:00Z",
            authorKey = "peer-key",
            ack = Ack.SENT,
            reactions = listOf(Reaction(reactor = reactor, emoji = "👍", at = 1L)),
        )
        compose.setContent {
            MaterialTheme {
                Surface {
                    Box(Modifier.testTag("bubble")) {
                        Bubble(
                            msg = msg,
                            time = "09:00",
                            maxBubbleWidth = 320.dp,
                            mediaHub = hub,
                            onOpenReactionPicker = {},
                            ownIdentity = null,
                            isOwn = false,
                            actionsAvailable = true,
                            readState = OwnReadState.None,
                            background = MaterialTheme.colorScheme.surfaceVariant,
                            contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                            onPreview = {},
                            onSave = {},
                            onEdit = {},
                            onDelete = {},
                            onReact = {},
                            onRetrySend = {},
                        )
                    }
                }
            }
        }
        val chip = hasContentDescription("👍", substring = true)
        // The font scale the bubble text was laid out under (Robolectric does
        // not measure glyph extents faithfully, so bounds are not a measure;
        // the sp-to-px conversion of that scale is the platform's own).
        fun textHeight(): Float {
            val node = compose.onNodeWithText(msg.text, useUnmergedTree = true).fetchSemanticsNode()
            val results = mutableListOf<androidx.compose.ui.text.TextLayoutResult>()
            node.config[androidx.compose.ui.semantics.SemanticsActions.GetTextLayoutResult].action?.invoke(results)
            val input = results.single().layoutInput
            return input.density.fontScale
        }
        fun chipHeight() = compose.onNode(chip, useUnmergedTree = true).getUnclippedBoundsInRoot().let { (it.bottom - it.top).value }

        compose.waitForIdle()
        val mediumText = textHeight()
        assertEquals(REACTION_CHIP_HEIGHT.value, chipHeight(), 0.5f)

        TextSizePreference.set(TextSize.Large)
        compose.waitForIdle()
        assertEquals(REACTION_CHIP_HEIGHT.value * 1.2f, chipHeight(), 0.5f)
        val largeText = textHeight()
        assertEquals(1.2f, largeText / mediumText, 0.01f)

        TextSizePreference.set(TextSize.Small)
        compose.waitForIdle()
        assertEquals(REACTION_CHIP_HEIGHT.value * 0.87f, chipHeight(), 0.5f)
        assertEquals(0.87f, textHeight() / mediumText, 0.01f)
    }
}
