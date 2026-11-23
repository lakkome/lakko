// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.ComponentActivity
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.getBoundsInRoot
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.unit.DpRect
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.InlineKind
import me.lakko.app.media.KIND_VIDEO_NOTE
import me.lakko.app.media.MediaAllowlist
import me.lakko.app.media.VideoNoteLayout
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import uniffi.lakko_ffi_uniffi.AttachmentRef

/**
 * `docs/ux/media-messages.md` section 3 (2026-11-22) — seen on a real Pixel:
 * the duration sat under the circle at the left and the time on a line of its
 * own further down at the right, reading as loose text floating under a
 * video. Now the duration and the time ride pills INSIDE the circle and `Aa`
 * sits beside it on the side away from the screen edge, so nothing is drawn
 * under the circle. Measured on the production composable.
 */
@RunWith(RobolectricTestRunner::class)
class VideoNoteBubbleLayoutTest {
    init {
        EnglishForTests.install()
    }

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private lateinit var scope: CoroutineScope
    private lateinit var hub: ChatMediaHub

    private val allowlist: MediaAllowlist = { mime ->
        if (mime.startsWith("video/")) InlineKind.VIDEO else null
    }

    @Before
    fun setUp() {
        me.lakko.app.contacts.AcceptedContactsStore.bind(ByteArray(16) { 0x0B })
        scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        hub = ChatMediaHub(compose.activity, scope, allowlist)
    }

    @After
    fun tearDown() {
        hub.dispose()
        scope.cancel()
        me.lakko.app.contacts.AcceptedContactsStore.unbind()
    }

    private fun render(isOwn: Boolean) {
        compose.setContent {
            MaterialTheme {
                Surface {
                    androidx.compose.foundation.layout.Box(Modifier.testTag(ROOT)) {
                        VideoNoteBubble(
                            ref = NOTE,
                            hub = hub,
                            isOwn = isOwn,
                            meta = { Text("22:56") },
                        )
                    }
                }
            }
        }
        compose.waitForIdle()
    }

    private fun bounds(tag: String): DpRect = compose.onNodeWithTag(tag, useUnmergedTree = true).getBoundsInRoot()

    private fun assertInsideCircle(pill: DpRect, circle: DpRect) {
        val r = (circle.right - circle.left).value / 2f
        val cx = circle.left.value + r
        val cy = circle.top.value + r
        for ((x, y) in listOf(pill.left to pill.bottom, pill.right to pill.bottom)) {
            val dx = x.value - cx
            val dy = y.value - cy
            // A pill's corner is rounded by half its height, so the square
            // corner may stand a little outside; the rounded one may not.
            val slack = (pill.bottom - pill.top).value / 2f
            assertTrue(
                "pill corner ($x, $y) outside the circle",
                kotlin.math.sqrt(dx * dx + dy * dy) <= r + slack,
            )
        }
        assertTrue(pill.top >= circle.top && pill.bottom <= circle.bottom)
        assertTrue(pill.left >= circle.left && pill.right <= circle.right)
    }

    @Test
    fun anIncomingNoteCarriesItsLinesOnAndBesideTheCircle() {
        render(isOwn = false)
        val circle = bounds(VIDEO_NOTE_CIRCLE_TAG)
        val duration = bounds(VIDEO_NOTE_DURATION_TAG)
        val meta = bounds(VIDEO_NOTE_META_TAG)
        val aside = bounds(VIDEO_NOTE_ASIDE_TAG)
        val root = bounds(ROOT)
        assertInsideCircle(duration, circle)
        assertInsideCircle(meta, circle)
        // Duration at the leading side, the time at the trailing side.
        assertTrue(duration.right <= meta.left)
        // Aa after the circle, vertically centred on it.
        assertTrue(aside.left >= circle.right)
        val asideMid = (aside.top + aside.bottom).value / 2f
        val circleMid = (circle.top + circle.bottom).value / 2f
        assertEquals(circleMid, asideMid, 0.5f)
        // Nothing under the circle.
        assertEquals((circle.bottom - circle.top).value, (root.bottom - root.top).value, 0.5f)
    }

    @Test
    fun anOwnNoteHasAaBeforeTheCircle() {
        render(isOwn = true)
        val circle = bounds(VIDEO_NOTE_CIRCLE_TAG)
        val aside = bounds(VIDEO_NOTE_ASIDE_TAG)
        assertTrue(aside.right <= circle.left)
        assertEquals(VIDEO_NOTE_DIAMETER.value, (circle.right - circle.left).value, 0.5f)
        val root = bounds(ROOT)
        assertEquals((circle.bottom - circle.top).value, (root.bottom - root.top).value, 0.5f)
        assertTrue(bounds(VIDEO_NOTE_META_TAG).right <= circle.right - 0.dp)
    }

    @Test
    fun thePillPlacementStaysInsideTheCircleAtAnyDiameter() {
        for (d in listOf(96f, 160f, 200f, 240f, 320f)) {
            assertTrue(me.lakko.app.media.VideoNoteLayout.pillCornerInside(d))
        }
    }

    @Test
    fun theDurationIsTheTotalUntilTheNotePlaysWithSound() {
        val l = me.lakko.app.media.VideoNoteLayout
        assertEquals(VideoNoteLayout.DurationLabel.Total(3_400), l.durationLabel(false, 0.5f, 3_400))
        assertEquals(VideoNoteLayout.DurationLabel.Elapsed(1_700), l.durationLabel(true, 0.5f, 3_400))
        assertEquals(VideoNoteLayout.DurationLabel.Elapsed(3_400), l.durationLabel(true, 2f, 3_400))
        assertEquals(null, l.durationLabel(false, 0f, null))
        assertTrue(l.asideFirst(isOwn = true))
        assertTrue(!l.asideFirst(isOwn = false))
    }

    private companion object {
        const val ROOT = "video-note-root"
        val NOTE = AttachmentRef(
            blobId = ByteArray(32) { 0x21 },
            key = ByteArray(32) { 0x22 },
            nonce = ByteArray(12) { 0x23 },
            mime = "video/mp4",
            size = 16_384u,
            name = "note.mp4",
            kind = KIND_VIDEO_NOTE,
            durationMs = 3_400u,
        )
    }
}
