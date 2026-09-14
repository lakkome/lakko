// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.lifecycle.ViewModelStore
import java.time.Instant
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import me.lakko.app.calls.CallSession
import me.lakko.app.calls.OneToOneCallHost
import me.lakko.app.calls.OneToOneCallHostTest
import me.lakko.app.chat.ChatRepository
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiCallMediaFlags
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody

/**
 * The 1x1 call's state is independent of the chat view models.
 *
 * ## The defect behind this
 *
 * Each `ChatViewModel` used to own the 1x1 `CallSession` and overwrite the
 * repository's single call-signaling slot with a closure filtered to its own
 * chat. Opening chat B took the slot from chat A (whose view model the activity
 * keeps), so A's incoming calls went nowhere; and the call, its banner and its
 * in-call screen went away with the chat screen. Here the call lives in the
 * host: view models for other chats are created and destroyed around it, and it
 * is still ringing, still reachable from every view model, and still one call.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class OneToOneCallSurvivesChatScreensTest {

    private val groupA = "0a".repeat(16)
    private val groupB = "0b".repeat(16)
    private val callId = ByteArray(16) { 0x44 }

    @Test
    fun `a call rings for chat A while chat B is open, and survives both chats being torn down`() = runTest {
        Dispatchers.setMain(UnconfinedTestDispatcher(testScheduler))
        val repo = ChatRepository(applicationContext = InMemoryContext())
        try {
            val now = System.currentTimeMillis()
            val port = OneToOneCallHostTest.FakePort()
            val host = OneToOneCallHost<OneToOneCallHostTest.FakeSession>(
                scope = backgroundScope,
                clock = { now },
                warn = { _, _ -> },
            )
            host.attach(port)
            host.setForeground(true)
            repo.oneToOneCallInbound = host::deliver

            val store = ViewModelStore()
            val vmA = ChatViewModel(repo = repo, groupIdHex = groupA, callHost = host)
            store.put("chat-$groupA", vmA)
            // Chat B opened after A — under the old slot, B now owned signaling.
            val vmB = ChatViewModel(repo = repo, groupIdHex = groupB, callHost = host)
            store.put("chat-$groupB", vmB)

            repo.routeParsedCallSignaling(
                groupA,
                3,
                FfiCallSignalingBody.Offer(callId, "v=0", 1uL, FfiCallMediaFlags(audio = true, video = false)),
                Instant.ofEpochMilli(now).toString(),
                senderLeaf = 1u,
            )
            runCurrent()

            val ringing = host.active.value
            assertNotNull("the offer for chat A rang although chat B was the latest view", ringing)
            assertEquals(groupA, ringing!!.groupIdHex)
            // Every chat sees the one app-wide call (so each hides its call buttons).
            assertSame(ringing, vmA.oneToOneCall.value)
            assertSame(ringing, vmB.oneToOneCall.value)

            // Leaving both chats destroys their view models: the call is untouched.
            store.clear()
            runCurrent()
            assertSame(ringing, host.active.value)
            assertEquals(CallSession.WebCallState.Ringing, ringing.session.state.value)
            assertTrue(ringing.session.hangUps.isEmpty())
            assertTrue(host.hasCall)

            // A chat opened afterwards cannot start a second call over it.
            val vmC = ChatViewModel(repo = repo, groupIdHex = groupB, callHost = host)
            vmC.startCall()
            runCurrent()
            assertEquals(1, port.created.size)
            assertEquals("Another call is in progress.", vmC.callNotice.value)
            assertFalse(host.startOutgoing(groupB))
        } finally {
            repo.close()
            Dispatchers.resetMain()
        }
    }
}
