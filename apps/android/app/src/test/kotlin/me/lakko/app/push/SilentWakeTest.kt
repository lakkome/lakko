// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import kotlinx.coroutines.test.runTest
import me.lakko.app.calls.CallRingSignal
import me.lakko.app.calls.TEST_RING_WINDOW_MS
import me.lakko.app.chat.ConversationKind
import me.lakko.app.chat.HeldRow
import me.lakko.app.chat.HeldRowDirection
import me.lakko.app.chat.LineageEntry
import me.lakko.app.chat.ResendOrigin
import me.lakko.app.chat.groupB64OfHex
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0065 on the push path, with the app closed: a wake posts nothing for
 * what a superseded sender posted (R.5 / R.8) and nothing for a re-sent
 * message whose original this device holds (R.7 case 1).
 *
 * The background reader only READS (C-59): the reset ledger, the continuity
 * facts, the lineage mirror and the app's stored message log. [FakeDeps] has
 * no write method, like the interface.
 */
class SilentWakeTest {
    private val accountId = "acct-1"
    private val oldHex = "0a".repeat(32)
    private val newHex = "0b".repeat(32)
    private val me = "key-me"
    private val retired = "key-retired"
    private val peer = "key-peer"
    private val now = 1_800_000_000_000L
    private val callIdHex = "11".repeat(16)

    private class FakeGroup(
        private val facts: WakeGroupFacts?,
        private val decode: (ByteArray) -> ProcessedRow?,
    ) : ThrowawayGroup {
        override fun process(payload: ByteArray): ProcessedRow? = decode(payload)
        override fun facts(): WakeGroupFacts? = facts
        override fun close() = Unit
    }

    private class FakeDeps(
        val accountId: String,
        val group: ThrowawayGroup,
        val rows: List<FetchedRow>,
        val superseded: Set<String> = emptySet(),
        val moved: Boolean = false,
        val lineage: List<LineageEntry> = emptyList(),
        val held: Map<Pair<String, Long>, HeldRow> = emptyMap(),
        val ledgerThrows: Boolean = false,
    ) : PushDecryptDeps {
        var heldAsked = 0

        override fun lookupRoute(accountIdB64: String, pushGidHex: String): String? = null
        override fun accountKnowsGroup(accountIdB64: String, groupHex: String): Boolean = accountIdB64 == accountId
        override fun loadIdentitySeed(accountIdB64: String): ByteArray = ByteArray(64) { 1 }
        override fun loadSnapshot(accountIdB64: String): ByteArray = ByteArray(8) { 2 }
        override fun openThrowawayGroup(seed: ByteArray, snapshot: ByteArray, groupIdBytes: ByteArray): ThrowawayGroup = group
        override suspend fun dsFetch(
            groupIdBytes: ByteArray,
            afterId: Long,
            signer: me.lakko.app.ds.DsJournalSigner?,
        ): List<FetchedRow> = rows
        override fun startCursor(accountIdB64: String, groupHex: String): Long? = null

        override fun supersededKeys(accountIdB64: String): Set<String> {
            if (ledgerThrows) throw RuntimeException("ledger failure")
            return superseded
        }
        override fun conversationMoved(accountIdB64: String, groupHex: String): Boolean = moved
        override fun lineage(accountIdB64: String): List<LineageEntry> = lineage
        override fun heldRow(accountIdB64: String, groupHex: String, messageId: Long): HeldRow? {
            heldAsked += 1
            return held[groupHex to messageId]
        }
    }

    private fun oneToOne(other: String) = WakeGroupFacts(ConversationKind.Chat, listOf(me, other), null, me)

    private fun group(owner: String, vararg people: String) =
        WakeGroupFacts(ConversationKind.Group, listOf(me) + people, owner, me)

    private fun chat(text: String, sender: String?, resend: ResendOrigin? = null) =
        ProcessedRow(ProcessedKind.CHAT, text, 0, senderAccountB64 = sender, resendOrigin = resend)

    private fun offer(sender: String?) = ProcessedRow(
        ProcessedKind.CALL_SIGNALING,
        "",
        0,
        callSignal = CallRingSignal.Offer(callIdHex, video = false, offeredAtMs = now - 1_000),
        senderAccountB64 = sender,
    )

    private suspend fun wake(d: FakeDeps, groupHex: String = oldHex) = decryptPushWake(
        PushWake(groupHex = groupHex, pointer = 11, accountIdB64 = accountId),
        d,
        nowMs = now,
        ringWindowMs = TEST_RING_WINDOW_MS,
    )

    private fun deps(
        facts: WakeGroupFacts?,
        rows: List<ProcessedRow?>,
        superseded: Set<String> = emptySet(),
        moved: Boolean = false,
        lineage: List<LineageEntry> = emptyList(),
        held: Map<Pair<String, Long>, HeldRow> = emptyMap(),
        ledgerThrows: Boolean = false,
    ) = FakeDeps(
        accountId = accountId,
        group = FakeGroup(facts) { payload -> rows[payload[0].toInt()] },
        rows = rows.indices.map { FetchedRow(10L + it, byteArrayOf(it.toByte())) },
        superseded = superseded,
        moved = moved,
        lineage = lineage,
        held = held,
        ledgerThrows = ledgerThrows,
    )

    // ---- superseded senders ---------------------------------------------

    @Test
    fun `a message from a retired key posts nothing, not even the generic banner`() = runTest {
        val outcome = wake(deps(oneToOne(retired), listOf(chat("it is me, send money", retired)), setOf(retired)))
        assertNull(outcome.preview)
        assertNull(outcome.ring)
        assertTrue(outcome.suppressed)
    }

    @Test
    fun `a call offer from the retired key of a one-to-one never rings`() = runTest {
        val outcome = wake(deps(oneToOne(retired), listOf(offer(retired)), setOf(retired)))
        assertNull(outcome.ring)
        assertTrue(outcome.suppressed)
    }

    @Test
    fun `a call offer from a retired key rings in a group that is repaired in place, and not once it moved`() = runTest {
        val repaired = wake(deps(group(owner = peer, retired, peer), listOf(offer(retired)), setOf(retired)))
        assertEquals(callIdHex, repaired.ring?.callIdHex)
        assertFalse(repaired.suppressed)

        val moved = wake(deps(group(owner = peer, retired, peer), listOf(offer(retired)), setOf(retired), moved = true))
        assertNull(moved.ring)
        assertTrue(moved.suppressed)

        // The retired key OWNED the group: unrepairable because of its reset.
        val owned = wake(deps(group(owner = retired, retired, peer), listOf(offer(retired)), setOf(retired)))
        assertNull(owned.ring)
        assertTrue(owned.suppressed)
    }

    @Test
    fun `a live member's message is still the preview beside a retired key's`() = runTest {
        val outcome = wake(
            deps(group(owner = peer, retired, peer), listOf(chat("real", peer), chat("fake", retired)), setOf(retired)),
        )
        assertEquals("real", outcome.preview?.body)
        assertFalse(outcome.suppressed)
    }

    @Test
    fun `an unreadable ledger drops nothing`() = runTest {
        val outcome = wake(
            deps(oneToOne(retired), listOf(chat("hello", retired)), setOf(retired), ledgerThrows = true),
        )
        assertEquals("hello", outcome.preview?.body)
    }

    @Test
    fun `nothing is retired - an ordinary wake is untouched and no store is opened`() = runTest {
        val d = deps(oneToOne(peer), listOf(chat("hello", peer)))
        val outcome = wake(d)
        assertEquals("hello", outcome.preview?.body)
        assertFalse(outcome.suppressed)
        assertEquals(0, d.heldAsked)
    }

    // ---- the re-send marker ---------------------------------------------

    private val lineage = listOf(LineageEntry(successorHex = newHex, rootHex = oldHex, hlc = 1uL))
    private val marker = ResendOrigin(groupB64OfHex(oldHex), 42)
    private val heldOwn = mapOf((oldHex to 42L) to HeldRow(HeldRowDirection.OUTGOING, null))

    @Test
    fun `a push for a re-sent message whose original this device holds posts nothing`() = runTest {
        val outcome = wake(
            deps(oneToOne(peer), listOf(chat("the same words", me, marker)), lineage = lineage, held = heldOwn),
            groupHex = newHex,
        )
        assertNull(outcome.preview)
        assertTrue(outcome.suppressed)
    }

    @Test
    fun `a re-sent message this device does not hold the original of is a message`() = runTest {
        val outcome = wake(
            deps(oneToOne(peer), listOf(chat("the same words", peer, marker)), lineage = lineage),
            groupHex = newHex,
        )
        assertEquals("the same words", outcome.preview?.body)
        assertFalse(outcome.suppressed)
    }

    @Test
    fun `a marker is believed only against the held row - another author, another conversation, a later segment`() = runTest {
        // The held row is someone else's: the sender did not write it.
        val foreign = mapOf((oldHex to 42L) to HeldRow(HeldRowDirection.INCOMING, peer))
        assertEquals(
            "x",
            wake(deps(oneToOne(peer), listOf(chat("x", me, marker)), lineage = lineage, held = foreign), newHex).preview?.body,
        )
        // No lineage: the named group is another conversation.
        assertEquals(
            "x",
            wake(deps(oneToOne(peer), listOf(chat("x", me, marker)), held = heldOwn), newHex).preview?.body,
        )
        // The marker names the receiving group itself.
        val self = ResendOrigin(groupB64OfHex(newHex), 42)
        val heldHere = mapOf((newHex to 42L) to HeldRow(HeldRowDirection.OUTGOING, null))
        assertEquals(
            "x",
            wake(deps(oneToOne(peer), listOf(chat("x", me, self)), lineage = lineage, held = heldHere), newHex).preview?.body,
        )
    }

    @Test
    fun `a wake posts nothing only when what it carried is all not acted on`() {
        assertTrue(wakePostsNothing(hasPreview = false, rings = false, suppressedRows = 1))
        assertFalse(wakePostsNothing(hasPreview = false, rings = false, suppressedRows = 0))
        assertFalse(wakePostsNothing(hasPreview = true, rings = false, suppressedRows = 3))
        assertFalse(wakePostsNothing(hasPreview = false, rings = true, suppressedRows = 3))
    }
}
