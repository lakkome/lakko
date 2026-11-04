// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** State bundle v3 — the display-only copies of messages held on another of
 *  the account's devices, and the rule that the real message replaces one. */
class HeldCopiesTest {

    private fun copy(text: String, at: Long, files: Int = 0) = HeldCopy(text, files, null, at)

    @Test
    fun `an exact text match removes the first copy in order`() {
        val list = listOf(copy("a", 1), copy("b", 2), copy("a", 3))
        val (rest, at) = HeldCopies.replacedBy(list, "a", 0)
        assertEquals(0, at)
        assertEquals(listOf(copy("b", 2), copy("a", 3)), rest)
    }

    @Test
    fun `a different text or a different number of attachments keeps the copy`() {
        val list = listOf(copy("hello", 1), copy("photo", 2, files = 1))
        assertEquals(-1, HeldCopies.replacedBy(list, "Hello", 0).second)
        assertEquals(-1, HeldCopies.replacedBy(list, "photo", 0).second)
        assertEquals(1, HeldCopies.replacedBy(list, "photo", 1).second)
    }

    @Test
    fun `two identical texts are matched one each`() {
        val store = HeldCopyStore(InMemoryHeldCopyBackend()) { 10L }
        store.addCarried("acct", "g", listOf(copy("hi", 1), copy("hi", 2)))
        assertEquals(copy("hi", 1), store.replace("acct", "g", "hi", 0))
        assertEquals(listOf(copy("hi", 2)), store.copies("acct", "g"))
        assertEquals(copy("hi", 2), store.replace("acct", "g", "hi", 0))
        assertNull(store.replace("acct", "g", "hi", 0))
        assertEquals(emptyList<HeldCopy>(), store.copies("acct", "g"))
    }

    @Test
    fun `copies still unmatched seven days after they were held are dropped`() {
        var now = 0L
        val store = HeldCopyStore(InMemoryHeldCopyBackend()) { now }
        store.addCarried("acct", "q", listOf(copy("old", 0), copy("new", HELD_COPY_TTL_MS / 2)))
        now = HELD_COPY_TTL_MS
        assertEquals(listOf("new"), store.copies("acct", "q").map { it.text })
    }

    @Test
    fun `copies move from the pending contact to the one-to-one and merge without duplicates`() {
        val store = HeldCopyStore(InMemoryHeldCopyBackend()) { 10L }
        store.addCarried("acct", "q", listOf(copy("a", 1), copy("b", 2)))
        store.addCarried("acct", "q", listOf(copy("a", 1)))
        assertEquals(2, store.copies("acct", "q").size)
        assertEquals(listOf("a", "b"), store.move("acct", "q", "g").map { it.text })
        assertEquals(emptyList<HeldCopy>(), store.copies("acct", "q"))
        assertEquals(listOf("a", "b"), store.copies("acct", "g").map { it.text })
        // Another account's copies are not touched.
        assertEquals(emptyMap<String, List<HeldCopy>>(), store.load("other"))
    }
}
