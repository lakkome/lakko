// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0065 R.5 / R.8 - nothing from a superseded sender acts
 * ([supersededActDropped], [senderActOf], [withoutSupersededActs]): the rule
 * the foreground poll applies to every batch before any sink sees it.
 */
class SupersededSenderTest {
    private val retired = "key-retired"
    private val other = "key-other-retired"
    private val live = "key-live"
    private val superseded = setOf(retired, other)

    @Test
    fun `a chat row or an op of a retired key is dropped wherever it arrives`() {
        for (moved in listOf(false, true)) {
            for (unrepairable in listOf(null, retired, other)) {
                assertTrue(supersededActDropped(SenderAct.ROW, retired, superseded, unrepairable, moved))
            }
        }
    }

    @Test
    fun `a call offer or a control envelope of a retired key is dropped in a moved or unrepairable conversation`() {
        for (act in listOf(SenderAct.CALL, SenderAct.CONTROL)) {
            // Already moved.
            assertTrue(supersededActDropped(act, retired, superseded, unrepairableKeyB64 = null, moved = true))
            // Unrepairable because of THIS sender's reset.
            assertTrue(supersededActDropped(act, retired, superseded, unrepairableKeyB64 = retired, moved = false))
            // Repaired in place: the group's own gates judge it.
            assertFalse(supersededActDropped(act, retired, superseded, unrepairableKeyB64 = null, moved = false))
            // Unrepairable for ANOTHER reset, not moved yet.
            assertFalse(supersededActDropped(act, retired, superseded, unrepairableKeyB64 = other, moved = false))
        }
    }

    @Test
    fun `a live sender and an unresolved one are never dropped`() {
        for (act in SenderAct.entries) {
            assertFalse(supersededActDropped(act, live, superseded, unrepairableKeyB64 = retired, moved = true))
            assertFalse(supersededActDropped(act, null, superseded, unrepairableKeyB64 = retired, moved = true))
            assertFalse(supersededActDropped(act, retired, emptySet(), unrepairableKeyB64 = retired, moved = true))
        }
    }

    private fun chat(id: Long, sender: String?) = IncomingChatEvent.Application(
        messageId = id,
        text = "hi",
        attachments = emptyList(),
        plaintext = "hi",
        senderLeaf = 1u,
        senderKey = sender,
        createdAt = "t",
    )

    private fun call(id: Long, person: String?, leaf: UInt? = 1u) = IncomingChatEvent.CallSignaling(
        messageId = id,
        payloadBytes = byteArrayOf(1),
        createdAt = "t",
        senderLeaf = leaf,
        senderKey = "device-key",
        senderPersonKey = person,
    )

    private fun name(id: Long, sender: String?) = IncomingChatEvent.GroupName(id, null, sender, "t")

    private fun pin(id: Long, sender: String?) = IncomingChatEvent.Pin(id, null, sender, "t")

    private fun beacon(id: Long, leaf: UInt?) = IncomingChatEvent.CommitterBeacon(id, leaf, "t")

    private fun secret(id: Long, leaf: UInt?) = IncomingChatEvent.PushRoutingSecret(id, ByteArray(32), 1uL, leaf, "t")

    private fun commit(id: Long) = IncomingChatEvent.System(id, SystemKind.Commit, "t")

    @Test
    fun `every frame kind is classified, and an MLS frame is never a candidate`() {
        val none = { _: UInt? -> null as String? }
        assertEquals(SenderAct.ROW to retired, senderActOf(chat(1, retired), none))
        assertEquals(SenderAct.CALL to retired, senderActOf(call(2, retired), none))
        assertEquals(SenderAct.CONTROL to retired, senderActOf(name(3, retired), none))
        assertEquals(SenderAct.CONTROL to retired, senderActOf(pin(4, retired), none))
        assertNull(senderActOf(commit(5), none))
        // The two envelopes that carry only the leaf are resolved against
        // the roster the batch left.
        val byLeaf = { leaf: UInt? -> if (leaf == 7u) retired else null }
        assertEquals(SenderAct.CONTROL to retired, senderActOf(beacon(6, 7u), byLeaf))
        assertEquals(SenderAct.CONTROL to retired, senderActOf(secret(7, 7u), byLeaf))
        assertEquals(SenderAct.CONTROL to null, senderActOf(beacon(8, 9u), byLeaf))
        // A call whose person key did not resolve at receipt falls back to
        // the leaf.
        assertEquals(SenderAct.CALL to retired, senderActOf(call(9, person = null, leaf = 7u), byLeaf))
    }

    @Test
    fun `a batch of an unrepairable one-to-one keeps only what the live side sent`() {
        val events = listOf(
            chat(1, retired),
            chat(2, live),
            call(3, retired),
            name(4, retired),
            commit(5),
            beacon(6, 7u),
            call(7, live),
        )
        val kept = withoutSupersededActs(
            events = events,
            supersededKeys = superseded,
            unrepairableKeyB64 = retired,
            moved = false,
            personKeyOfLeaf = { leaf -> if (leaf == 7u) retired else null },
        )
        assertEquals(listOf(2L, 5L, 7L), kept.map { messageIdOf(it) })
    }

    @Test
    fun `a group repaired in place loses the retired key's rows and keeps its envelopes for the role gates`() {
        val events = listOf(chat(1, retired), name(2, retired), call(3, retired), chat(4, live))
        val kept = withoutSupersededActs(events, superseded, unrepairableKeyB64 = null, moved = false) { null }
        assertEquals(listOf(2L, 3L, 4L), kept.map { messageIdOf(it) })
        // The same group once it moved: nothing of the retired key is left.
        val moved = withoutSupersededActs(events, superseded, unrepairableKeyB64 = null, moved = true) { null }
        assertEquals(listOf(4L), moved.map { messageIdOf(it) })
    }

    @Test
    fun `with no retired key the batch is returned as it is`() {
        val events = listOf(chat(1, retired), call(2, retired))
        assertSame(events, withoutSupersededActs(events, emptySet(), retired, true) { null })
    }

    private fun messageIdOf(event: IncomingChatEvent): Long = when (event) {
        is IncomingChatEvent.Application -> event.messageId
        is IncomingChatEvent.CallSignaling -> event.messageId
        is IncomingChatEvent.GroupName -> event.messageId
        is IncomingChatEvent.Pin -> event.messageId
        is IncomingChatEvent.System -> event.messageId
        is IncomingChatEvent.CommitterBeacon -> event.messageId
        is IncomingChatEvent.PushRoutingSecret -> event.messageId
        else -> -1L
    }
}
