// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import me.lakko.app.chat.Ack
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.InlineKind
import me.lakko.app.media.KIND_GIF
import me.lakko.app.media.KIND_VIDEO_NOTE
import me.lakko.app.media.MediaAllowlist
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import uniffi.lakko_ffi_uniffi.AttachmentRef

/**
 * `docs/ux/client-redesign.md` — the deliberate gesture reaches the action menu
 * from anywhere on the bubble, INCLUDING over the bubble's own controls.
 *
 * `VoiceBubbleUiTest` pins that on the voice bubble, where the defect was
 * reported. This one pins it on the other four, because the spec's sentence is
 * about every control inside a bubble and each of these four is a single
 * control covering its whole bubble — a photo, a video poster, an animation and
 * a video-note circle all answer a tap over every pixel a person could press,
 * so if any of them claims the press there is nowhere left to long-press at all.
 *
 * Two of the four already behaved (the photo and the video were handed the
 * bubble's callback when they were written) and two did not, and that is the
 * point of testing all four rather than the two that were broken: the
 * difference between them was never a platform constraint, only whether
 * somebody remembered to pass a parameter. The two that worked are the control
 * that says this suite can tell the two states apart.
 *
 * The gesture is aimed at the BUBBLE's centre rather than at a control found by
 * its label, because that is the press a person makes and because a bubble
 * whose media fills it has its control under that centre by construction.
 */
@RunWith(RobolectricTestRunner::class)
class MediaBubbleActionMenuTest {
    init {
        EnglishForTests.install()
    }


    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private lateinit var scope: CoroutineScope
    private lateinit var hub: ChatMediaHub

    /** The same shape as the production allowlist without the native core —
     *  see `VoiceBubbleUiTest`. */
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
        // Every one of these bubbles fetches on compose. Filing the bytes
        // locally under the same key makes each find a file, so no test touches
        // the network. The bytes decode as nothing, which is fine: a bubble that
        // failed to decode still carries its control (relabelled "Retry"), and
        // the control is the whole subject here.
        for (ref in listOf(PHOTO, VIDEO, GIF, VIDEO_NOTE)) {
            hub.fileLocal(ChatMediaHub.mediaKey(ref.blobId), ref.name ?: "media", ByteArray(64))
        }
    }

    @After
    fun tearDown() {
        hub.dispose()
        scope.cancel()
    }

    @Test
    fun aLongPressOnAPhotoOpensTheActionMenu() = assertLongPressOpensTheMenu(PHOTO)

    @Test
    fun aLongPressOnAVideoOpensTheActionMenu() = assertLongPressOpensTheMenu(VIDEO)

    @Test
    fun aLongPressOnAGifOpensTheActionMenu() = assertLongPressOpensTheMenu(GIF)

    @Test
    fun aLongPressOnAVideoNoteOpensTheActionMenu() = assertLongPressOpensTheMenu(VIDEO_NOTE)

    @Test
    fun aLongPressOnAVideoNoteDoesNotStartIt() {
        // The second half of the rule on a bubble that is not the voice one: the
        // press that opened the menu was not also a tap on the control under it.
        // Read off the control's own click label, which is the one piece of a
        // video note's playing state that reaches the semantics tree — it says
        // "Play" while stopped and "Pause" while playing, so a press that
        // started the video would rename the control.
        compose.setContent { BubbleUnderTest(incoming(VIDEO_NOTE)) }

        compose.onNodeWithTag(BUBBLE).performTouchInput { longClick() }

        compose.onNodeWithContentDescription(MENU_SCRIM).assertExists()
        // The label carries the duration since ADR-0061 (`chat.video_note.play`),
        // so the stopped state is matched on its start.
        compose.onNode(
            androidx.compose.ui.test.SemanticsMatcher("click label starts with Play video message") {
                it.config.getOrNull(androidx.compose.ui.semantics.SemanticsActions.OnClick)
                    ?.label?.startsWith("Play video message") == true
            },
            useUnmergedTree = true,
        ).assertExists()
    }

    private fun assertLongPressOpensTheMenu(ref: AttachmentRef) {
        compose.setContent { BubbleUnderTest(incoming(ref)) }

        compose.onNodeWithTag(BUBBLE).performTouchInput { longClick() }

        // Existence, not display: the menu lives in a `Popup`, which
        // Robolectric never places on a real display — see
        // `VoiceBubbleUiTest.assertMenuOpen`, which also says why the anchor is
        // the scrim rather than a row of the card.
        compose.onNodeWithContentDescription(MENU_SCRIM).assertExists()
    }

    /** A control's `onClick` label, which no stock matcher reaches. */
    private fun hasClickLabel(label: String) =
        SemanticsMatcher("onClick label is '$label'") { node ->
            node.config.getOrNull(SemanticsActions.OnClick)?.label == label
        }

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

    private companion object {
        const val BUBBLE = "media-bubble"

        /** [CLOSE_MENU_LABEL] — the action menu's scrim, the one node present
         *  whenever the menu is open whatever rows it offers. */
        val MENU_SCRIM get() = en(CLOSE_MENU_LABEL)
        const val TIME = "09:00"

        /** Every bubble here is the message's SOLE attachment with no caption,
         *  which is what `soleMediaBubble` needs to render the chromeless,
         *  edge-to-edge shapes the rule is hardest on. */
        private fun incoming(ref: AttachmentRef) = ChatMessage(
            messageId = 7L,
            direction = Direction.Incoming,
            text = "",
            attachments = listOf(ref),
            createdAt = "2026-09-25T09:00:00Z",
            authorKey = "peer-key",
            ack = Ack.SENT,
        )

        private fun ref(
            seed: Byte,
            mime: String,
            name: String,
            kind: String? = null,
            durationMs: UInt? = null,
        ) = AttachmentRef(
            blobId = ByteArray(32) { seed },
            key = ByteArray(32) { 22 },
            nonce = ByteArray(12) { 33 },
            mime = mime,
            size = 4_096u,
            name = name,
            kind = kind,
            durationMs = durationMs,
            width = 240u,
            height = 240u,
        )

        // `renderDecision`: a null kind with an image MIME is a photo, with a
        // video MIME a video; `gif` and `video_note` name their own bubbles.
        val PHOTO = ref(1, "image/jpeg", "photo.jpg")
        val VIDEO = ref(2, "video/mp4", "clip.mp4")
        val GIF = ref(3, "image/gif", "loop.gif", kind = KIND_GIF)
        val VIDEO_NOTE = ref(4, "video/mp4", "note.mp4", kind = KIND_VIDEO_NOTE, durationMs = 8_000u)
    }
}
