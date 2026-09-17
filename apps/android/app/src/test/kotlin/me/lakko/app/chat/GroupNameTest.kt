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
import uniffi.lakko_ffi_uniffi.FfiGroupName
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage

/**
 * ADR-0051 — the shared group name on the receive side.
 *
 *  1. The verdict matrix: identical / owner / admin / member / no role table /
 *     unreadable role table / unresolvable sender / stale.
 *  2. The store codec round-trips a stamped row (an empty, cleared name too)
 *     and reads garbage as absent.
 *  3. The wire peek is a complete classification, and the foreground
 *     classifier turns a group-name envelope into a `GroupName` event bound to
 *     the resolved sender key — never a chat bubble, even when malformed.
 *  4. The background push filter suppresses the envelope.
 *
 * Plain JVM: the FFI order is a stand-in (`strictly greater`), matching the
 * documented contract of `groupNameIsNewer`.
 */
class GroupNameTest {

    private val owner = ByteArray(32) { 1 }
    private val admin = ByteArray(32) { 2 }
    private val member = ByteArray(32) { 3 }
    private val table = GroupPushRouting.OwnerLookup.Known(owner = owner, admins = listOf(admin))
    private val strictlyNewer: (ULong, ULong) -> Boolean = { a, b -> a > b }

    private fun verdict(
        sender: ByteArray?,
        table: GroupPushRouting.OwnerLookup = this.table,
        incoming: StoredGroupName = StoredGroupName("Trio", 200uL),
        stored: StoredGroupName? = StoredGroupName("Duo", 100uL),
    ) = groupNameVerdict(sender, table, incoming, stored, strictlyNewer)

    // --- 1. verdict matrix -------------------------------------------------

    @Test
    fun `an identical name and stamp is accepted from anyone without authority`() {
        val same = StoredGroupName("Trio", 200uL)
        assertEquals(GroupNameVerdict.ACCEPT_IDENTICAL, verdict(member, incoming = same, stored = same))
        assertEquals(GroupNameVerdict.ACCEPT_IDENTICAL, verdict(null, incoming = same, stored = same))
        assertEquals(
            GroupNameVerdict.ACCEPT_IDENTICAL,
            verdict(member, table = GroupPushRouting.OwnerLookup.Unavailable, incoming = same, stored = same),
        )
    }

    @Test
    fun `owner and admin may set a newer name, a plain member may not`() {
        assertEquals(GroupNameVerdict.ACCEPT_OWNER, verdict(owner))
        assertEquals(GroupNameVerdict.ACCEPT_ADMIN, verdict(admin))
        assertEquals(GroupNameVerdict.REJECT_NOT_AUTHORIZED, verdict(member))
        assertEquals(GroupNameVerdict.REJECT_NOT_AUTHORIZED, verdict(ByteArray(32) { 9 }))
    }

    @Test
    fun `an unresolvable sender or an unreadable role table fails closed`() {
        assertEquals(GroupNameVerdict.REJECT_UNVERIFIABLE, verdict(null))
        assertEquals(GroupNameVerdict.REJECT_UNVERIFIABLE, verdict(owner, table = GroupPushRouting.OwnerLookup.Unavailable))
    }

    @Test
    fun `a group with no role table accepts a newer name from any member`() {
        assertEquals(GroupNameVerdict.ACCEPT_NO_ROLE_TABLE, verdict(member, table = GroupPushRouting.OwnerLookup.NoRoleTable))
        assertEquals(GroupNameVerdict.ACCEPT_NO_ROLE_TABLE, verdict(null, table = GroupPushRouting.OwnerLookup.NoRoleTable))
        assertEquals(
            GroupNameVerdict.REJECT_STALE,
            verdict(member, table = GroupPushRouting.OwnerLookup.NoRoleTable, incoming = StoredGroupName("Old", 50uL)),
        )
    }

    @Test
    fun `the stamp decides which authorised name survives, equal is not newer`() {
        assertEquals(GroupNameVerdict.REJECT_STALE, verdict(owner, incoming = StoredGroupName("Old", 100uL)))
        assertEquals(GroupNameVerdict.REJECT_STALE, verdict(owner, incoming = StoredGroupName("Older", 99uL)))
        assertEquals(GroupNameVerdict.ACCEPT_OWNER, verdict(owner, incoming = StoredGroupName("Newer", 101uL)))
        // The authority gate runs BEFORE the order: a member's newer name is still refused.
        assertEquals(GroupNameVerdict.REJECT_NOT_AUTHORIZED, verdict(member, incoming = StoredGroupName("X", 999uL)))
    }

    @Test
    fun `the first name needs no order and an empty name clears with the same rules`() {
        assertEquals(GroupNameVerdict.ACCEPT_OWNER, verdict(owner, stored = null))
        assertEquals(GroupNameVerdict.REJECT_NOT_AUTHORIZED, verdict(member, stored = null))
        assertEquals(GroupNameVerdict.ACCEPT_ADMIN, verdict(admin, incoming = StoredGroupName("", 300uL)))
        assertEquals(GroupNameVerdict.REJECT_STALE, verdict(admin, incoming = StoredGroupName("", 100uL)))
    }

    @Test
    fun `a growth re-send of the stored name with its original stamp is inert`() {
        // ADR-0044 lesson: the re-send carries the stamp verbatim, so after a
        // later rename (stored at 300) it is stale, and when nothing changed it
        // is identical. Neither moves the store.
        val original = StoredGroupName("Trio", 200uL)
        assertEquals(GroupNameVerdict.ACCEPT_IDENTICAL, verdict(admin, incoming = original, stored = original))
        assertEquals(GroupNameVerdict.REJECT_STALE, verdict(admin, incoming = original, stored = StoredGroupName("Quartet", 300uL)))
    }

    // --- 2. store codec ---------------------------------------------------

    @Test
    fun `the store row round-trips, including a cleared name and a 64-bit stamp`() {
        for (row in listOf(StoredGroupName("Trio", 1_758_000_000_000uL), StoredGroupName("", ULong.MAX_VALUE), StoredGroupName("a \"quoted\" name", 0uL))) {
            assertEquals(row, decodeStoredGroupName(encodeStoredGroupName(row)))
        }
    }

    @Test
    fun `garbage reads as absent`() {
        assertNull(decodeStoredGroupName(null))
        assertNull(decodeStoredGroupName(""))
        assertNull(decodeStoredGroupName("not json"))
        assertNull(decodeStoredGroupName("""{"name":"x"}"""))
        assertNull(decodeStoredGroupName("""{"name":"x","updatedAtMs":"-1"}"""))
    }

    // --- 3. wire peek + foreground classification -------------------------

    private val domain: ByteArray = "lakko/group-name/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

    /** `domain || version || updated_at_ms(8) || name_len || name`. */
    private fun envelope(name: String = "Trio"): ByteArray {
        val n = name.toByteArray(Charsets.UTF_8)
        return domain + byteArrayOf(0x01) + ByteArray(8) { if (it == 7) 42 else 0 } + byteArrayOf(n.size.toByte()) + n
    }

    private fun lossyParseBody(bytes: ByteArray): ParsedBody = ParsedBody(
        kind = MessageBodyKind.CHAT,
        text = String(bytes, Charsets.UTF_8),
        attachments = emptyList(),
        target = null,
        emoji = null,
        isTyping = null,
    )

    private val decoded = FfiGroupName(name = "Trio", updatedAtMs = 42uL)

    private fun classify(
        payload: ByteArray,
        decodeGroupName: (ByteArray) -> FfiGroupName?,
        senderLeaf: UInt? = 3u,
    ): IncomingChatEvent = toEvent(
        messageId = 7L,
        createdAt = "2026-09-17T00:00:00Z",
        processed = ProcessedMessage.Application(payload, senderLeaf, null),
        resolveSender = { leaf -> "KEY_OF_LEAF_$leaf" },
        resolveLeafSignatureKey = { null },
        isCommitterBeacon = { false },
        parseBody = ::lossyParseBody,
        decodeProfile = { null },
        decodeGroupName = decodeGroupName,
    )

    @Test
    fun `the peek is 20 bytes and a complete classification`() {
        assertEquals(20, domain.size)
        assertTrue(looksLikeGroupNameEnvelope(envelope()))
        assertTrue(looksLikeGroupNameEnvelope(domain))
        assertFalse(looksLikeGroupNameEnvelope(domain.copyOf(19)))
        assertFalse(looksLikeGroupNameEnvelope("""{"v":1,"text":"lakko/group-name/v1"}""".toByteArray()))
        assertFalse(looksLikeGroupNameEnvelope("lakko/group-name/v1 is the domain".toByteArray()))
        assertFalse(looksLikeGroupNameEnvelope("lakko/profile/v1".toByteArray() + byteArrayOf(0)))
    }

    @Test
    fun `a group-name envelope classifies as a GroupName event bound to the resolved sender key`() {
        val event = classify(envelope(), decodeGroupName = { decoded })
        assertTrue("expected GroupName, got $event", event is IncomingChatEvent.GroupName)
        event as IncomingChatEvent.GroupName
        assertEquals(decoded, event.groupName)
        assertEquals("KEY_OF_LEAF_3", event.senderKey)
        assertEquals(7L, event.messageId)
    }

    @Test
    fun `a malformed group-name envelope is a no-op event, never a bubble`() {
        val event = classify(envelope(), decodeGroupName = { null })
        assertTrue("expected GroupName, got $event", event is IncomingChatEvent.GroupName)
        assertNull((event as IncomingChatEvent.GroupName).groupName)
    }

    @Test
    fun `an unattributable sender still never becomes a bubble`() {
        val event = classify(envelope(), decodeGroupName = { decoded }, senderLeaf = null)
        assertTrue(event is IncomingChatEvent.GroupName)
        assertNull((event as IncomingChatEvent.GroupName).senderKey)
    }

    @Test
    fun `a chat message whose text mentions the domain is still a chat message`() {
        val chat = """{"v":1,"text":"lakko/group-name/v1"}""".toByteArray()
        val event = classify(chat, decodeGroupName = { error("must not be called") })
        assertTrue(event is IncomingChatEvent.Application)
    }

    // --- 4. background suppression --------------------------------------

    @Test
    fun `the background push filter suppresses a group-name envelope`() {
        assertTrue(isServiceEnvelope(envelope(), { false }, { false }, 27))
        assertTrue(isServiceEnvelope(envelope("Tr io"), { false }, { false }, 27))
        assertFalse(isServiceEnvelope("""{"v":1,"text":"hi"}""".toByteArray(), { false }, { false }, 27))
    }
}
