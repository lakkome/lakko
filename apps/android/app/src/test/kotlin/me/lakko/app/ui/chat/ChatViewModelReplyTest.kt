// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.Direction
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test

/**
 * ADR-0052 / `docs/ux/message-actions.md` section 3.1 — "One thing at a
 * time": the view model's half of the reply strip's exclusivity rules.
 *
 * The edit dialog's own state lives in the screen, so what is asserted here is
 * the seam the screen drives: arming, replacing, the close control, an edit
 * starting, and a send taking the reply with it. Built the way
 * [ChatViewModelConstructionTest] builds it — the real repository over an
 * in-memory `Context`, never bootstrapped, so no native code is touched.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ChatViewModelReplyTest {
    private lateinit var repo: ChatRepository
    private lateinit var vm: ChatViewModel

    @Before
    fun setUp() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
        repo = ChatRepository(applicationContext = InMemoryContext())
        vm = ChatViewModel(repo = repo, groupIdHex = "00".repeat(16))
    }

    @After
    fun tearDown() {
        // Before `resetMain`: see [clearAndJoin].
        vm.clearAndJoin()
        repo.close()
        Dispatchers.resetMain()
    }

    private fun message(id: Long, text: String) = ChatMessage(
        messageId = id,
        direction = Direction.Incoming,
        text = text,
        createdAt = "1970-01-01T00:00:00Z",
        authorKey = "Ym9i",
    )

    @Test
    fun `choosing Reply arms the strip with the snapshot taken then`() {
        vm.onReplyToMessage(message(5L, "  hello\nthere "), "Anna")
        val armed = vm.replyingTo.value
        assertNotNull(armed)
        assertEquals(5L, armed!!.messageId)
        assertEquals("Anna", armed.snapshot.author)
        assertEquals("hello there", armed.snapshot.excerpt)
    }

    @Test
    fun `Reply on another message replaces the strip`() {
        vm.onReplyToMessage(message(5L, "first"), "Anna")
        vm.onReplyToMessage(message(6L, "second"), "Ben")
        assertEquals(6L, vm.replyingTo.value?.messageId)
        assertEquals("second", vm.replyingTo.value?.snapshot?.excerpt)
    }

    @Test
    fun `Reply on the same message twice is a new arm, so the field refocuses`() {
        vm.onReplyToMessage(message(5L, "first"), "Anna")
        val first = vm.replyingTo.value
        vm.onReplyToMessage(message(5L, "first"), "Anna")
        val second = vm.replyingTo.value
        assertEquals(first!!.messageId, second!!.messageId)
        assert(first.armedSeq != second.armedSeq) { "two arms must not compare equal" }
    }

    @Test
    fun `a row with no id cannot be replied to`() {
        vm.onReplyToMessage(message(0L, "unsent"), "You")
        assertNull(vm.replyingTo.value)
    }

    @Test
    fun `the close control clears it`() {
        vm.onReplyToMessage(message(5L, "first"), "Anna")
        vm.onCancelReply()
        assertNull(vm.replyingTo.value)
    }

    @Test
    fun `starting an edit drops the reply`() {
        vm.onReplyToMessage(message(5L, "first"), "Anna")
        vm.onEditStarted()
        assertNull(vm.replyingTo.value)
    }

    @Test
    fun `a send takes the reply with it`() {
        // The repository is not bootstrapped, so the publish itself fails
        // inside the launched coroutine and is caught there; the strip is
        // cleared BEFORE that, in the same step as the draft, because the
        // quote now lives on the row.
        vm.onReplyToMessage(message(5L, "first"), "Anna")
        vm.onDraftChange("an answer")
        vm.onSendClicked()
        assertNull(vm.replyingTo.value)
        // The typing signal and the send each hop to the repository's MLS
        // thread and resume on Main. Wait for them here, while the test Main
        // is still installed: a resumption after `resetMain` reaches the
        // global handler and is charged to whichever `runTest` comes next in
        // this JVM. The send's failure path is the observable (`sending`
        // flips back), and the MLS dispatcher is one FIFO thread, so the
        // typing tasks dispatched before it have finished by then. Joining the
        // scope's children instead would never return: the constructor's
        // collectors live as long as the view model.
        val deadline = System.nanoTime() + 10_000_000_000L
        while (vm.state.value.sending && System.nanoTime() < deadline) Thread.sleep(5)
        assertFalse(vm.state.value.sending)
    }
}
