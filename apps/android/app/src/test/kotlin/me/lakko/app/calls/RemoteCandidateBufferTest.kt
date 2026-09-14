// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Remote ICE candidates that arrive before the remote description is set
 * (while an incoming 1x1 call rings, or while the SDP is still being applied)
 * are kept and released in order, never dropped.
 */
class RemoteCandidateBufferTest {

    private fun candidate(n: Int) = RemoteIceCandidate(sdpMid = "$n", sdpMLineIndex = n, candidateSdp = "candidate:$n")

    @Test
    fun candidatesBeforeTheRemoteDescriptionAreKeptAndReleasedInOrder() {
        val buffer = RemoteCandidateBuffer()
        assertTrue(buffer.offer(candidate(1)))
        assertTrue(buffer.offer(candidate(2)))
        assertTrue(buffer.offer(candidate(3)))
        assertEquals(3, buffer.size)

        assertEquals(listOf(candidate(1), candidate(2), candidate(3)), buffer.markRemoteDescriptionApplied())
        assertEquals(0, buffer.size)
    }

    @Test
    fun candidatesAfterTheRemoteDescriptionAreAppliedDirectly() {
        val buffer = RemoteCandidateBuffer()
        assertEquals(emptyList<RemoteIceCandidate>(), buffer.markRemoteDescriptionApplied())
        assertFalse(buffer.offer(candidate(1)))
        assertEquals(0, buffer.size)
    }

    @Test
    fun theKeptCandidatesAreReleasedOnlyOnce() {
        val buffer = RemoteCandidateBuffer()
        buffer.offer(candidate(1))
        assertEquals(1, buffer.markRemoteDescriptionApplied().size)
        assertEquals(emptyList<RemoteIceCandidate>(), buffer.markRemoteDescriptionApplied())
    }

    @Test
    fun capacityBoundsWhatIsKept() {
        val buffer = RemoteCandidateBuffer(capacity = 2)
        assertTrue(buffer.offer(candidate(1)))
        assertTrue(buffer.offer(candidate(2)))
        // Over capacity the candidate is still taken off the caller (not
        // applied to a connection that cannot accept it), just discarded.
        assertTrue(buffer.offer(candidate(3)))
        assertEquals(1, buffer.dropped)
        assertEquals(listOf(candidate(1), candidate(2)), buffer.markRemoteDescriptionApplied())
    }

    @Test
    fun resetForgetsAndBuffersAgain() {
        val buffer = RemoteCandidateBuffer(capacity = 1)
        buffer.offer(candidate(1))
        buffer.offer(candidate(2))
        buffer.markRemoteDescriptionApplied()
        buffer.reset()

        assertEquals(0, buffer.dropped)
        assertTrue(buffer.offer(candidate(4)))
        assertEquals(listOf(candidate(4)), buffer.markRemoteDescriptionApplied())
    }
}
