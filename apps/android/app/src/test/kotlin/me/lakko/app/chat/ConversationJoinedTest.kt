// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * `docs/ux/group-and-channel-privacy.md` section 10 — what the chat list sorts a
 * conversation by, and the moment it falls back to, on real
 * `SharedPreferences`.
 */
@RunWith(RobolectricTestRunner::class)
class ConversationJoinedTest {

    private lateinit var context: Context

    @Before
    fun setUp() {
        context = ApplicationProvider.getApplicationContext()
    }

    @Test
    fun `a conversation with no message sorts by the moment it was joined`() {
        assertEquals(500L, conversationActivityMs(lastMessageMs = null, joinedAtMs = 500L))
    }

    @Test
    fun `the last message wins even when the join is newer`() {
        // A device added to an account joins every chat at once, and the
        // history bundle gives them their real last message: the join must not
        // stack them all at one moment.
        assertEquals(100L, conversationActivityMs(lastMessageMs = 100L, joinedAtMs = 500L))
        assertEquals(900L, conversationActivityMs(lastMessageMs = 900L, joinedAtMs = 500L))
    }

    @Test
    fun `nothing happened and no moment is no time at all`() {
        assertNull(conversationActivityMs(lastMessageMs = null, joinedAtMs = null))
    }

    @Test
    fun `a moment survives a reload and belongs to one account and one group`() {
        ConversationJoinedStore.save(context, "acct", "AA", 42L)
        assertEquals(42L, ConversationJoinedStore.load(context, "acct", "aa"))
        assertNull(ConversationJoinedStore.load(context, "acct", "bb"))
        assertNull(ConversationJoinedStore.load(context, "other", "aa"))
    }

    @Test
    fun `a rejoin moves the moment, a founded group brought back does not`() {
        ConversationJoinedStore.saveIfAbsent(context, "acct", "aa", 10L)
        ConversationJoinedStore.saveIfAbsent(context, "acct", "aa", 20L)
        assertEquals(10L, ConversationJoinedStore.load(context, "acct", "aa"))
        ConversationJoinedStore.save(context, "acct", "aa", 30L)
        assertEquals(30L, ConversationJoinedStore.load(context, "acct", "aa"))
    }

    @Test
    fun `the end of a membership and forgetting the device take it`() {
        ConversationJoinedStore.save(context, "acct", "aa", 1L)
        ConversationJoinedStore.save(context, "acct", "bb", 2L)
        ConversationJoinedStore.save(context, "other", "aa", 3L)
        ConversationJoinedStore.delete(context, "acct", "AA")
        assertNull(ConversationJoinedStore.load(context, "acct", "aa"))
        assertEquals(2L, ConversationJoinedStore.load(context, "acct", "bb"))
        ConversationJoinedStore.deleteAccount(context, "acct")
        assertNull(ConversationJoinedStore.load(context, "acct", "bb"))
        assertEquals(3L, ConversationJoinedStore.load(context, "other", "aa"))
    }
}
