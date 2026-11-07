// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import me.lakko.app.contacts.applyContactKeyVerdicts
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiContactKeyRowVerdict

/**
 * ADR-0060 section 1 (2026-11-06) — Remove contact rotates the one shared
 * `contact` key; the last retired versions stay registered, and the receiving
 * device decides each row by the key that admitted it.
 */
class ContactKeyRotationTest {

    private val contact = ReachSlot(ReachKind.CONTACT, "")

    private fun inputs(contactVersion: Int, registered: Set<Int> = emptySet()) = ReachInputs(
        versions = mapOf(contact to contactVersion),
        joinedGroups = emptySet(),
        visibleGroups = emptySet(),
        visibleChannels = emptySet(),
        contactsOfContacts = false,
        links = emptyMap(),
        replies = emptyMap(),
        registeredContactVersions = registered,
    )

    @Test
    fun `the window is the current version and the last eight retired`() {
        assertEquals(listOf(0), contactKeyVersionWindow(0))
        assertEquals(listOf(3, 2, 1, 0), contactKeyVersionWindow(3))
        assertEquals((12 downTo 4).toList(), contactKeyVersionWindow(12))
        assertEquals(RETIRED_CONTACT_VERSIONS_KEPT + 1, contactKeyVersionWindow(40).size)
    }

    @Test
    fun `a registered retired version in the window stays, with no expiry`() {
        val keys = desiredReachKeys(inputs(10, registered = (0..9).toSet()), 0L).filter { it.slot == contact }
        assertEquals((10 downTo 2).toList(), keys.map { it.version })
        assertEquals(setOf<Long?>(null), keys.map { it.expiresAtMs }.toSet())
    }

    @Test
    fun `a retired version that is not registered is never registered again`() {
        // A restore at v1 (nothing known registered), or a version removed
        // earlier: only the current one is desired.
        assertEquals(listOf(1), desiredReachKeys(inputs(1), 0L).filter { it.slot == contact }.map { it.version })
        assertEquals(
            listOf(5, 3),
            desiredReachKeys(inputs(5, registered = setOf(3)), 0L).filter { it.slot == contact }.map { it.version },
        )
    }

    @Test
    fun `a rotation adds the new version and removes only the one that left the window`() {
        // v0..v8 registered while v8 is current; Remove contact moves to v9.
        val held = (0..8).toSet()
        val registered = held.associate { "pk$it" to RegisteredReachKey(contact, it, null) }
        val desired = desiredReachKeys(inputs(9, registered = held), 0L)
            .filter { it.slot == contact }
            .associateBy { "pk${it.version}" }
        val diff = diffReachKeys(desired, registered)
        assertEquals(setOf("pk9"), diff.add.keys)
        assertEquals(setOf("pk0"), diff.remove)
        // Below the window's size nothing is removed.
        val early = diffReachKeys(
            desiredReachKeys(inputs(2, registered = setOf(0, 1)), 0L).filter { it.slot == contact }.associateBy { "pk${it.version}" },
            setOf(0, 1).associate { "pk$it" to RegisteredReachKey(contact, it, null) },
        )
        assertEquals(setOf("pk2"), early.add.keys)
        assertEquals(emptySet<String>(), early.remove)
    }

    @Test
    fun `a row is processed, processed and regranted, or dropped by its verdict`() {
        val rows = listOf("current", "retired-contact", "retired-stranger", "proofless")
        val verdicts = mapOf(
            "current" to FfiContactKeyRowVerdict.PROCESS,
            "retired-contact" to FfiContactKeyRowVerdict.PROCESS_AND_REGRANT,
            "retired-stranger" to FfiContactKeyRowVerdict.DROP,
            "proofless" to FfiContactKeyRowVerdict.PROCESS,
        )
        val regrant = ArrayList<String>()
        val kept = applyContactKeyVerdicts(rows, { verdicts.getValue(it) }, { regrant += it })
        assertEquals(listOf("current", "retired-contact", "proofless"), kept)
        assertEquals(listOf("retired-contact"), regrant)
    }

    @Test
    fun `a removed person's one-to-one profile carries no contact grant`() {
        fun grant(c: uniffi.lakko_ffi_uniffi.FfiReachClass) =
            uniffi.lakko_ffi_uniffi.FfiReachGrant(c, 1u, ByteArray(32), ByteArray(97))
        val grants = listOf(grant(uniffi.lakko_ffi_uniffi.FfiReachClass.CONTACT), grant(uniffi.lakko_ffi_uniffi.FfiReachClass.MEMBER))
        val withheld = me.lakko.app.chat.withoutContactGrant(grants, withhold = true)
        assertEquals(listOf(uniffi.lakko_ffi_uniffi.FfiReachClass.MEMBER), withheld.map { it.`class` })
        assertEquals(grants, me.lakko.app.chat.withoutContactGrant(grants, withhold = false))
    }

    @Test
    fun `a retired-key Welcome is joined only when its founder is let in`() {
        val contact = ByteArray(32) { 1 }
        val stranger = ByteArray(32) { 2 }
        val verdict: (ByteArray) -> FfiContactKeyRowVerdict = {
            if (it.contentEquals(contact)) FfiContactKeyRowVerdict.PROCESS_AND_REGRANT else FfiContactKeyRowVerdict.DROP
        }
        assertEquals(true, me.lakko.app.contacts.retiredKeyWelcomeJoins(contact, verdict))
        assertEquals(false, me.lakko.app.contacts.retiredKeyWelcomeJoins(stranger, verdict))
        // A Welcome whose founder could not be read is not joined.
        assertEquals(false, me.lakko.app.contacts.retiredKeyWelcomeJoins(null, verdict))
    }
}
