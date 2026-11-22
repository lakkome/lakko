// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import me.lakko.app.chat.AddMemberPick
import me.lakko.app.chat.ChatRepository
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * `docs/ux/conversation-info.md` section 2.4 — the Add members screen over
 * the real [ChatViewModel] (un-bootstrapped repository, in-memory context):
 * it opens with no pick, its back drops the picks, closing the info closes
 * it, and **Add** returns to the info at once, before any pick is added.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class AddMembersFlowTest {

    @Before
    fun setUp() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private val anna = AddMemberPick(keyB64 = "AA==", accountIdB64 = "AA==", name = "Anna", grantWireB64 = "g")

    @Test
    fun `back drops the picks and closing the info closes the screen over it`() {
        val repo = ChatRepository(applicationContext = InMemoryContext())
        val chats = OpenChats()
        try {
            val vm = chats.open(ChatViewModel(repo = repo, groupIdHex = "00".repeat(16)))
            vm.toggleMembersPanel()
            vm.openAddMembers()
            assertTrue(vm.addMembersOpen.value)
            vm.toggleAddPick(anna)
            assertEquals(listOf(anna), vm.addPicks.value)
            vm.closeAddMembers()
            assertFalse(vm.addMembersOpen.value)
            assertEquals(emptyList<AddMemberPick>(), vm.addPicks.value)

            vm.openAddMembers()
            vm.toggleAddPick(anna)
            vm.toggleMembersPanel()
            assertFalse(vm.membersPanelOpen.value)
            assertFalse(vm.addMembersOpen.value)
            assertEquals(emptyList<AddMemberPick>(), vm.addPicks.value)
        } finally {
            chats.leaveAllAndSettle()
            repo.close()
        }
    }

    @Test
    fun `Add members in a group of one opens the picker over the info, not the info alone`() {
        val repo = ChatRepository(applicationContext = InMemoryContext())
        val chats = OpenChats()
        try {
            val vm = chats.open(ChatViewModel(repo = repo, groupIdHex = "00".repeat(16)))
            vm.openAddMembersFromConversation()
            assertTrue(vm.membersPanelOpen.value)
            assertTrue(vm.addMembersOpen.value)
            assertEquals(
                me.lakko.app.chat.GroupInfoLayer.ADD_MEMBERS,
                me.lakko.app.chat.groupInfoLayer(
                    infoOpen = vm.membersPanelOpen.value, isGroup = true,
                    addMembersOpen = vm.addMembersOpen.value, transferOpen = false,
                ),
            )
            // Its back returns to the info, which stays open under it.
            vm.closeAddMembers()
            assertTrue(vm.membersPanelOpen.value)
            assertFalse(vm.addMembersOpen.value)
            // Pressed again with the info already open: the info is not closed.
            vm.openAddMembersFromConversation()
            assertTrue(vm.membersPanelOpen.value)
            assertTrue(vm.addMembersOpen.value)
        } finally {
            chats.leaveAllAndSettle()
            repo.close()
        }
    }

    @Test
    fun `Add returns to the info at once and the picks are added there`() {
        val held = mutableListOf<String>()
        val gids = mutableListOf<String>()
        val repo = ChatRepository(applicationContext = InMemoryContext())
        val chats = OpenChats()
        try {
            val vm = chats.open(
                ChatViewModel(
                    repo = repo,
                    groupIdHex = "00".repeat(16),
                    keepFoundGrant = { gid, _, wire ->
                        // G-53 — kept as the found grant of the add into this group.
                        held += wire
                        gids += gid
                        awaitCancellation()
                    },
                ),
            )
            vm.toggleMembersPanel()
            vm.openAddMembers()
            vm.toggleAddPick(anna)
            vm.onAddPicked()
            // The screen is gone while the first pick is still being added.
            assertFalse(vm.addMembersOpen.value)
            assertTrue(vm.membersPanelOpen.value)
            assertEquals(emptyList<AddMemberPick>(), vm.addPicks.value)
            assertEquals(listOf("g"), held)
            assertEquals(listOf("00".repeat(16)), gids)
            assertTrue(vm.membershipBusy.value)
        } finally {
            chats.leaveAllAndSettle()
            repo.close()
        }
    }

    @Test
    fun `an Everyone pick records its binding for this group and keeps no grant`() {
        val bindings = mutableListOf<Pair<String, String>>()
        val grants = mutableListOf<String>()
        val eve = AddMemberPick(keyB64 = "AQ==", accountIdB64 = "AQ==", name = "Eve", mailboxBindingB64 = "bind")
        val repo = ChatRepository(applicationContext = InMemoryContext())
        val chats = OpenChats()
        try {
            val vm = chats.open(
                ChatViewModel(
                    repo = repo,
                    groupIdHex = "11".repeat(16),
                    keepFoundGrant = { _, _, wire -> grants += wire },
                    keepFoundBinding = { gid, _, binding ->
                        bindings += gid to binding
                        awaitCancellation()
                    },
                ),
            )
            vm.toggleMembersPanel()
            vm.openAddMembers()
            vm.toggleAddPick(eve)
            vm.onAddPicked()
            assertEquals(listOf("11".repeat(16) to "bind"), bindings)
            assertEquals(emptyList<String>(), grants)
        } finally {
            chats.leaveAllAndSettle()
            repo.close()
        }
    }
}
