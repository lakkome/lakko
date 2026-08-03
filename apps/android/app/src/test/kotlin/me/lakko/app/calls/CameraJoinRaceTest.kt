// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.webrtc.VideoTrack

/**
 * Host-side coverage for the camera-state JOIN-RACE guard shared by both SFU
 * call adapters — `SfuCallUi` and `MultiJoinerCall`.
 *
 * The race: in a group call, a newcomer B subscribing to an already
 * camera-muted peer A still receives A's present-but-DISABLED video track, so
 * `onRemoteVideoTrack` fires and — before the guard — set A's roster
 * `camOn=true` (a frozen / black tile). A's `onRemoteMediaState(off)`
 * re-announce also arrives, but the two are NOT ordered: if the track-add lands
 * AFTER the media-state, A was wrongly shown camera-on. The fix is an
 * adapter-local sticky `lastKnownCameraOn` map that the track-arrival path
 * defers to, so a known-muted peer is never flipped back on by a track arrival.
 *
 * Both adapters implement the guard IDENTICALLY (same "peer-<peerId>" key, same
 * `map[id] = cameraOn` on media-state, same
 * `if (track != null) map[id] ?: true else false` on track-arrival, same
 * `map.remove(id)` on peer-left). The adapters cannot be unit-instantiated —
 * their callbacks live inside a [SfuCallSession] that needs a Context + WebRTC +
 * the SFrame shim at `connect()` — so, exactly like [MediaStateMappingTest],
 * this replays the two callbacks' post-decode behaviour against a REAL
 * [CallUiState] + a REAL map on the host JVM. It pins the deterministic result
 * of every arrival order.
 *
 * `VideoTrack(handle)` is JVM-constructible (its constructor only stores the
 * handle; no native libwebrtc call), so a fake non-zero handle stands in.
 */
class CameraJoinRaceTest {

    private fun newState(): CallUiState =
        // Synchronous StateFlow setters (no launch) -> an Unconfined scope
        // avoids referencing Dispatchers.Main off-device.
        CallUiState(CoroutineScope(Dispatchers.Unconfined))

    /** The exact key both adapters derive from a relay sender's peer id. */
    private fun participantId(peerId: String): String = "peer-$peerId"

    private fun remote(peerId: String, camOn: Boolean): CallUiState.Participant =
        CallUiState.Participant(
            id = participantId(peerId),
            displayName = peerId,
            isLocal = false,
            micOn = true,
            camOn = camOn,
            handRaised = false,
            quality = CallUiState.QualityTier.Good,
            speaking = false,
            screenSharing = false,
        )

    private fun local(): CallUiState.Participant =
        CallUiState.Participant(
            id = "local",
            displayName = "You",
            isLocal = true,
            micOn = true,
            camOn = true,
            handRaised = false,
            quality = CallUiState.QualityTier.Good,
            speaking = false,
            screenSharing = false,
        )

    private fun camOnOf(state: CallUiState, id: String): Boolean =
        state.participants.value.first { it.id == id }.camOn

    // ---- Adapter behaviour replay (verbatim from SfuCallUi / MultiJoinerCall) --

    /** Replays `onRemoteMediaState`: media-state is authoritative — record it,
     *  then apply. */
    private fun onMediaState(
        state: CallUiState,
        map: MutableMap<String, Boolean>,
        peerId: String,
        cameraOn: Boolean,
    ) {
        val id = participantId(peerId)
        map[id] = cameraOn
        state.setParticipantCamera(id, cameraOn)
    }

    /** Replays `onRemoteVideoTrack`: defer to the sticky last-known state. */
    private fun onTrackArrival(
        state: CallUiState,
        map: MutableMap<String, Boolean>,
        peerId: String,
        track: VideoTrack?,
    ) {
        val id = participantId(peerId)
        state.setRemoteVideoTrack(track, id)
        val on = if (track != null) map[id] ?: true else false
        state.setParticipantCamera(id, on)
    }

    /** Replays `onPeerLeft`: drop the sticky entry. */
    private fun onPeerLeft(map: MutableMap<String, Boolean>, peerId: String) {
        map.remove(participantId(peerId))
    }

    // ---- The race ------------------------------------------------------------

    @Test
    fun mediaStateOffThenTrackArrivalKeepsCamOff() {
        // THE RACE: A is already camera-muted; the media-state(off) lands FIRST,
        // then A's still-present (disabled) track arrives at the newcomer. The
        // track arrival must NOT flip A back on.
        val state = newState()
        val map = mutableMapOf<String, Boolean>()
        state.setParticipants(listOf(local(), remote("A", camOn = false)))

        onMediaState(state, map, "A", cameraOn = false)
        onTrackArrival(state, map, "A", VideoTrack(1L))

        assertFalse(
            "a track ARRIVAL for a known-muted peer must keep camOn=false",
            camOnOf(state, "peer-A"),
        )
    }

    @Test
    fun trackArrivalThenMediaStateOffEndsCamOff() {
        // The other order: the track arrives first (defaults camOn=true), then
        // the authoritative media-state(off) corrects it to false.
        val state = newState()
        val map = mutableMapOf<String, Boolean>()
        state.setParticipants(listOf(local(), remote("A", camOn = false)))

        onTrackArrival(state, map, "A", VideoTrack(2L))
        assertTrue(
            "with no prior media-state a track arrival defaults camOn=true",
            camOnOf(state, "peer-A"),
        )

        onMediaState(state, map, "A", cameraOn = false)
        assertFalse(
            "the authoritative media-state(off) corrects camOn to false",
            camOnOf(state, "peer-A"),
        )
    }

    @Test
    fun trackArrivalWithNoPriorMediaStateDefaultsCamOn() {
        // The happy path: a genuinely camera-on peer whose track arrives with no
        // prior media-state -> camOn=true.
        val state = newState()
        val map = mutableMapOf<String, Boolean>()
        state.setParticipants(listOf(local(), remote("A", camOn = false)))

        onTrackArrival(state, map, "A", VideoTrack(3L))

        assertTrue(
            "an unknown (no sticky entry) peer defaults camOn=true on a track arrival",
            camOnOf(state, "peer-A"),
        )
    }

    @Test
    fun mediaStateOnThenTrackArrivalStaysOn() {
        // A known camera-ON peer's track arrival stays on (the sticky state is
        // true).
        val state = newState()
        val map = mutableMapOf<String, Boolean>()
        state.setParticipants(listOf(local(), remote("A", camOn = false)))

        onMediaState(state, map, "A", cameraOn = true)
        onTrackArrival(state, map, "A", VideoTrack(4L))

        assertTrue(camOnOf(state, "peer-A"))
    }

    @Test
    fun peerLeftClearsStickyStateSoAReJoinDefaultsOn() {
        // A muted peer leaves; its sticky entry is cleared. A subsequent
        // re-join's track arrival must NOT inherit the stale "off" — it defaults
        // on again (the fresh peer decides its own state).
        val state = newState()
        val map = mutableMapOf<String, Boolean>()
        state.setParticipants(listOf(local(), remote("A", camOn = false)))

        onMediaState(state, map, "A", cameraOn = false)
        onPeerLeft(map, "A")

        // A re-joins (roster re-adds the tile) and its track arrives.
        state.setParticipants(listOf(local(), remote("A", camOn = false)))
        onTrackArrival(state, map, "A", VideoTrack(5L))

        assertTrue(
            "a cleared sticky entry must not leave a re-joined peer stuck off",
            camOnOf(state, "peer-A"),
        )
    }
}
