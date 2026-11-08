// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * This device's camera and microphone, as told to the other side of a call
 * (2026-11-07). A mute is `track.setEnabled(false)`, which fires nothing on the
 * other side, so the other side's tile shows the mute mark and the camera-off
 * picture only from what this device says (`docs/ux/group-call-grid.md` 7.2).
 *
 * Kept out of the sessions so the rules run in a host unit test, where
 * libwebrtc and the native core are absent. Mirror of the web
 * `sfu-call-session.ts` / `call-session.ts` rules.
 */
internal data class MediaFlags(val cameraOn: Boolean, val microphoneOn: Boolean)

/**
 * A group or link call (SFU relay): the version-2 media state goes to the
 * whole call on every camera change, after the publish offer and, targeted, to
 * each peer that joins. A microphone change is sent only when the value
 * changed. Not a decision about sealing or sending: [send] does both and may
 * send nothing (no relay key, no socket).
 *
 * The microphone is recorded even before capture, so the session applies a
 * mute chosen early to the microphone it captures and the first state it says
 * already carries it.
 */
internal class GroupMediaStateAnnouncer(
    cameraOn: Boolean,
    microphoneOn: Boolean,
    /** Seal and send the state to every other peer of the call. */
    private val send: (MediaFlags) -> Unit,
) {
    @Volatile
    private var camera: Boolean = cameraOn

    @Volatile
    private var microphone: Boolean = microphoneOn

    /** What this device currently says (the targeted per-joiner send). */
    val current: MediaFlags get() = MediaFlags(camera, microphone)

    /** The recorded microphone choice, applied to the captured track. */
    val microphoneOn: Boolean get() = microphone

    /** The camera changed (or was asserted again): always said. */
    fun setCamera(on: Boolean) {
        camera = on
        announce()
    }

    /** The microphone choice: recorded always, said only when it changed. */
    fun setMicrophone(on: Boolean) {
        if (microphone == on) return
        microphone = on
        announce()
    }

    /** Say the current state to the whole call. */
    fun announce() {
        send(current)
    }
}

/**
 * A one-to-one call (the `mediaState` call-signalling body): said once the call
 * is connected and then on every change, never before `connected`, never a
 * wake. Main thread only.
 */
internal class OneToOneMediaStateAnnouncer {
    private var announced: MediaFlags? = null

    /**
     * The state to send now, or `null`: nothing before [connected], and
     * nothing when [camera] and [microphone] are what was last sent. A
     * returned state counts as sent.
     */
    fun next(connected: Boolean, camera: Boolean, microphone: Boolean): MediaFlags? {
        if (!connected) return null
        val flags = MediaFlags(camera, microphone)
        if (flags == announced) return null
        announced = flags
        return flags
    }

    /** The call is over: the next call starts with nothing said. */
    fun reset() {
        announced = null
    }
}

/**
 * The other person of a one-to-one call, as their tile draws them: the mute
 * mark from what they said, and a picture only while their video track is
 * there AND they have not said the camera is off (a camera turned off is
 * `track.setEnabled(false)`, which the receiving track never reports).
 */
internal fun oneToOneRemoteMedia(
    remoteHasVideo: Boolean,
    remoteCameraOn: Boolean,
    remoteMicOn: Boolean,
): MediaFlags = MediaFlags(cameraOn = remoteHasVideo && remoteCameraOn, microphoneOn = remoteMicOn)

/**
 * What each remote peer of a group or link call last said about its
 * microphone, keyed on its roster id (`peer-<peerId>`). The adapters
 * (`SfuCallUi`, `MultiJoinerCall`) record each opened media state here and on
 * the tile; a state that arrives before the tile exists is applied when the
 * tile is built ([micOn]). Unmuted until a peer says otherwise. Not
 * thread-safe: the adapters touch it from the session's scope only.
 */
internal class RemoteMicStates {
    private val known = mutableMapOf<String, Boolean>()

    /** Record what [participantId] said and set its tile's mute mark (a
     *  no-op on the tile when it does not exist yet). */
    fun record(ui: CallUiState, participantId: String, microphoneOn: Boolean) {
        known[participantId] = microphoneOn
        ui.setParticipantMic(participantId, microphoneOn)
    }

    /** The `micOn` a tile for [participantId] is built with. */
    fun micOn(participantId: String): Boolean = known[participantId] ?: true

    /** The peer left: someone who later joins under the same id starts unmuted. */
    fun forget(participantId: String) {
        known.remove(participantId)
    }

    /** The call is over. */
    fun clear() {
        known.clear()
    }
}
