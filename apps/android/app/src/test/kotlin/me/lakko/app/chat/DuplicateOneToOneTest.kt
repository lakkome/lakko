// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Test

/** [duplicateOneToOneFolds]; the cases of the web client's
 *  `duplicate-one-to-one.test.ts`. */
class DuplicateOneToOneTest {
    private val me = "me-key"
    private val vg3 = "vg3-key"
    private val anna = "anna-key"

    private fun chat(
        groupHex: String,
        founder: String?,
        peer: String = vg3,
        conversationHex: String = groupHex,
    ) = HeldConversationGroup(groupHex, conversationHex, isOneToOne = true, people = listOf(me, peer), founderKey = founder)

    @Test
    fun `continues the one-to-one this account founded with the one the person founded`() {
        // The desktop founded aa (VG3 never opened it); VG3 founded bb in
        // answer to the desktop's link answer (seen live 2026-11-07).
        assertEquals(
            listOf(DuplicateFold("bb", "aa")),
            duplicateOneToOneFolds(listOf(chat("aa", me), chat("bb", vg3)), me),
        )
    }

    @Test
    fun `is order-independent and writes nothing once folded`() {
        assertEquals(
            listOf(DuplicateFold("bb", "aa")),
            duplicateOneToOneFolds(listOf(chat("bb", vg3), chat("aa", me)), me),
        )
        assertEquals(
            emptyList<DuplicateFold>(),
            duplicateOneToOneFolds(listOf(chat("aa", me), chat("bb", vg3, vg3, "aa")), me),
        )
    }

    @Test
    fun `leaves one-to-ones with different people apart`() {
        assertEquals(
            emptyList<DuplicateFold>(),
            duplicateOneToOneFolds(listOf(chat("aa", me, anna), chat("bb", vg3)), me),
        )
    }

    @Test
    fun `does nothing when both were founded by the same side, or the founder is unknown`() {
        val none = emptyList<DuplicateFold>()
        assertEquals(none, duplicateOneToOneFolds(listOf(chat("aa", me), chat("bb", me)), me))
        assertEquals(none, duplicateOneToOneFolds(listOf(chat("aa", vg3), chat("bb", vg3)), me))
        assertEquals(none, duplicateOneToOneFolds(listOf(chat("aa", null), chat("bb", vg3)), me))
        assertEquals(none, duplicateOneToOneFolds(listOf(chat("aa", me), chat("bb", vg3)), null))
    }

    @Test
    fun `never folds a group, nor into one`() {
        val group = HeldConversationGroup("aa", "aa", isOneToOne = false, people = listOf(me, vg3), founderKey = me)
        assertEquals(emptyList<DuplicateFold>(), duplicateOneToOneFolds(listOf(group, chat("bb", vg3)), me))
    }

    @Test
    fun `does not fold a group that already continues or is continued`() {
        // bb is already a successor of cc.
        assertEquals(
            emptyList<DuplicateFold>(),
            duplicateOneToOneFolds(listOf(chat("aa", me), chat("bb", vg3, vg3, "cc"), chat("cc", vg3)), me),
        )
    }

    @Test
    fun `picks the same root on every device when several own one-to-ones exist`() {
        assertEquals(
            listOf(DuplicateFold("bb", "aa")),
            duplicateOneToOneFolds(listOf(chat("cc", me), chat("aa", me), chat("bb", vg3)), me),
        )
    }

    // Base scenario 9 (2026-11-22) — ordered by when this device first held them.

    private fun HeldConversationGroup.at(ms: Long) = copy(heldSinceMs = ms)

    @Test
    fun `the newer one-to-one the same person founded continues the older one`() {
        assertEquals(
            listOf(DuplicateFold("bb", "aa")),
            duplicateOneToOneFolds(listOf(chat("aa", vg3).at(1), chat("bb", vg3).at(2)), me),
        )
        assertEquals(
            listOf(DuplicateFold("bb", "aa")),
            duplicateOneToOneFolds(listOf(chat("bb", vg3).at(2), chat("aa", vg3).at(1)), me),
        )
    }

    @Test
    fun `a one-to-one another device of this account founded continues an older one the person founded`() {
        assertEquals(
            listOf(DuplicateFold("bb", "aa")),
            duplicateOneToOneFolds(listOf(chat("aa", vg3).at(1), chat("bb", me).at(2)), me),
        )
    }

    @Test
    fun `equal or unknown times keep the rule as it was`() {
        assertEquals(emptyList<DuplicateFold>(), duplicateOneToOneFolds(listOf(chat("aa", vg3).at(1), chat("bb", vg3).at(1)), me))
        assertEquals(emptyList<DuplicateFold>(), duplicateOneToOneFolds(listOf(chat("aa", vg3), chat("bb", vg3).at(2)), me))
        assertEquals(
            listOf(DuplicateFold("bb", "aa")),
            duplicateOneToOneFolds(listOf(chat("aa", me).at(1), chat("bb", vg3).at(1)), me),
        )
        assertEquals(
            emptyList<DuplicateFold>(),
            duplicateOneToOneFolds(listOf(chat("aa", vg3).at(1), chat("bb", vg3, conversationHex = "aa").at(2)), me),
        )
    }

    @Test
    fun `the held-since record keeps first times and drops groups no longer held`() {
        val first = ConversationHeldSince.next(emptyMap(), listOf("aa"), 100)
        assertEquals(mapOf("aa" to 100L), first)
        val second = ConversationHeldSince.next(first, listOf("aa", "bb"), 200)
        assertEquals(mapOf("aa" to 100L, "bb" to 200L), second)
        assertEquals(mapOf("bb" to 200L), ConversationHeldSince.next(second, listOf("bb"), 300))
        assertEquals(second, ConversationHeldSince.decode(ConversationHeldSince.encode(second)))
        assertEquals(emptyMap<String, Long>(), ConversationHeldSince.decode("garbage;=1;x=y"))
    }
}
