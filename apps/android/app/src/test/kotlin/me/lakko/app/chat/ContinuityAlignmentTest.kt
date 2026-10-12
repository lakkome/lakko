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
 * ADR-0065 R.13b alignment pass - the pure decisions it added on Android:
 * the sealed claim store, the resumable and faithful re-send, the re-send
 * marker read from the stored log, the composer text of a conversation, and
 * the read position across segments.
 */
class ContinuityAlignmentTest {

    // ---- B: the claim store is sealed, one text per account --------------

    private fun claim(id: Long) = SuccessorClaim(id, "c3VjYw==", "key", senderWasAdmin = false, adminKeysAtReceiptB64 = emptyList())

    private val prepared = StoredClaims(
        accepted = claim(3),
        own = OwnClaim("aa", 4),
        prepared = PreparedSuccessor("aa", "V2VsY29tZQ==", listOf("k1"), false),
    )

    @Test
    fun `the claim file round-trips and a damaged text is no claims`() {
        val rows = mapOf("AB" to prepared, "cd" to StoredClaims(pending = listOf(claim(9))))
        assertEquals(
            mapOf("ab" to prepared, "cd" to StoredClaims(pending = listOf(claim(9)))),
            decodeClaimFile(encodeClaimFile(rows)),
        )
        assertEquals(emptyMap<String, StoredClaims>(), decodeClaimFile(null))
        assertEquals(emptyMap<String, StoredClaims>(), decodeClaimFile("{"))
        // A row that does not decode is left out, never trusted.
        assertEquals(emptyMap<String, StoredClaims>(), decodeClaimFile("""{"ab":"{"}"""))
    }

    @Test
    fun `the sealed backend writes through its blobs and reads back after a restart`() {
        val blobs = InMemoryClaimBlobs()
        val first = SealedSuccessorClaimBackend(blobs)
        assertNull(first.load("acct", "ab"))
        assertTrue(first.save("acct", "AB", prepared))
        assertEquals(prepared, first.load("acct", "ab"))
        // What is on "disk" is the whole account's state, and nothing of it
        // leaves through any other path than the blobs.
        assertEquals(setOf("acct"), blobs.texts.keys)

        val afterRestart = SealedSuccessorClaimBackend(blobs)
        assertEquals(prepared, afterRestart.load("acct", "AB"))
        assertEquals(mapOf("ab" to prepared), afterRestart.loadAll("acct"))
        assertEquals(emptyMap<String, StoredClaims>(), afterRestart.loadAll("other"))

        afterRestart.remove("acct", "ab")
        assertNull(afterRestart.load("acct", "ab"))
        // The last row gone: the file goes too.
        assertTrue(blobs.texts.isEmpty())
    }

    @Test
    fun `a write that does not land is reported, and the row still stands for the session`() {
        val blobs = InMemoryClaimBlobs()
        val backend = SealedSuccessorClaimBackend(blobs)
        blobs.failing = true
        assertFalse("the caller that needs the row on disk must learn it is not", backend.save("acct", "ab", prepared))
        assertEquals(prepared, backend.load("acct", "ab"))
        assertTrue(blobs.texts.isEmpty())
        // A fresh process sees nothing: the row never reached the disk.
        assertNull(SealedSuccessorClaimBackend(blobs).load("acct", "ab"))
    }

    // ---- F: the re-send is resumable and faithful ------------------------

    @Test
    fun `an interrupted re-send continues after the last message that landed`() {
        val ids = listOf(11L, 14L, 20L, 31L)
        assertEquals(ids, resendRemaining(ids, cursor = null))
        assertEquals(listOf(20L, 31L), resendRemaining(ids, cursor = 14))
        assertEquals(emptyList<Long>(), resendRemaining(ids, cursor = 31))
        // A cursor between two ids (the row it named was deleted since).
        assertEquals(listOf(31L), resendRemaining(ids, cursor = 25))
    }

    @Test
    fun `the re-send cursor only moves forward and is spent when the re-send ends`() {
        var row = ContinuityRow()
        row = row.withResendCursor("old", 14)
        assertEquals(14L, row.resendCursor["old"])
        assertSame(row, row.withResendCursor("old", 11))
        row = row.withResendCursor("old", 20)
        assertEquals(20L, row.resendCursor["old"])
        assertEquals(row, decodeContinuityRow(encodeContinuityRow(row)))

        val done = row.withResent("old")
        assertTrue("old" in done.resent)
        assertNull(done.resendCursor["old"])
        assertSame(done, done.withResent("old"))
    }

    @Test
    fun `a re-sent reply keeps its quote with no jump target`() {
        val parts = resendEnvelopeParts("b3JpZ2lu", 14, reply = ResendReply("Anna", "see you there"), forward = null)
        assertEquals(REPLY_NO_JUMP_TARGET, parts.replyTo)
        assertEquals("Anna", parts.replyAuthor)
        assertEquals("see you there", parts.replyExcerpt)
        assertNull(parts.forwardedFrom)
        assertEquals("b3JpZ2lu", parts.resendOriginGroupB64)
        assertEquals(14L, parts.resendOriginMessageId)
    }

    @Test
    fun `a re-sent forward keeps its attribution and its origin`() {
        val parts = resendEnvelopeParts("b3JpZ2lu", 20, reply = null, forward = ResendForward("Ben", "b3RoZXI=", 7))
        assertNull(parts.replyTo)
        assertEquals("Ben", parts.forwardedFrom)
        assertEquals("b3RoZXI=", parts.forwardOriginGroupB64)
        assertEquals(7L, parts.forwardOriginMessageId)
        // Half an origin pair is dropped; the name stays.
        val half = resendEnvelopeParts("b3JpZ2lu", 20, null, ResendForward("Ben", "b3RoZXI=", null))
        assertEquals("Ben", half.forwardedFrom)
        assertNull(half.forwardOriginGroupB64)
        assertNull(half.forwardOriginMessageId)
    }

    @Test
    fun `a plain re-sent message carries the marker alone`() {
        val parts = resendEnvelopeParts("b3JpZ2lu", 31, reply = null, forward = null)
        assertEquals(
            ResendEnvelopeParts(null, null, null, null, null, null, "b3JpZ2lu", 31),
            parts,
        )
        // A quote with nothing in it is nothing to carry.
        assertNull(resendEnvelopeParts("b3JpZ2lu", 31, ResendReply(null, ""), null).replyTo)
    }

    // ---- E: the marker against the STORED log ----------------------------

    private val oldHex = "0a".repeat(32)
    private val newHex = "0b".repeat(32)
    private val lineage = buildLineage(listOf(LineageEntry(newHex, oldHex, 1uL)))
    private val marker = ResendOrigin(groupB64OfHex(oldHex), 42)

    private fun stored(direction: String, author: String? = null) = MessageLogPayload(
        groups = mapOf(
            oldHex to PersistedGroup(
                cursor = 50,
                messages = listOf(
                    PersistedMessage(42, direction, "hello", emptyList(), "2026-01-01T00:00:00Z", authorKeyB64 = author),
                ),
            ),
        ),
    )

    @Test
    fun `the stored log answers what this device holds`() {
        assertEquals(HeldRow(HeldRowDirection.OUTGOING, null), heldRowIn(stored("outgoing"), oldHex, 42))
        assertEquals(HeldRow(HeldRowDirection.INCOMING, "peer"), heldRowIn(stored("incoming", "peer"), oldHex.uppercase(), 42))
        assertNull(heldRowIn(stored("outgoing"), oldHex, 43))
        assertNull(heldRowIn(stored("outgoing"), newHex, 42))
        assertNull(heldRowIn(null, oldHex, 42))
        assertNull(heldRowIn(stored("something-else"), oldHex, 42))
        assertNull(heldRowIn(stored("outgoing"), oldHex, 0))
    }

    @Test
    fun `a re-send of a message this device sent is skipped from the stored log alone`() {
        val log = stored("outgoing")
        val verdict = { sender: String?, received: String ->
            storedResendVerdict(marker, received, lineage, { g, id -> heldRowIn(log, g, id) }, "me", sender)
        }
        assertEquals(ResendVerdict.SKIP, verdict("me", newHex))
        // Another account sent it: the held row is not theirs.
        assertEquals(ResendVerdict.DRAW, verdict("peer", newHex))
        // Received in the old group itself: not an older segment of it.
        assertEquals(ResendVerdict.DRAW, verdict("me", oldHex))
        // No marker, or one that names nothing: an ordinary message.
        assertEquals(ResendVerdict.DRAW, storedResendVerdict(null, newHex, lineage, { _, _ -> null }, "me", "me"))
        assertEquals(
            ResendVerdict.DRAW,
            storedResendVerdict(ResendOrigin("not base64 !", 42), newHex, lineage, { _, _ -> HeldRow(HeldRowDirection.OUTGOING, null) }, "me", "me"),
        )
        // The device holds no such row.
        assertEquals(ResendVerdict.DRAW, storedResendVerdict(marker, newHex, lineage, { _, _ -> null }, "me", "me"))
    }

    // ---- H: the composer text and the read position ----------------------

    @Test
    fun `unsent text is kept per conversation and forgotten when the composer empties`() {
        val carry = ComposerCarry()
        assertNull(carry.textOf("root"))
        carry.note("ROOT", "half a thou")
        assertEquals("half a thou", carry.textOf("root"))
        carry.note("root", "half a thought")
        assertEquals("half a thought", carry.textOf("Root"))
        assertNull(carry.textOf("other"))
        carry.note("root", "")
        assertNull(carry.textOf("root"))
        carry.note("root", "again")
        carry.clear()
        assertNull(carry.textOf("root"))
    }

    @Test
    fun `a rebuilt composer starts with the conversation's unsent text, never over its own`() {
        assertEquals("unsent", composerSeed(current = "", carried = "unsent"))
        assertEquals("mine", composerSeed(current = "mine", carried = "unsent"))
        assertEquals("", composerSeed(current = "", carried = null))
        assertEquals("", composerSeed(current = "", carried = ""))
    }

    private fun message(id: Long, direction: Direction = Direction.Incoming) =
        ChatMessage(messageId = id, direction = direction, text = "m$id", createdAt = "2026-01-01T00:00:00Z")

    private fun thread(hex: String, vararg messages: ChatMessage, ended: Boolean = false) =
        ChatThread(groupIdHex = hex, messages = messages.toList(), cursor = 0, membershipEnded = ended)

    @Test
    fun `a folded thread knows the lineage index of every segment it shows`() {
        val three = buildLineage(listOf(LineageEntry("s1", "root", 10UL), LineageEntry("s2", "root", 20UL)))
        val all = foldConversations(
            listOf(thread("root", message(1), ended = true), thread("s1", message(1), ended = true), thread("s2", message(1))),
            three,
        ).single()
        assertEquals("s2", all.groupIdHex)
        assertEquals(2, all.segmentIndex)
        assertEquals(listOf(0, 1), all.olderSegmentIndices)

        // A device that joined at the first successor holds no "root": the
        // head's index is still the lineage's, not its place among the held.
        val late = foldConversations(listOf(thread("s1", message(1), ended = true), thread("s2", message(1))), three).single()
        assertEquals(2, late.segmentIndex)
        assertEquals(listOf(1), late.olderSegmentIndices)

        // A successor held alone.
        val alone = foldConversations(listOf(thread("s2", message(1))), three).single()
        assertEquals(2, alone.segmentIndex)
        assertEquals("root", alone.conversationHex)
    }

    @Test
    fun `a row of an older segment is unread until the read position passes it`() {
        val two = buildLineage(listOf(LineageEntry("s1", "root", 10UL)))
        val folded = foldConversations(
            listOf(thread("root", message(1), message(2), message(3), ended = true), thread("s1", message(1), message(2))),
            two,
        ).single()
        fun unread(position: TimelinePosition) = folded.messages.count { rowPastReadPosition(folded, it, position) }
        // Read up to message 1 of the OLD group: two old rows and the whole
        // successor are unread.
        assertEquals(4, unread(TimelinePosition(0, 1)))
        // The old group read to its end: only the successor's rows.
        assertEquals(2, unread(TimelinePosition(0, 3)))
        // A position in the successor: nothing of the old group, and the
        // successor's ids are its own (message 1 there is read, 2 is not).
        assertEquals(1, unread(TimelinePosition(1, 1)))
        assertEquals(0, unread(TimelinePosition(1, 2)))
        // A row the server has not named has no position and is not counted.
        val pending = folded.copy(messages = folded.messages + ChatMessage(0, Direction.Incoming, "x", createdAt = "t"))
        assertEquals(0, pending.messages.count { rowPastReadPosition(pending, it, TimelinePosition(1, 2)) })
    }
}
