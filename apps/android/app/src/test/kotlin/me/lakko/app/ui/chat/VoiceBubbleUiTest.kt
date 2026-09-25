// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.click
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.longClick
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
    // ----- 4. the deliberate gesture reaches the menu over the bubble's own
    //          controls, and is not also a tap on them ---------------------

    /**
     * `docs/ux/client-redesign.md` §"reactions and actions": the deliberate
     * gesture reaches the menu from anywhere on the bubble, INCLUDING over the
     * bubble's own controls, and a press long enough to be that gesture is not
     * also a tap on the control underneath it. Both halves are the rule, and
     * the second is what makes the first safe — a long press that opened the
     * menu and seeked the audio under it would have done something nobody
     * asked for.
     *
     * A voice bubble is where the rule bites hardest, because two of its three
     * controls cover most of it: the waveform is a seek surface 170dp wide and
     * the play disc's touch target is 48dp, so the padding neither covers is a
     * few pixels of border. That is why the two gesture tests above use a
     * plain paragraph — they are about the bubble — and these four use the
     * voice bubble, which is about the controls.
     */
    @Test
    fun aLongPressOnTheWaveformOpensTheActionMenu() {
        compose.setContent { BubbleUnderTest(VOICE_MESSAGE) }

        waveform().performTouchInput { longClick() }

        assertMenuOpen()
    }

    @Test
    fun aLongPressOnThePlayDiscOpensTheActionMenu() {
        compose.setContent { BubbleUnderTest(VOICE_MESSAGE) }

        playDisc().performTouchInput { longClick() }

        assertMenuOpen()
    }

    @Test
    fun aLongPressOnTheWaveformNeitherSeeksNorPlays() {
        // The second half of the rule, on the waveform. Asserted from the idle
        // state because from idle EVERY seek is visible in one field: the
        // waveform's `onSeek` runs `hub.seekVoice`, which on a message that is
        // not the current one starts it — so a player key at all means the
        // press moved the playhead, whether or not the audio device then got
        // as far as a sample.
        compose.setContent { BubbleUnderTest(VOICE_MESSAGE) }

        waveform().performTouchInput { longClick() }

        assertVoiceUntouched()
    }

    @Test
    fun aLongPressOnThePlayDiscNeitherPlaysNorSeeks() {
        compose.setContent { BubbleUnderTest(VOICE_MESSAGE) }

        playDisc().performTouchInput { longClick() }

        assertVoiceUntouched()
    }

    // ----- 5. a plain press on either control still does its own job --------

    /**
     * A plain tap still seeks, and to the fraction of the waveform it landed
     * on — the half of the rule that a fix declining every press would break
     * silently, leaving a waveform that opens a menu and does nothing else.
     *
     * This one and the drag below render [WaveformSeekBar] on its own with a
     * recording callback rather than inside the real [Bubble], because the
     * question is WHICH FRACTION the gesture produced and the fraction is what
     * this composable computes; inside a bubble it is handed to a player whose
     * position then belongs to the audio device, so a bubble-level assertion
     * would be measuring Robolectric's `ShadowMediaPlayer`. The tests that ask
     * whether the MENU opens use the real bubble, because the menu is the
     * bubble's.
     */
    @Test
    fun aPlainTapOnTheWaveformSeeksToTheTappedFraction() {
        val seeks = mutableListOf<Float>()
        compose.setContent { RecordingWaveform(seeks::add) }

        compose.onNodeWithContentDescription(WAVEFORM).performTouchInput {
            click(percentOffset(TAP_FRACTION, 0.5f))
        }

        assertEquals("the tap produced no seek at all", 1, seeks.size)
        assertTrue(
            "a tap ${TAP_FRACTION * 100}% across the waveform seeked to ${seeks.first()}",
            abs(seeks.first() - TAP_FRACTION) <= FRACTION_TOLERANCE,
        )
    }

    @Test
    fun aHorizontalDragOnTheWaveformScrubs() {
        val seeks = mutableListOf<Float>()
        compose.setContent { RecordingWaveform(seeks::add) }

        compose.onNodeWithContentDescription(WAVEFORM).performTouchInput {
            down(percentOffset(0.1f, 0.5f))
            moveTo(percentOffset(0.5f, 0.5f))
            moveTo(percentOffset(0.9f, 0.5f))
            up()
        }

        // Scrubbing is a seek that FOLLOWS the finger, so one seek is not
        // enough and the last one has to be where the finger ended. A drag
        // that only reported its start would look like a working seek bar and
        // feel like a broken one.
        assertTrue("the drag produced no seek at all", seeks.isNotEmpty())
        assertTrue(
            "the drag reported $seeks, which does not end near the finger",
            abs(seeks.last() - 0.9f) <= FRACTION_TOLERANCE,
        )
        assertTrue("the drag did not move the playhead forward: $seeks", seeks.last() > seeks.first())
    }

    // ----- 6. TalkBack reaches both, over the same control ------------------

    /**
     * A screen reader's long press never travels through the pointer pipeline —
     * it invokes the `OnLongClick` semantics action directly — so a control that
     * declines a touch long press would have taken the menu away from TalkBack
     * entirely unless the action is there too.
     *
     * And the seek has to survive beside it: the waveform is exposed as a slider
     * (UX spec §7), and `SetProgress` is the ONLY way a screen-reader user
     * moves the playhead, there being no drag for them to make. Both actions are
     * offered and the user picks, which is why this asserts the pair rather than
     * either one.
     */
    @Test
    fun theWaveformOffersTalkBackBothTheSeekAndTheMenu() {
        compose.setContent { BubbleUnderTest(VOICE_MESSAGE) }

        val config = waveform().fetchSemanticsNode().config

        assertTrue(
            "TalkBack lost the waveform's seek action",
            config.contains(SemanticsActions.SetProgress),
        )
        assertTrue(
            "TalkBack cannot reach the action menu from the waveform",
            config.contains(SemanticsActions.OnLongClick),
        )
    }

    @Test
    fun thePlayDiscOffersTalkBackBothPlaybackAndTheMenu() {
        compose.setContent { BubbleUnderTest(VOICE_MESSAGE) }

        val config = playDisc().fetchSemanticsNode().config

        assertTrue("TalkBack lost the play button", config.contains(SemanticsActions.OnClick))
        assertTrue(
            "TalkBack cannot reach the action menu from the play button",
            config.contains(SemanticsActions.OnLongClick),
        )
    }

    // ----- what the gesture tests reach ------------------------------------

    /**
     * The waveform inside the bubble under test.
     *
     * The size guard is the same one [startPlayback] carries and for the same
     * reason: the bubble's `combinedClickable` merges its descendants, so a
     * control that stops being a merge boundary is ABSORBED into the bubble's
     * single node, which the finder would then match — and a gesture aimed at
     * the bubble's centre lands on the waveform anyway, so the test would pass
     * while measuring nothing.
     */
    private fun waveform() = compose
        .onNodeWithContentDescription(WAVEFORM, useUnmergedTree = true)
        .also { node ->
            val size = node.fetchSemanticsNode().size
            assertTrue(
                "this is not the waveform but something $size big — the finder has retargeted",
                size.width <= WAVEFORM_WIDTH_PX && size.height <= WAVEFORM_HEIGHT_PX,
            )
        }

    private fun playDisc() = compose
        .onNode(hasContentDescription(PLAY, substring = true), useUnmergedTree = true)
        .also { node ->
            val size = node.fetchSemanticsNode().size
            assertTrue(
                "this is not the play disc but something $size big — the finder has retargeted",
                size.width <= TOUCH_TARGET_PX && size.height <= TOUCH_TARGET_PX,
            )
        }

    /** Nothing about the player moved: no message is loaded, none is playing,
     *  and the playhead is still at the start. */
    private fun assertVoiceUntouched() {
        val voice = hub.voice.value
        assertEquals("the press picked a message up for the player", null, voice.key)
        assertTrue("the press started playback", !voice.playing)
        assertEquals("the press moved the playhead", 0L, voice.positionMs)
    }

    /** The production waveform at its production size, reporting every seek it
     *  decides on. */
    @Composable
    private fun RecordingWaveform(onSeek: (Float) -> Unit) {
        MaterialTheme {
            Surface {
                WaveformSeekBar(
                    waveform = VOICE.waveform,
                    progress = 0f,
                    playedColor = MaterialTheme.colorScheme.onSurfaceVariant,
                    idleColor = MaterialTheme.colorScheme.onSurfaceVariant,
                    label = WAVEFORM,
                    modifier = Modifier.width(170.dp).height(26.dp),
                    // A menu to reach, so the long-press branch is armed the
                    // way it is in a bubble that offers actions. Without it
                    // these two would pass against a waveform that has no
                    // long-press rule at all — which is what they did on the
                    // first run, before the fix existed to arm.
                    onLongPress = {},
                    onSeek = onSeek,
                )
            }
        }
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

        /** [WaveformSeekBar]'s own label, as [VoiceBubble] passes it. */
        const val WAVEFORM = "Voice message position"
        const val REACT = "React"
        const val TIME = "09:00"
        const val DURATION_MS = 12_000L

        /** `formatTotalClock(DURATION_MS)`. */
        const val TOTAL_CLOCK = "0:12"
        const val WAIT_MS = 5_000L

        /** The play disc's 48dp touch target, in px at the test density. */
        const val TOUCH_TARGET_PX = 48

        /** [VoiceBubble]'s waveform, 170dp x 26dp, in px at the test density. */
        const val WAVEFORM_WIDTH_PX = 170
        const val WAVEFORM_HEIGHT_PX = 26

        /** Where the tap test lands: past the middle, so a fix that reported
         *  the centre or zero instead of the touched point still fails. */
        const val TAP_FRACTION = 0.78f

        /** The touched point is a whole pixel and the fraction is that pixel
         *  over the width, so the two can differ by half a bar. */
        const val FRACTION_TOLERANCE = 0.02f

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
         * A voice bubble is the wrong subject for those two, and still is now
         * that a press over its controls reaches the menu: a long press aimed at
         * a voice bubble's centre lands on the waveform, so a test using one
         * would be reading the waveform's rule rather than the bubble's. Which
         * of the two it read stopped mattering to the VERDICT once both open the
         * menu — and that is exactly why the subject has to stay a paragraph,
         * because a test that cannot distinguish them would keep passing if the
         * bubble's own long press were removed.
         */
        val TEXT_MESSAGE = incoming(text = "tap me", attachments = emptyList())

        val VOICE_MESSAGE = incoming(text = "", attachments = listOf(VOICE))
    }
}
