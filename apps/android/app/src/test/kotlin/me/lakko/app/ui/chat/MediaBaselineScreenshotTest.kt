// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.unit.dp
import com.github.takahirom.roborazzi.captureRoboImage
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.InlineKind
import me.lakko.app.media.KIND_VOICE
import me.lakko.app.media.MediaAllowlist
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.GraphicsMode
import org.robolectric.shadows.ShadowMediaPlayer
import org.robolectric.shadows.util.DataSource
import uniffi.lakko_ffi_uniffi.AttachmentRef

/**
 * Baseline images for the media surfaces a person looks at most: the voice
 * bubble, idle and playing, and the composer's record button.
 *
 * These catch what a measurement does not — a glyph that stopped being drawn, a
 * colour that stopped reading against its background, a disc that lost its fill.
 * They are deliberately few: a baseline that covers a whole screen fails on
 * every unrelated change and is deleted within a month, so each of these frames
 * one component against a plain surface.
 *
 * The images live in `app/src/test/screenshots/` and are compared on every
 * ordinary run.
 *
 * There is NO camera-mode baseline, and the reason is worth recording because
 * the missing one was there first and was a lie: camera mode cannot be held
 * under Robolectric (see `RecordButtonUiTest` — CameraX reports no cameras and
 * the controller correctly leaves the mode again), so the frame taken "in camera
 * mode" was byte-identical to the mic one. A baseline that silently pictures the
 * wrong state is worse than an absent one.
 *
 * To re-record them after an intended change:
 *
 * ```
 * cd apps/android && ./gradlew :app:testDebugUnitTest -Plakko.roborazzi.record=true
 * ```
 *
 * then look at the diff before committing it. Without that flag a run can only
 * compare, which is the point: a run that silently re-recorded would make every
 * baseline agree with whatever the code does today.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class MediaBaselineScreenshotTest {
    init {
        EnglishForTests.install()
    }


    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private lateinit var scope: CoroutineScope
    private lateinit var hub: ChatMediaHub

    private val allowlist: MediaAllowlist = { mime ->
        if (mime.startsWith("audio/")) InlineKind.AUDIO else null
    }

    @Before
    fun setUp() {
        // A chat is only ever open with an account signed in; the played marks
        // are that account's (2026-09-30), and with none bound a mark is not
        // kept at all.
        me.lakko.app.contacts.AcceptedContactsStore.bind(ByteArray(16) { 0x0A })
        scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        hub = ChatMediaHub(compose.activity, scope, allowlist)
        val file = hub.fileLocal(ChatMediaHub.mediaKey(VOICE.blobId), "voice.m4a", ByteArray(64))
        requireNotNull(file) { "the media cache refused to file the fixture" }
        ShadowMediaPlayer.addMediaInfo(
            DataSource.toDataSource(file.absolutePath),
            ShadowMediaPlayer.MediaInfo(DURATION_MS.toInt(), 0),
        )
        shadowOf(compose.activity.application)
            .grantPermissions(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA)
        shadowOf(compose.activity.packageManager)
            .setSystemFeature(PackageManager.FEATURE_CAMERA_ANY, true)
    }

    @After
    fun tearDown() {
        hub.dispose()
        scope.cancel()
        me.lakko.app.contacts.AcceptedContactsStore.unbind()
    }

    @Composable
    private fun Framed(content: @Composable () -> Unit) {
        MaterialTheme {
            Surface {
                Box(Modifier.testTag(SUBJECT).padding(8.dp)) { content() }
            }
        }
    }

    /**
     * Compare (or, with `-Plakko.roborazzi.record=true`, record) the framed
     * subject against `app/src/test/screenshots/<name>.png`.
     *
     * The directory comes from a system property the build sets rather than from
     * a relative path: a relative one resolves against the test's working
     * directory, which put the baselines in the module root.
     */
    private fun capture(name: String) {
        // A state change reaches the pixels only after the next frame, so an
        // unforced capture can hand back the frame BEFORE the change. Two
        // baselines that should differ came out byte-identical without this.
        compose.waitForIdle()
        compose.mainClock.advanceTimeByFrame()
        compose.waitForIdle()
        val dir = requireNotNull(System.getProperty("roborazzi.output.dir")) {
            "the build did not set roborazzi.output.dir"
        }
        compose.onNodeWithTag(SUBJECT).captureRoboImage("$dir/$name.png")
    }

    @Test
    fun voiceBubbleIdle() {
        compose.setContent {
            Framed {
                VoiceBubble(
                    ref = VOICE,
                    hub = hub,
                    isOwn = false,
                    contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                    bubbleColor = MaterialTheme.colorScheme.surfaceVariant,
                    meta = null,
                )
            }
        }

        capture("voice-bubble-idle")
    }

    @Test
    fun voiceBubblePlaying() {
        compose.setContent {
            Framed {
                VoiceBubble(
                    ref = VOICE,
                    hub = hub,
                    isOwn = false,
                    contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                    bubbleColor = MaterialTheme.colorScheme.surfaceVariant,
                    meta = null,
                )
            }
        }
        compose.onNode(hasContentDescription("Play voice message", substring = true)).performClick()
        compose.waitUntil(WAIT_MS) { hub.voice.value.playing }
        // The elapsed clock counts while this is the current message, so the
        // frame is taken at a stated position rather than at whatever the tick
        // happened to reach — otherwise the baseline disagrees with itself.
        hub.seekVoice(VOICE, 0.5f, DURATION_MS, sentByMe = false)

        capture("voice-bubble-playing")
    }

    @Test
    fun recordButtonInAudioMode() {
        compose.setContent { Framed { RecordButton(rememberRecordController(hub) {}) } }

        capture("record-button-audio")
    }

    private companion object {
        const val SUBJECT = "screenshot-subject"
        const val DURATION_MS = 12_000L
        const val WAIT_MS = 5_000L

        val VOICE = AttachmentRef(
            blobId = ByteArray(32) { 11 },
            key = ByteArray(32) { 22 },
            nonce = ByteArray(12) { 33 },
            mime = "audio/mp4",
            size = 4_096u,
            name = "voice.m4a",
            kind = KIND_VOICE,
            durationMs = DURATION_MS.toUInt(),
            waveform = ByteArray(32) { i -> if (i % 3 == 0) -1 else 40 },
        )
    }
}
