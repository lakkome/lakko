// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.lifecycle.viewModelScope
import java.util.concurrent.CountDownLatch
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import me.lakko.app.chat.ChatRepository
import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * A test that built a [ChatViewModel] must not return while the view model's
 * work is still on another thread ([OpenChats] says why).
 *
 * ## The defect this pins
 *
 * Four test classes built a view model, never cleared it, and reset `Main` in
 * teardown. The constructor's collector was by then inside
 * `withContext(Dispatchers.Default)`; when the worker thread got to it after
 * the reset, its resumption threw `Module with the Main dispatcher had failed
 * to initialize`, and the next `runTest` in the JVM failed with
 * `UncaughtExceptionsBeforeTest`. Which side won depended on how busy the
 * machine was, so the suite failed in some full runs and not in others.
 *
 * ## How the race is made certain here
 *
 * `Dispatchers.Default` is filled with tasks parked on a gate before the view
 * model is built, so the collector's block is queued behind them and cannot
 * run until the gate opens. That is the losing side of the race, every time.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ChatViewModelTeardownTest {
    private val gate = CountDownLatch(1)
    private val parked = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    @Before
    fun setUp() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
    }

    @After
    fun tearDown() {
        gate.countDown()
        parked.cancel()
        Dispatchers.resetMain()
    }

    @Test
    fun `settling waits for the work a cleared view model still has on another thread`() {
        // More parked tasks than Default has threads, all queued ahead of
        // anything the view model dispatches there.
        repeat(PARKED_TASKS) { parked.launch { gate.await() } }
        val repo = ChatRepository(applicationContext = InMemoryContext())
        val chats = OpenChats()
        try {
            val vm = chats.open(ChatViewModel(repo = repo, groupIdHex = "00".repeat(16)))
            val scope = vm.viewModelScope.coroutineContext.job

            // Clearing cancels the scope, and that alone does not finish it:
            // the collector's block has not come back from Default yet.
            chats.leaveAll()
            assertTrue(scope.isCancelled)
            assertFalse("the scope finished with work still queued on Default", scope.isCompleted)

            val opened = AtomicBoolean(false)
            val opener = thread {
                Thread.sleep(GATE_HELD_MS)
                opened.set(true)
                gate.countDown()
            }
            chats.leaveAllAndSettle()
            assertTrue("settling returned before the queued work could run", opened.get())
            assertTrue(scope.isCompleted)
            opener.join()
        } finally {
            gate.countDown()
            chats.leaveAllAndSettle()
            repo.close()
        }
        // Anything that did resume past this point is charged to the next
        // `runTest` in the JVM; make that this test rather than a stranger.
        Dispatchers.resetMain()
        Thread.sleep(LATE_RESUME_WINDOW_MS)
        runTest { }
    }

    private companion object {
        const val PARKED_TASKS: Int = 512
        const val GATE_HELD_MS: Long = 200
        const val LATE_RESUME_WINDOW_MS: Long = 200
    }
}
