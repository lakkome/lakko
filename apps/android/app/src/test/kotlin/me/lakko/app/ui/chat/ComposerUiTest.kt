// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextReplacement
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.InlineKind
import me.lakko.app.media.MediaAllowlist
import me.lakko.app.media.RecState
import me.lakko.app.media.RecordedTake
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import uniffi.lakko_ffi_uniffi.AttachmentRef

/**
 * `docs/ux/client-redesign.md` UX 5.2 — the composer row: "attach, text field
 * growing to five lines, emoji or GIF picker, a trailing button that is a
 * microphone with an empty draft and a send arrow otherwise".
 *
 * Every UI test on this client before these looked at ONE message bubble or at
 * the record button on its own. The row those things sit in was covered
 * nowhere, and it is the one piece of chat a person touches on every single
 * message they send.
 *
 * What these drive is the real [Composer] over a real [RecordController] — the
 * same pairing `RecordButtonUiTest` uses, for the same reason: the recording
 * reducer is pinned as a pure function elsewhere, and what no reducer test can
 * say is whether the ROW asks it the right question or swaps the right things
 * out when it answers.
 *
 * Three limits, stated rather than worked around:
 *  - the emoji and attach buttons are asserted to be REACHABLE (present,
 *    enabled, and answering a tap into their callback). What they then open —
 *    an OS document picker and a `ModalBottomSheet` owned by `ChatScreen` — is
 *    outside this composable, so a tap is followed only as far as the seam.
 *  - camera mode is not a state a JVM test can hold, for the reason
 *    `RecordButtonUiTest` documents (`probeCameras` finds no camera and the
 *    controller correctly leaves the mode again).
 *  - the field's growth is measured in HEIGHT rather than in line count,
 *    because a line count is not a thing a person sees and not a thing the
 *    semantics tree carries.
 */
@RunWith(RobolectricTestRunner::class)
class ComposerUiTest {
    init {
        EnglishForTests.install()
    }


    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private lateinit var scope: CoroutineScope
    private lateinit var hub: ChatMediaHub
    private var controller: RecordController? = null

    private val allowlist: MediaAllowlist = { mime ->
        if (mime.startsWith("audio/")) InlineKind.AUDIO else null
    }

    /** What the row handed back, so a tap can be followed to its seam. */
    private val sent = mutableListOf<RecordedTake>()
    private var sendClicks = 0
    private var pickerOpens = 0
    private var attachMenuPicks = 0
    private var cancelReplies = 0

    @Before
    fun setUp() {
        scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        hub = ChatMediaHub(compose.activity, scope, allowlist)
        // Granted, so a hold on the mic reaches recording rather than stopping
        // at the permission prompt — the denied device is the reducer's
        // business and is covered there.
        shadowOf(compose.activity.application)
            .grantPermissions(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA)
        shadowOf(compose.activity.packageManager)
            .setSystemFeature(PackageManager.FEATURE_CAMERA_ANY, true)
    }

    @After
    fun tearDown() {
        hub.dispose()
        scope.cancel()
    }

    // ----- what the tests render ------------------------------------------

    /**
     * The real composer, with the draft as the one thing a test moves.
     *
     * Bounded to a phone-ish width because the field's growth is the subject of
     * two tests below and an unbounded row would fit every draft on one line.
     */
    @Composable
    private fun ComposerUnderTest(
        draft: String,
        draftRevision: Int = 0,
        replyStrip: me.lakko.app.chat.ReplyArmed? = null,
    ) {
        MaterialTheme {
            Surface {
                val c = rememberRecordController(hub) { take -> sent += take }
                controller = c
                Box(Modifier.width(WIDTH.dp)) {
                    Composer(
                        recorder = c,
                        emojiInsert = null,
                        onEmojiInserted = {},
                        onOpenPicker = { pickerOpens += 1 },
                        draft = draft,
                        draftRevision = draftRevision,
                        sending = false,
                        sendError = null,
                        pendingAttachments = emptyList<AttachmentRef>(),
                        uploading = false,
                        inFlightUploads = emptyList(),
                        pendingPreviews = emptyMap(),
                        onDraftChange = {},
                        onSendClicked = { sendClicks += 1 },
                        onFocusChanged = {},
                        onPickMedia = { attachMenuPicks += 1 },
                        onAttachClicked = { attachMenuPicks += 1 },
                        onRemoveAttachment = {},
                        replyStrip = replyStrip,
                        onCancelReply = { cancelReplies += 1 },
                    )
                }
            }
        }
    }

    // ----- The field follows outside edits, never its own late echo -------

    @Test
    fun aDraftThatArrivesLateDoesNotPutOlderTextOverWhatWasTyped() {
        var draft by mutableStateOf("")
        compose.setContent { ComposerUnderTest(draft = draft) }
        val field = compose.onNode(hasSetTextAction())
        field.performTextReplacement("abcd")
        compose.waitForIdle()
        // The view model's state is behind by a keystroke: this is its echo of
        // "abc", arriving after "abcd" was typed.
        draft = "abc"
        compose.waitForIdle()
        field.assert(hasText("abcd"))
    }

    @Test
    fun aClearedDraftAfterSendClearsTheField() {
        var draft by mutableStateOf("hello")
        var revision by mutableStateOf(0)
        compose.setContent { ComposerUnderTest(draft = draft, draftRevision = revision) }
        draft = ""
        revision = 1
        compose.waitForIdle()
        compose.onNode(hasSetTextAction()).assert(!hasText("hello"))
    }

    // ----- ADR-0052 / message-actions §3.1: the reply strip ---------------

    @Test
    fun theReplyStripShowsTheSnapshotAndItsCloseControlClearsIt() {
        val armed = me.lakko.app.chat.ReplyArmed(
            messageId = 7L,
            snapshot = me.lakko.app.chat.ReplyRef(target = 7L, author = "Anna", excerpt = "the question"),
        )
        compose.setContent { ComposerUnderTest(draft = "", replyStrip = armed) }
        // What the strip says is what the recipient will see: the name and
        // the excerpt as they were snapshotted.
        compose.onNodeWithText("Anna").assertExists()
        compose.onNodeWithText("the question").assertExists()
        compose.onNodeWithContentDescription(en(CANCEL_REPLY_LABEL)).performClick()
        assertEquals(1, cancelReplies)
    }

    @Test
    fun aSnapshotLessReplyStripReadsMessage() {
        val armed = me.lakko.app.chat.ReplyArmed(
            messageId = 7L,
            snapshot = me.lakko.app.chat.ReplyRef(target = 7L, author = null, excerpt = null),
        )
        compose.setContent { ComposerUnderTest(draft = "", replyStrip = armed) }
        // The text field's placeholder is the same word, so the strip's line
        // is the "Message" that is NOT an editable field.
        compose.onNode(hasText(en("chat.reply.fallback_excerpt")) and !hasSetTextAction()).assertExists()
    }

    private fun controller(): RecordController = requireNotNull(controller) {
        "the composer never composed, so there is no controller to read"
    }

    /**
     * The text field's measured height.
     *
     * Found by its edit action rather than by its placeholder, which is what
     * these two tests first reached for: a placeholder is drawn only while the
     * field is EMPTY, so the finder vanished the moment a draft was typed and
     * both growth tests failed on a missing node rather than on a height. The
     * composer is the only editable thing on screen, so the action names it
     * uniquely — and it names it whatever the draft is.
     */
    private fun fieldHeight(): Int =
        compose.onNode(hasSetTextAction()).fetchSemanticsNode().size.height

    // ----- 1. the trailing button is the mic or the arrow, never both ------

    @Test
    fun theTrailingButtonIsTheMicrophoneWithAnEmptyDraft() {
        compose.setContent { ComposerUnderTest(draft = "") }

        compose.onNodeWithContentDescription(MIC).assertExists()
        // And the arrow is not merely disabled but ABSENT. A greyed-out Send
        // beside a mic would be two trailing buttons, which is the thing UX 5.2
        // describes as one.
        compose.onNodeWithContentDescription(SEND).assertDoesNotExist()
    }

    @Test
    fun theTrailingButtonIsTheSendArrowOnceThereIsADraft() {
        compose.setContent { ComposerUnderTest(draft = "hello") }

        compose.onNodeWithContentDescription(SEND).assertExists()
        compose.onNodeWithContentDescription(MIC).assertDoesNotExist()
    }

    @Test
    fun theTrailingButtonSwapsWhenTheDraftIsTypedAndAgainWhenItIsCleared() {
        // The swap in both directions on ONE composition, which is what a
        // person does: they type, they change their mind, they delete it. Two
        // separate tests of the two end states would both pass against a row
        // that decided the button once and never looked again.
        var draft by mutableStateOf("")
        compose.setContent { ComposerUnderTest(draft = draft) }

        compose.onNodeWithContentDescription(MIC).assertExists()

        draft = "hello"
        compose.onNodeWithContentDescription(SEND).assertExists()
        compose.onNodeWithContentDescription(MIC).assertDoesNotExist()

        draft = ""
        compose.onNodeWithContentDescription(MIC).assertExists()
        compose.onNodeWithContentDescription(SEND).assertDoesNotExist()
    }

    @Test
    fun theSendArrowHandsTheDraftOnToTheRow() {
        compose.setContent { ComposerUnderTest(draft = "hello") }

        compose.onNodeWithContentDescription(SEND).performClick()

        assertEquals("the send arrow is not wired to anything", 1, sendClicks)
    }

    // ----- 2. attach and emoji are reachable ------------------------------

    @Test
    fun attachAndEmojiAreBothReachableBesideTheField() {
        compose.setContent { ComposerUnderTest(draft = "") }

        compose.onNodeWithContentDescription(ATTACH).assertExists().assertIsEnabled()
        compose.onNodeWithContentDescription(EMOJI).assertExists().assertIsEnabled()
    }

    @Test
    fun theEmojiButtonOpensThePicker() {
        compose.setContent { ComposerUnderTest(draft = "") }

        compose.onNodeWithContentDescription(EMOJI).performClick()

        // Followed only as far as the seam: the sheet itself is `ChatScreen`'s.
        assertEquals("the emoji button is not wired to the picker", 1, pickerOpens)
    }

    @Test
    fun theAttachButtonOffersPhotoOrVideoAndFile() {
        // UX spec §10 — one tap, two destinations, because a person who wants
        // to send a photo and a person who wants to send a PDF press the same
        // thing. Both items have to be THERE: an attach button that only ever
        // opened the document picker is how a photo became a file attachment.
        compose.setContent { ComposerUnderTest(draft = "") }

        compose.onNodeWithContentDescription(ATTACH).performClick()

        compose.onNodeWithText(PHOTO_OR_VIDEO).assertExists()
        compose.onNodeWithText(FILE).assertExists()

        compose.onNodeWithText(PHOTO_OR_VIDEO).performClick()
        assertEquals("the gallery item is not wired to anything", 1, attachMenuPicks)
    }

    // ----- 3. the field grows, and then stops ------------------------------

    @Test
    fun theFieldGrowsWithTheDraft() {
        var draft by mutableStateOf("one line")
        compose.setContent { ComposerUnderTest(draft = draft, draftRevision = draft.length) }

        val oneLine = fieldHeight()
        assertTrue("the field measured nothing at all", oneLine > 0)

        draft = lines(4)

        assertTrue(
            "a four-line draft left the field at its one-line height ($oneLine)",
            fieldHeight() > oneLine,
        )
    }

    @Test
    fun theFieldStopsGrowingAtFiveLines() {
        // "growing to five lines" is a CAP, and the cap is the half that
        // matters: without it a long message eats the timeline it is a reply
        // to, and on a short phone it eats the keyboard as well.
        var draft by mutableStateOf(lines(5))
        compose.setContent { ComposerUnderTest(draft = draft, draftRevision = draft.length) }

        val atCap = fieldHeight()

        draft = lines(20)

        assertEquals(
            "a twenty-line draft grew the field past its five-line cap",
            atCap,
            fieldHeight(),
        )
    }

    // ----- 4. the recording bar takes the field's place, and gives it back --

    @Test
    fun theRecordingBarReplacesTheFieldWhileATakeRunsAndGivesItBackAfterwards() {
        compose.setContent { ComposerUnderTest(draft = "") }

        // Before: the field is there and the recording bar is not.
        compose.onNodeWithText(PLACEHOLDER).assertExists()
        compose.onNodeWithText(SLIDE_TO_CANCEL, substring = true).assertDoesNotExist()

        // A finger down on the mic, held. The arming timer is a coroutine on
        // real time, so the hold is real rather than a jump of the compose
        // clock — the same shape `RecordButtonUiTest` uses.
        compose.onNodeWithContentDescription(MIC).performTouchInput { down(center) }
        compose.waitUntil(WAIT_MS) { controller().state is RecState.Recording }

        // During: §3.1 — the bar is IN PLACE OF the field, not beside it. Both
        // halves are asserted because a bar that appeared above a field still
        // showing would pass a test that only looked for the bar, and would
        // shift the whole row down mid-gesture.
        compose.onNodeWithText(SLIDE_TO_CANCEL, substring = true).assertExists()
        compose.onNodeWithText(PLACEHOLDER).assertDoesNotExist()
        // And the attach and emoji buttons went with it: the row has one job
        // while a take is running.
        compose.onNodeWithContentDescription(ATTACH).assertDoesNotExist()
        compose.onNodeWithContentDescription(EMOJI).assertDoesNotExist()

        // After: the finger lifts, and the field comes back. A take too short
        // to keep is discarded by the reducer, which is why this asserts the
        // field's RETURN rather than anything about a recording.
        compose.onNodeWithContentDescription(MIC).performTouchInput { up() }
        compose.waitUntil(WAIT_MS) { controller().state !is RecState.Recording }
        compose.waitForIdle()

        compose.onNodeWithText(PLACEHOLDER).assertExists()
        compose.onNodeWithText(SLIDE_TO_CANCEL, substring = true).assertDoesNotExist()
        compose.onNodeWithContentDescription(ATTACH).assertExists()
    }

    private companion object {
        /** A phone-ish composer width, so a long draft has to wrap. */
        const val WIDTH = 320

        /** [Composer]'s own placeholder, which is the field's handle here: an
         *  `OutlinedTextField` with no label carries no other text of its own. */
        val PLACEHOLDER get() = en("chat.composer.placeholder")
        val ATTACH get() = en("chat.composer.attach")
        val EMOJI get() = en("chat.composer.emoji_gifs")
        val SEND get() = en("chat.composer.send")

        /** [RecordButton]'s label in mic mode. */
        val MIC get() = en("chat.record.voice")
        val PHOTO_OR_VIDEO get() = en("chat.composer.attach_media")
        val FILE get() = en("chat.media.file")

        /** `RecordCopy.SLIDE_TO_CANCEL`, drawn with its own glyph. */
        val SLIDE_TO_CANCEL get() = en("chat.record.slide_cancel")
        const val WAIT_MS = 5_000L

        /** A draft that wraps to about [n] lines at [WIDTH]. */
        fun lines(n: Int): String = (1..n).joinToString("\n") { "line $it" }
    }
}
