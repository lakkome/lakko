// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.click
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.sp
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.contacts.LakkoLink
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import uniffi.lakko_ffi_uniffi.MessageTextPiece

/**
 * `docs/ux/message-links.md` sections 1 and 2, on the gestures. The pieces are
 * handed in: the host JVM has no native library, and which runs are links is
 * tested with the rule in Rust.
 *
 * Wide on purpose, so the link stays on one line and the text is laid out in
 * lines of a known height: 20 px at this density, with the second line from
 * 20 to 40.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w900dp-h600dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class MessageLinkTextTest {

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private val slug = "abcdEF_-"
    private val secret = "0123456789abcdEF"
    private val url = "${ApiEndpoints.webOrigin}/i/$slug#$secret"
    private val pieces = listOf(
        MessageTextPiece(text = url, isLink = true),
        MessageTextPiece(text = "\nthe words under the link", isLink = false),
    )

    private val opened = mutableListOf<LakkoLink>()
    private var menus = 0

    private val onTheLink = Offset(10f, 10f)
    private val onTheLineBelow = Offset(10f, 25f)

    private fun show(onLongPress: (() -> Unit)? = { menus += 1 }) {
        compose.setContent {
            MaterialTheme {
                Surface {
                    CompositionLocalProvider(LocalOpenLakkoLink provides { link -> opened += link }) {
                        Box(Modifier.fillMaxWidth().testTag(TAG)) {
                            MessageLinkText(
                                text = pieces.joinToString("") { it.text },
                                style = STYLE,
                                linkColor = Color.Blue,
                                onLongPress = onLongPress,
                                split = { pieces },
                            )
                        }
                    }
                }
            }
        }
    }

    @Test
    fun aTapOnAnOwnLinkGoesToTheLinkIntake() {
        show()
        compose.onNodeWithTag(TAG).performTouchInput { click(onTheLink) }
        compose.runOnIdle {
            assertEquals(listOf<LakkoLink>(LakkoLink.Contact(slug, secret)), opened)
            assertEquals(0, menus)
        }
    }

    @Test
    fun aLongPressOnALinkIsTheMenuAndNotTheLink() {
        show()
        compose.onNodeWithTag(TAG).performTouchInput { longClick(onTheLink) }
        compose.runOnIdle {
            assertEquals(1, menus)
            assertTrue("the link opened: $opened", opened.isEmpty())
        }
    }

    @Test
    fun aLongPressOnALinkWithNoMenuOpensNothing() {
        show(onLongPress = null)
        compose.onNodeWithTag(TAG).performTouchInput { longClick(onTheLink) }
        compose.runOnIdle { assertTrue("the link opened: $opened", opened.isEmpty()) }
    }

    /** The word under a link was within the 48dp target Compose grows a
     *  clickable to, and opened the link. */
    @Test
    fun aTapOnTheWordUnderALinkDoesNotOpenIt() {
        show()
        compose.onNodeWithTag(TAG).performTouchInput { click(onTheLineBelow) }
        compose.runOnIdle { assertTrue("the link opened: $opened", opened.isEmpty()) }
    }

    @Test
    fun aRuleThatCannotBeAskedLeavesTheTextAsText() {
        val noLibrary = messageTextPieces("see https://example.com") { throw NoClassDefFoundError("jna") }
        assertEquals(listOf(MessageTextPiece("see https://example.com", false)), noLibrary)
        val failed = messageTextPieces("text") { throw IllegalStateException("no") }
        assertEquals(listOf(MessageTextPiece("text", false)), failed)
    }

    private companion object {
        const val TAG = "message-link-text"
        val STYLE = TextStyle(fontSize = 14.sp, lineHeight = 20.sp)
    }
}
