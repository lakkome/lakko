// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The hold-to-record rules of UX spec §2 / §3 / §6, pinned on the pure reducer.
 * Each case names the spec line it enforces. The web client's
 * `recording-machine.test.ts` pins the same rules; a divergence between the
 * two would be a cross-client behaviour difference.
 */
class RecordingMachineTest {

    private fun press(
        at: Long = 0,
        permission: PermissionHint = PermissionHint.GRANTED,
        pointer: PointerKind = PointerKind.TOUCH,
        kind: CaptureKind = CaptureKind.VOICE,
    ) = RecEvent.Press(kind, pointer, x = 100f, y = 100f, at = at, permission = permission)

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

    @Test
    fun holdStartsRecordingImmediatelyWhenGranted() {
        val (state, effects) = run(press())
        assertTrue(state is RecState.Recording)
        assertEquals(listOf(RecEffect.StartCapture(CaptureKind.VOICE)), effects)
    }

    @Test
    fun releaseAfterMinimumSends() {
        val (state, effects) = run(press(at = 0), RecEvent.Release(at = MIN_TAKE_MS))
        assertEquals(RecState.Idle, state)
        assertEquals(RecEffect.Finish(FinishDisposition.SEND), effects.last())
    }

    @Test
    fun touchTapShorterThanMinimumDiscardsHintsAndTogglesMode() {
        val (state, effects) = run(press(at = 0), RecEvent.Release(at = MIN_TAKE_MS - 1))
        assertEquals(RecState.Idle, state)
        assertEquals(
            listOf(
                RecEffect.StartCapture(CaptureKind.VOICE),
                RecEffect.Finish(FinishDisposition.DISCARD),
                RecEffect.Hint(RecHint.HOLD_TO_RECORD),
                RecEffect.ToggleMode,
            ),
            effects,
        )
    }

    @Test
    fun mouseClickBecomesALockedRecording() {
        val (state, effects) = run(press(at = 0, pointer = PointerKind.MOUSE), RecEvent.Release(at = 100))
        assertEquals(RecState.Locked(CaptureKind.VOICE, 0), state)
        assertTrue(effects.none { it is RecEffect.Finish })
    }

    @Test
    fun slideLeftPastThresholdCancels() {
        val (state, effects) = run(press(), RecEvent.Move(x = 100f - CANCEL_SLIDE_PX, y = 100f))
        assertEquals(RecState.Idle, state)
        assertTrue(RecEffect.Finish(FinishDisposition.DISCARD) in effects)
        assertTrue(RecEffect.Hint(RecHint.CANCELLED) in effects)
    }

    @Test
    fun slideLeftShortOfThresholdKeepsRecording() {
        val (state, _) = run(press(), RecEvent.Move(x = 100f - CANCEL_SLIDE_PX + 1f, y = 100f))
        assertTrue(state is RecState.Recording)
        assertEquals(-CANCEL_SLIDE_PX + 1f, (state as RecState.Recording).dx)
    }

    @Test
    fun slideUpLocksAndReleaseNoLongerSends() {
        val (state, effects) = run(
            press(at = 0),
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
        val rec = run(press(at = 0)).first
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
            press(at = 0, permission = PermissionHint.PROMPT),
            RecEvent.PermissionResult(granted = true, at = 50),
        )
        assertEquals(RecState.Idle, state)
        assertTrue(effects.none { it is RecEffect.StartCapture })
        assertEquals(RecEffect.RequestPermission(CaptureKind.VOICE), effects.first())
    }

    @Test
    fun unknownPermissionThatResolvesQuicklyWhileHeldRecords() {
        val (state, effects) = run(
            press(at = 0, permission = PermissionHint.UNKNOWN),
            RecEvent.PermissionResult(granted = true, at = PROMPT_INFERENCE_MS),
        )
        assertTrue(state is RecState.Recording)
        assertTrue(RecEffect.StartCapture(CaptureKind.VOICE) in effects)
    }

    @Test
    fun deniedPermissionShowsTheCard() {
        assertEquals(
            listOf(RecEffect.ShowDenied(CaptureKind.VIDEO_NOTE)),
            reduce(RecState.Idle, press(permission = PermissionHint.DENIED, kind = CaptureKind.VIDEO_NOTE)).effects,
        )
        val requesting = run(press(permission = PermissionHint.PROMPT)).first
        assertEquals(
            listOf(RecEffect.ShowDenied(CaptureKind.VOICE)),
            reduce(requesting, RecEvent.PermissionResult(granted = false, at = 10)).effects,
        )
    }

    @Test
    fun revokedWhileRecordingDiscardsAndShowsTheCard() {
        val rec = run(press()).first
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
        val requesting = run(press(permission = PermissionHint.UNKNOWN)).first
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
}
