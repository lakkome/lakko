// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import me.lakko.app.devices.DeviceRosterChanges
import me.lakko.app.devices.RosterChangeAfterPersist
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * `docs/ux/second-device.md` section 3, "The list follows the account": an
 * open Devices screen reads the list again whenever the roster changes,
 * without being left and reopened and without a timer.
 */
@RunWith(RobolectricTestRunner::class)
class DevicesRosterReloadTest {

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun `an open screen reloads on each change, and not on opening`() {
        var reloads = 0
        // A change from before the screen opened is what its first read sees.
        DeviceRosterChanges.note()
        compose.setContent { ReloadOnRosterChange { reloads += 1 } }
        compose.waitForIdle()
        assertEquals(0, reloads)

        DeviceRosterChanges.note()
        compose.waitForIdle()
        assertEquals(1, reloads)

        DeviceRosterChanges.note()
        compose.waitForIdle()
        assertEquals(2, reloads)
    }

    @Test
    fun `a received commit is announced once the write that holds it landed`() {
        var announced = 0
        val change = RosterChangeAfterPersist { announced += 1 }

        change.onCommitApplied()
        assertEquals("the list is read from disk: not before the write", 0, announced)

        change.onPersisted()
        assertEquals(1, announced)

        // A batch with no commit in it announces nothing.
        change.onPersisted()
        assertEquals(1, announced)
    }

    @Test
    fun `a write that failed leaves the change owed to the next one`() {
        var announced = 0
        val change = RosterChangeAfterPersist { announced += 1 }

        change.onCommitApplied()
        // The batch's persist threw, so onPersisted was not reached.
        change.onCommitApplied()
        change.onPersisted()
        assertEquals(1, announced)
    }
}
