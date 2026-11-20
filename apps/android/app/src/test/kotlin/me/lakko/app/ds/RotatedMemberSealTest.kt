// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0074 section 3.7 (2026-11-20) — an owner appoints a member that rotated
 * its account keys, and that member opens what the owner sealed.
 *
 * The host JVM has no native core, so the link verifier is a fake that
 * behaves like `verifyConversationLink`: it answers the conversation key only
 * when the link's current key is the trusted one, and throws otherwise.
 */
class RotatedMemberSealTest {
    private val conversation = ByteArray(32) { 0x11 }
    private val current = ByteArray(32) { 0x22 }
    private val other = ByteArray(32) { 0x33 }

    /** A fake link: `current(32) ‖ conversation(32)`. */
    private fun link(conv: ByteArray, cur: ByteArray): ByteArray = cur + conv

    private val verify: (ByteArray, ByteArray) -> ByteArray = { l, trusted ->
        require(l.size == 64) { "malformed" }
        require(l.copyOfRange(0, 32).contentEquals(trusted)) { "not signed by the trusted key" }
        l.copyOfRange(32, 64)
    }

    @Test
    fun `an unrotated member is accepted on the served key alone`() {
        assertTrue(bundleServesRosterKey(conversation, null, conversation, verify))
    }

    @Test
    fun `a rotated member is accepted when the link names the roster key`() {
        assertTrue(bundleServesRosterKey(current, link(conversation, current), conversation, verify))
    }

    @Test
    fun `a rotated member without a link is refused`() {
        assertFalse(bundleServesRosterKey(current, null, conversation, verify))
        assertFalse(bundleServesRosterKey(current, ByteArray(0), conversation, verify))
    }

    @Test
    fun `a link that does not verify against the served key is refused`() {
        assertFalse(bundleServesRosterKey(current, link(conversation, other), conversation, verify))
        assertFalse(bundleServesRosterKey(current, ByteArray(7), conversation, verify))
    }

    @Test
    fun `a link naming another conversation key is refused`() {
        assertFalse(bundleServesRosterKey(current, link(other, current), conversation, verify))
    }

    @Test
    fun `a malformed served key is refused`() {
        assertFalse(bundleServesRosterKey(ByteArray(31), null, conversation, verify))
    }

    @Test
    fun `the receiver opens with its own key first`() {
        val calls = mutableListOf<String>()
        val opened = openSealedToOwnAccount(
            conversation = "conv",
            current = "cur",
            openOwn = { a -> calls += "own:$a"; byteArrayOf(1) },
            openSealedTo = { a, c -> calls += "sealedTo:$a:$c"; byteArrayOf(2) },
        )
        assertArrayEquals(byteArrayOf(1), opened)
        assertEquals(listOf("own:conv"), calls)
    }

    @Test
    fun `the receiver opens an envelope addressed to its conversation key sealed to its current key`() {
        val opened = openSealedToOwnAccount(
            conversation = "conv",
            current = "cur",
            openOwn = { _ -> error("other X25519 key") },
            openSealedTo = { a, c ->
                assertEquals("conv", a)
                assertEquals("cur", c)
                byteArrayOf(2)
            },
        )
        assertArrayEquals(byteArrayOf(2), opened)
    }

    @Test
    fun `an unrotated receiver tries nothing else`() {
        var sealedToCalled = false
        assertNull(
            openSealedToOwnAccount(
                conversation = "conv",
                current = null,
                openOwn = { _ -> error("does not open") },
                openSealedTo = { _, _ -> sealedToCalled = true; byteArrayOf(9) },
            ),
        )
        assertFalse(sealedToCalled)
    }

    @Test
    fun `neither key opening is null`() {
        assertNull(
            openSealedToOwnAccount(
                conversation = "conv",
                current = "cur",
                openOwn = { _ -> error("no") },
                openSealedTo = { _, _ -> error("no") },
            ),
        )
    }
}
