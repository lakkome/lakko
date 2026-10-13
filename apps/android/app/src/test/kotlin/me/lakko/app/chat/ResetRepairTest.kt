// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0073 R.7 - the repair plans, row by row (mirror of the web
 *  `reset-repair` tables). */
class ResetRepairTest {
    private val roster = listOf(PersonLeaf(0u, "me"), PersonLeaf(1u, "old"), PersonLeaf(2u, "old"), PersonLeaf(3u, "c"))

    @Test
    fun `the four cases`() {
        assertEquals(ResetCase.ONE_TO_ONE, resetCase(ConversationKind.Chat, "old", "old"))
        assertEquals(ResetCase.ONE_TO_ONE, resetCase(ConversationKind.Chat, null, "old"))
        assertEquals(ResetCase.NO_ROLE_TABLE, resetCase(ConversationKind.Group, null, "old"))
        assertEquals(ResetCase.OWNED, resetCase(ConversationKind.Group, "old", "old"))
        assertEquals(ResetCase.REPAIR, resetCase(ConversationKind.Group, "me", "old"))
    }

    @Test
    fun `an owner or admin proposes one remove per pass and may add`() {
        for (role in listOf(OwnRole.OWNER, OwnRole.ADMIN)) {
            assertEquals(RepairPlan(1u, true), repairPlan(role, roster, "old", emptyList(), false, false))
        }
        // A pending Remove is not proposed again; the next leaf is taken.
        assertEquals(RepairPlan(2u, true), repairPlan(OwnRole.ADMIN, roster, "old", listOf(1u), false, false))
        assertEquals(RepairPlan(null, true), repairPlan(OwnRole.ADMIN, roster, "old", listOf(1u, 2u), false, false))
        assertEquals(RepairPlan(null, true), repairPlan(OwnRole.ADMIN, roster, "old", emptyList(), true, false))
        // The old leaves are gone: the Add of the new devices is still due.
        assertEquals(RepairPlan(null, true), repairPlan(OwnRole.OWNER, roster, "gone", emptyList(), false, false))
    }

    @Test
    fun `a member, a torn roster and an ended membership propose nothing`() {
        val none = RepairPlan(null, false)
        assertEquals(none, repairPlan(OwnRole.MEMBER, roster, "old", emptyList(), false, false))
        assertEquals(none, repairPlan(null, roster, "old", emptyList(), false, false))
        assertEquals(none, repairPlan(OwnRole.OWNER, null, "old", emptyList(), false, false))
        assertEquals(none, repairPlan(OwnRole.OWNER, roster, "old", emptyList(), false, true))
        assertTrue(mayAddResetIdentity(OwnRole.OWNER))
        assertTrue(mayAddResetIdentity(OwnRole.ADMIN))
        assertFalse(mayAddResetIdentity(OwnRole.MEMBER))
        assertFalse(mayAddResetIdentity(null))
    }

    @Test
    fun `the owner restores an admin - promote the new key, then demote the old`() {
        fun step(role: OwnRole?, admins: List<String>, people: List<String>, pending: Boolean = false) =
            adminRestoreStep(role, admins, "old", "new", people, pending)
        assertNull(step(OwnRole.OWNER, listOf("old"), listOf("me", "new"), pending = true))
        assertNull(step(OwnRole.OWNER, listOf("x"), listOf("me", "new")))
        assertNull(step(OwnRole.OWNER, listOf("old"), listOf("me", "old")))
        assertNull(step(OwnRole.ADMIN, listOf("old"), listOf("me", "new")))
        assertEquals(AdminRestoreStep.Promote("new"), step(OwnRole.OWNER, listOf("old"), listOf("me", "new")))
        assertEquals(AdminRestoreStep.Demote("old"), step(OwnRole.OWNER, listOf("old", "new"), listOf("me", "new")))
        assertEquals(listOf("x", "new"), adminRestorePlan(OwnRole.OWNER, listOf("old", "x"), "old", "new", listOf("new")))
    }

    private val accountOf = { k: String -> mapOf("old" to "A", "new" to "A")[k] }
    private val superseded = { k: String -> k == "old" }

    @Test
    fun `members changed is suppressed when the people are the same`() {
        assertTrue(membersChangedSuppressed(listOf("A", "b"), listOf("b", "A")))
        assertFalse(membersChangedSuppressed(listOf("A"), listOf("A", "b")))
        assertFalse(membersChangedSuppressed(null, listOf("A")))
        assertEquals(listOf("A", "c"), peopleByAccount(listOf("old", "new", "c"), accountOf))
    }

    @Test
    fun `a device swap in one commit leaves no row and no memory`() {
        val v = deviceSwapAcrossCommits(listOf("me", "old"), listOf("me", "new"), accountOf, superseded, emptyMap(), 100)
        assertEquals(DeviceSwapVerdict(true, emptyMap()), v)
    }

    @Test
    fun `a device swap across two commits leaves no row either`() {
        val first = deviceSwapAcrossCommits(listOf("me", "old"), listOf("me"), accountOf, superseded, emptyMap(), 100)
        assertEquals(DeviceSwapVerdict(true, mapOf("A" to 100L)), first)
        val second = deviceSwapAcrossCommits(listOf("me"), listOf("me", "new"), accountOf, superseded, first.awaited, 200)
        assertEquals(DeviceSwapVerdict(true, emptyMap()), second)
    }

    @Test
    fun `a person truly gone or new keeps the row`() {
        // Removed while still holding a current key: a person leaving.
        assertFalse(deviceSwapAcrossCommits(listOf("me", "c"), listOf("me"), accountOf, superseded, emptyMap(), 1).suppress)
        // Joining without having been awaited: a person joining.
        assertFalse(deviceSwapAcrossCommits(listOf("me"), listOf("me", "new"), accountOf, superseded, emptyMap(), 1).suppress)
        // A mix with one real change keeps the row.
        assertFalse(
            deviceSwapAcrossCommits(listOf("me", "old", "c"), listOf("me"), accountOf, superseded, emptyMap(), 1).suppress,
        )
        // No key changed at all: not this rule's row to suppress.
        assertFalse(deviceSwapAcrossCommits(listOf("me"), listOf("me"), accountOf, superseded, emptyMap(), 1).suppress)
        // A torn read keeps the row and the memory.
        assertEquals(
            DeviceSwapVerdict(false, mapOf("A" to 5L)),
            deviceSwapAcrossCommits(null, listOf("me"), accountOf, superseded, mapOf("A" to 5L), 10),
        )
        // An awaited person past the window is forgotten.
        val late = deviceSwapAcrossCommits(
            listOf("me"), listOf("me", "new"), accountOf, superseded, mapOf("A" to 0L), DEVICE_SWAP_PAIR_WINDOW_MS,
        )
        assertEquals(DeviceSwapVerdict(false, emptyMap()), late)
    }

    @Test
    fun `the two defaults awaiting the maintainer`() {
        assertTrue(restoreOwnershipToResetIdentity())
        assertTrue(resendOwnMessagesAfterReset())
    }

    @Test
    fun `only own, resendable messages after the reset was signed are sent again, in order`() {
        val messages = listOf(
            OldSegmentMessage(9, own = true, sentAtMs = 300, resendable = true),
            OldSegmentMessage(3, own = true, sentAtMs = 50, resendable = true),
            OldSegmentMessage(4, own = false, sentAtMs = 200, resendable = true),
            OldSegmentMessage(5, own = true, sentAtMs = 200, resendable = false),
            OldSegmentMessage(6, own = true, sentAtMs = null, resendable = true),
            OldSegmentMessage(7, own = true, sentAtMs = 101, resendable = true),
            OldSegmentMessage(8, own = true, sentAtMs = 100, resendable = true),
        )
        assertEquals(listOf(7L, 9L), messagesToResend(messages, resetSignedAtMs = 100))
    }

    @Test
    fun `the successor of an owned group - people, the missing ones and the admins`() {
        val old = listOf("me", "owner-old", "b", "b", "c")
        assertEquals(listOf("b", "c"), successorPeople(old, setOf("owner-old"), "me"))
        assertEquals(listOf("c"), successorMissingPeople(old, listOf("me", "b", "owner-new"), setOf("owner-old")))
        assertEquals(
            listOf("b"),
            successorAdmins(listOf("owner-old", "me", "b", "z"), listOf("me", "b"), "me", setOf("owner-old")),
        )
        assertTrue(dropsSupersededSender("owner-old", setOf("owner-old")))
        assertFalse(dropsSupersededSender("b", setOf("owner-old")))
        assertFalse(dropsSupersededSender(null, setOf("owner-old")))
    }
}
