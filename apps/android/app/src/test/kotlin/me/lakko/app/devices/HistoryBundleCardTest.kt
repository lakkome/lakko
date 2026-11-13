// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.chat.PersistedForward
import me.lakko.app.chat.PersistedMessage
import me.lakko.app.chat.PersistedReply
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The history placeholder (version 3) carries the channel invitation card,
 * the reply and the forward. Seen live 2026-11-13: an invitation the old
 * device drew as a card reached the new device as its bare `/g/` link. The
 * host JVM loads no native core, so the mapping to and from the placeholder
 * record is what is tested here; the codec itself is tested in Rust.
 */
class HistoryBundleCardTest {
    private val card = ByteArray(400) { 0x5A }
    private val cardB64 = java.util.Base64.getEncoder().encodeToString(card)
    private val originHex = "42".repeat(32)

    private fun row(id: Long, text: String) =
        PersistedMessage(id, "outgoing", text, emptyList(), "2026-11-13T12:00:00Z")

    @Test
    fun `the card, the reply and the forward go into the placeholder`() {
        val m = row(1, "https://lakko.me/g/abcdefgh#secret").copy(
            channelInviteB64 = cardB64,
            reply = PersistedReply(target = 10, author = "Ann", excerpt = "come along"),
            forward = PersistedForward(author = "Bob", originGroupIdHex = originHex, originMessageId = 77),
        )
        val p = HistoryBundleCodec.placeholderOf(m)
        assertArrayEquals(card, p.channelInvite)
        assertEquals(10L, p.reply?.target)
        assertEquals("Ann", p.reply?.author)
        assertEquals("come along", p.reply?.excerpt)
        assertEquals("Bob", p.forward?.author)
        assertArrayEquals(ByteArray(32) { 0x42 }, p.forward?.originGroupId)
        assertEquals(77L, p.forward?.originMessageId)

        val back = HistoryBundleCodec.persistedOf(p)
        assertEquals(cardB64, back.channelInviteB64)
        assertEquals(m.reply, back.reply)
        assertEquals(m.forward, back.forward)
    }

    @Test
    fun `a plain row carries none of them and a deleted row no card`() {
        val plain = HistoryBundleCodec.placeholderOf(row(2, "hi"))
        assertNull(plain.channelInvite)
        assertNull(plain.reply)
        assertNull(plain.forward)
        val back = HistoryBundleCodec.persistedOf(plain)
        assertNull(back.channelInviteB64)
        assertNull(back.reply)
        assertNull(back.forward)

        val deleted = row(3, "").copy(deleted = true, channelInviteB64 = cardB64)
        assertNull(HistoryBundleCodec.placeholderOf(deleted).channelInvite)
    }

    @Test
    fun `half a forward origin is not carried`() {
        val m = row(4, "look").copy(
            forward = PersistedForward(author = "Bob", originGroupIdHex = originHex, originMessageId = null),
        )
        val f = HistoryBundleCodec.placeholderOf(m).forward
        assertEquals("Bob", f?.author)
        assertNull(f?.originGroupId)
        assertNull(f?.originMessageId)
    }
}
