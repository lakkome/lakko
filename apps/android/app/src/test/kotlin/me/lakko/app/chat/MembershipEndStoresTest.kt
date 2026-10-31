// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import me.lakko.app.calls.CallHistoryEntry
import me.lakko.app.calls.callHistoryWithout
import me.lakko.app.channels.ChannelUnsubscribe
import me.lakko.app.profile.ProfileRequestPrefsStore
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * ADR-0087 section 9 - the stores an end writes and the per-group deletes it
 * calls, on real `SharedPreferences`, one account at a time.
 */
@RunWith(RobolectricTestRunner::class)
class MembershipEndStoresTest {

    private lateinit var context: Context

    @Before
    fun setUp() {
        context = ApplicationProvider.getApplicationContext()
    }

    @Test
    fun `an owed end survives a restart, per account`() {
        val store = SharedPrefsMembershipEndStore(context)
        val row = appliedEnd("aa", "Team", false, 9u)
        assertTrue(store.save("acct", row))
        val reopened = SharedPrefsMembershipEndStore(context).load("acct")
        assertEquals(listOf(row), reopened)
        assertTrue(SharedPrefsMembershipEndStore(context).load("other").isEmpty())
        store.remove("acct", "AA")
        assertTrue(store.load("acct").isEmpty())
    }

    @Test
    fun `an unreadable row is not an end`() {
        context.getSharedPreferences(SharedPrefsMembershipEndStore.PREFS_NAME, Context.MODE_PRIVATE)
            .edit().putString("v1:acct:aa", "not json").commit()
        assertTrue(SharedPrefsMembershipEndStore(context).load("acct").isEmpty())
    }

    @Test
    fun `a held Welcome survives a restart and goes when resolved`() {
        val store = SharedPrefsHeldWelcomeStore(context)
        val held = HeldWelcome("aa", "d2VsY29tZQ==")
        assertTrue(store.add("acct", held))
        assertEquals(listOf(held), SharedPrefsHeldWelcomeStore(context).load("acct"))
        assertTrue(store.load("other").isEmpty())
        store.remove("acct", held)
        assertTrue(store.load("acct").isEmpty())
    }

    @Test
    fun `a group's name goes and the next one stays`() {
        GroupNameStore.save(context, "acct", "aa", StoredGroupName("Team", 1u))
        GroupNameStore.save(context, "acct", "bb", StoredGroupName("Other", 1u))
        GroupNameStore.delete(context, "acct", "aa")
        assertNull(GroupNameStore.load(context, "acct", "aa"))
        assertNotNull(GroupNameStore.load(context, "acct", "bb"))
    }

    @Test
    fun `a group's call rows leave the call list`() {
        val entry = CallHistoryEntry(
            conversationIdHex = "aa",
            kind = ConversationKind.Group,
            outcome = me.lakko.app.calls.CallOutcome.OUTGOING,
            video = false,
            startedAtMs = 1L,
            durationSecs = 2L,
        )
        val other = entry.copy(conversationIdHex = "bb")
        assertEquals(listOf(other), callHistoryWithout(listOf(entry, other), "AA"))
    }

    @Test
    fun `what the group's removed devices were goes`() {
        DeviceReadmission.recordRemoved(context, "acct", "aa", listOf("x", "y"), listOf("x"))
        assertEquals(setOf("y"), DeviceReadmission.removed(context, "acct", "aa"))
        DeviceReadmission.clearGroup(context, "acct", "aa")
        assertTrue(DeviceReadmission.removed(context, "acct", "aa").isEmpty())
    }

    @Test
    fun `the profile request stamp of the group goes`() {
        val store = ProfileRequestPrefsStore(context)
        store.markRequested("acct", "aa", 5u)
        store.markRequested("acct", "bb", 5u)
        store.deleteGroup("acct", "aa")
        assertNull(store.lastRequestedAtMs("acct", "aa"))
        assertEquals(5uL, store.lastRequestedAtMs("acct", "bb"))
    }

    @Test
    fun `a heard channel end drops what this device follows, never what it publishes`() {
        val ended = listOf(
            HeardConversationEnd("aa", channel = true, epoch = 0u),
            HeardConversationEnd("bb", channel = true, epoch = 0u),
            HeardConversationEnd("cc", channel = false, epoch = 3u),
            HeardConversationEnd("dd", channel = true, epoch = 0u),
        )
        val leaving = ChannelUnsubscribe.channelsToLeave(
            ended = ended,
            followed = setOf("AA", "bb", "cc"),
            published = setOf("bb"),
        )
        assertEquals(listOf("aa"), leaving)
    }
}
