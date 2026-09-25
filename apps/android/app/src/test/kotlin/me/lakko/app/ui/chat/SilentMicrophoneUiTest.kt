// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.Manifest
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performTouchInput
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.InlineKind
import me.lakko.app.media.MediaAllowlist
import me.lakko.app.media.RecState
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.shadows.ShadowLooper
import java.util.concurrent.TimeUnit

/**
 * `docs/ux/media-messages.md` §2 "Hearing nothing" — a take that hears nothing
 * says so WHILE IT IS BEING MADE.
 *
 * Why this exists at all: silence was invisible from inside the app. A chain of
 * voice messages recorded on an emulator whose microphone toggle was off
 * arrived as digital silence, and nothing — not the bar, not the waveform, not
 * the sent bubble — said the microphone had given nothing; the cause was found
 * by measuring the file with an external tool. Every client already samples the
 * level for the live meter, so the signal the warning needs was one each of
 * them was holding and none of them read.
 *
 * Robolectric IS that emulator, which is what makes this testable at all: its
 * `MediaRecorder` reports `getMaxAmplitude` as 0 for the whole take. So this
 * drives the real [RecordButton], the real [RecordController] and the real
 * [RecordingBar] against a recorder that gives nothing — the exact device the
 * rule was written for, with no fake in the path.
 *
 * `MicrophoneSilenceTest` pins the fold behind the decision as a pure
 * function. What only a test at this level can say is whether the BAR reads it:
 * a perfectly correct rule wired to nothing leaves every pure test green.
 *
 * **Both clocks have to be advanced, and neither one alone is enough.** The
 * take's elapsed time is `SystemClock.elapsedRealtime`, which only moves when
 * the Robolectric looper is idled forward; the controller's 100 ms tick is a
 * coroutine `delay` on the UI dispatcher, which only fires when the compose
 * main clock advances. Advancing the looper alone froze the take at 0 ms with
 * the tick never running; advancing the compose clock alone ran the tick
 * against a frozen clock, so the take never aged. That is why [advance] does
 * both, and why nothing here sleeps: the whole two seconds pass in virtual
 * time.
 *
 * The spec's strings are written out below rather than imported from
 * [RecordCopy], deliberately: a test that reads the app's own constant agrees
 * with the app by construction and would not notice the copy drifting away
 * from the spec.
 */
@RunWith(RobolectricTestRunner::class)
class SilentMicrophoneUiTest {

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private lateinit var scope: CoroutineScope
    private lateinit var hub: ChatMediaHub
    private var controller: RecordController? = null

    private val allowlist: MediaAllowlist = { mime ->
        if (mime.startsWith("audio/")) InlineKind.AUDIO else null
    }

    @Before
    fun setUp() {
        scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        hub = ChatMediaHub(compose.activity, scope, allowlist)
        // Granted, so a hold records rather than stopping at the prompt.
        shadowOf(compose.activity.application)
            .grantPermissions(Manifest.permission.RECORD_AUDIO)
    }

    @After
    fun tearDown() {
        hub.dispose()
        scope.cancel()
    }

    @Composable
    private fun ComposerUnderTest() {
        MaterialTheme {
            Surface {
                val c = rememberRecordController(hub) { }
                controller = c
                Column {
                    RecordingBar(c)
                    Box(Modifier.testTag(BUTTON)) { RecordButton(c) }
                }
            }
        }
    }

    private fun controller(): RecordController = requireNotNull(controller) {
        "the composer never composed, so there is no controller to read"
    }


    /**
     * Step until the TAKE's own clock reaches [ms], rather than stepping a fixed
     * amount and trusting it to be enough.
     *
     * The rule is about elapsed time inside the take, and the take's clock does
     * not start when this helper does: the controller's tick loop is launched a
     * coroutine later, so the first level sample lands some ticks in, and under
     * a loaded full-suite run it lands later still. A fixed step count plus a
     * slack constant therefore measured the HOST, not the rule, and failed about
     * one full run in two while passing alone every time. Driving to the take's
     * own reading removes the host from the question.
     */
    private fun advanceTakeTo(ms: Long) {
        // Bounded so a take that never starts fails as a test rather than hanging.
        val budget = ms * 4 + SILENCE_MS
        var spent = 0L
        while (controller().elapsedMs < ms && spent < budget) {
            ShadowLooper.idleMainLooper(TICK_MS, TimeUnit.MILLISECONDS)
            compose.mainClock.advanceTimeBy(TICK_MS)
            compose.waitForIdle()
            spent += TICK_MS
        }
        assert(controller().elapsedMs >= ms) {
            "the take's clock reached only ${controller().elapsedMs}ms of $ms within $budget ms of stepping"
        }
    }

    private fun startHolding() {
        compose.setContent { ComposerUnderTest() }
        compose.onNodeWithTag(BUTTON).performTouchInput { down(center) }
        compose.waitUntil(WAIT_MS) { controller().state is RecState.Recording }
    }

    @Test
    fun aMicrophoneThatGivesNothingSaysSoDuringTheTake() {
        startHolding()

        // Not yet: the run of silence has to LAST two seconds. A bar that
        // accused the microphone on its first zero sample would accuse every
        // microphone, since one that is merely slow to open reads zero for its
        // first tick or two.
        advanceTakeTo(SILENCE_MS / 2)
        compose.onNodeWithText(WARNING).assertDoesNotExist()
        compose.onNodeWithText(SLIDE_TO_CANCEL, substring = true).assertIsDisplayed()

        // Past the window, with the take still running — the whole point is
        // that the warning arrives in time to be acted on rather than after the
        // message has already been sent.
        advanceTakeTo(SILENCE_MS + TICK_SLACK_MS)
        assert(controller().state is RecState.Recording) {
            "the take should still be running when the warning appears"
        }
        compose.onNodeWithText(WARNING).assertIsDisplayed()
        // It REPLACES the hint rather than crowding in beside it (§2 states).
        compose.onNodeWithText(SLIDE_TO_CANCEL, substring = true).assertDoesNotExist()

        compose.onNodeWithTag(BUTTON).performTouchInput { up() }
        compose.waitForIdle()
    }

    /**
     * §2 — "clears the moment any level is measured". The warning is a warning
     * about the present, not a verdict on the take: a microphone that was
     * muted and is then unmuted, or a person who was simply not speaking yet,
     * must not carry an accusation through the rest of the recording.
     */
    @Test
    fun theWarningClearsAsSoonAsSomethingIsHeard() {
        startHolding()
        advanceTakeTo(SILENCE_MS + TICK_SLACK_MS)
        compose.onNodeWithText(WARNING).assertIsDisplayed()

        // Robolectric's recorder cannot be made to hear anything, so the sound
        // arrives through the same call the tick makes — one level sample, the
        // narrowest possible fake, with the fold, the state and the bar all
        // still the production ones.
        controller().onLevelSample(0.4, controller().elapsedMs)
        compose.waitForIdle()

        compose.onNodeWithText(WARNING).assertDoesNotExist()
        compose.onNodeWithText(SLIDE_TO_CANCEL, substring = true).assertIsDisplayed()

        compose.onNodeWithTag(BUTTON).performTouchInput { up() }
        compose.waitForIdle()
    }

    private companion object {
        const val BUTTON = "record-button"
        const val WARNING = "No sound from the microphone"
        const val SLIDE_TO_CANCEL = "Slide to cancel"

        /** The spec's two seconds, written out rather than imported. */
        const val SILENCE_MS = 2_000L

        /** `MediaComposer.TICK_MS` — the controller's level-sampling cadence. */
        const val TICK_MS = 100L

        /** One tick, plus room for the next. */
        const val TICK_SLACK_MS = 300L
        const val WAIT_MS = 10_000L
    }
}
