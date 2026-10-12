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
            ownDeviceRoster = setOf("me-1", "me-2"),
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
            ownDeviceRoster = emptySet(),
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
            ownDeviceRoster = setOf("me-1"),
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
            ownDeviceRoster = setOf("me-1", "me-2"),
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
            ownDeviceRoster = null,
        )
        assertEquals(1, plan.admit.size)
    }

    // ---- ADR-0066 section 2.3: the DeviceGroup filter ------------------------

    @Test
    fun `an own device is admitted only when the account's DeviceGroup roster lists it`() {
        val plan = DeviceReadmission.plan(
            roster = listOf(RosterLeaf("me-1", "me"), RosterLeaf("bob-1", "bob")),
            ownPersonKey = "me",
            accounts = listOf(
                // me-removed published itself again after it was removed.
                AccountDevices("me", listOf(kp("me-1"), kp("me-2"), kp("me-removed"))),
                AccountDevices("bob", listOf(kp("bob-1"), kp("bob-2"))),
            ),
            removed = emptySet(),
            ownDeviceRoster = setOf("me-1", "me-2"),
        )
        // The removed device is not put back; a contact's device is not
        // filtered by OUR device list.
        assertEquals(listOf("me-2", "bob-2"), plan.admit.map { it.deviceKey })
    }

    @Test
    fun `an unreadable DeviceGroup roster admits no own device, and still admits a contact's`() {
        val plan = DeviceReadmission.plan(
            roster = listOf(RosterLeaf("me-1", "me"), RosterLeaf("bob-1", "bob")),
            ownPersonKey = "me",
            accounts = listOf(
                AccountDevices("me", listOf(kp("me-1"), kp("me-2"))),
                AccountDevices("bob", listOf(kp("bob-1"), kp("bob-2"))),
            ),
            removed = emptySet(),
            ownDeviceRoster = null,
        )
        assertEquals(listOf("bob-2"), plan.admit.map { it.deviceKey })
    }

    @Test
    fun `the removed memory still applies to an own device the roster lists`() {
        val plan = DeviceReadmission.plan(
            roster = listOf(RosterLeaf("me-1", "me")),
            ownPersonKey = "me",
            accounts = listOf(AccountDevices("me", listOf(kp("me-1"), kp("me-2")))),
            removed = setOf("me-2"),
            ownDeviceRoster = setOf("me-1", "me-2"),
        )
        assertTrue(plan.admit.isEmpty())
    }

    @Test
    fun `the filter does not change which own leaves are retired`() {
        val plan = DeviceReadmission.plan(
            roster = listOf(RosterLeaf("me-1", "me"), RosterLeaf("me-forgotten", "me")),
            ownPersonKey = "me",
            accounts = listOf(AccountDevices("me", listOf(kp("me-1")))),
            removed = emptySet(),
            ownDeviceRoster = null,
        )
        assertEquals(listOf("me-forgotten"), plan.retire)
    }

    @Test
    fun `the pairing fan-out obeys the same rule`() {
        assertTrue(DeviceReadmission.ownDeviceAdmissible("me-2", setOf("me-1", "me-2")))
        assertFalse(DeviceReadmission.ownDeviceAdmissible("me-removed", setOf("me-1", "me-2")))
        assertFalse(DeviceReadmission.ownDeviceAdmissible("me-2", null))
        assertFalse(DeviceReadmission.ownDeviceAdmissible("me-2", emptySet()))
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

    @Test
    fun `an own device whose KeyPackage names another account key is not admitted and not retired`() {
        // ADR-0066: a device paired after a key rotation published under the
        // current key before the state bundle reached it.
        val stale = DirectoryDevice("me-2", "kp:me-2".toByteArray(), admissible = false)
        val plan = DeviceReadmission.plan(
            roster = listOf(RosterLeaf("me-1", "me"), RosterLeaf("me-3", "me")),
            ownPersonKey = "me",
            accounts = listOf(AccountDevices("me", listOf(kp("me-1"), stale, kp("me-3")))),
            removed = emptySet(),
            ownDeviceRoster = setOf("me-1", "me-2", "me-3"),
        )
        assertTrue(plan.admit.isEmpty())
        // Its row is still listed, so nothing of the account is retired.
        assertTrue(plan.retire.isEmpty())
        // Once it re-published under the conversation key it is admitted.
        val again = DeviceReadmission.plan(
            roster = listOf(RosterLeaf("me-1", "me")),
            ownPersonKey = "me",
            accounts = listOf(AccountDevices("me", listOf(kp("me-1"), kp("me-2")))),
            removed = emptySet(),
            ownDeviceRoster = setOf("me-1", "me-2"),
        )
        assertEquals(listOf("me-2"), again.admit.map { it.deviceKey })
    }

    @Test
    fun `an own KeyPackage is usable only when bound to the conversation key`() {
        val conversation = ByteArray(32) { 1 }
        val current = ByteArray(32) { 2 }
        assertTrue(DeviceReadmission.ownKeyPackageUsable(conversation.copyOf(), conversation))
        assertTrue(!DeviceReadmission.ownKeyPackageUsable(current, conversation))
        // An unbound KeyPackage and an unknown conversation key change nothing.
        assertTrue(DeviceReadmission.ownKeyPackageUsable(null, conversation))
        assertTrue(DeviceReadmission.ownKeyPackageUsable(current, null))
    }
}
