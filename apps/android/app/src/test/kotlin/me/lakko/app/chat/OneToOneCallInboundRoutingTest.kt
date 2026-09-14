// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.time.Instant
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import me.lakko.app.calls.CallSession
import me.lakko.app.calls.OneToOneCallHost
import me.lakko.app.calls.OneToOneCallHostTest
import me.lakko.app.calls.callIdHexOf
import me.lakko.app.ui.chat.InMemoryContext
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallMediaFlags
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody

/**
 * The repository's side of the app-level call host: every parsed body a poll
 * applies, for ANY group, goes to the one-to-one hook or to the group-call
 * ledger — never filtered by which chat is open, and never a group-control body
 * into the 1x1 path (C-11).
 *
 * Runs the real [ChatRepository.routeParsedCallSignaling] over an in-memory
 * `Context` (no session, no native core). The payload parse that precedes it
 * needs the UniFFI `.so`, so the parsed body is supplied directly.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class OneToOneCallInboundRoutingTest {

    private val groupA = "aa".repeat(16)
    private val groupB = "bb".repeat(16)
    private val callId = ByteArray(16) { 0x33 }
    private val media = FfiCallMediaFlags(audio = true, video = false)
    private val now = System.currentTimeMillis()
    private val createdAt = Instant.ofEpochMilli(now).toString()

    private data class Delivered(val groupIdHex: String, val messageId: Long, val body: FfiCallSignalingBody)

    private fun repoWithRecorder(): Pair<ChatRepository, MutableList<Delivered>> {
        val repo = ChatRepository(applicationContext = InMemoryContext())
        val seen = mutableListOf<Delivered>()
        repo.oneToOneCallInbound = { g, id, body, _ -> seen += Delivered(g, id, body) }
        return repo to seen
    }

    @Test
    fun `one-to-one bodies reach the call hook with their group and row id`() {
        val (repo, seen) = repoWithRecorder()
        try {
            val offer = FfiCallSignalingBody.Offer(callId, "v=0", 1uL, media)
            val ice = FfiCallSignalingBody.IceCandidate(callId, "c", "0", 0u)
            val answer = FfiCallSignalingBody.Answer(callId, "v=0")
            repo.routeParsedCallSignaling(groupB, 41, offer, createdAt, senderLeaf = 1u)
            repo.routeParsedCallSignaling(groupB, 42, ice, createdAt, senderLeaf = 1u)
            repo.routeParsedCallSignaling(groupA, 7, answer, createdAt, senderLeaf = 0u)

            assertEquals(
                listOf(Delivered(groupB, 41, offer), Delivered(groupB, 42, ice), Delivered(groupA, 7, answer)),
                seen,
            )
            assertTrue("a 1x1 body is not a group-call record", repo.groupCalls.value.isEmpty())
        } finally {
            repo.close()
        }
    }

    @Test
    fun `group-control bodies feed the ledger and never the call hook (C-11)`() {
        val (repo, seen) = repoWithRecorder()
        try {
            repo.routeParsedCallSignaling(
                groupA,
                1,
                FfiCallSignalingBody.GroupStart(callId, 2u, now.toULong(), media),
                createdAt,
                senderLeaf = 2u,
            )
            repo.routeParsedCallSignaling(groupA, 2, FfiCallSignalingBody.GroupJoin(callId, 2u, 1uL), createdAt, 2u)
            repo.routeParsedCallSignaling(
                groupA,
                3,
                FfiCallSignalingBody.GroupLeave(callId, 2u, FfiCallEndReason.HUNG_UP),
                createdAt,
                2u,
            )
            assertTrue("no group-control body may reach the 1x1 path", seen.isEmpty())
            assertEquals(callIdHexOf(callId), repo.groupCalls.value[groupA]?.callIdHex)
        } finally {
            repo.close()
        }
    }

    @Test
    fun `End goes to both the ledger and the call hook, each filtering on its own call`() {
        val (repo, seen) = repoWithRecorder()
        try {
            repo.routeParsedCallSignaling(
                groupA,
                1,
                FfiCallSignalingBody.GroupStart(callId, 2u, now.toULong(), media),
                createdAt,
                2u,
            )
            val end = FfiCallSignalingBody.End(callId, FfiCallEndReason.HUNG_UP)
            repo.routeParsedCallSignaling(groupA, 2, end, createdAt, 2u)
            assertEquals(listOf(Delivered(groupA, 2, end)), seen)
            assertNull(repo.groupCalls.value[groupA])
        } finally {
            repo.close()
        }
    }

    @Test
    fun `an offer routed by the repository rings the host with no chat view in existence`() = runTest {
        val repo = ChatRepository(applicationContext = InMemoryContext())
        try {
            val port = OneToOneCallHostTest.FakePort()
            val host = OneToOneCallHost<OneToOneCallHostTest.FakeSession>(
                scope = backgroundScope,
                clock = { now },
                warn = { _, _ -> },
            )
            host.attach(port)
            host.setForeground(true)
            repo.oneToOneCallInbound = host::deliver

            repo.routeParsedCallSignaling(
                groupB,
                9,
                FfiCallSignalingBody.Offer(callId, "v=0", 1uL, media),
                createdAt,
                senderLeaf = 1u,
            )
            runCurrent()

            val call = host.active.value
            assertNotNull(call)
            assertEquals(groupB, call!!.groupIdHex)
            assertEquals(CallSession.WebCallState.Ringing, call.session.state.value)
        } finally {
            repo.close()
        }
    }
}
