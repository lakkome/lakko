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
import uniffi.lakko_ffi_uniffi.FfiPinOp
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.Role

/**
 * ADR-0052 decision 3 / UX 4.1 — pins, every rule on the host JVM.
 *
 *  1. The receive verdict: owner / admin / member / no table / one-to-one /
 *     unreadable table / unresolvable sender / equal stamp / stale pin after
 *     a later unpin.
 *  2. The local half of the same rule (who may set a shared pin here).
 *  3. The two store codecs round-trip and read garbage as empty.
 *  4. The bar: shared before personal, newest first, unheld / deleted /
 *     unpinned skipped, both-scope shown once, advance wraps.
 *  5. The row and the sheet by authority and state, with the one-to-one copy.
 *  6. The wire peek, the foreground classification and the background
 *     suppression.
 *
 * The FFI order is a stand-in (`strictly greater`), matching the documented
 * contract of `pinIsNewer`.
 */
class PinTest {

    private val owner = ByteArray(32) { 1 }
    private val admin = ByteArray(32) { 2 }
    private val member = ByteArray(32) { 3 }
    private val table = GroupPushRouting.OwnerLookup.Known(owner = owner, admins = listOf(admin))
    private val strictlyNewer: (ULong, ULong) -> Boolean = { a, b -> a > b }

    private fun verdict(
        sender: ByteArray?,
        table: GroupPushRouting.OwnerLookup = this.table,
        oneToOne: Boolean = false,
        incomingMs: ULong = 200uL,
        stored: StoredPin? = StoredPin(pinned = true, updatedAtMs = 100uL),
    ) = pinVerdict(sender, table, oneToOne, incomingMs, stored, strictlyNewer)

    // --- 1. receive verdict ------------------------------------------------

    @Test
    fun `owner and admin may pin, a plain member may not`() {
        assertEquals(PinVerdict.ACCEPT_OWNER, verdict(owner))
        assertEquals(PinVerdict.ACCEPT_ADMIN, verdict(admin))
        assertEquals(PinVerdict.REJECT_NOT_AUTHORIZED, verdict(member))
        assertEquals(PinVerdict.REJECT_NOT_AUTHORIZED, verdict(ByteArray(32) { 9 }))
    }

    @Test
    fun `an unresolvable sender or an unreadable role table fails closed`() {
        assertEquals(PinVerdict.REJECT_UNVERIFIABLE, verdict(null))
        assertEquals(PinVerdict.REJECT_UNVERIFIABLE, verdict(owner, table = GroupPushRouting.OwnerLookup.Unavailable))
    }

    @Test
    fun `a group with no role table accepts a newer action from any member`() {
        assertEquals(PinVerdict.ACCEPT_NO_ROLE_TABLE, verdict(member, table = GroupPushRouting.OwnerLookup.NoRoleTable))
        assertEquals(PinVerdict.ACCEPT_NO_ROLE_TABLE, verdict(null, table = GroupPushRouting.OwnerLookup.NoRoleTable))
    }

    @Test
    fun `either participant of a one-to-one may pin, whatever the table says`() {
        // `Group::create` seeds an owner table even for two people; the other
        // participant is a plain member of it and must still be accepted.
        assertEquals(PinVerdict.ACCEPT_ONE_TO_ONE, verdict(member, oneToOne = true))
        assertEquals(PinVerdict.ACCEPT_ONE_TO_ONE, verdict(owner, oneToOne = true))
        assertEquals(PinVerdict.ACCEPT_ONE_TO_ONE, verdict(null, oneToOne = true))
        assertEquals(
            PinVerdict.ACCEPT_ONE_TO_ONE,
            verdict(member, table = GroupPushRouting.OwnerLookup.Unavailable, oneToOne = true),
        )
        // ...and the order still applies there.
        assertEquals(PinVerdict.REJECT_STALE, verdict(member, oneToOne = true, incomingMs = 100uL))
    }

    @Test
    fun `the stamp decides per message, equal is not newer`() {
        assertEquals(PinVerdict.REJECT_STALE, verdict(owner, incomingMs = 100uL))
        assertEquals(PinVerdict.REJECT_STALE, verdict(owner, incomingMs = 99uL))
        assertEquals(PinVerdict.ACCEPT_OWNER, verdict(owner, incomingMs = 101uL))
        // Authority runs BEFORE the order: a member's newer action is still refused.
        assertEquals(PinVerdict.REJECT_NOT_AUTHORIZED, verdict(member, incomingMs = 999uL))
    }

    @Test
    fun `the first action for a message needs no order`() {
        assertEquals(PinVerdict.ACCEPT_OWNER, verdict(owner, stored = null))
        assertEquals(PinVerdict.REJECT_NOT_AUTHORIZED, verdict(member, stored = null))
    }

    @Test
    fun `a late pin never undoes a later unpin, because the unpin is stored`() {
        // The unpin landed at 300 and is kept as a record with pinned = false.
        val unpinned = StoredPin(pinned = false, updatedAtMs = 300uL)
        // A pin stamped 250 arriving afterwards (a relay, a re-send) is stale.
        assertEquals(PinVerdict.REJECT_STALE, verdict(owner, incomingMs = 250uL, stored = unpinned))
        // A growth re-send of the ORIGINAL pin (stamp 100) is stale too.
        assertEquals(PinVerdict.REJECT_STALE, verdict(admin, incomingMs = 100uL, stored = unpinned))
        // A genuinely newer pin re-pins it.
        assertEquals(PinVerdict.ACCEPT_ADMIN, verdict(admin, incomingMs = 301uL, stored = unpinned))
    }

    // --- 2. local authority --------------------------------------------------

    @Test
    fun `who may set a shared pin here mirrors the receive gate`() {
        assertTrue(mayPinForEveryone(Role.OWNER, hasRoleTable = true, isOneToOne = false))
        assertTrue(mayPinForEveryone(Role.ADMIN, hasRoleTable = true, isOneToOne = false))
        assertFalse(mayPinForEveryone(Role.MEMBER, hasRoleTable = true, isOneToOne = false))
        assertFalse(mayPinForEveryone(null, hasRoleTable = true, isOneToOne = false))
        assertTrue(mayPinForEveryone(Role.MEMBER, hasRoleTable = false, isOneToOne = false))
        assertTrue(mayPinForEveryone(null, hasRoleTable = false, isOneToOne = false))
        assertTrue(mayPinForEveryone(Role.MEMBER, hasRoleTable = true, isOneToOne = true))
    }

    // --- 3. store codecs -----------------------------------------------------

    @Test
    fun `the shared store round-trips pins and unpins with 64-bit stamps`() {
        val map = mapOf(
            7L to StoredPin(pinned = true, updatedAtMs = 1_758_000_000_000uL),
            9L to StoredPin(pinned = false, updatedAtMs = ULong.MAX_VALUE),
            -3L to StoredPin(pinned = true, updatedAtMs = 0uL),
        )
        assertEquals(map, decodeSharedPins(encodeSharedPins(map)))
        assertEquals(emptyMap<Long, StoredPin>(), decodeSharedPins(encodeSharedPins(emptyMap())))
    }

    @Test
    fun `the personal store round-trips and garbage reads as empty`() {
        val map = mapOf(4L to 10uL, 8L to ULong.MAX_VALUE)
        assertEquals(map, decodePersonalPins(encodePersonalPins(map)))
        assertEquals(emptyMap<Long, ULong>(), decodePersonalPins(null))
        assertEquals(emptyMap<Long, ULong>(), decodePersonalPins("not json"))
        assertEquals(emptyMap<Long, StoredPin>(), decodeSharedPins(""))
        assertEquals(emptyMap<Long, StoredPin>(), decodeSharedPins("""{"a":1}"""))
        // A malformed row is skipped, the others survive.
        assertEquals(
            mapOf(1L to StoredPin(true, 5uL)),
            decodeSharedPins("""[{"messageId":"1","pinned":true,"updatedAtMs":"5"},{"messageId":"x","pinned":true,"updatedAtMs":"6"}]"""),
        )
    }

    // --- 4. the bar ----------------------------------------------------------

    private fun message(id: Long, text: String = "m$id", deleted: Boolean = false) = ChatMessage(
        messageId = id,
        direction = Direction.Incoming,
        text = text,
        createdAt = "2026-09-26T00:00:00Z",
        authorKey = "a",
        deleted = deleted,
    )

    @Test
    fun `shared pins come before personal, each newest pin first`() {
        val shared = mapOf(
            1L to StoredPin(true, 100uL),
            2L to StoredPin(true, 300uL),
            3L to StoredPin(false, 400uL), // an unpin: not shown
        )
        val personal = mapOf(4L to 50uL, 5L to 900uL)
        val entries = pinBarEntries(shared, personal, (1L..5L).map { message(it) })
        assertEquals(
            listOf(
                PinBarEntry(2L, PinScope.SHARED),
                PinBarEntry(1L, PinScope.SHARED),
                PinBarEntry(5L, PinScope.PERSONAL),
                PinBarEntry(4L, PinScope.PERSONAL),
            ),
            entries,
        )
    }

    @Test
    fun `a message pinned in both scopes is shown once, as shared`() {
        val entries = pinBarEntries(
            shared = mapOf(1L to StoredPin(true, 100uL)),
            personal = mapOf(1L to 200uL),
            messages = listOf(message(1L)),
        )
        assertEquals(listOf(PinBarEntry(1L, PinScope.SHARED)), entries)
    }

    @Test
    fun `a pin whose message is not held or is deleted is skipped, not shown`() {
        val entries = pinBarEntries(
            shared = mapOf(1L to StoredPin(true, 100uL), 2L to StoredPin(true, 200uL)),
            personal = mapOf(3L to 5uL, 4L to 6uL),
            // 1 is not held; 2 is deleted; 3 is held; 4 is not held.
            messages = listOf(message(2L, deleted = true), message(3L)),
        )
        assertEquals(listOf(PinBarEntry(3L, PinScope.PERSONAL)), entries)
        assertTrue(pinBarEntries(emptyMap(), emptyMap(), listOf(message(1L))).isEmpty())
    }

    @Test
    fun `a tap advances to the next older pin and wraps past the oldest`() {
        assertEquals(1, advancePinIndex(0, 3))
        assertEquals(2, advancePinIndex(1, 3))
        assertEquals(0, advancePinIndex(2, 3))
        assertEquals(0, advancePinIndex(0, 1))
        assertEquals(0, advancePinIndex(5, 0))
        // A remembered position past a list that shrank is clamped.
        assertEquals(1, clampPinIndex(4, 2))
        assertEquals(0, clampPinIndex(4, 0))
    }

    @Test
    fun `the excerpt is the text, else the reply quote's one-word stand-in`() {
        assertEquals("hello there", pinExcerpt(message(1L, text = "hello  there")))
        assertEquals(REPLY_FALLBACK_EXCERPT, pinExcerpt(message(2L, text = "")))
    }

    // --- 5. the row and the sheet -------------------------------------------

    @Test
    fun `an unpinned message offers Pin for me always and Pin for everyone by standing`() {
        assertEquals(
            PinMenu("Pin", listOf(PinAction.PIN_ME, PinAction.PIN_EVERYONE)),
            pinMenuFor(sharedPinned = false, personalPinned = false, mayShared = true),
        )
        assertEquals(
            PinMenu("Pin", listOf(PinAction.PIN_ME)),
            pinMenuFor(sharedPinned = false, personalPinned = false, mayShared = false),
        )
    }

    @Test
    fun `a message pinned in one scope reads Unpin and acts at once`() {
        assertEquals(
            PinMenu("Unpin", listOf(PinAction.UNPIN_ME)),
            pinMenuFor(sharedPinned = false, personalPinned = true, mayShared = true),
        )
        assertEquals(
            PinMenu("Unpin", listOf(PinAction.UNPIN_EVERYONE)),
            pinMenuFor(sharedPinned = true, personalPinned = false, mayShared = true),
        )
    }

    @Test
    fun `a plain member holding an admin's pin sees Pin for me alone, then Unpin for me only`() {
        assertEquals(
            PinMenu("Pin", listOf(PinAction.PIN_ME)),
            pinMenuFor(sharedPinned = true, personalPinned = false, mayShared = false),
        )
        assertEquals(
            PinMenu("Unpin", listOf(PinAction.UNPIN_ME)),
            pinMenuFor(sharedPinned = true, personalPinned = true, mayShared = false),
        )
    }

    @Test
    fun `a message pinned in both scopes asks which to unpin`() {
        assertEquals(
            PinMenu("Unpin", listOf(PinAction.UNPIN_ME, PinAction.UNPIN_EVERYONE)),
            pinMenuFor(sharedPinned = true, personalPinned = true, mayShared = true),
        )
    }

    @Test
    fun `the sheet copy names both in a one-to-one and everyone in a group`() {
        assertEquals("Pin for both", pinActionLabel(PinAction.PIN_EVERYONE, oneToOne = true))
        assertEquals("Pin for everyone", pinActionLabel(PinAction.PIN_EVERYONE, oneToOne = false))
        assertEquals("Unpin for both", pinActionLabel(PinAction.UNPIN_EVERYONE, oneToOne = true))
        assertEquals("Pin for me", pinActionLabel(PinAction.PIN_ME, oneToOne = true))
        assertEquals("Only on this device", pinActionSubline(PinAction.PIN_ME))
        assertEquals("Every member sees it", pinActionSubline(PinAction.UNPIN_EVERYONE))
    }

    // --- 6. wire peek, foreground classification, background suppression ----

    private val domain: ByteArray = "lakko/pin/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

    /** `domain || version || updated_at_ms(8) || action || message_id(8)`. */
    private fun envelope(): ByteArray =
        domain + byteArrayOf(0x01) + ByteArray(8) { if (it == 7) 42 else 0 } + byteArrayOf(0x01) + ByteArray(8) { if (it == 7) 7 else 0 }

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

    private val decoded = FfiPinOp(messageId = 7L, pinned = true, updatedAtMs = 42uL)

    private fun classify(
        payload: ByteArray,
        decodePin: (ByteArray) -> FfiPinOp?,
        senderLeaf: UInt? = 3u,
    ): IncomingChatEvent = toEvent(
        messageId = 11L,
        createdAt = "2026-09-26T00:00:00Z",
        processed = ProcessedMessage.Application(payload, senderLeaf, null),
        resolveSender = { leaf -> "KEY_OF_LEAF_$leaf" },
        resolveLeafSignatureKey = { null },
        isCommitterBeacon = { false },
        parseBody = ::lossyParseBody,
        decodeProfile = { null },
        decodeGroupName = { null },
        isProfileRequest = { false },
        decodePin = decodePin,
    )

    @Test
    fun `the peek is 13 bytes and a complete classification`() {
        assertEquals(13, domain.size)
        assertEquals(31, envelope().size)
        assertTrue(looksLikePinEnvelope(envelope()))
        assertTrue(looksLikePinEnvelope(domain))
        assertFalse(looksLikePinEnvelope(domain.copyOf(12)))
        assertFalse(looksLikePinEnvelope("""{"v":1,"text":"lakko/pin/v1"}""".toByteArray()))
        assertFalse(looksLikePinEnvelope("lakko/pin/v1 is the domain".toByteArray()))
        assertFalse(looksLikePinEnvelope("lakko/profile/v1".toByteArray() + byteArrayOf(0)))
    }

    @Test
    fun `a pin envelope classifies as a Pin event bound to the resolved sender key`() {
        val event = classify(envelope(), decodePin = { decoded })
        assertTrue("expected Pin, got $event", event is IncomingChatEvent.Pin)
        event as IncomingChatEvent.Pin
        assertEquals(decoded, event.pin)
        assertEquals("KEY_OF_LEAF_3", event.senderKey)
        assertEquals(11L, event.messageId)
    }

    @Test
    fun `a malformed pin envelope is a no-op event, never a bubble`() {
        val event = classify(envelope(), decodePin = { null })
        assertTrue("expected Pin, got $event", event is IncomingChatEvent.Pin)
        assertNull((event as IncomingChatEvent.Pin).pin)
    }

    @Test
    fun `an unattributable sender still never becomes a bubble`() {
        val event = classify(envelope(), decodePin = { decoded }, senderLeaf = null)
        assertTrue(event is IncomingChatEvent.Pin)
        assertNull((event as IncomingChatEvent.Pin).senderKey)
    }

    @Test
    fun `a chat message whose text mentions the domain is still a chat message`() {
        val chat = """{"v":1,"text":"lakko/pin/v1"}""".toByteArray()
        val event = classify(chat, decodePin = { error("must not be called") })
        assertTrue(event is IncomingChatEvent.Application)
    }

    @Test
    fun `the background push filter suppresses a pin envelope`() {
        assertTrue(isServiceEnvelope(envelope(), { false }, { false }, 27))
        assertFalse(isServiceEnvelope("""{"v":1,"text":"pinned"}""".toByteArray(), { false }, { false }, 27))
    }
}
