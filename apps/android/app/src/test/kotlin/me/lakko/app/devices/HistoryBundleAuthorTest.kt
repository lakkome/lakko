// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.chat.PersistedMessage
import me.lakko.app.chat.ReentryRouteKind
import me.lakko.app.chat.reentryBundleRow
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0091 walk 3, W3-R3 — a bundled message keeps its author. Seen on a
 * phone: the messages a gap bundle brought back showed a "?" avatar and no
 * name, while live messages of the same person showed the name, because the
 * placeholder carried no author. Placeholder version 6 carries it; this tests
 * what this client writes and reads (the host JVM loads no native core, so
 * the codec itself is tested in Rust).
 */
class HistoryBundleAuthorTest {
    private val owner = ByteArray(32) { 0x11 }
    private val ownerB64 = java.util.Base64.getEncoder().encodeToString(owner)
    private val me = ByteArray(32) { 0x22 }
    private val meB64 = java.util.Base64.getEncoder().encodeToString(me)

    private fun row(id: Long, direction: String, author: String? = null) =
        PersistedMessage(id, direction, "text $id", emptyList(), "2026-11-20T12:00:00Z", authorKeyB64 = author)

    // -- sender ------------------------------------------------------------

    @Test
    fun `an incoming row carries the key it was attributed to`() {
        val p = HistoryBundleCodec.placeholderOf(row(1, "incoming", ownerB64), ownAuthorKeyB64 = meB64)
        assertArrayEquals(owner, p.authorKey)
    }

    @Test
    fun `an outgoing row carries this account's own key, else the key it was stamped with`() {
        // The own key in the conversation wins over the stamp.
        assertArrayEquals(me, HistoryBundleCodec.placeholderOf(row(2, "outgoing", ownerB64), meB64).authorKey)
        // No own key known: the key the sent row was stamped with.
        assertArrayEquals(me, HistoryBundleCodec.placeholderOf(row(3, "outgoing", meB64)).authorKey)
        // Neither: none.
        assertNull(HistoryBundleCodec.placeholderOf(row(4, "outgoing")).authorKey)
    }

    @Test
    fun `a system row or a malformed key carries none`() {
        assertNull(HistoryBundleCodec.placeholderOf(row(5, "system", ownerB64), meB64).authorKey)
        assertNull(HistoryBundleCodec.placeholderOf(row(6, "incoming", "not base64!")).authorKey)
        val short = java.util.Base64.getEncoder().encodeToString(ByteArray(16))
        assertNull(HistoryBundleCodec.placeholderOf(row(7, "incoming", short)).authorKey)
    }

    // -- pairing receiver --------------------------------------------------

    @Test
    fun `the pairing receiver gives an incoming row its author, direction as sent`() {
        val incoming = HistoryBundleCodec.persistedOf(HistoryBundleCodec.placeholderOf(row(8, "incoming", ownerB64)))
        assertEquals("incoming", incoming.direction)
        assertEquals(ownerB64, incoming.authorKeyB64)
        val outgoing = HistoryBundleCodec.persistedOf(HistoryBundleCodec.placeholderOf(row(9, "outgoing", meB64)))
        assertEquals("outgoing", outgoing.direction)
        assertNull(outgoing.authorKeyB64)
    }

    // -- gap bundle receiver -----------------------------------------------

    /** What a gap bundle row turns into on the device that came back. */
    private fun received(sent: PersistedMessage, senderOwnKey: String?, receiverOwnKey: String, route: ReentryRouteKind): PersistedMessage {
        val p = HistoryBundleCodec.placeholderOf(sent, senderOwnKey)
        return reentryBundleRow(HistoryBundleCodec.persistedOf(p), HistoryBundleCodec.authorKeyB64Of(p), receiverOwnKey, route)
    }

    @Test
    fun `the owner's own messages reach a member as the owner's, named like a live row`() {
        // The owner re-admits by the knock: its outgoing rows are incoming here,
        // authored by the owner (the live row's author key).
        val r = received(row(10, "outgoing", ownerB64), senderOwnKey = ownerB64, receiverOwnKey = meB64, route = ReentryRouteKind.KNOCK)
        assertEquals("incoming", r.direction)
        assertEquals(ownerB64, r.authorKeyB64)
    }

    @Test
    fun `the member's own messages come back as its own`() {
        // In the owner's log the member's message is incoming, authored by the member.
        val r = received(row(11, "incoming", meB64), senderOwnKey = ownerB64, receiverOwnKey = meB64, route = ReentryRouteKind.KNOCK)
        assertEquals("outgoing", r.direction)
        assertEquals(meB64, r.authorKeyB64)
    }

    @Test
    fun `a third person's message keeps that person`() {
        val third = java.util.Base64.getEncoder().encodeToString(ByteArray(32) { 0x33 })
        val r = received(row(12, "incoming", third), senderOwnKey = ownerB64, receiverOwnKey = meB64, route = ReentryRouteKind.MAILBOX)
        assertEquals("incoming", r.direction)
        assertEquals(third, r.authorKeyB64)
    }

    @Test
    fun `with no author the old route rule decides, and a system row stays one`() {
        val unknown = received(row(13, "outgoing"), senderOwnKey = null, receiverOwnKey = meB64, route = ReentryRouteKind.MAILBOX)
        assertEquals("incoming", unknown.direction)
        assertNull(unknown.authorKeyB64)
        val sameAccount = received(row(14, "outgoing"), senderOwnKey = null, receiverOwnKey = meB64, route = ReentryRouteKind.OWN_DEVICE)
        assertEquals("outgoing", sameAccount.direction)
        val system = received(row(15, "system", ownerB64), senderOwnKey = ownerB64, receiverOwnKey = meB64, route = ReentryRouteKind.KNOCK)
        assertEquals("system", system.direction)
        assertNull(system.authorKeyB64)
    }
}
