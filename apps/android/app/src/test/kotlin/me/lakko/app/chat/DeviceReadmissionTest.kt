// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.chat.DeviceReadmission.AccountDevices
import me.lakko.app.chat.DeviceReadmission.DirectoryDevice
import me.lakko.app.chat.DeviceReadmission.RosterLeaf
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0050 section 2.4 — mirror of `apps/web/src/chat/device-readmission.test.ts`. */
class DeviceReadmissionTest {

    private fun kp(tag: String) = DirectoryDevice(tag, "kp:$tag".toByteArray())

    @Test
    fun `admits a listed device that holds no leaf, for any account in the chat`() {
        val plan = DeviceReadmission.plan(
            roster = listOf(RosterLeaf("me-1", "me"), RosterLeaf("bob-1", "bob")),
            ownPersonKey = "me",
            accounts = listOf(
                AccountDevices("me", listOf(kp("me-1"), kp("me-2"))),
                AccountDevices("bob", listOf(kp("bob-1"), kp("bob-2"))),
            ),
            removed = emptySet(),
        )
        assertEquals(listOf("me-2", "bob-2"), plan.admit.map { it.deviceKey })
        assertEquals(emptyList<String>(), plan.retire)
    }

    @Test
    fun `never re-admits a device this client saw removed from the chat`() {
        val plan = DeviceReadmission.plan(
            roster = listOf(RosterLeaf("bob-1", "bob")),
            ownPersonKey = "me",
            accounts = listOf(AccountDevices("bob", listOf(kp("bob-1"), kp("bob-old")))),
            removed = setOf("bob-old"),
        )
        assertTrue(plan.admit.isEmpty())
    }

    @Test
    fun `retires an own leaf whose directory row is gone, and nobody else's`() {
        val plan = DeviceReadmission.plan(
            roster = listOf(
                RosterLeaf("me-1", "me"),
                RosterLeaf("me-forgotten", "me"),
                RosterLeaf("bob-gone", "bob"),
            ),
            ownPersonKey = "me",
            accounts = listOf(
                AccountDevices("me", listOf(kp("me-1"))),
                AccountDevices("bob", emptyList()),
            ),
            removed = emptySet(),
        )
        assertEquals(listOf("me-forgotten"), plan.retire)
        assertTrue(plan.admit.isEmpty())
    }

    @Test
    fun `decides nothing for an account whose directory fetch failed`() {
        val plan = DeviceReadmission.plan(
            roster = listOf(RosterLeaf("me-1", "me"), RosterLeaf("me-2", "me")),
            ownPersonKey = "me",
            accounts = listOf(AccountDevices("me", null)),
            removed = emptySet(),
        )
        assertTrue(plan.admit.isEmpty())
        assertTrue(plan.retire.isEmpty())
    }

    @Test
    fun `proposes each device once even when two accounts list it`() {
        val plan = DeviceReadmission.plan(
            roster = emptyList(),
            ownPersonKey = "me",
            accounts = listOf(
                AccountDevices("a", listOf(kp("x"))),
                AccountDevices("b", listOf(kp("x"))),
            ),
            removed = emptySet(),
        )
        assertEquals(1, plan.admit.size)
    }

    @Test
    fun `departed records nothing on a torn read and the leaves that left otherwise`() {
        assertTrue(DeviceReadmission.departed(null, listOf("a")).isEmpty())
        assertTrue(DeviceReadmission.departed(listOf("a"), null).isEmpty())
        assertEquals(listOf("b"), DeviceReadmission.departed(listOf("a", "b"), listOf("a", "c")))
    }

    @Test
    fun `the throttle admits one check per interval per chat`() {
        DeviceReadmission.clearSlots()
        assertTrue(DeviceReadmission.takeSlot("g", 1_000L))
        assertFalse(DeviceReadmission.takeSlot("g", 1_000L + DeviceReadmission.INTERVAL_MS - 1))
        assertTrue(DeviceReadmission.takeSlot("other", 1_000L))
        assertTrue(DeviceReadmission.takeSlot("g", 1_000L + DeviceReadmission.INTERVAL_MS))
        DeviceReadmission.clearSlots()
    }
}
