// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.push.conversationHexForWake
import me.lakko.app.push.notificationGroupKey
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** ADR-0073 R.2 - the wider re-admission lookup and what a roster key is
 *  worth for an account; and the conversation a push wake belongs to. */
class DeviceReadmissionLookupTest {
    private val idA = java.util.Base64.getEncoder().encodeToString(ByteArray(16) { 1 })
    private val idB = java.util.Base64.getEncoder().encodeToString(ByteArray(16) { 2 })
    private val idC = java.util.Base64.getEncoder().encodeToString(ByteArray(16) { 3 })

    @Test
    fun `every roster account whose id this device knows is a candidate`() {
        val candidates = DeviceReadmission.readmissionCandidates(
            rosterPersonKeysB64 = listOf("me", "a-key", "a-key", "b-old", "c-key", "nobody", "junk"),
            ownPersonKeyB64 = "me",
            contacts = listOf(
                DeviceReadmission.LookupContact("a-key", idA),
                // The row already moved to the new key; the roster still names the old one.
                DeviceReadmission.LookupContact("b-new", idB),
                DeviceReadmission.LookupContact("no-id", null),
            ),
            accountOfRetiredKey = { if (it == "b-old") idB else null },
            profileAccountIdB64Of = { mapOf("c-key" to idC, "a-key" to idC, "junk" to "AAAA")[it] },
        )
        assertEquals(
            listOf(
                // The contact row wins over the profile's claim.
                DeviceReadmission.LookupCandidate("a-key", idA),
                DeviceReadmission.LookupCandidate("b-old", idB),
                // No contact: the id the member's own profile claims (judged later).
                DeviceReadmission.LookupCandidate("c-key", idC),
            ),
            candidates,
        )
    }

    @Test
    fun `a claimed id is followed to devices only for a key this device trusts`() {
        fun s(roster: String, chain: List<String>?, superseded: Set<String> = emptySet(), binding: String? = null) =
            DeviceReadmission.lookupStanding(roster, chain, superseded, binding)
        assertEquals(DeviceReadmission.LookupStanding.SAME_PERSON, s("pin", listOf("pred", "pin")))
        assertEquals(DeviceReadmission.LookupStanding.SAME_PERSON, s("pred", listOf("pred", "pin")))
        assertEquals(DeviceReadmission.LookupStanding.RESET_IDENTITY, s("old", listOf("pin"), setOf("old")))
        assertEquals(DeviceReadmission.LookupStanding.SKIP, s("claimed", listOf("pin"), setOf("old")))
        assertEquals(DeviceReadmission.LookupStanding.SAME_PERSON, s("k", null, binding = "k"))
        assertEquals(DeviceReadmission.LookupStanding.SKIP, s("k", null, binding = "other"))
        assertEquals(DeviceReadmission.LookupStanding.SKIP, s("k", null))
        // A retired key is not followed without a pin, whatever was superseded.
        assertEquals(DeviceReadmission.LookupStanding.SKIP, s("old", null, setOf("old")))
    }

    @Test
    fun `a successor group's wake belongs to its conversation`() {
        val lineage = listOf(LineageEntry("succ", "root", 1UL))
        assertEquals("root", conversationHexForWake("SUCC", lineage))
        assertEquals("alone", conversationHexForWake("alone", lineage))
        assertNull(notificationGroupKey(null, "succ", lineage))
        // Unresolved, or a conversation's first group: the key it always had.
        assertEquals("gid", notificationGroupKey("gid", null, lineage))
        assertEquals("gid", notificationGroupKey("gid", "gid", lineage))
        assertEquals("gid", notificationGroupKey("gid", "root", lineage))
        assertEquals("gid", notificationGroupKey("gid", "alone", lineage))
        assertEquals("root", notificationGroupKey("gid", "succ", lineage))
    }
}
