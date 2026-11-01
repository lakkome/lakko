// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import androidx.test.core.app.ApplicationProvider
import java.util.concurrent.Executor
import me.lakko.app.calls.CallSoundController.SoundName
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * Preparing a looping cue waits on the media service, and the call screen starts
 * and stops those cues from the main thread: nothing of it may run on the caller.
 */
@RunWith(RobolectricTestRunner::class)
class CallSoundControllerTest {
    private val queued = mutableListOf<Runnable>()

    private fun controller() = CallSoundController(
        ApplicationProvider.getApplicationContext(),
        1.0f,
        Executor { queued += it },
    )

    @Test
    fun startingAndStoppingALoopOnlyQueuesWork() {
        val sounds = controller()
        sounds.startLoop(SoundName.RINGBACK_OUTGOING)
        sounds.stopLoop(SoundName.RINGBACK_OUTGOING)
        assertEquals(2, queued.size)
    }

    @Test
    fun aShortCueIsNeverALoop() {
        val sounds = controller()
        sounds.startLoop(SoundName.JOIN)
        assertTrue(queued.isEmpty())
    }
}
