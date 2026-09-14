// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.calls.GroupCallWireFixtures
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** [looksLikeCallSignaling] — the one tag sniff the foreground timeline and the
 *  background notification path share. */
class CallSignalingSniffTest {

    @Test
    fun `every oneof body tag is recognised, including the two byte field 16`() {
        for (tag in listOf(0x52, 0x5A, 0x62, 0x6A)) {
            assertTrue(looksLikeCallSignaling(byteArrayOf(tag.toByte(), 0x04, 0x01, 0x02, 0x03, 0x04)))
        }
        assertTrue(looksLikeCallSignaling(GroupCallWireFixtures.groupStart()))
        assertTrue(looksLikeCallSignaling(GroupCallWireFixtures.groupJoin()))
        assertTrue(looksLikeCallSignaling(GroupCallWireFixtures.groupLeave()))
    }

    @Test
    fun `a group tag without the call id header is not call signaling`() {
        assertFalse(looksLikeCallSignaling("rabbit".toByteArray(Charsets.UTF_8)))
        assertFalse(looksLikeCallSignaling("zebra".toByteArray(Charsets.UTF_8)))
        // Truncated right after the tag and length.
        assertFalse(looksLikeCallSignaling(byteArrayOf(0x72, 0x12, 0x0A)))
        // 0x82 followed by anything but 0x01 is not field 16.
        val leave = GroupCallWireFixtures.groupLeave()
        assertFalse(looksLikeCallSignaling(leave.copyOf().also { it[1] = 0x02 }))
        // A call id field of the wrong length.
        val start = GroupCallWireFixtures.groupStart()
        assertFalse(looksLikeCallSignaling(start.copyOf().also { it[3] = 0x0F }))
    }

    @Test
    fun `chat envelopes and empty payloads are not call signaling`() {
        assertFalse(looksLikeCallSignaling("""{"v":1,"text":"hi"}""".toByteArray(Charsets.UTF_8)))
        assertFalse(looksLikeCallSignaling(ByteArray(0)))
        assertFalse(looksLikeCallSignaling(byteArrayOf(0x82.toByte())))
    }
}
