// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.calls.OneToOneCallTarget
import me.lakko.app.calls.oneToOneCallEndsOnMove
import me.lakko.app.calls.oneToOneCallTarget
import me.lakko.app.reach.ContinuityMarker
import me.lakko.app.reach.CONTINUITY_KEY_TTL_MS
import me.lakko.app.reach.WantedContinuityKey
import me.lakko.app.reach.continuityKeyUsable
import me.lakko.app.reach.continuityKeyWanted
import me.lakko.app.reach.decodeContinuityMarker
import me.lakko.app.reach.encodeContinuityMarker
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0065 R.7 case 1 - the held send, the dead-session guard, the re-send
 *  marker, the call and the continuity key (mirror of the web tables). */
class HeldSendTest {
    @Test
    fun `a message into a dead one-to-one is held, an op is local, a group never is`() {
        assertEquals(SendRoute.HOLD, sendRoute(ConversationKind.Chat, "old"))
        assertEquals(SendRoute.SEND, sendRoute(ConversationKind.Chat, null))
        assertEquals(SendRoute.SEND, sendRoute(ConversationKind.Group, "old"))
        assertEquals(OpRoute.LOCAL_ONLY, opRoute(ConversationKind.Chat, "old"))
        assertEquals(OpRoute.SEND, opRoute(ConversationKind.Chat, null))
        assertEquals(OpRoute.SEND, opRoute(ConversationKind.Group, "old"))
        assertTrue(isDeadSession(ConversationKind.Chat, "old"))
        assertFalse(isDeadSession(ConversationKind.Group, "old"))
    }

    @Test
    fun `nothing goes into a dead session except the claim`() {
        assertTrue(publishAllowed(deadSession = false, kind = PublishKind.OTHER))
        assertFalse(publishAllowed(deadSession = true, kind = PublishKind.OTHER))
        assertTrue(publishAllowed(deadSession = true, kind = PublishKind.CLAIM))
    }

    @Test
    fun `the flush and the give-up`() {
        assertEquals(FlushVerdict.WAIT, flushVerdict(0, false, true, SendRoute.SEND))
        assertEquals(FlushVerdict.WAIT, flushVerdict(2, true, true, SendRoute.SEND))
        assertEquals(FlushVerdict.WAIT, flushVerdict(2, false, false, SendRoute.SEND))
        assertEquals(FlushVerdict.WAIT, flushVerdict(2, false, true, SendRoute.HOLD))
        assertEquals(FlushVerdict.FLUSH, flushVerdict(2, false, true, SendRoute.SEND))
        assertEquals(HeldVerdict.SEND, heldVerdict(SendRoute.SEND, 0, Long.MAX_VALUE))
        assertEquals(HeldVerdict.WAIT, heldVerdict(SendRoute.HOLD, 0, HELD_SEND_GIVE_UP_MS - 1))
        assertEquals(HeldVerdict.GIVE_UP, heldVerdict(SendRoute.HOLD, 0, HELD_SEND_GIVE_UP_MS))
    }

    @Test
    fun `a held reply flushed into another group keeps its quote and drops its jump target`() {
        assertNull(replyTargetForSend(null, "g", "g"))
        assertEquals(41L, replyTargetForSend(41, "g", "g"))
        assertEquals(REPLY_NO_JUMP_TARGET, replyTargetForSend(41, "old", "new"))
        assertEquals(REPLY_NO_JUMP_TARGET, replyTargetForSend(41, null, "new"))
        assertEquals(0L, REPLY_NO_JUMP_TARGET)
    }

    @Test
    fun `the identity check before a send is due at the arm's interval`() {
        assertTrue(identityCheckDue(null, 0, 600_000))
        assertFalse(identityCheckDue(1_000, 1_000 + 599_999, 600_000))
        assertTrue(identityCheckDue(1_000, 1_000 + 600_000, 600_000))
    }

    @Test
    fun `the re-send marker is believed only against a row this device holds`() {
        val marker = ResendOrigin("old-group", 7)
        fun v(
            m: ResendOrigin? = marker,
            origin: String? = "conv",
            older: Boolean = true,
            held: String? = "A",
            sender: String? = "A",
        ) = resendVerdict(m, "conv", origin, older, held, sender)
        assertEquals(ResendVerdict.SKIP, v())
        assertEquals(ResendVerdict.DRAW, v(m = null))
        assertEquals(ResendVerdict.DRAW, v(origin = null))
        assertEquals(ResendVerdict.DRAW, v(origin = "another"))
        assertEquals(ResendVerdict.DRAW, v(older = false))
        assertEquals(ResendVerdict.DRAW, v(held = null))
        assertEquals(ResendVerdict.DRAW, v(sender = null))
        assertEquals(ResendVerdict.DRAW, v(held = "B"))
        val canonical = { k: String -> if (k == "me-old") "me" else k }
        assertEquals("me", heldRowAuthor(HeldRowDirection.OUTGOING, null, "me-old", canonical))
        assertEquals("peer", heldRowAuthor(HeldRowDirection.INCOMING, "peer", "me", canonical))
        assertNull(heldRowAuthor(HeldRowDirection.INCOMING, "", "me", canonical))
        assertNull(heldRowAuthor(HeldRowDirection.SYSTEM, "peer", "me", canonical))
        assertNull(heldRowAuthor(null, "peer", "me", canonical))
        assertNull(heldRowAuthor(HeldRowDirection.OUTGOING, null, null, canonical))
    }

    @Test
    fun `the marker is carried as a whole pair or not at all`() {
        assertEquals(ResendOrigin("g", 4), resendOriginFrom("g", 4))
        assertNull(resendOriginFrom("g", null))
        assertNull(resendOriginFrom(null, 4))
        assertNull(resendOriginFrom("", 4))
    }

    @Test
    fun `the continuity key on the mailbox follows the wanted key`() {
        val a = me.lakko.app.reach.RegisteredContinuityKey("aa", 100)
        val b = me.lakko.app.reach.RegisteredContinuityKey("bb", 200)
        fun change(w: me.lakko.app.reach.RegisteredContinuityKey?, r: me.lakko.app.reach.RegisteredContinuityKey?) =
            me.lakko.app.reach.continuityChange(w, r)
        assertTrue(change(null, null).isEmpty)
        assertEquals(me.lakko.app.reach.ContinuityChange(null, "aa"), change(null, a))
        assertEquals(me.lakko.app.reach.ContinuityChange(a, null), change(a, null))
        assertTrue(change(a, a).isEmpty)
        assertEquals(me.lakko.app.reach.ContinuityChange(a.copy(expiresAtMs = 5), null), change(a.copy(expiresAtMs = 5), a))
        assertEquals(me.lakko.app.reach.ContinuityChange(b, "aa"), change(b, a))
    }

    @Test
    fun `a live one-to-one call ends when its conversation moves`() {
        assertFalse(oneToOneCallEndsOnMove(null, "old", "h"))
        assertTrue(oneToOneCallEndsOnMove("g", "old", "g"))
        assertTrue(oneToOneCallEndsOnMove("g", null, "h"))
        assertFalse(oneToOneCallEndsOnMove("g", null, "g"))
        assertFalse(oneToOneCallEndsOnMove("g", null, null))
    }

    @Test
    fun `a call asked for in a dead session dials the successor or does not start`() {
        assertEquals(OneToOneCallTarget.Dial("g"), oneToOneCallTarget("g", null, "g", null))
        assertEquals(OneToOneCallTarget.Dial("g"), oneToOneCallTarget("g", null, null, null))
        assertEquals(OneToOneCallTarget.Dial("h"), oneToOneCallTarget("g", null, "h", null))
        assertEquals(OneToOneCallTarget.Unavailable, oneToOneCallTarget("g", null, "h", "old2"))
        assertEquals(OneToOneCallTarget.Unavailable, oneToOneCallTarget("g", "old", "g", null))
        assertEquals(OneToOneCallTarget.Unavailable, oneToOneCallTarget("g", "old", null, null))
        assertEquals(OneToOneCallTarget.Dial("h"), oneToOneCallTarget("g", "old", "h", null))
        assertEquals(OneToOneCallTarget.Unavailable, oneToOneCallTarget("g", "old", "h", "old2"))
    }

    @Test
    fun `the continuity key is wanted and usable for thirty days from the signing`() {
        val marker = ContinuityMarker(keyGeneration = 3, signedAtMs = 1_000)
        assertNull(continuityKeyWanted(null, 0))
        assertEquals(WantedContinuityKey(3, 1_000 + CONTINUITY_KEY_TTL_MS), continuityKeyWanted(marker, 2_000))
        assertNull(continuityKeyWanted(marker, 1_000 + CONTINUITY_KEY_TTL_MS))
        assertTrue(continuityKeyUsable(1_000, 1_000 + CONTINUITY_KEY_TTL_MS - 1))
        assertFalse(continuityKeyUsable(1_000, 1_000 + CONTINUITY_KEY_TTL_MS))
        assertEquals(30L * 24 * 60 * 60 * 1000, CONTINUITY_KEY_TTL_MS)
        assertEquals(marker, decodeContinuityMarker(encodeContinuityMarker(marker)))
        assertNull(decodeContinuityMarker("{}x"))
        assertNull(decodeContinuityMarker("""{"keyGeneration":0,"signedAtMs":5}"""))
    }
}
