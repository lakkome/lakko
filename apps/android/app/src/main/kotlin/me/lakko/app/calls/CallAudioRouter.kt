// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.Build
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Where the call's audio comes out — `docs/ux/calls.md` section 4.1.
 *
 * ## The defect this closes
 *
 * "Speaker was not on the screen at all, although it is the control people
 * reach for most on a voice call." It was not merely missing from the control
 * row: nothing in the app had ever touched [AudioManager] for a call, so a 1x1
 * call played through whichever route the system happened to pick and the user
 * had no way to change it. A voice call held to the ear and a voice call on a
 * desk are different calls, and only the second one is usable hands-free.
 *
 * ## What it does
 *
 * While a call is up the stream is put into [AudioManager.MODE_IN_COMMUNICATION]
 * — the mode that makes the earpiece, the proximity blanking and the volume
 * rocker behave like a phone call rather than like media playback — and the
 * output is pinned to the earpiece or the loudspeaker.
 *
 * Android has two APIs for that and they do not overlap:
 *
 *  - API 31 and up, [AudioManager.setCommunicationDevice] picks a device from
 *    the live list. This is the supported route, and the older one is formally
 *    deprecated there.
 *  - Below 31, `isSpeakerphoneOn`. It is the only route that exists.
 *
 * Both are attempted defensively: an OEM that refuses the request leaves the
 * route where it was, and [speakerOn] re-reads the system rather than trusting
 * the write, so the button never claims a route the device did not take.
 *
 * ## What it deliberately does not do
 *
 * No Bluetooth or wired-headset picker. Section 4.1 asks for one Speaker button,
 * not a route menu, and a headset already wins over both built-in devices by the
 * platform's own rules. A picker is a separate feature with its own UX.
 */
public class CallAudioRouter(context: Context) {

    private val appContext: Context = context.applicationContext
    private val audio: AudioManager? =
        appContext.getSystemService(Context.AUDIO_SERVICE) as? AudioManager

    private val mutableSpeakerOn = MutableStateFlow(false)

    /** `true` when the call is playing out of the loudspeaker. */
    public val speakerOn: StateFlow<Boolean> = mutableSpeakerOn.asStateFlow()

    private var previousMode: Int? = null
    private var engaged: Boolean = false

    /**
     * Take the audio focus shape of a call: communication mode, earpiece by
     * default. Idempotent — a second call while engaged does nothing, so an
     * activity recreate cannot stack modes.
     */
    public fun start() {
        val manager = audio ?: return
        if (engaged) return
        engaged = true
        previousMode = manager.mode
        runCatching { manager.mode = AudioManager.MODE_IN_COMMUNICATION }
            .onFailure { warn("could not enter communication mode", it) }
        setSpeaker(false)
    }

    /** Put the route back the way it was found. Idempotent. */
    public fun stop() {
        val manager = audio ?: return
        if (!engaged) return
        engaged = false
        runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                manager.clearCommunicationDevice()
            } else {
                @Suppress("DEPRECATION")
                manager.isSpeakerphoneOn = false
            }
        }.onFailure { warn("could not clear the call audio route", it) }
        previousMode?.let { mode ->
            runCatching { manager.mode = mode }
                .onFailure { warn("could not restore the audio mode", it) }
        }
        previousMode = null
        mutableSpeakerOn.value = false
    }

    /** Route to the loudspeaker ([on]) or back to the earpiece. */
    public fun setSpeaker(on: Boolean) {
        val manager = audio ?: return
        runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                val wanted = if (on) {
                    AudioDeviceInfo.TYPE_BUILTIN_SPEAKER
                } else {
                    AudioDeviceInfo.TYPE_BUILTIN_EARPIECE
                }
                val device = manager.availableCommunicationDevices.firstOrNull { it.type == wanted }
                if (device != null) {
                    manager.setCommunicationDevice(device)
                } else if (!on) {
                    // A tablet with no earpiece: clearing returns the platform
                    // default rather than leaving the speaker pinned on.
                    manager.clearCommunicationDevice()
                }
            } else {
                @Suppress("DEPRECATION")
                manager.isSpeakerphoneOn = on
            }
        }.onFailure { warn("could not change the call audio route", it) }
        // Re-read rather than assume: the button must show the route the device
        // actually took, not the one that was asked for.
        mutableSpeakerOn.value = readSpeakerOn(manager)
    }

    /** Flip the route. What the Speaker button calls. */
    public fun toggleSpeaker() {
        setSpeaker(!mutableSpeakerOn.value)
    }

    private fun readSpeakerOn(manager: AudioManager): Boolean = runCatching {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            manager.communicationDevice?.type == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER
        } else {
            @Suppress("DEPRECATION")
            manager.isSpeakerphoneOn
        }
    }.getOrDefault(false)

    private fun warn(message: String, cause: Throwable?) {
        android.util.Log.w(TAG, message, cause)
    }

    private companion object {
        private const val TAG = "CallAudioRouter"
    }
}
