// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The hold-to-record rules of UX spec §2 / §3 / §3.1 / §6, pinned on the pure
 * reducer.
 * Each case names the spec line it enforces. The web client's
 * `recording-machine.test.ts` pins the same rules; a divergence between the
 * two would be a cross-client behaviour difference.
 */
class RecordingMachineTest {
    init {
        EnglishForTests.install()
    }


    private fun press(
        at: Long = 0,
        pointer: PointerKind = PointerKind.TOUCH,
        kind: CaptureKind = CaptureKind.VOICE,
    ) = RecEvent.Press(kind, pointer, x = 100f, y = 100f, at = at)

    /**
     * §3.1 — a hold is a press that outlived [TAP_MAX_MS]. Every test below
     * that means "the person held the button" goes through this, because a
     * press alone is a tap and records nothing.
     *
     * The take is timed from the arming, so `at` is when the recording starts.
     */
    private fun hold(
        at: Long = 0,
        permission: PermissionHint = PermissionHint.GRANTED,
        pointer: PointerKind = PointerKind.TOUCH,
        kind: CaptureKind = CaptureKind.VOICE,
    ) = arrayOf<RecEvent>(
        press(at = at - TAP_MAX_MS, pointer = pointer, kind = kind),
        RecEvent.HoldElapsed(at = at, permission = permission),
    )

    private fun run(vararg events: RecEvent, from: RecState = RecState.Idle): Pair<RecState, List<RecEffect>> {
        var state = from
        val effects = ArrayList<RecEffect>()
        for (e in events) {
            val step = reduce(state, e)
            state = step.state
            effects += step.effects
        }
        return state to effects
    }

    /**
     * §3.1 — "a tap therefore never records". THE regression test for the
     * screen flash: a press that is let go before [TAP_MAX_MS] must never reach
     * [RecState.Recording], because that state is what swaps the composer for
     * the recording bar and mounts the full-screen round camera preview.
     */
    @Test
    fun aTapSwitchesTheModeAndRecordsNothing() {
        val (pressed, pressEffects) = run(press(at = 0))
        assertTrue("a press alone must not record", pressed is RecState.Pressing)
        assertEquals(emptyList<RecEffect>(), pressEffects)

        val (state, effects) = run(press(at = 0), RecEvent.Release(at = TAP_MAX_MS - 1))
        assertEquals(RecState.Idle, state)
        // Exactly the mode switch: nothing was started, so there is nothing to
        // discard and no `Hold to record` hint to show.
        assertEquals(listOf(RecEffect.ToggleMode), effects)
    }

    /**
     * The other half of the flash: the composer must look identical in every
     * state a tap can pass through, or the swap itself is the blink even when
     * nothing was recorded.
     */
    @Test
    fun theComposerIsUntouchedInEveryStateATapPassesThrough() {
        assertFalse(showsRecordingUi(RecState.Idle))
        assertFalse(showsRecordingUi(run(press(at = 0)).first))
        assertFalse(
            showsRecordingUi(run(*hold(permission = PermissionHint.PROMPT)).first),
        )
        // And it is shown for every state that has a take behind it.
        assertTrue(showsRecordingUi(run(*hold()).first))
        assertTrue(showsRecordingUi(RecState.Locked(CaptureKind.VOICE, 0)))
        assertTrue(showsRecordingUi(RecState.Finalizing(CaptureKind.VOICE)))
        assertTrue(showsRecordingUi(RecState.Preview(CaptureKind.VOICE, 1_000)))
    }

    /** §3.1 — nor may a tap ask for a permission it is not going to use. */
    @Test
    fun aTapNeitherPromptsNorShowsTheDeniedCard() {
        for (kind in CaptureKind.entries) {
            val (state, effects) = run(press(at = 0, kind = kind), RecEvent.Release(at = 10))
            assertEquals(RecState.Idle, state)
            assertEquals("$kind", listOf(RecEffect.ToggleMode), effects)
        }
    }

    @Test
    fun holdStartsRecordingWhenGranted() {
        val (state, effects) = run(*hold())
        assertTrue(state is RecState.Recording)
        assertEquals(listOf(RecEffect.StartCapture(CaptureKind.VOICE)), effects)
        // The take is timed from the arming, not from the press.
        assertEquals(0L, (state as RecState.Recording).startedAt)
    }

    @Test
    fun releaseAfterMinimumSends() {
        val (state, effects) = run(*hold(at = 0), RecEvent.Release(at = MIN_TAKE_MS))
        assertEquals(RecState.Idle, state)
        assertEquals(RecEffect.Finish(FinishDisposition.SEND), effects.last())
    }

    /**
     * §3.1 — the accidental press: it armed, so the microphone WAS opened, and
     * that is exactly why it is discarded with the hint rather than read as the
     * mode switch.
     */
    @Test
    fun anArmedTakeUnderTheFloorIsDiscardedAndIsNotAModeSwitch() {
        val (state, effects) = run(*hold(at = 0), RecEvent.Release(at = MIN_TAKE_MS - 1))
        assertEquals(RecState.Idle, state)
        assertEquals(
            listOf(
                RecEffect.StartCapture(CaptureKind.VOICE),
                RecEffect.Finish(FinishDisposition.DISCARD),
                RecEffect.Hint(RecHint.HOLD_TO_RECORD),
            ),
            effects,
        )
        assertTrue("an opened microphone is not a mode switch", RecEffect.ToggleMode !in effects)
    }

    /** §3.1 — a click is a tap, so it switches the mode like any other. */
    @Test
    fun aMouseClickTooShortToArmSwitchesTheMode() {
        val (state, effects) = run(
            press(at = 0, pointer = PointerKind.MOUSE),
            RecEvent.Release(at = TAP_MAX_MS - 1),
        )
        assertEquals(RecState.Idle, state)
        assertEquals(listOf(RecEffect.ToggleMode), effects)
    }

    /**
     * §3.1 — "no press ever ends in a locked recording". A mouse takes exactly
     * the paths a thumb does, through both routes a press can reach a take by:
     * an armed take let go early, and a release that beat the permission grant.
     */
    @Test
    fun noPressEverEndsInALockedRecording() {
        for (pointer in PointerKind.entries) {
            val (early, earlyEffects) = run(
                *hold(at = 0, pointer = pointer),
                RecEvent.Release(at = MIN_TAKE_MS - 1),
            )
            assertEquals("$pointer let go early", RecState.Idle, early)
            assertTrue("$pointer", RecEffect.Hint(RecHint.HOLD_TO_RECORD) in earlyEffects)

            val (beatTheGrant, grantEffects) = run(
                *hold(at = 0, permission = PermissionHint.UNKNOWN, pointer = pointer),
                RecEvent.Release(at = 30),
                RecEvent.PermissionResult(granted = true, at = 60),
            )
            assertEquals("$pointer beat the grant", RecState.Idle, beatTheGrant)
            assertTrue("$pointer", grantEffects.none { it is RecEffect.StartCapture })
        }
    }

    /** Locking is still reached by holding and sliding up, on every pointer. */
    @Test
    fun everyPointerReachesLockByHoldingAndSlidingUp() {
        for (pointer in PointerKind.entries) {
            val (state, _) = run(
                *hold(at = 0, pointer = pointer),
                RecEvent.Move(x = 100f, y = 100f - LOCK_SLIDE_PX),
            )
            assertEquals("$pointer", RecState.Locked(CaptureKind.VOICE, 0), state)
        }
    }

    @Test
    fun slideLeftPastThresholdCancels() {
        val (state, effects) = run(*hold(), RecEvent.Move(x = 100f - CANCEL_SLIDE_PX, y = 100f))
        assertEquals(RecState.Idle, state)
        assertTrue(RecEffect.Finish(FinishDisposition.DISCARD) in effects)
        assertTrue(RecEffect.Hint(RecHint.CANCELLED) in effects)
    }

    @Test
    fun slideLeftShortOfThresholdKeepsRecording() {
        val (state, _) = run(*hold(), RecEvent.Move(x = 100f - CANCEL_SLIDE_PX + 1f, y = 100f))
        assertTrue(state is RecState.Recording)
        assertEquals(-CANCEL_SLIDE_PX + 1f, (state as RecState.Recording).dx)
    }

    /** §3.1 — a press still inside the slop is a press, and is still judged. */
    @Test
    fun aPressThatBarelyMovesIsStillBeingJudged() {
        val (state, effects) = run(
            press(at = 0),
            RecEvent.Move(x = 100f + TAP_SLOP_PX - 1f, y = 100f),
        )
        assertTrue(state is RecState.Pressing)
        assertEquals(emptyList<RecEffect>(), effects)
    }

    /**
     * §3.1 — "a press that travels past the slop is neither": a finger on its
     * way to the cancel target must not flip the mode when it is let go, and
     * nothing was opened, so there is no take to discard either.
     */
    @Test
    fun aPressThatTravelsPastTheSlopIsNeitherGesture() {
        val (state, effects) = run(
            press(at = 0),
            RecEvent.Move(x = 100f - CANCEL_SLIDE_PX, y = 100f),
            RecEvent.Release(at = TAP_MAX_MS - 1),
        )
        assertEquals(RecState.Idle, state)
        assertEquals(emptyList<RecEffect>(), effects)
    }

    /** And the arming timer must not revive a press the finger walked away from. */
    @Test
    fun aHoldElapsingAfterTheSlopStartsNothing() {
        val (state, effects) = run(
            press(at = 0),
            RecEvent.Move(x = 100f, y = 100f - TAP_SLOP_PX - 1f),
            RecEvent.HoldElapsed(at = TAP_MAX_MS, permission = PermissionHint.GRANTED),
        )
        assertEquals(RecState.Idle, state)
        assertEquals(emptyList<RecEffect>(), effects)
    }

    @Test
    fun slideUpLocksAndReleaseNoLongerSends() {
        val (state, effects) = run(
            *hold(at = 0),
            RecEvent.Move(x = 100f, y = 100f - LOCK_SLIDE_PX),
            RecEvent.Release(at = 5_000),
        )
        assertEquals(RecState.Locked(CaptureKind.VOICE, 0), state)
        assertTrue(effects.none { it is RecEffect.Finish })
    }

    @Test
    fun lockedStopGoesToPreviewAndDeleteDiscards() {
        val locked = RecState.Locked(CaptureKind.VOICE, startedAt = 0)
        val (stopped, stopEffects) = run(RecEvent.Stop(at = 2_000), from = locked)
        assertEquals(RecState.Finalizing(CaptureKind.VOICE), stopped)
        assertEquals(listOf(RecEffect.Finish(FinishDisposition.PREVIEW)), stopEffects)
        val (deleted, deleteEffects) = run(RecEvent.Delete, from = locked)
        assertEquals(RecState.Idle, deleted)
        assertEquals(listOf(RecEffect.Finish(FinishDisposition.DISCARD)), deleteEffects)
    }

    @Test
    fun voiceCapStopsIntoPreviewAtFifteenMinutes() {
        val rec = run(*hold(at = 0)).first
        assertTrue(reduce(rec, RecEvent.Tick(at = VOICE_CAP_MS - 1)).state is RecState.Recording)
        val step = reduce(rec, RecEvent.Tick(at = VOICE_CAP_MS))
        assertEquals(RecState.Finalizing(CaptureKind.VOICE), step.state)
        assertEquals(listOf(RecEffect.Finish(FinishDisposition.PREVIEW)), step.effects)
    }

    @Test
    fun videoNoteCapIsSixtySeconds() {
        val locked = RecState.Locked(CaptureKind.VIDEO_NOTE, startedAt = 0)
        assertEquals(RecState.Finalizing(CaptureKind.VIDEO_NOTE), reduce(locked, RecEvent.Tick(VIDEO_NOTE_CAP_MS)).state)
        assertEquals(locked, reduce(locked, RecEvent.Tick(VIDEO_NOTE_CAP_MS - 1)).state)
    }

    @Test
    fun previewShorterThanMinimumIsDiscardedWithHint() {
        val step = reduce(RecState.Finalizing(CaptureKind.VOICE), RecEvent.PreviewReady(MIN_TAKE_MS - 1))
        assertEquals(RecState.Idle, step.state)
        assertEquals(listOf(RecEffect.DiscardPreview, RecEffect.Hint(RecHint.HOLD_TO_RECORD)), step.effects)
    }

    @Test
    fun previewSendAndDelete() {
        val preview = RecState.Preview(CaptureKind.VOICE, durationMs = 3_000)
        assertEquals(listOf(RecEffect.SendPreview), reduce(preview, RecEvent.Send).effects)
        assertEquals(listOf(RecEffect.DiscardPreview), reduce(preview, RecEvent.Delete).effects)
    }

    @Test
    fun theHoldThatRaisedAPromptDoesNotRecord() {
        val (state, effects) = run(
            *hold(at = 0, permission = PermissionHint.PROMPT),
            RecEvent.PermissionResult(granted = true, at = 50),
        )
        assertEquals(RecState.Idle, state)
        assertTrue(effects.none { it is RecEffect.StartCapture })
        assertEquals(RecEffect.RequestPermission(CaptureKind.VOICE), effects.first())
    }

    @Test
    fun unknownPermissionThatResolvesQuicklyWhileHeldRecords() {
        val (state, effects) = run(
            *hold(at = 0, permission = PermissionHint.UNKNOWN),
            RecEvent.PermissionResult(granted = true, at = PROMPT_INFERENCE_MS),
        )
        assertTrue(state is RecState.Recording)
        assertTrue(RecEffect.StartCapture(CaptureKind.VOICE) in effects)
    }

    /**
     * §3.1 — a press released before the grant arrived is the accidental press,
     * not the tap: it had already armed and opened the request, so it gets the
     * hint and NOT the mode switch.
     */
    @Test
    fun aReleaseBeforeTheGrantArrivesHintsAndIsNotAModeSwitch() {
        val (state, effects) = run(
            *hold(at = 0, permission = PermissionHint.UNKNOWN),
            RecEvent.Release(at = 30),
            RecEvent.PermissionResult(granted = true, at = 60),
        )
        assertEquals(RecState.Idle, state)
        assertTrue(effects.none { it is RecEffect.StartCapture })
        assertTrue(RecEffect.Hint(RecHint.HOLD_TO_RECORD) in effects)
        assertTrue(RecEffect.ToggleMode !in effects)
    }

    @Test
    fun deniedPermissionShowsTheCard() {
        assertEquals(
            listOf(RecEffect.ShowDenied(CaptureKind.VIDEO_NOTE)),
            run(*hold(permission = PermissionHint.DENIED, kind = CaptureKind.VIDEO_NOTE)).second,
        )
        val requesting = run(*hold(permission = PermissionHint.PROMPT)).first
        assertEquals(
            listOf(RecEffect.ShowDenied(CaptureKind.VOICE)),
            reduce(requesting, RecEvent.PermissionResult(granted = false, at = 10)).effects,
        )
    }

    @Test
    fun revokedWhileRecordingDiscardsAndShowsTheCard() {
        val rec = run(*hold()).first
        val step = reduce(rec, RecEvent.Revoked)
        assertEquals(RecState.Idle, step.state)
        assertEquals(
            listOf(RecEffect.Finish(FinishDisposition.DISCARD), RecEffect.ShowDenied(CaptureKind.VOICE)),
            step.effects,
        )
    }

    @Test
    fun accessibilityActivationStartsALockedRecording() {
        val step = reduce(RecState.Idle, RecEvent.Activate(CaptureKind.VOICE, at = 7, permission = PermissionHint.GRANTED))
        assertEquals(RecState.Locked(CaptureKind.VOICE, 7), step.state)
        assertEquals(listOf(RecEffect.StartCapture(CaptureKind.VOICE)), step.effects)
    }

    @Test
    fun cancelWhileRequestingIgnoresALateGrant() {
        val requesting = run(*hold(permission = PermissionHint.UNKNOWN)).first
        val idle = reduce(requesting, RecEvent.Cancel).state
        assertEquals(RecState.Idle, idle)
        assertEquals(RecStep(RecState.Idle, emptyList()), reduce(idle, RecEvent.PermissionResult(true, 10)))
    }

    @Test
    fun clockAndSpokenDuration() {
        assertEquals("0:07", formatClock(7_400))
        assertEquals("1:02", formatClock(62_000))
        assertEquals("15:00", formatClock(VOICE_CAP_MS))
        assertEquals("12 seconds", spokenDuration(12_000))
        assertEquals("1 minute 5 seconds", spokenDuration(65_000))
        assertEquals("2 minutes", spokenDuration(120_000))
    }

    @Test
    fun totalClockRoundsAnyPositiveDurationUp() {
        assertEquals("0:00", formatTotalClock(0))
        assertEquals("0:00", formatTotalClock(-250))
        assertEquals("0:01", formatTotalClock(1))
        assertEquals("0:01", formatTotalClock(500))
        assertEquals("0:01", formatTotalClock(999))
        assertEquals("0:01", formatTotalClock(1_000))
        assertEquals("0:02", formatTotalClock(1_001))
        assertEquals("1:00", formatTotalClock(59_001))
        assertEquals("15:00", formatTotalClock(VOICE_CAP_MS))
        // The live counter keeps rounding down.
        assertEquals("0:00", formatClock(999))
    }

    /**
     * **Every way out of a live capture hands the recorder back.**
     *
     * [RecEffect.Finish] is the ONLY effect that stops the microphone and lets
     * go of the camera — the composer performs nothing else that does — so a
     * transition that leaves [RecState.Recording] or [RecState.Locked] without
     * one leaves the hardware running with nobody owning it. On a video note
     * that is not a leak you can ignore: the camera stays bound, and the next
     * hold flashes the recording overlay and fails, which is exactly what the
     * emulator showed on 2026-09-24.
     *
     * Exhaustive over every event rather than case by case, so an arm added to
     * the reducer later cannot quietly skip the release.
     */
    @Test
    fun everyExitFromALiveCaptureReleasesTheRecorder() {
        val events: List<RecEvent> = listOf(
            press(at = 1_000),
            RecEvent.HoldElapsed(at = 1_000, permission = PermissionHint.GRANTED),
            RecEvent.HoldElapsed(at = 1_000, permission = PermissionHint.DENIED),
            RecEvent.Move(x = 100f, y = 100f),
            RecEvent.Move(x = 100f - CANCEL_SLIDE_PX, y = 100f),
            RecEvent.Move(x = 100f, y = 100f - LOCK_SLIDE_PX),
            RecEvent.Release(at = 1_000),
            RecEvent.Release(at = MIN_TAKE_MS - 1),
            RecEvent.Activate(CaptureKind.VOICE, at = 1_000, permission = PermissionHint.GRANTED),
            RecEvent.PermissionResult(granted = true, at = 1_000),
            RecEvent.PermissionResult(granted = false, at = 1_000),
            RecEvent.Tick(at = 1_000),
            RecEvent.Tick(at = VOICE_CAP_MS + 1),
            RecEvent.Stop(at = 1_000),
            RecEvent.Stop(at = MIN_TAKE_MS - 1),
            RecEvent.Delete,
            RecEvent.Cancel,
            RecEvent.Send,
            RecEvent.Revoked,
            RecEvent.CaptureFailed,
            RecEvent.PreviewReady(durationMs = 4_000),
            RecEvent.PreviewFailed,
        )
        for (kind in CaptureKind.entries) {
            val live = listOf(
                RecState.Recording(
                    kind = kind,
                    pointer = PointerKind.TOUCH,
                    startedAt = 0,
                    originX = 100f,
                    originY = 100f,
                    dx = 0f,
                    dy = 0f,
                ),
                RecState.Locked(kind, startedAt = 0),
            )
            for (state in live) {
                for (event in events) {
                    val step = reduce(state, event)
                    // Still capturing (a slide up to lock, a tick under the
                    // cap): the recorder is meant to keep running.
                    if (isCapturing(step.state)) continue
                    assertTrue(
                        "$state + $event went to ${step.state} without releasing the recorder",
                        step.effects.any { it is RecEffect.Finish },
                    )
                }
            }
        }
    }

    /** §2.8 — a release that landed while the recorder was still starting: the
     *  reducer judges the hold from the press and sends; the take is judged
     *  again once the recorder has stopped. */
    @Test
    fun releaseDuringSlowStartIsJudgedAgainAfterStop() {
        val (state, effects) = run(*hold(at = 0), RecEvent.Release(at = 3_500))
        assertEquals(RecState.Idle, state)
        assertEquals(RecEffect.Finish(FinishDisposition.SEND), effects.last())
        // Start finished just before the stop: nothing written.
        assertEquals(SentTakeOutcome.HOLD_TO_RECORD, sentTakeOutcome(null, captureStarted = true))
        // A little audio, still under the minimum.
        assertEquals(SentTakeOutcome.HOLD_TO_RECORD, sentTakeOutcome(MIN_TAKE_MS - 1, captureStarted = true))
        assertEquals(SentTakeOutcome.SEND, sentTakeOutcome(MIN_TAKE_MS, captureStarted = true))
        // The start itself failed.
        assertEquals(SentTakeOutcome.START_FAILED, sentTakeOutcome(null, captureStarted = false))
    }
}
