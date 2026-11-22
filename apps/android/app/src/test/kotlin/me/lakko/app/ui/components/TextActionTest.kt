// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import androidx.activity.ComponentActivity
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.click
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Touch targets of text actions on Android: at least 48dp, the whole area
 * clickable, one node with the button role for TalkBack, and no change to
 * the layout of what is drawn.
 *
 * Native graphics at a Pixel's density (420dpi): with it, the `Create invite
 * link` text measures [74, 349] x 53px, the bounds uiautomator reported on a
 * real Pixel, so the geometry below is the device's.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-420dpi")
class TextActionTest {
    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private val density get() = compose.activity.resources.displayMetrics.density

    private fun tap(x: Float, y: Float) {
        compose.onRoot().performTouchInput { click(Offset(x, y)) }
        compose.waitForIdle()
    }

    /**
     * Pins what Group info's `Create invite link` (a material3 TextButton)
     * gives: one button node, and a 48dp target centred on the 20dp label,
     * so taps 13dp above and below the glyphs and at the button's side edges
     * land. A tap 18dp above the glyphs (the y=1116 probe on the Pixel, 47px
     * above a text top of 1163) is outside it.
     */
    @Test
    fun `a TextButton is one button node with a 48dp target around its label`() {
        var hits = 0
        compose.setContent {
            Column(Modifier.verticalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 8.dp)) {
                Text("above")
                Spacer(Modifier.height(8.dp))
                TextButton(onClick = { hits++ }) { Text("Create invite link") }
                Text("below")
            }
        }
        val node = compose.onNodeWithText("Create invite link").fetchSemanticsNode()
        assertEquals(Role.Button, node.config.getOrNull(SemanticsProperties.Role))
        assertTrue(node.config.contains(SemanticsActions.OnClick))
        val button = node.boundsInRoot
        val label = compose.onNodeWithText("Create invite link", useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
        assertTrue("label ${label.height / density}dp", label.height < 24 * density)

        tap(label.center.x, label.top - 13 * density)
        tap(label.center.x, label.bottom + 13 * density)
        tap(button.left + 2 * density, label.center.y)
        tap(button.right - 2 * density, label.center.y)
        assertEquals(4, hits)

        tap(label.center.x, label.top - 18 * density)
        assertEquals(4, hits)
    }

    @Test
    fun `a small text action keeps its size and takes taps in a 48dp area around it`() {
        var hits = 0
        compose.setContent {
            Column(Modifier.padding(48.dp)) {
                TextAction(onClick = { hits++ }, modifier = Modifier.testTag("action")) {
                    Text("Retry", modifier = Modifier.size(40.dp, 16.dp))
                }
            }
        }
        val shown: Rect = compose.onNodeWithTag("action").fetchSemanticsNode().boundsInRoot
        assertEquals(40f * density, shown.width, 0.5f)
        assertEquals(16f * density, shown.height, 0.5f)

        // 16dp above and below the 16dp glyph box: the edges of a 48dp target.
        tap(shown.center.x, shown.top - 15 * density)
        tap(shown.center.x, shown.bottom + 15 * density)
        tap(shown.left - 3 * density, shown.center.y)
        tap(shown.center.x, shown.center.y)
        assertEquals(4, hits)
        // Beyond the 48dp target: nothing.
        tap(shown.center.x, shown.top - 18 * density)
        tap(shown.center.x, shown.bottom + 18 * density)
        assertEquals(4, hits)
    }

    /** The bubble's `Retry`: the bubble itself takes long-presses and taps. */
    @OptIn(ExperimentalFoundationApi::class)
    @Test
    fun `inside a clickable bubble the area around the word goes to the word`() {
        var retries = 0
        var bubble = 0
        compose.setContent {
            Box(
                Modifier
                    .padding(24.dp)
                    .combinedClickable(onClick = { bubble++ }, onLongClick = {})
                    .padding(24.dp),
            ) {
                TextAction(onClick = { retries++ }, modifier = Modifier.testTag("action")) {
                    Text("Retry", modifier = Modifier.size(40.dp, 16.dp))
                }
            }
        }
        val shown = compose.onNodeWithTag("action", useUnmergedTree = true).fetchSemanticsNode().boundsInRoot
        tap(shown.center.x, shown.top - 10 * density)
        tap(shown.center.x, shown.center.y)
        assertEquals(2, retries)
        assertEquals(0, bubble)
        // Beyond the word's 48dp target, still on the bubble: the bubble's.
        tap(shown.center.x, shown.bottom + 20 * density)
        assertEquals(2, retries)
        assertEquals(1, bubble)
    }

    @Test
    fun `TalkBack reads one button with the text`() {
        compose.setContent {
            TextAction(onClick = {}) { Text("Retry") }
        }
        val node = compose.onNodeWithText("Retry").fetchSemanticsNode()
        assertEquals(Role.Button, node.config.getOrNull(SemanticsProperties.Role))
        assertTrue(node.config.contains(SemanticsActions.OnClick))
    }

    @Test
    fun `a disabled text action takes no tap anywhere`() {
        var hits = 0
        compose.setContent {
            Column(Modifier.padding(48.dp)) {
                TextAction(onClick = { hits++ }, enabled = false, modifier = Modifier.testTag("action")) {
                    Text("Retry", modifier = Modifier.size(40.dp, 16.dp))
                }
            }
        }
        val shown = compose.onNodeWithTag("action").fetchSemanticsNode().boundsInRoot
        tap(shown.center.x, shown.center.y)
        tap(shown.center.x, shown.top - 10 * density)
        assertEquals(0, hits)
        val node = compose.onNodeWithText("Retry").fetchSemanticsNode()
        assertTrue(node.config.contains(SemanticsProperties.Disabled))
    }
}
