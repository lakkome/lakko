// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import me.lakko.app.chat.ChatRepository
import me.lakko.app.l10n.EnglishForTests
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test

/**
 * ADR-0073 R.5 - what the person typed and did not send survives the chat
 * screen being rebuilt.
 *
 * The screen's view model is bound to ONE group. When the conversation moves
 * to its successor the screen is rebuilt on the new group with a new view
 * model; the unsent text went with the old one. It is the conversation's, so
 * the new view model starts with it.
 *
 * The repository is not bootstrapped here, so it knows no lineage and a
 * group is its own conversation: what is driven end to end is the view
 * model's half (it reports every change and seeds once), on one group. That
 * two groups of one conversation share the key is the repository's
 * `conversationIdOf`, covered by the lineage tests.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ComposerSurvivesMoveTest {
    init {
        EnglishForTests.install()
    }

    private val group = "0c".repeat(16)
    private lateinit var repo: ChatRepository
    private val built = ArrayList<ChatViewModel>()

    private fun vm(groupIdHex: String = group) = ChatViewModel(repo = repo, groupIdHex = groupIdHex).also { built += it }

    @Before
    fun setUp() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
        repo = ChatRepository(applicationContext = InMemoryContext())
    }

    @After
    fun tearDown() {
        built.forEach { it.clearAndJoin() }
        repo.close()
        Dispatchers.resetMain()
    }

    @Test
    fun `a screen rebuilt on the conversation starts with the unsent text`() {
        val first = vm()
        first.onDraftChange("half a thought")
        assertEquals("half a thought", repo.unsentComposerText(group))

        val rebuilt = vm()
        assertEquals("half a thought", rebuilt.state.value.draft)
    }

    @Test
    fun `text cleared in the composer is not brought back`() {
        val first = vm()
        first.onDraftChange("half a thought")
        first.onDraftChange("")
        assertEquals(null, repo.unsentComposerText(group))
        assertEquals("", vm().state.value.draft)
    }

    @Test
    fun `another conversation's composer is not touched`() {
        vm().onDraftChange("for this one")
        assertEquals("", vm("0d".repeat(16)).state.value.draft)
    }
}
