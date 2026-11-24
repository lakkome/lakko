// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTouchInput
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import me.lakko.app.media.CaptureKind
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.InlineKind
import me.lakko.app.media.MediaAllowlist
import me.lakko.app.media.RecState
import me.lakko.app.media.RecordedTake
import me.lakko.app.media.TAP_MAX_MS
import me.lakko.app.media.VideoNoteAvailability
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf

/**
 * ADR-0047 / `docs/ux/media-messages.md` §3.1 — ONE record button: a tap
 * switches between mic and camera and records NOTHING, a hold records.
 *
 * `513158df` rewrote this because the mode used to be decided by the
 * recording's OUTCOME — every press opened the microphone, and a press too
 * short to keep was thrown away and read as "they meant the other mode". On
 * Android that showed as the screen flashing on every tap. The judgement moved
 * onto the gesture, where `RecordingMachineTest` pins it as a pure reducer.
 *
 * What the reducer cannot say is whether the BUTTON asks it the right question:
 * a `pointerInput` that reported a release as a hold, or never fed the press in
 * at all, would leave every reducer test green. That is what these drive — the
 * real [RecordButton] over a real [RecordController], with real touch input.
 *
 * One limit, stated rather than worked around: camera mode is not a state a JVM
 * test can HOLD. CameraX cannot bind a camera on a JVM device, so nothing here
 * can assert what a take in camera mode looks like, and the camera-mode
 * baseline that `MediaBaselineScreenshotTest` would otherwise hold does not
 * exist for this reason. What the controller asks CameraX up front — can this
 * device record a video note at all (§3 states) — is a seam, so the tests
 * below answer it themselves rather than depend on what Robolectric reports.
 */
@RunWith(RobolectricTestRunner::class)
class RecordButtonUiTest {

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private lateinit var scope: CoroutineScope
    private lateinit var hub: ChatMediaHub
    private var controller: RecordController? = null
    private val sent = mutableListOf<RecordedTake>()

    private val allowlist: MediaAllowlist = { mime ->
        if (mime.startsWith("audio/")) InlineKind.AUDIO else null
    }

    @Before
    fun setUp() {
        scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        hub = ChatMediaHub(compose.activity, scope, allowlist)
        // The mode is remembered per device and outlives a test: a tap that
        // left the previous test in camera mode must not start this one there.
        hub.prefs.recordMode = CaptureKind.VOICE
        // Granted, so a hold reaches `PermissionHint.GRANTED` and records rather
        // than stopping at the permission prompt. A denied device is the
        // reducer's business and is covered there.
        shadowOf(compose.activity.application)
            .grantPermissions(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA)
        // The mode switch refuses to enter camera mode on a device that reports
        // no camera, which a bare Robolectric device does.
        shadowOf(compose.activity.packageManager)
            .setSystemFeature(PackageManager.FEATURE_CAMERA_ANY, true)
    }

    @After
    fun tearDown() {
        hub.dispose()
        scope.cancel()
    }

    @Composable
    private fun ButtonUnderTest(
        probe: suspend () -> VideoNoteAvailability? = { VideoNoteAvailability.AVAILABLE },
    ) {
        MaterialTheme {
            Surface {
                val c = rememberRecordController(hub, probe) { take -> sent += take }
                controller = c
                Box(Modifier.testTag(BUTTON)) { RecordButton(c) }
            }
        }
    }

    private fun controller(): RecordController = requireNotNull(controller) {
        "the button never composed, so there is no controller to read"
    }

    @Test
    fun aTapRecordsNothing() {
        compose.setContent { ButtonUnderTest() }
        assertEquals("the button should start in mic mode", CaptureKind.VOICE, controller().mode)

        compose.onNodeWithTag(BUTTON).performClick()

        // The defect, and the half of §3.1 that matters most: a tap records
        // nothing. The reducer leaves `Idle` with no capture effect, so there is
        // no take, no preview, and nothing was handed to the send callback.
        assertSame(RecState.Idle, controller().state)
        assertNull("a tap produced a take to preview", controller().preview)
        assertEquals("a tap sent a recording", emptyList<RecordedTake>(), sent)

        // And it stays that way past the arming timer. The version this replaced
        // opened the microphone on the press and only decided afterwards, so
        // "nothing has happened yet" was never the question — the question is
        // whether anything happens NEXT. The timer is a coroutine on real time
        // rather than on the compose clock, so this waits in real time.
        Thread.sleep(TAP_MAX_MS * 3)
        compose.waitForIdle()
        assertSame("the tap became a recording after the fact", RecState.Idle, controller().state)
        assertEquals(emptyList<RecordedTake>(), sent)
    }

    @Test
    fun aTapSwitchesTheMode() {
        compose.setContent { ButtonUnderTest() }

        compose.onNodeWithTag(BUTTON).performClick()

        assertEquals("the tap did not switch the mode", CaptureKind.VIDEO_NOTE, controller().mode)
    }

    @Test
    fun aHoldStartsRecordingAndDoesNotSwitchMode() {
        compose.setContent { ButtonUnderTest() }

        // A finger down, held. The arming timer is a coroutine on real time, so
        // the hold is real rather than a jump of the compose clock.
        compose.onNodeWithTag(BUTTON).performTouchInput { down(center) }
        compose.waitUntil(WAIT_MS) { controller().state is RecState.Recording }

        // Recording, in mic mode, and the mode was NOT switched — a hold that
        // also toggled would flip the button under the take it just started.
        assertEquals(CaptureKind.VOICE, controller().mode)

        compose.onNodeWithTag(BUTTON).performTouchInput { up() }
        compose.waitForIdle()
    }

    /**
     * §3 states "Video format unsupported on this device" — the emulator
     * finding: the tap switched to camera mode, and the hold then did nothing
     * visible because the camera had no H.264 + AAC profile. Decided up front
     * now: the tap stays on the mic and the composer's notice says why.
     */
    @Test
    fun aTapOnADeviceThatCannotRecordVideoNotesStaysOnTheMicAndSaysWhy() {
        compose.setContent { ButtonUnderTest { VideoNoteAvailability.NO_COMPATIBLE_FORMAT } }
        compose.waitForIdle()

        compose.onNodeWithTag(BUTTON).performClick()
        compose.waitForIdle()

        assertEquals("camera mode was offered", CaptureKind.VOICE, controller().mode)
        assertEquals(RecordCopy.NO_COMPATIBLE_FORMAT, controller().notice)
        assertSame(RecState.Idle, controller().state)
        assertEquals(emptyList<RecordedTake>(), sent)
    }

    @Test
    fun aTapOnADeviceWithNoCameraSaysNoCameraFound() {
        compose.setContent { ButtonUnderTest { VideoNoteAvailability.NO_CAMERA } }
        compose.waitForIdle()

        compose.onNodeWithTag(BUTTON).performClick()
        compose.waitForIdle()

        assertEquals(CaptureKind.VOICE, controller().mode)
        assertEquals(RecordCopy.NO_CAMERA, controller().notice)
    }

    /**
     * A tap that came before the up-front answer switched optimistically; a
     * "no" arriving afterwards takes the switch back AND says so, never
     * silently.
     */
    @Test
    fun aSwitchMadeBeforeTheAnswerIsTakenBackAndSaid() {
        val answer = CompletableDeferred<VideoNoteAvailability?>()
        compose.setContent { ButtonUnderTest { answer.await() } }
        compose.waitForIdle()

        compose.onNodeWithTag(BUTTON).performClick()
        compose.waitForIdle()
        assertEquals(CaptureKind.VIDEO_NOTE, controller().mode)

        answer.complete(VideoNoteAvailability.NO_COMPATIBLE_FORMAT)
        compose.waitUntil(WAIT_MS) { controller().mode == CaptureKind.VOICE }
        assertEquals(RecordCopy.NO_COMPATIBLE_FORMAT, controller().notice)
    }

    private companion object {
        const val BUTTON = "record-button"
        const val WAIT_MS = 5_000L
    }
}
