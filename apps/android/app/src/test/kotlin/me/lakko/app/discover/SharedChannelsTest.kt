// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import me.lakko.app.chat.IncomingChatEvent
import me.lakko.app.chat.toEvent
import me.lakko.app.push.isServiceEnvelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage

/**
 * ADR-0059 section 6 on Android: the envelope peek, the foreground
 * classification (never a bubble), the background suppression, the per-member
 * strictly-newer filing, the share decision and the aggregation. Mirrors
 * `apps/web/src/discover/shared-channels.test.ts` and
 * `chat/client.shared-channels-envelope.test.ts`.
 */
class SharedChannelsTest {

    private fun id(n: Int) = n.toString().padStart(32, '0')
    private val domain = "lakko/shared-channels/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)
    private val newer: (ULong, ULong) -> Boolean = { a, b -> a > b }

    @Test
    fun `the peek is the NUL-terminated domain and a complete classification`() {
        assertEquals(25, domain.size)
        assertTrue(looksLikeSharedChannelsEnvelope(domain + byteArrayOf(1, 2, 3)))
        assertFalse(looksLikeSharedChannelsEnvelope(domain.copyOf(24)))
        assertFalse(looksLikeSharedChannelsEnvelope("""{"v":1,"text":"lakko/shared-channels/v1"}""".toByteArray()))
        assertFalse(looksLikeSharedChannelsEnvelope("lakko/pin/v1".toByteArray() + byteArrayOf(0)))
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

    private val list = SharedList(42uL, listOf(SharedChannel(id(1), "news", "News")))

    private fun classify(payload: ByteArray, decode: (ByteArray) -> SharedList?, leaf: UInt? = 3u): IncomingChatEvent =
        toEvent(
            messageId = 11L,
            createdAt = "2026-09-29T00:00:00Z",
            processed = ProcessedMessage.Application(payload, leaf, null),
            resolveSender = { l -> "KEY_$l" },
            resolveLeafSignatureKey = { null },
            isCommitterBeacon = { false },
            parseBody = ::lossyParseBody,
            decodeProfile = { null },
            decodeGroupName = { null },
            isProfileRequest = { false },
            decodePin = { null },
            decodeSharedChannels = decode,
        )

    @Test
    fun `a shared-channels envelope is its own event bound to the resolved sender key`() {
        val e = classify(domain + byteArrayOf(1), { list })
        assertTrue("got $e", e is IncomingChatEvent.SharedChannels)
        e as IncomingChatEvent.SharedChannels
        assertEquals(list, e.list)
        assertEquals("KEY_3", e.senderKey)
    }

    @Test
    fun `a malformed list is a no-op event, never a bubble, and unattributed stays unattributed`() {
        val bad = classify(domain + byteArrayOf(9), { null }) as IncomingChatEvent.SharedChannels
        assertNull(bad.list)
        val anon = classify(domain + byteArrayOf(1), { list }, leaf = null) as IncomingChatEvent.SharedChannels
        assertNull(anon.senderKey)
        val chat = classify("""{"v":1,"text":"lakko/shared-channels/v1"}""".toByteArray(), { list })
        assertFalse(chat is IncomingChatEvent.SharedChannels)
    }

    @Test
    fun `the background preview filter suppresses the envelope`() {
        assertTrue(isServiceEnvelope(domain + byteArrayOf(1), { false }, { false }, 27))
        assertFalse(isServiceEnvelope("""{"v":1,"text":"channels"}""".toByteArray(), { false }, { false }, 27))
    }

    @Test
    fun `a list is filed per member and replaced only by a strictly newer stamp`() {
        var row = SharedChannelsRow()
        val a = applyIncomingList(row, "g", "m1", list, newer)
        assertEquals(SharedChannelsVerdict.ACCEPT, a.verdict)
        row = a.row
        assertEquals(SharedChannelsVerdict.REJECT_STALE, applyIncomingList(row, "g", "m1", list, newer).verdict)
        assertEquals(SharedChannelsVerdict.REJECT_STALE,
            applyIncomingList(row, "g", "m1", list.copy(updatedAtMs = 1uL), newer).verdict)
        val b = applyIncomingList(row, "g", "m1", SharedList(43uL, emptyList()), newer)
        assertEquals(SharedChannelsVerdict.ACCEPT, b.verdict)
        assertEquals(emptyList<SharedChannel>(), b.row.received["g"]!!["m1"]!!.channels)
        assertEquals(SharedChannelsVerdict.REJECT_UNATTRIBUTED, applyIncomingList(row, "g", null, list, newer).verdict)
        assertEquals(SharedChannelsVerdict.REJECT_UNATTRIBUTED, applyIncomingList(row, "g", "", list, newer).verdict)
    }

    @Test
    fun `a fresh stamp is strictly after the last sent`() {
        assertEquals(100uL, freshListStamp(100, null))
        assertEquals(43uL, freshListStamp(10, list))
    }

    @Test
    fun `only subscribed listed channels with a canonical handle and a name are shared, in id order`() {
        val out = channelsToShare(
            listOf(
                ShareableChannel(id(3), subscribed = true, listed = true, handle = "tech", name = " Tech "),
                ShareableChannel(id(1), subscribed = true, listed = true, handle = "books", name = "Books"),
                ShareableChannel(id(2), subscribed = true, listed = false, handle = "secret", name = "Secret"),
                ShareableChannel(id(4), subscribed = false, listed = true, handle = "mine", name = "Mine"),
                ShareableChannel(id(5), subscribed = true, listed = true, handle = null, name = "No handle"),
                ShareableChannel(id(6), subscribed = true, listed = true, handle = "Bad Handle", name = "X"),
                ShareableChannel(id(7), subscribed = true, listed = true, handle = "empty", name = "  "),
            ),
        )
        assertEquals(listOf(SharedChannel(id(1), "books", "Books"), SharedChannel(id(3), "tech", "Tech")), out)
        val many = (0 until 40).map { ShareableChannel(id(100 + it), true, true, "c$it" + "xx", "C") }
        assertEquals(SHARED_CHANNELS_MAX, channelsToShare(many).size)
    }

    @Test
    fun `aggregation skips the person, dead groups, and takes the most common claim`() {
        val row = SharedChannelsRow(
            received = mapOf(
                "g1" to mapOf(
                    "me" to SharedList(1uL, listOf(SharedChannel(id(9), "own", "Own"))),
                    "a" to SharedList(1uL, listOf(SharedChannel(id(1), "news", "News"))),
                    "b" to SharedList(1uL, listOf(SharedChannel(id(1), "news", "News"))),
                    "c" to SharedList(1uL, listOf(SharedChannel(id(1), "fake", "Fake"))),
                ),
                "dead" to mapOf("d" to SharedList(1uL, listOf(SharedChannel(id(2), "gone", "Gone")))),
            ),
        )
        val out = aggregateShared(row, "me", setOf("g1"))
        assertEquals(1, out.size)
        assertEquals("news", out[0].handle)
        assertEquals("News", out[0].name)
        assertEquals(listOf("a", "b", "c"), out[0].byGroup["g1"])
    }

    @Test
    fun `the row codec round-trips and forgetting a group drops all of it`() {
        val row = SharedChannelsRow(
            received = mapOf("g" to mapOf("m" to list)),
            sent = mapOf("g" to SharedList(ULong.MAX_VALUE, emptyList())),
            shareOn = setOf("g", "h"),
        )
        assertEquals(row, decodeSharedChannelsRow(encodeSharedChannelsRow(row)))
        assertEquals(SharedChannelsRow(), decodeSharedChannelsRow("{"))
        assertEquals(SharedChannelsRow(), decodeSharedChannelsRow("""{"version":2}"""))
        val forgotten = forgetGroup(row, "g")
        assertEquals(SharedChannelsRow(shareOn = setOf("h")), forgotten)
    }
}
