// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.longClick
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
import me.lakko.app.media.KIND_VOICE
import me.lakko.app.media.MediaAllowlist
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.shadows.ShadowMediaPlayer
import org.robolectric.shadows.util.DataSource
import uniffi.lakko_ffi_uniffi.AttachmentRef
import kotlin.math.abs

/**
 * ADR-0047 / `docs/ux/media-messages.md` §2 — the voice bubble, measured.
 *
 * These are the first UI tests on Android, and they exist because of what a
 * person found in one day with two phones and a browser. Two of those were
 * Android's own: the bubble grew when playback started, and the message's time
 * sat on a line below the recording's duration where iOS had them level. The
 * third subject here — which gesture opens the action menu — was reported on the
 * other two clients rather than this one (`89475ac3`: web's menu opened on hover
 * and covered the play button, iOS's waveform seeked on the touch-up a long
 * press ends with), and is pinned here because it is one cross-platform rule and
 * Android is the client with nothing holding it.
 *
 * All three are layout or gesture, which is the layer no test on this client
 * covered.
 *
 * Everything here drives production composables against a real [ChatMediaHub].
 * Nothing is faked but two things that cannot exist on a JVM: the MIME
 * allowlist (the real one loads the native core on first call) and the audio
 * device (Robolectric's `ShadowMediaPlayer`, fed a registered data source so
 * `prepare` and `start` succeed). The bubble therefore reaches the playing state
 * the way a person's tap reaches it — through `hub.toggleVoice` — rather than by
 * a flag a test set.
 */
@RunWith(RobolectricTestRunner::class)
class VoiceBubbleUiTest {

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private lateinit var scope: CoroutineScope
    private lateinit var hub: ChatMediaHub

    /** Audio and image by MIME class. The production allowlist is the core's
     *  `inlineMediaType`, which loads the native library; this is the same
     *  shape without it. */
    private val allowlist: MediaAllowlist = { mime ->
        when {
            mime.startsWith("audio/") -> InlineKind.AUDIO
            mime.startsWith("image/") -> InlineKind.IMAGE
            mime.startsWith("video/") -> InlineKind.VIDEO
            else -> null
        }
    }

    @Before
    fun setUp() {
        scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        hub = ChatMediaHub(compose.activity, scope, allowlist)
        // The bubble fetches its blob on compose, and `toggleVoice` opens
        // whatever `load` returns. Filing the bytes locally under the same key
        // makes both of those find a file, so no test touches the network.
        val file = hub.fileLocal(ChatMediaHub.mediaKey(VOICE.blobId), "voice.m4a", ByteArray(64))
        requireNotNull(file) { "the media cache refused to file the fixture" }
        // Robolectric plays nothing, but it will report a prepared player of a
        // stated duration for a data source that was registered first. Without
        // this the production `prepare()` throws and the bubble shows its
        // failure line instead of playing.
        ShadowMediaPlayer.addMediaInfo(
            DataSource.toDataSource(file.absolutePath),
            ShadowMediaPlayer.MediaInfo(DURATION_MS.toInt(), 0),
        )
    }

    @After
    fun tearDown() {
        hub.dispose()
        scope.cancel()
    }

    // ----- what the tests render ------------------------------------------

    /** The bubble under a tag that wraps it exactly, so its measured size IS
     *  the bubble's. */
    @Composable
    private fun TaggedVoiceBubble(meta: (@Composable () -> Unit)?) {
        MaterialTheme {
            Surface {
                Box(Modifier.testTag(BUBBLE)) {
                    VoiceBubble(
                        ref = VOICE,
                        hub = hub,
                        isOwn = false,
                        contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                        bubbleColor = MaterialTheme.colorScheme.surfaceVariant,
                        meta = meta,
                    )
                }
            }
        }
    }

    /**
     * Press the play disc the way a finger does, and wait for the hub to be
     * playing rather than for a fixed time.
     *
     * `useUnmergedTree` and the size guard are both load-bearing, and both were
     * added after a deliberately broken build passed this. A bubble's own
     * `combinedClickable` merges its descendants, so an unwired play disc stops
     * being a merge boundary and is ABSORBED into the bubble's single node —
     * which also carries this description. The finder then matched the whole
     * bubble, `performClick` aimed at its centre, that centre is the waveform,
     * and the waveform's seek starts playback. The test passed with the button
     * disconnected.
     */
    private fun startPlayback() {
        val disc = compose.onNode(hasContentDescription(PLAY, substring = true), useUnmergedTree = true)
        val size = disc.fetchSemanticsNode().size
        assertTrue(
            "this is not the play disc but something $size big — the finder has retargeted",
            size.width <= TOUCH_TARGET_PX && size.height <= TOUCH_TARGET_PX,
        )
        disc.performClick()
        compose.waitUntil(WAIT_MS) { hub.voice.value.playing }
    }

    private fun bubbleHeight(): Int =
        compose.onNodeWithTag(BUBBLE).fetchSemanticsNode().size.height

    /**
     * The action menu is open, judged by its first item being in the tree.
     *
     * Not `assertIsDisplayed`: the menu is a `DropdownMenu`, which Compose puts
     * in a `Popup` — its own window, which Robolectric never places on a real
     * display, so every popup here reports itself as not displayed whether it
     * opened or not. Existence is the question this can answer, and it is the
     * one that discriminates: [assertMenuClosed] is the same finder, and the
     * gestures that must not open the menu leave nothing for it to find.
     */
    private fun assertMenuOpen() = compose.onNodeWithText(REACT).assertExists()

    private fun assertMenuClosed() = compose.onNodeWithText(REACT).assertDoesNotExist()

    // ----- 1. the bubble does not change height when playback starts -------

    @Test
    fun theBubbleIsTheSameHeightPlayingAsItIsIdle() {
        compose.setContent { TaggedVoiceBubble(meta = null) }

        val idle = bubbleHeight()
        assertTrue("the bubble measured nothing at all", idle > 0)

        startPlayback()

        // §2, in as many words: "Nothing about the row's geometry changes,
        // ever." Three children of the quiet line appear and disappear while a
        // person watches — the speed control, the clock switching from total to
        // elapsed, the unplayed dot — and each RESERVES its space rather than
        // taking it. A person reported the bubble growing on the first frame of
        // playback; this is the measurement that says whether it does.
        assertEquals("the bubble changed height when playback started", idle, bubbleHeight())
    }

    @Test
    fun theBubbleIsTheSameHeightWhetherOrNotItCarriesItsOwnTime() {
        // The other half of the same rule, and the one a caller can break from
        // outside: a lone voice message carries its time on the quiet line
        // (`BubbleMetaSlot.VOICE_LINE`) and one with a caption does not, so the
        // row has to measure the same either way or the two shapes disagree
        // about how tall a voice message is.
        // One composition, two measurements: `setContent` may be called once
        // per test, and re-measuring the SAME bubble as the slot fills is
        // closer to what happens anyway — a row arrives, and its time appears.
        var carriesTime by mutableStateOf(false)
        compose.setContent {
            TaggedVoiceBubble(meta = if (carriesTime) ({ MetaTime() }) else null)
        }

        val without = bubbleHeight()
        carriesTime = true
        val with = bubbleHeight()

        assertEquals("the meta line changed the row's height", without, with)
    }

    // ----- 2. the time sits on the same row as the duration ----------------

    @Test
    fun theMessageTimeSitsOnTheSameRowAsTheRecordingDuration() {
        compose.setContent { TaggedVoiceBubble(meta = { MetaTime() }) }

        // The clock is the recording's own length until playback starts —
        // `formatTotalClock(12_000) == "0:12"`. Its transparent reservation twin
        // has its semantics cleared, so there is exactly one node with this text.
        val clock = compose.onNodeWithText(TOTAL_CLOCK).fetchSemanticsNode().boundsInWindow
        val time = compose.onNodeWithTag(META).fetchSemanticsNode().boundsInWindow

        val clockCentre = (clock.top + clock.bottom) / 2f
        val timeCentre = (time.top + time.bottom) / 2f
        // "Visually on one level", from the person who reported them on two:
        // the centres agree to within a pixel. A row below would put them a
        // whole line apart, which is what iOS never did and Android did.
        assertTrue(
            "the duration is centred at $clockCentre and the message time at $timeCentre, " +
                "which is not one row",
            abs(clockCentre - timeCentre) <= 1f,
        )
        // And the time is at the TRAILING end of that row, after the clock,
        // rather than merely level with it.
        assertTrue(
            "the message time ($time) is not after the duration ($clock)",
            time.left >= clock.right,
        )
    }

    // ----- 3. only a long press opens the action menu ----------------------

    @Test
    fun aLongPressOnABubbleOpensTheActionMenu() {
        compose.setContent { BubbleUnderTest(TEXT_MESSAGE) }

        compose.onNodeWithTag(BUBBLE).performTouchInput { longClick() }

        assertMenuOpen()
    }

    @Test
    fun aPlainTapOnABubbleOpensNothing() {
        // The maintainer's rule: every action is explicitly initiated — right
        // click on web, long press on native. A tap that opened the menu would
        // make a message impossible to touch without being offered Delete.
        compose.setContent { BubbleUnderTest(TEXT_MESSAGE) }

        compose.onNodeWithTag(BUBBLE).performClick()

        assertMenuClosed()
    }

    @Test
    fun aTapOnThePlayButtonPlaysAndDoesNotOpenTheMenu() {
        // The collision `89475ac3` fixed on web and iOS, asserted on Android:
        // the two gestures share the same pixels, because the play disc sits
        // inside a bubble whose surface holds the long press. A press on the
        // disc has to reach the player and stop there — on web it reached the
        // reaction picker instead and the message could not be listened to.
        compose.setContent { BubbleUnderTest(VOICE_MESSAGE) }

        startPlayback()

        assertTrue("the play button did not start playback", hub.voice.value.playing)
        assertMenuClosed()
    }

    /**
     * One incoming message in the real [Bubble] — the composable that owns the
     * long press — with the menu it opens and nothing else on screen.
     */
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

    /** Stands in for the private `BubbleMeta` the screen passes: the same
     *  typography, under a tag so its bounds can be read. */
    @Composable
    private fun MetaTime() {
        Text(
            text = TIME,
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.testTag(META),
        )
    }

    private companion object {
        const val BUBBLE = "voice-bubble"
        const val META = "voice-meta"
        const val PLAY = "Play voice message"
        const val REACT = "React"
        const val TIME = "09:00"
        const val DURATION_MS = 12_000L

        /** `formatTotalClock(DURATION_MS)`. */
        const val TOTAL_CLOCK = "0:12"
        const val WAIT_MS = 5_000L

        /** The play disc's 48dp touch target, in px at the test density. */
        const val TOUCH_TARGET_PX = 48

        val VOICE = AttachmentRef(
            blobId = ByteArray(32) { 11 },
            key = ByteArray(32) { 22 },
            nonce = ByteArray(12) { 33 },
            mime = "audio/mp4",
            size = 4_096u,
            name = "voice.m4a",
            kind = KIND_VOICE,
            durationMs = DURATION_MS.toUInt(),
            // Loud and quiet, so the waveform is a wave — the drawing itself is
            // pinned in `VoiceWaveformUiTest`.
            waveform = ByteArray(32) { i -> if (i % 3 == 0) -1 else 40 },
        )

        private fun incoming(text: String, attachments: List<AttachmentRef>) = ChatMessage(
            messageId = 7L,
            direction = Direction.Incoming,
            text = text,
            attachments = attachments,
            createdAt = "2026-09-25T09:00:00Z",
            authorKey = "peer-key",
            ack = Ack.SENT,
        )

        /**
         * A plain paragraph, for the two gesture tests that are about the
         * BUBBLE rather than about the media in it.
         *
         * A voice bubble is the wrong subject for those two: its waveform is a
         * seek surface and its play disc is a button, and each claims the press
         * where it is drawn — which between them is most of the bubble. A long
         * press aimed at a voice bubble's centre lands on the waveform, so a
         * test using one would be measuring which child won rather than whether
         * the menu opens.
         */
        val TEXT_MESSAGE = incoming(text = "tap me", attachments = emptyList())

        val VOICE_MESSAGE = incoming(text = "", attachments = listOf(VOICE))
    }
}
