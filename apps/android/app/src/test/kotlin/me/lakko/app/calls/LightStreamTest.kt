// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import org.webrtc.VideoTrack
import uniffi.lakko_ffi_uniffi.TileVideoQuality

/**
 * The copies of a remote camera (ADR-0070 steps 1 and 5): reading the stream
 * id, what the two SFU call adapters do with a track
 * ([applyRemoteVideoTrack], the one function both call), and how what the SFU
 * gives and what a tile shows travel between the call and the tiles.
 *
 * `VideoTrack(handle)` is JVM-constructible (its constructor only stores the
 * handle), and nothing here touches the track, so a fake handle stands in.
 */
class LightStreamTest {

    private val peer = "p000000000000000518db68c125428388"

    private fun newState(): CallUiState = CallUiState(CoroutineScope(Dispatchers.Unconfined))

    private fun remote(peerId: String, camOn: Boolean = false): CallUiState.Participant =
        SfuCallUi.remoteParticipant(name = peerId, peerId = peerId).copy(camOn = camOn)

    // ---- the stream id ---------------------------------------------------

    @Test
    fun `a stream id ending in a suffix is that copy of the peer before it`() {
        assertEquals("~light", LIGHT_STREAM_SUFFIX)
        assertEquals("~mid", MIDDLE_STREAM_SUFFIX)
        assertEquals(RemoteStreamId(peer, RemoteStreamRole.LIGHT), splitStreamId("$peer~light"))
        assertEquals(RemoteStreamId(peer, RemoteStreamRole.MIDDLE), splitStreamId("$peer~mid"))
    }

    @Test
    fun `any other stream id is the peer's main stream`() {
        assertEquals(RemoteStreamId(peer, RemoteStreamRole.MAIN), splitStreamId(peer))
        assertEquals(RemoteStreamId("$peer~lightx", RemoteStreamRole.MAIN), splitStreamId("$peer~lightx"))
        assertEquals(RemoteStreamId("$peer~LIGHT", RemoteStreamRole.MAIN), splitStreamId("$peer~LIGHT"))
        assertEquals(RemoteStreamId("$peer~middle", RemoteStreamRole.MAIN), splitStreamId("$peer~middle"))
        assertEquals(RemoteStreamId("", RemoteStreamRole.MAIN), splitStreamId(""))
    }

    @Test
    fun `a bare suffix names no peer and is read as an ordinary stream id`() {
        assertEquals(RemoteStreamId("~light", RemoteStreamRole.MAIN), splitStreamId("~light"))
        assertEquals(RemoteStreamId("~mid", RemoteStreamRole.MAIN), splitStreamId("~mid"))
    }

    @Test
    fun `the light and the middle stream id of a peer split back to that peer`() {
        assertEquals("$peer~light", lightStreamIdOf(peer))
        assertEquals(RemoteStreamId(peer, RemoteStreamRole.LIGHT), splitStreamId(lightStreamIdOf(peer)))
        assertEquals("$peer~mid", middleStreamIdOf(peer))
        assertEquals(RemoteStreamId(peer, RemoteStreamRole.MIDDLE), splitStreamId(middleStreamIdOf(peer)))
    }

    @Test
    fun `a roster id and its peer id convert both ways`() {
        assertEquals("peer-$peer", remoteParticipantId(peer))
        assertEquals(peer, peerIdOfParticipant(remoteParticipantId(peer)))
        assertEquals(SfuCallUi.participantId(peer), remoteParticipantId(peer))
    }

    // ---- what a call does with the track ---------------------------------

    @Test
    fun `the companion never creates a participant, a camera flag or a main track`() {
        val state = newState()
        state.setParticipants(listOf(remote("alice")))
        val light = VideoTrack(7L)

        applyRemoteVideoTrack(state, light, "alice~light", emptyMap())

        assertSame(light, state.remoteLightVideoTracks.value["peer-alice"])
        assertEquals(1, state.remoteLightVideoTracks.value.size)
        // Not in the main map under any key: neither the peer's nor the raw
        // stream id's, which is where it used to leak.
        assertTrue(state.remoteVideoTracks.value.isEmpty())
        // The roster is as it was, and the camera flag was not touched.
        assertEquals(listOf("peer-alice"), state.participants.value.map { it.id })
        assertFalse(state.participants.value.single().camOn)
    }

    @Test
    fun `the middle track goes to its own map and touches nothing else`() {
        val state = newState()
        state.setParticipants(listOf(remote("alice")))
        val main = VideoTrack(1L)
        val middle = VideoTrack(2L)
        val light = VideoTrack(3L)

        applyRemoteVideoTrack(state, middle, "alice~mid", emptyMap())
        // No participant, no camera flag, not in the main or the light map.
        assertSame(middle, state.remoteMiddleVideoTracks.value["peer-alice"])
        assertTrue(state.remoteVideoTracks.value.isEmpty())
        assertTrue(state.remoteLightVideoTracks.value.isEmpty())
        assertFalse(state.participants.value.single().camOn)

        applyRemoteVideoTrack(state, main, "alice", emptyMap())
        applyRemoteVideoTrack(state, light, "alice~light", emptyMap())
        assertSame(main, state.remoteVideoTracks.value["peer-alice"])
        assertSame(middle, state.remoteMiddleVideoTracks.value["peer-alice"])
        assertSame(light, state.remoteLightVideoTracks.value["peer-alice"])

        // The middle track going away leaves the other two.
        applyRemoteVideoTrack(state, null, "alice~mid", emptyMap())
        assertNull(state.remoteMiddleVideoTracks.value["peer-alice"])
        assertSame(main, state.remoteVideoTracks.value["peer-alice"])
        assertSame(light, state.remoteLightVideoTracks.value["peer-alice"])
        assertTrue(state.participants.value.single().camOn)
    }

    @Test
    fun `what the SFU gives is put on the tiles by roster id, and goes with its peer`() {
        val state = newState()
        state.setParticipants(listOf(remote("alice"), remote("bob")))
        applyVideoGiven(state, mapOf("alice" to TileVideoQuality.LIGHT, "bob" to TileVideoQuality.MIDDLE))
        assertEquals(
            mapOf("peer-alice" to TileVideoQuality.LIGHT, "peer-bob" to TileVideoQuality.MIDDLE),
            state.remoteVideoGiven.value,
        )
        state.participantLeft("peer-alice")
        assertEquals(mapOf("peer-bob" to TileVideoQuality.MIDDLE), state.remoteVideoGiven.value)
    }

    @Test
    fun `a tile's report reaches the call under the SFU peer id`() {
        val state = newState()
        val reports = ArrayList<Triple<Any, String, TileVideoQuality?>>()
        val tile = Any()
        // A tile drawn before the call is bound is heard of when it is.
        state.reportTileVideoQuality(tile, "peer-alice", TileVideoQuality.MIDDLE)
        bindTileVideoQuality(state) { t, peerId, quality -> reports += Triple(t, peerId, quality) }
        assertEquals(listOf(Triple(tile, "alice", TileVideoQuality.MIDDLE)), reports.toList())

        state.reportTileVideoQuality(tile, "peer-alice", TileVideoQuality.MAIN)
        state.reportTileVideoQuality(tile, "peer-alice", null)
        assertEquals(
            listOf<TileVideoQuality?>(TileVideoQuality.MIDDLE, TileVideoQuality.MAIN, null),
            reports.map { it.third },
        )

        // The call ended: a tile taken down afterwards reports to nobody.
        state.clearRemoteCopyVideoTracks()
        state.reportTileVideoQuality(tile, "peer-alice", TileVideoQuality.MAIN)
        assertEquals(3, reports.size)
    }

    @Test
    fun `the companion of a peer not on the roster creates nobody`() {
        val state = newState()
        applyRemoteVideoTrack(state, VideoTrack(8L), "stranger~light", emptyMap())
        assertTrue(state.participants.value.isEmpty())
        assertTrue(state.remoteVideoTracks.value.isEmpty())
    }

    @Test
    fun `the companion never replaces the main track`() {
        val state = newState()
        state.setParticipants(listOf(remote("alice")))
        val main = VideoTrack(1L)
        val light = VideoTrack(2L)

        applyRemoteVideoTrack(state, main, "alice", emptyMap())
        applyRemoteVideoTrack(state, light, "alice~light", emptyMap())

        assertSame(main, state.remoteVideoTracks.value["peer-alice"])
        assertSame(light, state.remoteLightVideoTracks.value["peer-alice"])
        assertTrue(state.participants.value.single().camOn)

        // The companion going away leaves the main track and the camera flag.
        applyRemoteVideoTrack(state, null, "alice~light", emptyMap())
        assertNull(state.remoteLightVideoTracks.value["peer-alice"])
        assertSame(main, state.remoteVideoTracks.value["peer-alice"])
        assertTrue(state.participants.value.single().camOn)
    }

    @Test
    fun `the main track still defers to the last known camera state`() {
        val state = newState()
        state.setParticipants(listOf(remote("alice")))
        applyRemoteVideoTrack(state, VideoTrack(1L), "alice", mapOf("peer-alice" to false))
        assertFalse(state.participants.value.single().camOn)
        // And a companion arriving for a muted peer does not turn it on.
        applyRemoteVideoTrack(state, VideoTrack(2L), "alice~light", mapOf("peer-alice" to false))
        assertFalse(state.participants.value.single().camOn)
    }

    @Test
    fun `a track with no stream id is ignored`() {
        val state = newState()
        applyRemoteVideoTrack(state, VideoTrack(1L), null, emptyMap())
        assertTrue(state.remoteVideoTracks.value.isEmpty())
        assertTrue(state.remoteLightVideoTracks.value.isEmpty())
    }

    @Test
    fun `the companion is pruned with its peer`() {
        val state = newState()
        state.setParticipants(listOf(remote("alice"), remote("bob")))
        val bobLight = VideoTrack(4L)
        applyRemoteVideoTrack(state, VideoTrack(3L), "alice~light", emptyMap())
        applyRemoteVideoTrack(state, bobLight, "bob~light", emptyMap())

        state.participantLeft("peer-alice")

        assertNull(state.remoteLightVideoTracks.value["peer-alice"])
        assertSame(bobLight, state.remoteLightVideoTracks.value["peer-bob"])
    }

    @Test
    fun `every companion is dropped when the call ends`() {
        val state = newState()
        applyRemoteVideoTrack(state, VideoTrack(3L), "alice~light", emptyMap())
        applyRemoteVideoTrack(state, VideoTrack(4L), "bob~light", emptyMap())

        applyRemoteVideoTrack(state, VideoTrack(6L), "alice~mid", emptyMap())
        applyVideoGiven(state, mapOf("alice" to TileVideoQuality.LIGHT))

        // What both adapters do when the session fails or closes.
        state.clearRemoteCopyVideoTracks()
        assertTrue(state.remoteLightVideoTracks.value.isEmpty())
        assertTrue(state.remoteMiddleVideoTracks.value.isEmpty())
        assertTrue(state.remoteVideoGiven.value.isEmpty())

        // And a reset for the next call leaves none behind either.
        applyRemoteVideoTrack(state, VideoTrack(5L), "carol~light", emptyMap())
        state.reset()
        assertTrue(state.remoteLightVideoTracks.value.isEmpty())
    }
}
