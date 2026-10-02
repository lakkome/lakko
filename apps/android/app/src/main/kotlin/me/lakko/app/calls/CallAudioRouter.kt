// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.Build
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Where the call's audio comes out, and which microphone it takes —
 * `docs/ux/calls.md` section 4.1.
 *
 * ## What it does
 *
 * While a call is up the stream is put into [AudioManager.MODE_IN_COMMUNICATION]
 * — the mode that makes the earpiece, the proximity blanking and the volume
 * rocker behave like a phone call rather than like media playback — and the
 * output is sent where [callOutputTarget] says:
 *
 *  - the output the person chose in the call's Settings sheet, while it is
 *    still there;
 *  - otherwise, for a group call and any call with video, the loudspeaker,
 *    with a wired headset or a Bluetooth device taking over by itself, also
 *    when it is connected or removed in the middle of the call;
 *  - otherwise, for a one-to-one voice call, the earpiece.
 *
 * There is no Speaker button: the rule and the Settings sheet replace it.
 *
 * Android has two APIs for the route and they do not overlap:
 *
 *  - API 31 and up, [AudioManager.setCommunicationDevice] picks a device from
 *    the live list. This is the supported route, and the older one is formally
 *    deprecated there.
 *  - Below 31 there is no way to name a device: the app has the loudspeaker
 *    switch and the Bluetooth switch, and with both off the platform itself
 *    picks a wired headset when one is plugged in and the earpiece otherwise.
 *
 * Both are attempted defensively: an OEM that refuses the request leaves the
 * route where it was.
 *
 * The microphone is the platform's by default: it follows the route (a
 * headset's own microphone with the headset). A microphone chosen in the
 * Settings sheet is handed to the audio module as its preferred input.
 */
public class CallAudioRouter(
    context: Context,
    /** Hand the audio module its preferred input, or `null` for the
     *  platform's own. Injected so the router holds no WebRTC type. */
    private val preferInput: (AudioDeviceInfo?) -> Unit = WebRtcPeerConnection::setPreferredAudioInput,
) {

    private val appContext: Context = context.applicationContext
    private val audio: AudioManager? =
        appContext.getSystemService(Context.AUDIO_SERVICE) as? AudioManager

    private var previousMode: Int? = null
    private var engaged: Boolean = false

    /** The call's default output is the loudspeaker ([callPlaysOnLoudspeaker]). */
    private var loudspeaker: Boolean = false

    /** The output chosen in the Settings sheet, or `null` for the rule. */
    private var selectedOutputId: Int? = null

    /** The microphone chosen in the Settings sheet, or `null`. */
    private var selectedInputId: Int? = null

    /** Bluetooth SCO was started by this router (below API 31 only). */
    private var scoStarted: Boolean = false

    private val mutableOutputs = MutableStateFlow<List<CallAudioOutput>>(emptyList())

    /** The outputs the call can be put on now, for the Settings sheet. Empty
     *  when the platform lets the app choose nothing. */
    public val outputs: StateFlow<List<CallAudioOutput>> = mutableOutputs.asStateFlow()

    private val mutableInputs = MutableStateFlow<List<CallAudioInput>>(emptyList())

    /** The microphones the call can be put on now, for the Settings sheet. */
    public val inputs: StateFlow<List<CallAudioInput>> = mutableInputs.asStateFlow()

    private val deviceCallback = object : AudioDeviceCallback() {
        override fun onAudioDevicesAdded(addedDevices: Array<out AudioDeviceInfo>?) {
            if (engaged) apply()
        }

        override fun onAudioDevicesRemoved(removedDevices: Array<out AudioDeviceInfo>?) {
            if (engaged) apply()
        }
    }

    /**
     * Take the audio focus shape of a call: communication mode, and the call's
     * route. Idempotent — a second call while engaged does nothing, so an
     * activity recreate cannot stack modes.
     *
     * @param loudspeaker the call's default output is the loudspeaker
     *   ([callPlaysOnLoudspeaker]); otherwise the earpiece.
     * @param selectedOutputId what the person already chose for this call,
     *   when the screen is coming back to it; likewise [selectedInputId].
     */
    public fun start(loudspeaker: Boolean, selectedOutputId: Int? = null, selectedInputId: Int? = null) {
        val manager = audio ?: return
        if (engaged) return
        engaged = true
        this.loudspeaker = loudspeaker
        this.selectedOutputId = selectedOutputId
        this.selectedInputId = selectedInputId
        previousMode = manager.mode
        runCatching { manager.mode = AudioManager.MODE_IN_COMMUNICATION }
            .onFailure { warn("could not enter communication mode", it) }
        runCatching { manager.registerAudioDeviceCallback(deviceCallback, null) }
            .onFailure { warn("could not watch the audio devices", it) }
        apply()
    }

    /** Put the route back the way it was found. Idempotent. */
    public fun stop() {
        val manager = audio ?: return
        if (!engaged) return
        engaged = false
        runCatching { manager.unregisterAudioDeviceCallback(deviceCallback) }
            .onFailure { warn("could not stop watching the audio devices", it) }
        stopSco(manager)
        runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                manager.clearCommunicationDevice()
            } else {
                @Suppress("DEPRECATION")
                manager.isSpeakerphoneOn = false
            }
        }.onFailure { warn("could not clear the call audio route", it) }
        runCatching { preferInput(null) }
            .onFailure { warn("could not clear the preferred microphone", it) }
        previousMode?.let { mode ->
            runCatching { manager.mode = mode }
                .onFailure { warn("could not restore the audio mode", it) }
        }
        previousMode = null
        mutableOutputs.value = emptyList()
        mutableInputs.value = emptyList()
    }

    /** The call's default output changed: a camera came on or went off. */
    public fun setLoudspeakerDefault(loudspeaker: Boolean) {
        if (this.loudspeaker == loudspeaker) return
        this.loudspeaker = loudspeaker
        if (engaged) apply()
    }

    /** Send the call's sound to the output with this id, or back to the rule
     *  for `null`. Applies at once. */
    public fun selectOutput(id: Int?) {
        selectedOutputId = id
        if (engaged) apply()
    }

    /** Take the call's sound from the microphone with this id, or from the
     *  platform's own choice for `null`. Applies at once. */
    public fun selectInput(id: Int?) {
        selectedInputId = id
        if (engaged) apply()
    }

    /** Read the devices that are there now and put the call on the right ones. */
    private fun apply() {
        val manager = audio ?: return
        runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                val devices = manager.availableCommunicationDevices
                val known = devices.mapNotNull { outputOf(it) }
                mutableOutputs.value = known
                val target = callOutputTarget(known, selectedOutputId, loudspeaker)
                val device = devices.firstOrNull { target != null && it.id == target.id }
                if (device != null) manager.setCommunicationDevice(device) else manager.clearCommunicationDevice()
            } else {
                val known = legacySelectableOutputs(
                    manager.getDevices(AudioManager.GET_DEVICES_OUTPUTS).mapNotNull { outputOf(it) },
                )
                mutableOutputs.value = known
                applyLegacy(manager, callOutputTarget(known, selectedOutputId, loudspeaker)?.kind)
            }
        }.onFailure { warn("could not set the call audio route", it) }
        runCatching {
            val all = manager.getDevices(AudioManager.GET_DEVICES_INPUTS)
            val known = selectableInputs(all.mapNotNull { inputOf(it) })
            mutableInputs.value = known
            val chosen = selectedInputId?.takeIf { id -> known.any { it.id == id } }
            preferInput(all.firstOrNull { chosen != null && it.id == chosen })
        }.onFailure { warn("could not set the call microphone", it) }
    }

    /** Below API 31: the two switches the app has. */
    private fun applyLegacy(manager: AudioManager, kind: CallOutputKind?) {
        when (kind) {
            CallOutputKind.BLUETOOTH -> {
                @Suppress("DEPRECATION")
                manager.isSpeakerphoneOn = false
                if (!scoStarted) {
                    scoStarted = true
                    @Suppress("DEPRECATION")
                    manager.startBluetoothSco()
                    @Suppress("DEPRECATION")
                    manager.isBluetoothScoOn = true
                }
            }
            CallOutputKind.LOUDSPEAKER -> {
                stopSco(manager)
                @Suppress("DEPRECATION")
                manager.isSpeakerphoneOn = true
            }
            // A wire takes the sound by the platform's own rule once the
            // loudspeaker is off, and the earpiece when there is no wire.
            CallOutputKind.WIRED, CallOutputKind.EARPIECE, null -> {
                stopSco(manager)
                @Suppress("DEPRECATION")
                manager.isSpeakerphoneOn = false
            }
        }
    }

    private fun stopSco(manager: AudioManager) {
        if (!scoStarted) return
        scoStarted = false
        runCatching {
            @Suppress("DEPRECATION")
            manager.isBluetoothScoOn = false
            @Suppress("DEPRECATION")
            manager.stopBluetoothSco()
        }.onFailure { warn("could not stop Bluetooth audio", it) }
    }

    private fun outputOf(device: AudioDeviceInfo): CallAudioOutput? {
        val kind = when (device.type) {
            AudioDeviceInfo.TYPE_BUILTIN_EARPIECE -> CallOutputKind.EARPIECE
            AudioDeviceInfo.TYPE_BUILTIN_SPEAKER -> CallOutputKind.LOUDSPEAKER
            AudioDeviceInfo.TYPE_WIRED_HEADSET,
            AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
            AudioDeviceInfo.TYPE_USB_HEADSET,
            -> CallOutputKind.WIRED
            AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
            AudioDeviceInfo.TYPE_BLE_HEADSET,
            AudioDeviceInfo.TYPE_HEARING_AID,
            -> CallOutputKind.BLUETOOTH
            else -> return null
        }
        val external = kind == CallOutputKind.WIRED || kind == CallOutputKind.BLUETOOTH
        return CallAudioOutput(id = device.id, kind = kind, name = externalName(device, external))
    }

    private fun inputOf(device: AudioDeviceInfo): CallAudioInput? {
        val kind = when (device.type) {
            AudioDeviceInfo.TYPE_BUILTIN_MIC -> CallInputKind.BUILT_IN
            AudioDeviceInfo.TYPE_WIRED_HEADSET,
            AudioDeviceInfo.TYPE_USB_HEADSET,
            AudioDeviceInfo.TYPE_USB_DEVICE,
            -> CallInputKind.WIRED
            // A Bluetooth microphone is not offered on its own: it works only
            // while the call is routed to that headset, and then the platform
            // already uses it.
            else -> return null
        }
        return CallAudioInput(
            id = device.id,
            kind = kind,
            name = externalName(device, external = kind == CallInputKind.WIRED),
        )
    }

    /** A connected device's own name. A built-in one is named after the phone,
     *  which says nothing, so it has none. */
    private fun externalName(device: AudioDeviceInfo, external: Boolean): String {
        if (!external) return ""
        val name = device.productName?.toString()?.trim().orEmpty()
        return if (name == Build.MODEL) "" else name
    }

    private fun warn(message: String, cause: Throwable?) {
        android.util.Log.w(TAG, message, cause)
    }

    private companion object {
        private const val TAG = "CallAudioRouter"
    }
}
