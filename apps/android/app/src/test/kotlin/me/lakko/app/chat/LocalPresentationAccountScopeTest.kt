// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.Base64
import me.lakko.app.contacts.AcceptedContactsStore
import me.lakko.app.media.MediaPrefs
import me.lakko.app.ui.chat.FolderTabPrefs
import me.lakko.app.ui.chat.GroupPresentationStore
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * The local presentation facts about an account's conversations — the
 * persisted Chats/Groups kind, the created-as-group flag, the selected folder
 * tab and the played marks — belong to the signed-in account (2026-09-30:
 * they were keyed on the group, folder or blob id alone, so a second account
 * on the same phone that shared a conversation inherited them, and forget
 * device left them behind). Each follows the account bound in
 * [AcceptedContactsStore].
 */
@RunWith(RobolectricTestRunner::class)
class LocalPresentationAccountScopeTest {

    private val accountA = ByteArray(16) { 0x0A }
    private val accountB = ByteArray(16) { 0x0B }
    private val aB64 = Base64.getEncoder().encodeToString(accountA)
    private val group = "ab".repeat(32)

    @Before
    fun setUp() = AcceptedContactsStore.unbind()

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    @Test
    fun `the conversation kind is per account`() {
        val context = RuntimeEnvironment.getApplication()
        AcceptedContactsStore.bind(accountA)
        ConversationKindStore.set(context, group, ConversationKind.Group)
        assertEquals(ConversationKind.Group, ConversationKindStore.stored(context, group))

        AcceptedContactsStore.bind(accountB)
        assertNull(ConversationKindStore.stored(context, group))
        // B derives its own kind from the people count.
        assertEquals(ConversationKind.Chat, ConversationKindStore.resolve(context, group, peopleCount = 2))

        AcceptedContactsStore.unbind()
        assertNull(ConversationKindStore.stored(context, group))
        // No account: shown, never written.
        assertEquals(ConversationKind.Group, ConversationKindStore.set(context, "cd".repeat(32), ConversationKind.Group))
        AcceptedContactsStore.bind(accountA)
        assertNull(ConversationKindStore.stored(context, "cd".repeat(32)))
        assertEquals(ConversationKind.Group, ConversationKindStore.stored(context, group))

        ConversationKindStore.deleteAccount(context, aB64)
        assertNull(ConversationKindStore.stored(context, group))
        AcceptedContactsStore.bind(accountB)
        assertEquals(ConversationKind.Chat, ConversationKindStore.stored(context, group))
    }

    @Test
    fun `created-as-group and the selected folder are per account`() {
        val context = RuntimeEnvironment.getApplication()
        AcceptedContactsStore.bind(accountA)
        GroupPresentationStore.markCreatedAsGroup(context, group)
        FolderTabPrefs.save(context, "f00d")
        assertTrue(GroupPresentationStore.wasCreatedAsGroup(context, group))
        assertEquals("f00d", FolderTabPrefs.load(context))

        AcceptedContactsStore.bind(accountB)
        assertFalse(GroupPresentationStore.wasCreatedAsGroup(context, group))
        assertNull(FolderTabPrefs.load(context))
        FolderTabPrefs.save(context, "beef")

        AcceptedContactsStore.unbind()
        assertFalse(GroupPresentationStore.wasCreatedAsGroup(context, group))
        assertNull(FolderTabPrefs.load(context))

        GroupPresentationStore.deleteAccount(context, aB64)
        FolderTabPrefs.deleteAccount(context, aB64)
        AcceptedContactsStore.bind(accountA)
        assertFalse(GroupPresentationStore.wasCreatedAsGroup(context, group))
        assertNull(FolderTabPrefs.load(context))
        AcceptedContactsStore.bind(accountB)
        assertEquals("beef", FolderTabPrefs.load(context))
    }

    @Test
    fun `played marks are per account and survive a new instance`() {
        val context = RuntimeEnvironment.getApplication()
        val prefs = MediaPrefs(context)
        AcceptedContactsStore.bind(accountA)
        prefs.markPlayed("blob1")
        assertTrue(prefs.played.isPlayed("blob1"))

        AcceptedContactsStore.bind(accountB)
        assertFalse(prefs.played.isPlayed("blob1"))
        prefs.markPlayed("blob2")

        AcceptedContactsStore.unbind()
        assertFalse(prefs.played.isPlayed("blob1"))
        prefs.markPlayed("blob3")
        assertFalse(prefs.played.isPlayed("blob3"))

        // Writes are queued on one thread; a forget queued after them runs
        // after them. Wait for both, then read back through a new instance.
        AcceptedContactsStore.bind(accountA)
        prefs.forgetAccount(aB64)
        assertFalse(prefs.played.isPlayed("blob1"))
        prefs.awaitWrites()
        val reopened = MediaPrefs(context)
        assertFalse(reopened.played.isPlayed("blob1"))
        AcceptedContactsStore.bind(accountB)
        assertTrue(reopened.played.isPlayed("blob2"))
        assertFalse(reopened.played.isPlayed("blob3"))
    }

    @Test
    fun `the played file name is one path segment`() {
        val name = MediaPrefs.playedFileName("+/+/AAAAAAAAAAAAAAAAAAAA==")
        assertFalse(name.contains('/'))
        assertTrue(name.endsWith(".txt"))
    }
}
