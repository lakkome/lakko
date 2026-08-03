// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import org.webrtc.VideoTrack

/**
 * Unit coverage for the N-way SFU per-source-peer remote-video map on
 * [CallUiState] (add / remove / reset), plus the invariant that the
 * per-peer map is independent of the single-track field the 1×1 P2P
 * `driveFrom` path uses (no regression).
 *
 * `VideoTrack(handle)` is JVM-constructible: its constructor (and
 * `MediaStreamTrack`'s) only stores the handle + builds an
 * `IdentityHashMap`; no native libwebrtc call is made, and the map
 * setter only stores the reference (never touches the track), so a
 * fake non-zero handle is a valid stand-in here.
 */
class CallUiStateRemoteVideoTest {

    private fun newState(): CallUiState =
        // Explicit non-Main scope: the map setters are synchronous
        // StateFlow assignments (no launch), so no test dispatcher is
        // needed — this just avoids referencing Dispatchers.Main off-device.
        CallUiState(CoroutineScope(Dispatchers.Unconfined))

    @Test
    fun addsAndRemovesPerPeerTracksKeyedById() {
        val state = newState()
        val alice = VideoTrack(1L)
        val bob = VideoTrack(2L)

        state.setRemoteVideoTrack(alice, "peer-alice")
        state.setRemoteVideoTrack(bob, "peer-bob")

        assertEquals(2, state.remoteVideoTracks.value.size)
        assertSame(alice, state.remoteVideoTracks.value["peer-alice"])
        assertSame(bob, state.remoteVideoTracks.value["peer-bob"])

        // A null track removes exactly that peer's entry, leaving the rest.
        state.setRemoteVideoTrack(null, "peer-alice")
        assertNull(state.remoteVideoTracks.value["peer-alice"])
        assertSame(bob, state.remoteVideoTracks.value["peer-bob"])
        assertEquals(1, state.remoteVideoTracks.value.size)
    }

    @Test
    fun resetClearsThePerPeerMap() {
        val state = newState()
        state.setRemoteVideoTrack(VideoTrack(3L), "peer-x")
        state.setRemoteVideoTrack(VideoTrack(4L), "peer-y")
        assertEquals(2, state.remoteVideoTracks.value.size)

        state.reset()

        assertTrue(state.remoteVideoTracks.value.isEmpty())
    }

    @Test
    fun singleTrackFieldIsIndependentOfThePerPeerMap() {
        // The 1×1 P2P driveFrom path drives the single [remoteVideoTrack]
        // field; the SFU group path drives the map. Writing one must never
        // disturb the other.
        val state = newState()
        val single = VideoTrack(5L)

        state.setRemoteVideoTrack(single) // 1-arg overload (1×1)
        state.setRemoteVideoTrack(VideoTrack(6L), "peer-z") // 2-arg (SFU)

        assertSame(single, state.remoteVideoTrack.value)
        assertEquals(1, state.remoteVideoTracks.value.size)
        // And the map setter never wrote the single field.
        state.setRemoteVideoTrack(null, "peer-z")
        assertSame(single, state.remoteVideoTrack.value)
        assertTrue(state.remoteVideoTracks.value.isEmpty())
    }
}
