// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side coverage for the receive-side MAPPING TARGET of the
 * camera-off-cleanup media-state signal — the deterministic, FFI-free
 * half of the `classify -> setParticipantCamera` path.
 *
 * The classify half (`CallRelay.open` over a sealed
 * [uniffi.lakko_ffi_uniffi.composeMediaState] payload) is an FFI
 * round-trip that needs `liblakko_ffi_uniffi.so`, so it runs only in the
 * instrumented suite (`:app:connectedDebugAndroidTest`) — the same
 * host-vs-instrumented split as `PeerPinSupersessionInstrumentedTest`.
 *
 * What this asserts on the host JVM is exactly what
 * [SfuCallUi]'s `onRemoteMediaState` callback does once the session has
 * decoded `cameraOn`: `ui.setParticipantCamera("peer-<peerId>", cameraOn)`,
 * and what its `onRemoteMicrophoneState` does with `microphoneOn` (the
 * version-2 media state, 2026-11-07) through [RemoteMicStates]; plus the
 * send-side rules of [GroupMediaStateAnnouncer] and the one-to-one
 * [OneToOneMediaStateAnnouncer], which the sessions delegate to.
 * It pins:
 *   * the roster tile key contract (`"peer-<peerId>"`),
 *   * a mute (camera-off) flips `camOn` false without a track event,
 *   * an unmute (camera-back-on) flips it true again,
 *   * an unknown id (a media-state that raced ahead of `peer-joined`) is a
 *     safe no-op,
 *   * the local participant is never disturbed.
 */
class MediaStateMappingTest {

    private fun newState(): CallUiState =
        // The setters are synchronous StateFlow assignments (no launch), so
        // an Unconfined scope avoids referencing Dispatchers.Main off-device.
        CallUiState(CoroutineScope(Dispatchers.Unconfined))

    /** The exact key [SfuCallUi] derives from a relay sender's peer id. */
    private fun participantId(peerId: String): String = "peer-$peerId"

    private fun remote(peerId: String, camOn: Boolean): CallUiState.Participant =
        CallUiState.Participant(
            id = participantId(peerId),
            displayName = peerId,
            isLocal = false,
            micOn = true,
            camOn = camOn,
            quality = CallUiState.QualityTier.Good,
            speaking = false,
        )

    private fun local(): CallUiState.Participant =
        CallUiState.Participant(
            id = "local",
            displayName = "You",
            isLocal = true,
            micOn = true,
            camOn = true,
            quality = CallUiState.QualityTier.Good,
            speaking = false,
        )

    private fun camOnOf(state: CallUiState, id: String): Boolean =
        state.participants.value.first { it.id == id }.camOn

    @Test
    fun muteFlipsCamOffThenUnmuteFlipsCamBackOn() {
        val state = newState()
        // "bob" is present with its video track already arrived (camOn true).
        state.setParticipants(listOf(local(), remote("bob", camOn = true)))

        // camera-off media-state (a mute — no track event fires).
        state.setParticipantCamera(participantId("bob"), false)
        assertFalse("mute must flip bob's camOn off", camOnOf(state, "peer-bob"))

        // camera-back-on media-state (an unmute — also no track event).
        state.setParticipantCamera(participantId("bob"), true)
        assertTrue("unmute must flip bob's camOn back on", camOnOf(state, "peer-bob"))
    }

    @Test
    fun unknownPeerIsANoOp() {
        val state = newState()
        state.setParticipants(listOf(local(), remote("bob", camOn = true)))

        // A media-state for a peer not (yet) in the roster must not crash and
        // must not disturb the existing entries.
        state.setParticipantCamera(participantId("ghost"), false)

        assertEquals(2, state.participants.value.size)
        assertTrue(camOnOf(state, "peer-bob"))
        assertTrue(camOnOf(state, "local"))
    }

    @Test
    fun onlyTheTargetedRemoteIsAffected() {
        val state = newState()
        state.setParticipants(
            listOf(local(), remote("bob", camOn = true), remote("carol", camOn = true)),
        )

        state.setParticipantCamera(participantId("bob"), false)

        assertFalse("bob is muted", camOnOf(state, "peer-bob"))
        assertTrue("carol is untouched", camOnOf(state, "peer-carol"))
        assertTrue("local is untouched", camOnOf(state, "local"))
    }

    private fun micOnOf(state: CallUiState, id: String): Boolean =
        state.participants.value.first { it.id == id }.micOn

    // ---- Remote microphone (version-2 media state) ------------------------

    @Test
    fun aRemotePeerIsUnmutedByDefaultAndMutedAfterAnOffState() {
        val state = newState()
        val mics = RemoteMicStates()
        state.setParticipants(listOf(local(), remote("bob", camOn = false)))
        assertTrue(micOnOf(state, "peer-bob"))

        mics.record(state, participantId("bob"), false)
        assertFalse("an off state draws the mute mark", micOnOf(state, "peer-bob"))
        assertTrue("local is untouched", micOnOf(state, "local"))

        mics.record(state, participantId("bob"), true)
        assertTrue(micOnOf(state, "peer-bob"))
    }

    @Test
    fun aStateThatArrivesBeforeTheTileIsAppliedWhenTheTileIsBuilt() {
        val state = newState()
        val mics = RemoteMicStates()
        state.setParticipants(listOf(local()))
        // The media state raced ahead of `peer-joined`: no tile yet.
        mics.record(state, participantId("bob"), false)
        assertEquals(1, state.participants.value.size)

        val tile = SfuCallUi.remoteParticipant("", "bob", micOn = mics.micOn(participantId("bob")))
        state.participantJoined(tile)
        assertFalse(micOnOf(state, "peer-bob"))
        // A peer never heard from is built unmuted.
        assertTrue(mics.micOn(participantId("carol")))
    }

    @Test
    fun aPeerThatLeavesIsForgottenAndHangUpClearsAll() {
        val state = newState()
        val mics = RemoteMicStates()
        mics.record(state, participantId("bob"), false)
        mics.record(state, participantId("carol"), false)
        mics.forget(participantId("bob"))
        assertTrue(mics.micOn(participantId("bob")))
        assertFalse(mics.micOn(participantId("carol")))
        mics.clear()
        assertTrue(mics.micOn(participantId("carol")))
    }

    // ---- Send side: group / link call -------------------------------------

    @Test
    fun groupMuteSendsOneStateAndAnUnchangedValueSendsNothing() {
        val sent = mutableListOf<MediaFlags>()
        val announcer = GroupMediaStateAnnouncer(cameraOn = false, microphoneOn = true) { sent += it }

        announcer.setMicrophone(false)
        assertEquals(listOf(MediaFlags(cameraOn = false, microphoneOn = false)), sent)

        announcer.setMicrophone(false)
        assertEquals("an unchanged value sends nothing", 1, sent.size)

        announcer.setMicrophone(true)
        assertEquals(MediaFlags(cameraOn = false, microphoneOn = true), sent.last())
        assertEquals(2, sent.size)
    }

    @Test
    fun groupEveryAnnouncementCarriesBothFlags() {
        val sent = mutableListOf<MediaFlags>()
        val announcer = GroupMediaStateAnnouncer(cameraOn = false, microphoneOn = true) { sent += it }
        announcer.setMicrophone(false)
        announcer.setCamera(true)
        // After the publish offer.
        announcer.announce()
        assertEquals(
            listOf(
                MediaFlags(cameraOn = false, microphoneOn = false),
                MediaFlags(cameraOn = true, microphoneOn = false),
                MediaFlags(cameraOn = true, microphoneOn = false),
            ),
            sent,
        )
    }

    @Test
    fun groupMuteChosenBeforeAPeerJoinsIsInTheReannouncement() {
        val sent = mutableListOf<MediaFlags>()
        // Before capture: muted in the prejoin, nothing to send to yet.
        val announcer = GroupMediaStateAnnouncer(cameraOn = true, microphoneOn = true) { sent += it }
        announcer.setMicrophone(false)
        // The microphone is captured with the recorded choice ...
        assertFalse(announcer.microphoneOn)
        // ... and the targeted re-announcement to a newcomer carries it.
        assertEquals(MediaFlags(cameraOn = true, microphoneOn = false), announcer.current)
    }

    @Test
    fun groupMicrophoneSeededFromTheConfigAudioFlag() {
        val announcer = GroupMediaStateAnnouncer(cameraOn = false, microphoneOn = false) { }
        assertFalse(announcer.microphoneOn)
        assertEquals(MediaFlags(cameraOn = false, microphoneOn = false), announcer.current)
    }

    // ---- Send side: one-to-one call ----------------------------------------

    @Test
    fun oneToOneNothingBeforeConnectedThenTheFirstStateOnConnected() {
        val announcer = OneToOneMediaStateAnnouncer()
        assertNull(announcer.next(connected = false, camera = false, microphone = true))
        assertNull(announcer.next(connected = false, camera = false, microphone = false))
        assertEquals(
            MediaFlags(cameraOn = false, microphoneOn = true),
            announcer.next(connected = true, camera = false, microphone = true),
        )
    }

    @Test
    fun oneToOnePreCaptureMuteIsTheFirstStateSent() {
        val announcer = OneToOneMediaStateAnnouncer()
        // Muted before capture: nothing goes out yet.
        assertNull(announcer.next(connected = false, camera = false, microphone = false))
        assertEquals(
            MediaFlags(cameraOn = false, microphoneOn = false),
            announcer.next(connected = true, camera = false, microphone = false),
        )
    }

    @Test
    fun oneToOneMuteUnmuteAndCameraOffEachSendOneAndUnchangedSendsNothing() {
        val announcer = OneToOneMediaStateAnnouncer()
        assertNotNull(announcer.next(connected = true, camera = true, microphone = true))
        assertNull("unchanged", announcer.next(connected = true, camera = true, microphone = true))
        assertEquals(
            MediaFlags(cameraOn = true, microphoneOn = false),
            announcer.next(connected = true, camera = true, microphone = false),
        )
        assertEquals(
            MediaFlags(cameraOn = true, microphoneOn = true),
            announcer.next(connected = true, camera = true, microphone = true),
        )
        assertEquals(
            MediaFlags(cameraOn = false, microphoneOn = true),
            announcer.next(connected = true, camera = false, microphone = true),
        )
        assertNull(announcer.next(connected = true, camera = false, microphone = true))
    }

    @Test
    fun oneToOneResetStartsTheNextCallWithNothingSaid() {
        val announcer = OneToOneMediaStateAnnouncer()
        assertNotNull(announcer.next(connected = true, camera = false, microphone = true))
        announcer.reset()
        assertNotNull(announcer.next(connected = true, camera = false, microphone = true))
    }

    // ---- Receive side: one-to-one call -------------------------------------

    @Test
    fun oneToOneRemoteTileFollowsWhatThePersonSaid() {
        assertEquals(
            MediaFlags(cameraOn = true, microphoneOn = true),
            oneToOneRemoteMedia(remoteHasVideo = true, remoteCameraOn = true, remoteMicOn = true),
        )
        // A camera turned off keeps the track: the picture goes on the word.
        assertEquals(
            MediaFlags(cameraOn = false, microphoneOn = false),
            oneToOneRemoteMedia(remoteHasVideo = true, remoteCameraOn = false, remoteMicOn = false),
        )
        // No track, no picture, whatever was said.
        assertFalse(oneToOneRemoteMedia(remoteHasVideo = false, remoteCameraOn = true, remoteMicOn = true).cameraOn)
    }
}
