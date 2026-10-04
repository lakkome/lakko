// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.GroupKind
import uniffi.lakko_ffi_uniffi.Role

/**
 * ADR-0065 phase 2 / UX 16.3 - 16.5 — the chat side's pure decisions.
 *
 *  1. The three branches per shared group, plus the two "flag only" cases.
 *  2. The `applied` test on the previous key's device leaves.
 *  3. The re-admission arm's add-branch suppression.
 *  4. The closed one-to-one: the late-message drop, the composer affordance.
 *  5. The rows, the bubble warning, the system row, the owned-group notice.
 *  6. The eviction re-proposal rule and the two store codecs.
 */
class KeysResetTest {

    // --- 1. the branches -----------------------------------------------------

    @Test
    fun `a one-to-one closes whatever the role table says`() {
        assertEquals(
            KeysResetBranch.OneToOneClose,
            keysResetBranch(GroupKind.ONE_TO_ONE, hasRoleTable = true, ownerIsPrevious = false, ownRole = Role.OWNER),
        )
        assertEquals(
            KeysResetBranch.OneToOneClose,
            keysResetBranch(GroupKind.ONE_TO_ONE, hasRoleTable = false, ownerIsPrevious = false, ownRole = null),
        )
    }

    @Test
    fun `an owner or admin of a group the old identity did not own proposes the eviction`() {
        assertEquals(
            KeysResetBranch.EvictionProposal,
            keysResetBranch(GroupKind.GROUP, hasRoleTable = true, ownerIsPrevious = false, ownRole = Role.OWNER),
        )
        assertEquals(
            KeysResetBranch.EvictionProposal,
            keysResetBranch(GroupKind.GROUP, hasRoleTable = true, ownerIsPrevious = false, ownRole = Role.ADMIN),
        )
    }

    @Test
    fun `a plain member only flags`() {
        assertEquals(
            KeysResetBranch.FlagOnly,
            keysResetBranch(GroupKind.GROUP, hasRoleTable = true, ownerIsPrevious = false, ownRole = Role.MEMBER),
        )
        assertEquals(
            KeysResetBranch.FlagOnly,
            keysResetBranch(null, hasRoleTable = true, ownerIsPrevious = false, ownRole = null),
        )
    }

    @Test
    fun `a group the old identity owned gets the notice and never an eviction, whatever the own role`() {
        for (role in listOf(Role.OWNER, Role.ADMIN, Role.MEMBER, null)) {
            assertEquals(
                KeysResetBranch.OwnedGroupNotice,
                keysResetBranch(GroupKind.GROUP, hasRoleTable = true, ownerIsPrevious = true, ownRole = role),
            )
        }
    }

    @Test
    fun `a group with no role table only flags`() {
        assertEquals(
            KeysResetBranch.NoRoleTableFlag,
            keysResetBranch(GroupKind.GROUP, hasRoleTable = false, ownerIsPrevious = false, ownRole = null),
        )
        // `null` kind counts as a group.
        assertEquals(
            KeysResetBranch.NoRoleTableFlag,
            keysResetBranch(null, hasRoleTable = false, ownerIsPrevious = false, ownRole = null),
        )
    }

    // --- 2. applied ----------------------------------------------------------

    @Test
    fun `the eviction applied once no leaf resolves to the previous key, never on a torn read`() {
        assertTrue(evictionApplied(listOf("a", "b"), previousKeyB64 = "prev"))
        assertFalse(evictionApplied(listOf("a", "prev"), previousKeyB64 = "prev"))
        assertFalse(evictionApplied(null, previousKeyB64 = "prev"))
        assertTrue(evictionApplied(emptyList(), previousKeyB64 = "prev"))
    }

    // --- 3. the add-arm suppression -----------------------------------------

    @Test
    fun `the add branch is suppressed for an unverified contact or an unpinned published key`() {
        assertTrue(suppressReadmissionAdd(contactUnverified = true, publishedKeyIsPinned = true))
        assertTrue(suppressReadmissionAdd(contactUnverified = false, publishedKeyIsPinned = false))
        assertFalse(suppressReadmissionAdd(contactUnverified = false, publishedKeyIsPinned = true))
        // Unknown pin state with a verified contact: the arm runs as before.
        assertFalse(suppressReadmissionAdd(contactUnverified = false, publishedKeyIsPinned = null))
    }

    // --- 4. the closed one-to-one -------------------------------------------

    @Test
    fun `a late message into a closed conversation is dropped before it is stored`() {
        assertTrue(dropIncomingChatRow(conversationClosed = true))
        assertFalse(dropIncomingChatRow(conversationClosed = false))
    }

    @Test
    fun `only a one-to-one closed by a key reset is inert`() {
        assertTrue(conversationInert(ConversationClosed(ClosedReason.PEER_KEYS_RESET, 1L, peerAccountIdB64 = "peer")))
        // 16.5 — a group an admin closed is still a live group the person is in.
        assertFalse(conversationInert(ConversationClosed(ClosedReason.GROUP_CLOSED_BY_ADMIN, 1L)))
        assertFalse(conversationInert(null))
    }

    @Test
    fun `a closed one-to-one drops every kind of row that arrives and an open chat drops none`() {
        for (kind in ArrivingRow.entries) {
            assertTrue("a closed one-to-one must drop $kind", dropArrivingRow(inert = true, kind = kind))
            assertFalse("an open conversation must keep $kind", dropArrivingRow(inert = false, kind = kind))
        }
    }

    @Test
    fun `ops, call frames, typing and read receipts are classified as their own kinds, not as service rows`() {
        // The defect this closes: only chat rows were dropped, so an edit, a
        // delete, a reaction or a call offer into a closed one-to-one was
        // still applied or rung. Each must reach the rule under its own name.
        assertEquals(
            ArrivingRow.OP,
            arrivingRowOf(
                IncomingChatEvent.Op(
                    messageId = 2L,
                    op = MessageOp.Delete,
                    target = 1L,
                    text = "",
                    emoji = null,
                    senderLeaf = 1u,
                    senderKey = "k",
                    createdAt = "2026-10-04T00:00:00Z",
                ),
            ),
        )
        assertEquals(
            ArrivingRow.CALL_SIGNALING,
            arrivingRowOf(IncomingChatEvent.CallSignaling(3L, byteArrayOf(1), "2026-10-04T00:00:00Z")),
        )
        assertEquals(
            ArrivingRow.EPHEMERAL,
            arrivingRowOf(IncomingChatEvent.Typing(4L, true, 1u, "k", "2026-10-04T00:00:00Z")),
        )
        assertEquals(
            ArrivingRow.EPHEMERAL,
            arrivingRowOf(IncomingChatEvent.ReadReceipt(5L, 4L, 1u, "k", "2026-10-04T00:00:00Z")),
        )
    }

    @Test
    fun `a closed one-to-one neither fetches, sends nor keeps a push subscription`() {
        val closed = conversationActivity(ConversationClosed(ClosedReason.PEER_KEYS_RESET, 1L))
        assertFalse(closed.fetches)
        assertFalse(closed.sends)
        assertFalse(closed.keepsPushSubscription)

        for (live in listOf(null, ConversationClosed(ClosedReason.GROUP_CLOSED_BY_ADMIN, 1L))) {
            val activity = conversationActivity(live)
            assertTrue(activity.fetches)
            assertTrue(activity.sends)
            assertTrue(activity.keepsPushSubscription)
        }
    }

    @Test
    fun `start a new chat is disabled with the helper until verified, enabled afterwards`() {
        val blocked = startNewChatAffordance(contactUnverified = true, name = "Anna")
        assertFalse(blocked.enabled)
        assertEquals("contacts.keys_reset.verify_first", blocked.helper?.key)
        assertEquals("Anna", blocked.helper?.texts?.get("name"))
        assertEquals("contacts.keys_reset.open_contact", blocked.openContact?.key)

        val unnamed = startNewChatAffordance(contactUnverified = true, name = null)
        assertEquals("contacts.keys_reset.verify_first_short", unnamed.helper?.key)

        val open = startNewChatAffordance(contactUnverified = false, name = "Anna")
        assertTrue(open.enabled)
        assertNull(open.helper)
        assertNull(open.openContact)
    }

    @Test
    fun `the closed one-to-one copy names the person`() {
        assertEquals("chat.keys_reset.one_to_one_closed", OneToOneClosedCopy.terminalHeadline("Anna").key)
        assertEquals("Anna", OneToOneClosedCopy.listRow("Anna").texts["name"])
        assertEquals("chat.keys_reset.one_to_one_composer", OneToOneClosedCopy.COMPOSER.key)
        assertEquals("chat.keys_reset.start_new_chat", OneToOneClosedCopy.START_NEW_CHAT.key)
    }

    // --- 5. rows, warning, system row, notice --------------------------------

    @Test
    fun `the member row says Being removed only where this device proposed it`() {
        assertEquals("chat.keys_reset.member_being_removed", resetMemberLine(KeysResetBranch.EvictionProposal).key)
        assertEquals("chat.keys_reset.member_flag", resetMemberLine(KeysResetBranch.FlagOnly).key)
        assertEquals("chat.keys_reset.member_flag", resetMemberLine(KeysResetBranch.OwnedGroupNotice).key)
        assertEquals("chat.keys_reset.member_flag", resetMemberLine(KeysResetBranch.NoRoleTableFlag).key)
    }

    @Test
    fun `the bubble warning shows only for a flagged contact's new keys`() {
        val flagged = setOf("new-anna")
        assertTrue(bubbleWarningFor("new-anna", flagged))
        assertFalse(bubbleWarningFor("old-anna", flagged))
        assertFalse(bubbleWarningFor("stranger", flagged))
        assertFalse(bubbleWarningFor(null, flagged))
        // Verified (no longer flagged): no warning.
        assertFalse(bubbleWarningFor("new-anna", emptySet()))
        assertEquals("Anna", bubbleWarningText("Anna").texts["name"])
    }

    @Test
    fun `the system row is the reset's own on a device that knows it, else Members changed`() {
        assertEquals("chat.keys_reset.system_removed", evictionLandedRow(knowsReset = true, name = "Anna").key)
        assertEquals("chat.system.members_changed", evictionLandedRow(knowsReset = false, name = "Anna").key)
        assertEquals("chat.system.members_changed", evictionLandedRow(knowsReset = true, name = null).key)
    }

    @Test
    fun `the owned-group notice offers Recreate group to an admin only`() {
        val admin = ownedGroupNotice("Anna", Role.ADMIN)
        assertTrue(admin.canRecreate)
        assertNull(admin.memberLine)
        assertEquals("chat.keys_reset.owned_notice", admin.text.key)
        assertEquals("Anna", admin.text.texts["name"])

        val member = ownedGroupNotice("Anna", Role.MEMBER)
        assertFalse(member.canRecreate)
        assertEquals("chat.keys_reset.owned_notice_member", member.memberLine?.key)

        // The owner's own role is the one that was reset; a device reading
        // OWNER here is not an admin and gets no button.
        assertFalse(ownedGroupNotice("Anna", Role.OWNER).canRecreate)
        assertFalse(ownedGroupNotice("Anna", null).canRecreate)
    }

    // --- 6. re-proposal + codecs --------------------------------------------

    @Test
    fun `the eviction is proposed once per session while not applied and never once applied`() {
        assertTrue(shouldProposeEviction(record = null, proposedThisSession = false))
        assertTrue(shouldProposeEviction(EvictionRecord("prev", 1L, applied = false), proposedThisSession = false))
        assertFalse(shouldProposeEviction(EvictionRecord("prev", 1L, applied = false), proposedThisSession = true))
        assertFalse(shouldProposeEviction(EvictionRecord("prev", 1L, applied = true), proposedThisSession = false))
    }

    @Test
    fun `the closed-state codec round-trips both reasons and reads garbage as absent`() {
        val oneToOne = ConversationClosed(ClosedReason.PEER_KEYS_RESET, 10L, peerAccountIdB64 = "peer")
        assertEquals(oneToOne, decodeClosed(encodeClosed(oneToOne)))
        val group = ConversationClosed(ClosedReason.GROUP_CLOSED_BY_ADMIN, 20L, successorGroupIdHex = "ab".repeat(32), closerKeyB64 = "k")
        assertEquals(group, decodeClosed(encodeClosed(group)))
        // 16.5 — the closer's account key is in the stored row, so the
        // terminal row names the closer after a reload.
        assertEquals("k", decodeClosed(encodeClosed(group))?.closerKeyB64)
        assertTrue(encodeClosed(group).contains("\"closerKeyB64\":\"k\""))
        // A row written without one reads as "unresolvable", not as absent.
        assertNull(decodeClosed("""{"reason":"GROUP_CLOSED_BY_ADMIN","closedAtMs":20}""")?.closerKeyB64)
        assertNull(decodeClosed(null))
        assertNull(decodeClosed("garbage"))
        assertNull(decodeClosed("""{"reason":"NOPE","closedAtMs":1}"""))
    }

    @Test
    fun `the eviction codec round-trips and reads garbage as absent`() {
        val record = EvictionRecord("prev", 5L, applied = true)
        assertEquals(record, decodeEviction(encodeEviction(record)))
        assertEquals(EvictionRecord("prev", null, false), decodeEviction(encodeEviction(EvictionRecord("prev", null, false))))
        assertNull(decodeEviction(""))
        assertNull(decodeEviction("""{"previousKeyB64":""}"""))
    }
}
