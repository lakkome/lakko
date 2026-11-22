// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import me.lakko.app.chat.ChatRepository
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import uniffi.lakko_ffi_uniffi.Role

/**
 * ADR-0087 section 10 — a group of one: `Add members` is offered only to
 * whoever may add people (the same gate as the info's Add members row), and
 * pressing it opens the Add members screen (covered in [AddMembersFlowTest]).
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
class SoloGroupBlockTest {
    init {
        EnglishForTests.install()
    }

    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    @Before
    fun setUp() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
    }

    private val chats = OpenChats()
    private val repos = mutableListOf<ChatRepository>()

    @After
    fun tearDown() {
        chats.leaveAllAndSettle()
        repos.forEach { it.close() }
        Dispatchers.resetMain()
    }

    @Test
    fun `the gate is the info's own rule`() {
        assertTrue(canManageMembersFor(Role.OWNER, membershipEnded = false))
        assertTrue(canManageMembersFor(Role.ADMIN, membershipEnded = false))
        assertTrue(canManageMembersFor(null, membershipEnded = false), "a group with no role table is open")
        assertFalse(canManageMembersFor(Role.MEMBER, membershipEnded = false))
        assertFalse(canManageMembersFor(Role.OWNER, membershipEnded = true))
    }

    @Test
    fun `a member who may not add people does not see Add members`() {
        renderBlock(canAddMembers = false) { error("not reachable") }
        compose.onNodeWithText(en("group_solo.only_you")).assertExists()
        compose.onNodeWithText(en("group_solo.add_members")).assertDoesNotExist()
    }

    @Test
    fun `whoever may add people sees Add members and it is wired`() {
        var pressed = 0
        renderBlock(canAddMembers = true) { pressed++ }
        compose.onNodeWithText(en("group_solo.add_members")).performClick()
        assertEquals(1, pressed)
    }

    private fun renderBlock(canAddMembers: Boolean, onAddMembers: () -> Unit) {
        val repo = ChatRepository(applicationContext = InMemoryContext()).also { repos += it }
        val vm = chats.open(ChatViewModel(repo = repo, groupIdHex = "00".repeat(16)))
        compose.setContent {
            SoloGroupBlock(viewModel = vm, onAddMembers = onAddMembers, alone = true, canAddMembers = canAddMembers)
        }
    }

    private fun assertTrue(value: Boolean, message: String) = org.junit.Assert.assertTrue(message, value)
}
