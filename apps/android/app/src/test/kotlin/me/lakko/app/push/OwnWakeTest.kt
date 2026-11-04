// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A wake posts a banner only for a message from someone else (or a call).
 *
 * The DS wakes every subscription of a group, the sender's own among them, so
 * each message comes back to the device that sent it. That device cannot open
 * its own message, and before this rule it posted the generic "New message"
 * banner for it. These tests pin what [decryptPushWake] reports and what
 * [PushWakeOutcome.genericBannerWanted] decides from it.
 */
class OwnWakeTest {

    private val groupHex = "b".repeat(64)
    private val accountId = "acct-own=="
    private val ownPersonKey = "own-person-key=="

    private class Group(
        private val decode: (Int) -> ProcessedRow?,
        private val throwOn: Set<Int>,
        private val ownPersonKey: String?,
        private val ownEchoOn: Set<Int> = emptySet(),
    ) : ThrowawayGroup {
        override fun process(payload: ByteArray): ProcessedRow? {
            val tag = payload[0].toInt() and 0xFF
            if (tag in ownEchoOn) {
                throw RuntimeException("process_message: ValidationError(CannotDecryptOwnMessage)")
            }
            if (tag in throwOn) throw RuntimeException("cannot decrypt")
            return decode(tag)
        }

        override fun facts(): WakeGroupFacts =
            WakeGroupFacts(
                kind = me.lakko.app.chat.ConversationKind.Chat,
                rosterPersonKeysB64 = listOfNotNull(ownPersonKey, "peer=="),
                ownerKeyB64 = null,
                ownPersonKeyB64 = ownPersonKey,
            )

        override fun close() = Unit
    }

    private inner class Deps(
        private val group: Group,
        private val rows: List<FetchedRow>,
        private val heldIds: Set<Long> = emptySet(),
    ) : PushDecryptDeps {
        override fun lookupRoute(accountIdB64: String, pushGidHex: String, subscriptionIdHex: String?): String? =
            pushGidHex.takeIf { accountIdB64 == accountId }

        override fun loadIdentitySeed(accountIdB64: String): ByteArray = ByteArray(64) { 1 }

        override fun loadSnapshot(accountIdB64: String): ByteArray = ByteArray(8) { 2 }

        override fun openThrowawayGroup(seed: ByteArray, snapshot: ByteArray, groupIdBytes: ByteArray): ThrowawayGroup =
            group

        override suspend fun dsFetch(
            groupIdBytes: ByteArray,
            afterId: Long,
            signer: me.lakko.app.ds.DsJournalSigner?,
        ): List<FetchedRow> = rows

        override fun startCursor(accountIdB64: String, groupHex: String): Long? = null

        override fun heldRow(accountIdB64: String, groupHex: String, messageId: Long): me.lakko.app.chat.HeldRow? =
            if (messageId in heldIds) {
                me.lakko.app.chat.HeldRow(me.lakko.app.chat.HeldRowDirection.OUTGOING, ownPersonKey)
            } else {
                null
            }
    }

    private fun row(id: Long, tag: Int) = FetchedRow(id, byteArrayOf(tag.toByte()))

    private fun chat(text: String, sender: String?) =
        ProcessedRow(ProcessedKind.CHAT, text, 0, senderName = "Anna", senderAccountB64 = sender)

    private suspend fun wake(deps: Deps): PushWakeOutcome =
        decryptPushWake(
            PushWake(groupHex, pointer = 20, accountIdB64 = accountId),
            deps,
            nowMs = 0L,
            ringWindowMs = 0L,
        )

    @Test
    fun `this device's own message coming back posts nothing`() = runTest {
        // The row does not decrypt (a sender cannot open its own message) and
        // the app's log holds it: it is ours.
        val group = Group(decode = { null }, throwOn = setOf(1), ownPersonKey = ownPersonKey)
        val outcome = wake(Deps(group, listOf(row(20, 1)), heldIds = setOf(20)))

        assertNull(outcome.preview)
        assertTrue(outcome.readRan)
        assertEquals(0, outcome.unreadableRows)
        assertFalse(outcome.genericBannerWanted(appOnScreen = false))
        assertFalse(outcome.genericBannerWanted(appOnScreen = true))
    }

    @Test
    fun `the core's own-message refusal is quiet even when the log does not hold the row`() = runTest {
        // This device's own typing or receipt: the log never holds it.
        val group = Group(decode = { null }, throwOn = emptySet(), ownPersonKey = ownPersonKey, ownEchoOn = setOf(1, 2))
        val outcome = wake(Deps(group, listOf(row(19, 1), row(20, 2))))

        assertEquals(0, outcome.unreadableRows)
        assertFalse(outcome.genericBannerWanted(appOnScreen = false))
    }

    @Test
    fun `the own-message refusal is found through a wrapped cause`() {
        val wrapped = IllegalStateException("decrypt", RuntimeException("x CannotDecryptOwnMessage y"))
        assertTrue(isOwnMessageEcho(wrapped))
        assertFalse(isOwnMessageEcho(RuntimeException("process_message: WrongEpoch")))
    }

    @Test
    fun `a message from another device of the same account posts nothing`() = runTest {
        val group = Group(
            decode = { chat("sent from my laptop", sender = ownPersonKey) },
            throwOn = emptySet(),
            ownPersonKey = ownPersonKey,
        )
        val outcome = wake(Deps(group, listOf(row(20, 1))))

        assertNull(outcome.preview)
        assertFalse(outcome.genericBannerWanted(appOnScreen = false))
    }

    @Test
    fun `service traffic alone posts nothing`() = runTest {
        // A read receipt, a profile, a beacon: decrypted, nothing to show.
        val group = Group(
            decode = { tag -> if (tag == 1) ProcessedRow(ProcessedKind.READ_RECEIPT, "", 0, senderAccountB64 = "peer==") else null },
            throwOn = emptySet(),
            ownPersonKey = ownPersonKey,
        )
        val outcome = wake(Deps(group, listOf(row(19, 1), row(20, 2))))

        assertNull(outcome.preview)
        assertFalse(outcome.genericBannerWanted(appOnScreen = false))
    }

    @Test
    fun `a sender with no stored name gets the generic banner, never a placeholder word`() = runTest {
        me.lakko.app.l10n.EnglishForTests.install()
        val unnamedChat = ProcessedRow(ProcessedKind.CHAT, "hello", 0, senderName = null, senderAccountB64 = "peer==")
        val unnamedReaction = ProcessedRow(
            ProcessedKind.REACTION, "", 0,
            senderName = null,
            senderAccountB64 = "peer==",
            reactionAdd = true,
            reactionEmoji = "\uD83D\uDC4D",
            reactionTargetAuthorB64 = ownPersonKey,
        )
        for (processed in listOf(unnamedChat, unnamedReaction)) {
            val outcome = wake(Deps(Group(decode = { processed }, throwOn = emptySet(), ownPersonKey = ownPersonKey), listOf(row(20, 1))))
            assertEquals("Lakko", outcome.preview?.title)
            assertEquals(me.lakko.app.l10n.EnglishForTests.strings["notification.message.new"], outcome.preview?.body)
            assertNoPlaceholderWord(outcome.preview?.title, outcome.preview?.body)
        }
        // Named, the same rows read as the person and their words.
        val named = wake(Deps(Group(decode = { chat("hello", sender = "peer==") }, throwOn = emptySet(), ownPersonKey = ownPersonKey), listOf(row(20, 1))))
        assertEquals("Anna", named.preview?.title)
        assertEquals("hello", named.preview?.body)
        assertNoPlaceholderWord(named.preview?.title, named.preview?.body)
    }

    @Test
    fun `a message from someone else is the preview`() = runTest {
        val group = Group(
            decode = { tag ->
                when (tag) {
                    1 -> chat("hi", sender = "peer==")
                    2 -> chat("mine", sender = ownPersonKey)
                    else -> null
                }
            },
            throwOn = emptySet(),
            ownPersonKey = ownPersonKey,
        )
        // The newer row is ours; the older one from the peer stays the preview.
        val outcome = wake(Deps(group, listOf(row(19, 1), row(20, 2))))

        assertEquals("hi", outcome.preview?.body)
    }

    @Test
    fun `a row this device cannot read keeps the generic banner, off screen only`() = runTest {
        val group = Group(decode = { null }, throwOn = setOf(1), ownPersonKey = ownPersonKey)
        val outcome = wake(Deps(group, listOf(row(20, 1)), heldIds = emptySet()))

        assertEquals(1, outcome.unreadableRows)
        assertTrue(outcome.genericBannerWanted(appOnScreen = false))
        assertFalse(outcome.genericBannerWanted(appOnScreen = true))
    }

    @Test
    fun `a read that did not run keeps the generic banner`() {
        assertTrue(PushWakeOutcome.EMPTY.genericBannerWanted(appOnScreen = false))
        assertTrue(PushWakeOutcome.EMPTY.genericBannerWanted(appOnScreen = true))
    }
}
