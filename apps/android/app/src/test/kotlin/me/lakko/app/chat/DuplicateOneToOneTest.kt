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
}
