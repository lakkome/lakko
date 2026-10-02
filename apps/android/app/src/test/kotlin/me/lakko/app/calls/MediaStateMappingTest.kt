// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side coverage for the receive-side MAPPING TARGET of the
 * camera-off-cleanup media-state signal — the deterministic, FFI-free
 * half of the `classify -> setParticipantCamera` path.
 *
 * The classify half ([uniffi.lakko_ffi_uniffi.dispatchIncomingEnvelope]
 * over a [uniffi.lakko_ffi_uniffi.composeMediaState] payload) is an FFI
 * round-trip that needs `liblakko_ffi_uniffi.so`, so it runs only in the
 * instrumented suite (`:app:connectedDebugAndroidTest`) — the same
 * host-vs-instrumented split as `PeerPinSupersessionInstrumentedTest`.
 *
 * What this asserts on the host JVM is exactly what
 * [SfuCallUi]'s `onRemoteMediaState` callback does once the session has
 * decoded `cameraOn`: `ui.setParticipantCamera("peer-<peerId>", cameraOn)`.
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
}
