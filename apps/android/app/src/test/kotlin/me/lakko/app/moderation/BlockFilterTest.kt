// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import me.lakko.app.calls.CallSignalingBodyKind
import me.lakko.app.chat.IncomingChatEvent
import me.lakko.app.chat.MessageOp
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.lakko_ffi_uniffi.BlockPath
import uniffi.lakko_ffi_uniffi.BlockVerdict

/**
 * ADR-0089 section 6 — what a block removes from a received batch. The rule
 * itself is the core's (`blockVerdict`); the host suite has no native core, so
 * [BlockRules.verdict] is replaced by a table with the same answers, and the
 * tests assert that each path asks the rule about the right path and acts on
 * its answer.
 */
class BlockFilterTest {

    private val asked = mutableListOf<BlockPath>()
    private val original = BlockRules.verdict

    @Before
    fun stubRule() {
        BlockRules.verdict = { path, blocked ->
            asked += path
            if (!blocked) {
                BlockVerdict.SHOW
            } else {
                when (path) {
                    BlockPath.ONE_TO_ONE_MESSAGE, BlockPath.CONTACT_REQUEST, BlockPath.NOTIFICATION,
                    BlockPath.INCOMING_SIGNAL,
                    -> BlockVerdict.DROP
                    BlockPath.GROUP_MESSAGE -> BlockVerdict.COLLAPSE
                    BlockPath.INCOMING_CALL -> BlockVerdict.DECLINE_SILENTLY
                    BlockPath.CALL_TILE -> BlockVerdict.LABEL
                    BlockPath.OUTGOING_SIGNAL -> BlockVerdict.WITHHOLD
                }
            }
        }
    }

    @After
    fun restoreRule() {
        BlockRules.verdict = original
    }

    private val blockedKey = "QkxPQ0tFRC1BQ0NPVU5ULUtFWS0zMi1CWVRFUy0hISE="
    private val otherKey = "T1RIRVItQUNDT1VOVC1LRVktMzItQllURVMtISEhISE="
    private val isBlocked: (String?) -> Boolean = { it == blockedKey }

    private fun chat(id: Long, sender: String?) = IncomingChatEvent.Application(
        messageId = id,
        text = "hi",
        attachments = emptyList(),
        plaintext = "hi",
        senderLeaf = 1u,
        senderKey = sender,
        createdAt = "2026-11-08T00:00:00Z",
    )

    private fun op(id: Long, sender: String?) = IncomingChatEvent.Op(
        messageId = id,
        op = MessageOp.Reaction,
        target = 1L,
        text = "",
        emoji = "x",
        senderLeaf = 1u,
        senderKey = sender,
        createdAt = "2026-11-08T00:00:00Z",
    )

    private fun typing(id: Long, sender: String?) = IncomingChatEvent.Typing(
        messageId = id,
        isTyping = true,
        senderLeaf = 1u,
        senderKey = sender,
        createdAt = "2026-11-08T00:00:00Z",
    )

    private fun receipt(id: Long, sender: String?) = IncomingChatEvent.ReadReceipt(
        messageId = id,
        upTo = 1L,
        senderLeaf = 1u,
        senderKey = sender,
        createdAt = "2026-11-08T00:00:00Z",
    )

    private fun call(id: Long, person: String?) = IncomingChatEvent.CallSignaling(
        messageId = id,
        payloadBytes = byteArrayOf(id.toByte()),
        createdAt = "2026-11-08T00:00:00Z",
        senderLeaf = 1u,
        senderKey = null,
        senderPersonKey = person,
    )

    @Test
    fun `a blocked person's one-to-one rows and signals are dropped and nobody else's are`() {
        val events = listOf(
            chat(1, blockedKey), chat(2, otherKey), op(3, blockedKey), typing(4, blockedKey),
            receipt(5, blockedKey), receipt(6, otherKey),
        )
        val kept = withoutBlockedEvents(events, oneToOne = true, isBlocked = isBlocked) { null }
        assertEquals(listOf(2L, 6L), kept.map { it.messageIdOf() })
        assertTrue(BlockPath.ONE_TO_ONE_MESSAGE in asked)
        assertTrue(BlockPath.INCOMING_SIGNAL in asked)
    }

    @Test
    fun `a blocked person's group messages are kept to be collapsed`() {
        val events = listOf(chat(1, blockedKey), op(2, blockedKey), typing(3, blockedKey))
        val kept = withoutBlockedEvents(events, oneToOne = false, isBlocked = isBlocked) { null }
        assertEquals(listOf(1L, 2L, 3L), kept.map { it.messageIdOf() })
        assertEquals(
            BlockVerdict.COLLAPSE,
            receivedMessageVerdict(oneToOne = false, senderKeyB64 = blockedKey, isOwn = false, isBlocked = isBlocked),
        )
    }

    @Test
    fun `nobody is asked about when nobody is blocked`() {
        val events = listOf(chat(1, otherKey), typing(2, otherKey))
        val kept = withoutBlockedEvents(events, oneToOne = true, isBlocked = { false }) { null }
        assertEquals(events, kept)
        assertTrue(asked.isEmpty())
    }

    @Test
    fun `a blocked caller's one-to-one signaling and group start are dropped, a group join is not`() {
        val oneToOne = withoutBlockedEvents(listOf(call(1, blockedKey), call(2, otherKey)), true, isBlocked) { null }
        assertEquals(listOf(2L), oneToOne.map { it.messageIdOf() })
        val kinds = mapOf(10.toByte() to CallSignalingBodyKind.GROUP_START, 11.toByte() to CallSignalingBodyKind.GROUP_JOIN)
        val group = withoutBlockedEvents(listOf(call(10, blockedKey), call(11, blockedKey)), false, isBlocked) {
            kinds[it[0]]
        }
        assertEquals(listOf(11L), group.map { it.messageIdOf() })
        assertTrue(BlockPath.INCOMING_CALL in asked)
    }

    @Test
    fun `signals are withheld only from a blocked person in a one-to-one`() {
        assertTrue(withholdsOutgoingSignal(oneToOne = true, peerKeyB64 = blockedKey, isBlocked = isBlocked))
        assertFalse(withholdsOutgoingSignal(oneToOne = true, peerKeyB64 = otherKey, isBlocked = isBlocked))
        assertFalse(withholdsOutgoingSignal(oneToOne = false, peerKeyB64 = blockedKey, isBlocked = isBlocked))
    }

    @Test
    fun `requests, notifications, calls and tiles each ask their own path`() {
        assertTrue(dropsContactRequest(blockedKey, isBlocked))
        assertFalse(dropsContactRequest(otherKey, isBlocked))
        assertTrue(suppressesNotification(blockedKey, isBlocked))
        assertTrue(declinesCallSilently(blockedKey, isBlocked))
        assertTrue(labelsCallTile(blockedKey, isBlocked))
        assertFalse(labelsCallTile(null, isBlocked))
        assertEquals(
            listOf(BlockPath.CONTACT_REQUEST, BlockPath.NOTIFICATION, BlockPath.INCOMING_CALL, BlockPath.CALL_TILE),
            asked,
        )
    }

    @Test
    fun `own and unattributed rows are never subject to a block`() {
        assertEquals(BlockVerdict.SHOW, receivedMessageVerdict(true, blockedKey, isOwn = true, isBlocked = isBlocked))
        assertEquals(BlockVerdict.SHOW, receivedMessageVerdict(true, null, isOwn = false, isBlocked = isBlocked))
    }

    @Test
    fun `the register matches a key in any base64 spelling`() {
        val state = BlockedState("acct", listOf(BlockedPerson(blockedKey, 1L)))
        assertTrue(state.isBlocked(blockedKey))
        val urlSafe = blockedKey.replace('+', '-').replace('/', '_').trimEnd('=')
        assertTrue(state.isBlocked(urlSafe))
        assertFalse(state.isBlocked(otherKey))
        assertFalse(state.isBlocked(null))
    }

    private fun IncomingChatEvent.messageIdOf(): Long = when (this) {
        is IncomingChatEvent.Application -> messageId
        is IncomingChatEvent.Op -> messageId
        is IncomingChatEvent.Typing -> messageId
        is IncomingChatEvent.ReadReceipt -> messageId
        is IncomingChatEvent.CallSignaling -> messageId
        else -> -1L
    }
}
