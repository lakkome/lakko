// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import me.lakko.app.chat.ChatRepository
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * Leaving the chat screen cancels whatever a membership handler had in
 * flight, and a cancelled request is not a failed one.
 *
 * `AddMemberUsernameRuleTest` covers the plain case, where the handler
 * receives the `CancellationException` itself. This covers the case the
 * repository actually produces: its publish paths wrap what they catch
 * (`throw ChatError(dsErrorMessage(e, ...), e)`), so the handler receives an
 * ordinary exception and a test on the exception's type alone reports it as a
 * failed request to a screen that is no longer there.
 *
 * The lookup is the one suspension a host test can hold open in the
 * un-bootstrapped repository, so it stands in for the publish: it answers its
 * cancellation the way the repository does, with its own error carrying the
 * cancellation as the cause.
 *
 * The control (the same wrapped error on a screen that is still open IS
 * reported) is `CancellationTest`: the reporting path ends in `android.util.Log`,
 * which the host JVM does not have.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ChatViewModelCancellationTest {

    @Before
    fun setUp() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private class WrappedFailure(cause: Throwable) : Exception("DS publish failed", cause)

    @Test
    fun `a cancellation the layer below wrapped is not reported and the busy flag is released`() {
        val lookups = mutableListOf<String>()
        val repo = ChatRepository(applicationContext = InMemoryContext())
        val chats = OpenChats()
        try {
            val vm = chats.open(
                ChatViewModel(
                    repo = repo,
                    groupIdHex = "00".repeat(16),
                    resolveRecipient = { name ->
                        lookups += name
                        try {
                            awaitCancellation()
                        } catch (e: CancellationException) {
                            throw WrappedFailure(e)
                        }
                    },
                    validateUsername = { it.trim().removePrefix("@").lowercase() },
                ),
            )
            vm.setAddMemberInput("alice")
            vm.onAddMember()
            assertEquals(listOf("alice"), lookups)
            assertTrue(vm.membershipBusy.value)

            chats.leaveAllAndSettle()
            assertNull(vm.membershipError.value)
            assertFalse(vm.membershipBusy.value)
        } finally {
            chats.leaveAllAndSettle()
            repo.close()
        }
    }
}
