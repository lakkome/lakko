// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.push.GroupPushRouting
import me.lakko.app.push.isServiceEnvelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiGroupClosed
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage

/**
 * ADR-0065 phase 2 / UX 16.5 — the group-closed envelope on the receive side.
 *
 *  1. The verdict matrix: identical / owner / admin / member / no role table /
 *     unreadable table / unresolvable sender / stale — the group-name gate's
 *     shape, with one difference: a table-less group has nobody to close it.
 *  2. The wire peek is a complete classification, and the foreground
 *     classifier turns a group-closed envelope into a `GroupClosed` event
 *     bound to the resolved sender key — never a chat bubble, even malformed.
 *  3. The background push filter suppresses the envelope.
 *  4. `Open the new group` only once this device has joined the successor.
 *
 * Plain JVM: the FFI order is a stand-in (`strictly greater`), matching the
 * documented contract of `groupClosedIsNewer`.
 */
class GroupClosedTest {

    private val owner = ByteArray(32) { 1 }
    private val admin = ByteArray(32) { 2 }
    private val member = ByteArray(32) { 3 }
    private val table = GroupPushRouting.OwnerLookup.Known(owner = owner, admins = listOf(admin))
    private val strictlyNewer: (ULong, ULong) -> Boolean = { a, b -> a > b }
    private val successor = "ab".repeat(32)

    private fun verdict(
        sender: ByteArray?,
        table: GroupPushRouting.OwnerLookup = this.table,
        incoming: StoredGroupClosed = StoredGroupClosed(200uL, successor),
        stored: StoredGroupClosed? = StoredGroupClosed(100uL, null),
    ) = groupClosedVerdict(sender, table, incoming, stored, strictlyNewer)

    // --- 1. verdict matrix -------------------------------------------------

    @Test
    fun `an identical notice is accepted from anyone without authority`() {
        val same = StoredGroupClosed(200uL, successor)
        assertEquals(GroupClosedVerdict.ACCEPT_IDENTICAL, verdict(member, incoming = same, stored = same))
        assertEquals(GroupClosedVerdict.ACCEPT_IDENTICAL, verdict(null, incoming = same, stored = same))
    }

    @Test
    fun `the owner and an admin may close, a member may not`() {
        assertEquals(GroupClosedVerdict.ACCEPT_OWNER, verdict(owner))
        assertEquals(GroupClosedVerdict.ACCEPT_ADMIN, verdict(admin))
        assertEquals(GroupClosedVerdict.REJECT_NOT_AUTHORIZED, verdict(member))
        assertEquals(GroupClosedVerdict.REJECT_NOT_AUTHORIZED, verdict(ByteArray(32) { 9 }))
    }

    @Test
    fun `a table-less group, an unreadable table and an unresolvable sender all fail closed`() {
        assertEquals(GroupClosedVerdict.REJECT_UNVERIFIABLE, verdict(owner, table = GroupPushRouting.OwnerLookup.NoRoleTable))
        assertEquals(GroupClosedVerdict.REJECT_UNVERIFIABLE, verdict(owner, table = GroupPushRouting.OwnerLookup.Unavailable))
        assertEquals(GroupClosedVerdict.REJECT_UNVERIFIABLE, verdict(null))
    }

    @Test
    fun `a stale or equal stamp is refused and the first notice is accepted`() {
        assertEquals(GroupClosedVerdict.REJECT_STALE, verdict(admin, incoming = StoredGroupClosed(100uL, successor)))
        assertEquals(GroupClosedVerdict.REJECT_STALE, verdict(admin, incoming = StoredGroupClosed(50uL, successor)))
        assertEquals(GroupClosedVerdict.ACCEPT_ADMIN, verdict(admin, stored = null))
    }

    @Test
    fun `the second notice to land with a newer stamp wins the successor`() {
        val first = StoredGroupClosed(100uL, "cd".repeat(32))
        val second = StoredGroupClosed(200uL, successor)
        assertEquals(GroupClosedVerdict.ACCEPT_ADMIN, verdict(admin, incoming = second, stored = first))
        assertEquals(GroupClosedVerdict.REJECT_STALE, verdict(admin, incoming = first, stored = second))
    }

    // --- 2. wire peek + foreground classification -------------------------

    private val domain: ByteArray = "lakko/group-closed/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

    /** `domain || version || closed_at_ms(8) || successor_len || successor`. */
    private fun envelope(withSuccessor: Boolean = true): ByteArray {
        val id = if (withSuccessor) ByteArray(32) { 0xAB.toByte() } else ByteArray(0)
        return domain + byteArrayOf(0x01) + ByteArray(8) { if (it == 7) 42 else 0 } + byteArrayOf(id.size.toByte()) + id
    }

    private fun lossyParseBody(bytes: ByteArray): ParsedBody = ParsedBody(
        kind = MessageBodyKind.CHAT,
        text = String(bytes, Charsets.UTF_8),
        attachments = emptyList(),
        target = null,
        emoji = null,
        isTyping = null,
        replyTo = null,
        replyAuthor = null,
        replyExcerpt = null,
        forwardedFrom = null,
        forwardOriginGroupB64 = null,
        forwardOriginMessageId = null,
    )

    private val decoded = FfiGroupClosed(closedAtMs = 42uL, successorGroupId = ByteArray(32) { 0xAB.toByte() })

    private fun classify(
        payload: ByteArray,
        decodeGroupClosed: (ByteArray) -> FfiGroupClosed?,
        senderLeaf: UInt? = 3u,
    ): IncomingChatEvent = toEvent(
        messageId = 7L,
        createdAt = "2026-10-04T00:00:00Z",
        processed = ProcessedMessage.Application(payload, senderLeaf, null),
        resolveSender = { leaf -> "KEY_OF_LEAF_$leaf" },
        resolveLeafSignatureKey = { null },
        isCommitterBeacon = { false },
        parseBody = ::lossyParseBody,
        decodeProfile = { null },
        decodeGroupName = { error("not a group name") },
        decodeGroupClosed = decodeGroupClosed,
    )

    @Test
    fun `the peek is 22 bytes and a complete classification`() {
        assertEquals(22, domain.size)
        assertTrue(looksLikeGroupClosedEnvelope(envelope()))
        assertTrue(looksLikeGroupClosedEnvelope(envelope(withSuccessor = false)))
        assertTrue(looksLikeGroupClosedEnvelope(domain))
        assertFalse(looksLikeGroupClosedEnvelope(domain.copyOf(21)))
        assertFalse(looksLikeGroupClosedEnvelope("""{"v":1,"text":"lakko/group-closed/v1"}""".toByteArray()))
        assertFalse(looksLikeGroupClosedEnvelope("lakko/group-closed/v1 is the domain".toByteArray()))
        // The group-name domain is a different envelope.
        assertFalse(looksLikeGroupClosedEnvelope("lakko/group-name/v1".toByteArray() + byteArrayOf(0)))
        assertFalse(looksLikeGroupNameEnvelope(envelope()))
    }

    @Test
    fun `a group-closed envelope classifies as a GroupClosed event bound to the resolved sender key`() {
        val event = classify(envelope(), decodeGroupClosed = { decoded })
        assertTrue("expected GroupClosed, got $event", event is IncomingChatEvent.GroupClosed)
        event as IncomingChatEvent.GroupClosed
        assertEquals(decoded, event.closed)
        assertEquals("KEY_OF_LEAF_3", event.senderKey)
        assertEquals(7L, event.messageId)
    }

    @Test
    fun `a malformed group-closed envelope is a no-op event, never a bubble`() {
        val event = classify(envelope(), decodeGroupClosed = { null })
        assertTrue("expected GroupClosed, got $event", event is IncomingChatEvent.GroupClosed)
        assertNull((event as IncomingChatEvent.GroupClosed).closed)
    }

    @Test
    fun `an unattributable sender still never becomes a bubble`() {
        val event = classify(envelope(), decodeGroupClosed = { decoded }, senderLeaf = null)
        assertTrue(event is IncomingChatEvent.GroupClosed)
        assertNull((event as IncomingChatEvent.GroupClosed).senderKey)
    }

    @Test
    fun `a chat message whose text mentions the domain is still a chat message`() {
        val chat = """{"v":1,"text":"lakko/group-closed/v1"}""".toByteArray()
        val event = classify(chat, decodeGroupClosed = { error("must not be called") })
        assertTrue(event is IncomingChatEvent.Application)
    }

    // --- 3. background suppression --------------------------------------

    @Test
    fun `the background push filter suppresses a group-closed envelope`() {
        assertTrue(isServiceEnvelope(envelope(), { false }, { false }, 27))
        assertTrue(isServiceEnvelope(envelope(withSuccessor = false), { false }, { false }, 27))
        assertFalse(isServiceEnvelope("""{"v":1,"text":"hi"}""".toByteArray(), { false }, { false }, 27))
    }

    // --- 4. Open the new group ---------------------------------------------

    @Test
    fun `open the new group only once this device has joined the successor`() {
        assertNull(openNewGroupTarget(null, setOf(successor)))
        assertNull(openNewGroupTarget(successor, emptySet()))
        assertNull(openNewGroupTarget(successor, setOf("cd".repeat(32))))
        assertEquals(successor, openNewGroupTarget(successor, setOf(successor)))
        assertEquals(successor, openNewGroupTarget(successor.uppercase(), setOf(successor)))
    }

    @Test
    fun `the closed-group copy`() {
        assertEquals("group.closed.system", GroupClosedCopy.terminalHeadline("Ivan").key)
        assertEquals("Ivan", GroupClosedCopy.terminalHeadline("Ivan").texts["name"])
        assertEquals("group.closed.composer", GroupClosedCopy.COMPOSER.key)
        assertEquals("group.closed.open_new", GroupClosedCopy.OPEN_NEW.key)
        assertEquals("group.closed.row", GroupClosedCopy.LIST_ROW.key)
        assertEquals("Weekend", GroupClosedCopy.panel("Weekend").texts["name"])
    }

    // ---- 5. A retry after a failed closed notice founds nothing -------------

    @Test
    fun `the first press of recreate founds a group`() {
        assertEquals(RecreateStep.Create, recreateStep(pendingSuccessorHex = null, joinedGroupIdsHex = setOf(successor)))
        assertEquals(RecreateStep.Create, recreateStep(pendingSuccessorHex = "", joinedGroupIdsHex = setOf(successor)))
    }

    @Test
    fun `a retry after the closed notice failed re-posts it naming the same successor`() {
        val step = recreateStep(pendingSuccessorHex = successor.uppercase(), joinedGroupIdsHex = setOf("cd".repeat(32), successor))
        assertEquals(RecreateStep.RepostClose(successor), step)
        // Pressing any number of times decides the same thing: still one group.
        assertEquals(step, recreateStep(successor, setOf(successor)))
    }

    @Test
    fun `a recorded successor this device no longer holds is not reused`() {
        assertEquals(RecreateStep.Create, recreateStep(successor, joinedGroupIdsHex = setOf("cd".repeat(32))))
        assertEquals(RecreateStep.Create, recreateStep(successor, joinedGroupIdsHex = emptySet()))
    }
}
