// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import kotlinx.coroutines.test.runTest
import me.lakko.app.devices.DeviceGroupSnapshotStore
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

/** A DeviceGroup send whose handle was behind the disk reopens and runs once
 *  more; it is not lost, and it is not retried for ever. */
class SnapshotChangedRetryTest {

    @Test
    fun `a send behind the disk reopens the handle and goes out`() = runTest {
        val steps = ArrayList<String>()
        val answer = retryOnceAfterSnapshotChanged(
            send = {
                steps += "send"
                throw DeviceGroupSnapshotStore.SnapshotChanged()
            },
            catchUp = { steps += "reopen" },
            again = {
                steps += "again"
                7
            },
        )
        assertEquals(7, answer)
        assertEquals(listOf("send", "reopen", "again"), steps)
    }

    @Test
    fun `a send that went out runs once`() = runTest {
        var reopened = false
        val answer = retryOnceAfterSnapshotChanged(send = { 1 }, catchUp = { reopened = true }, again = { 2 })
        assertEquals(1, answer)
        assertEquals(false, reopened)
    }

    @Test
    fun `a second refusal is the caller's to handle`() {
        assertThrows(DeviceGroupSnapshotStore.SnapshotChanged::class.java) {
            kotlinx.coroutines.runBlocking {
                retryOnceAfterSnapshotChanged<Unit>(
                    send = { throw DeviceGroupSnapshotStore.SnapshotChanged() },
                    catchUp = {},
                    again = { throw DeviceGroupSnapshotStore.SnapshotChanged() },
                )
            }
        }
    }
}
