// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.Strings

/**
 * The call settings sheet: which microphone, camera and speaker the call uses
 * (`docs/ux/calls.md` section 4.1) — the Kotlin twin of the web
 * `apps/web/src/calls/call-settings.ts`.
 *
 * The sheet is the app's own list, never a system picker and never the
 * system's output switcher. Each section is a group of radio rows: the system
 * default first, then every device the call can be put on.
 *
 * A section exists only where choosing does something. A section with no
 * device to choose is absent rather than a list whose rows change nothing.
 *
 * Pure: the screen passes the devices in and draws the answer.
 */

/** Which of the three things a device is. */
public enum class SettingsDeviceKind { AUDIO_INPUT, VIDEO_INPUT, AUDIO_OUTPUT }

/** A device as the sheet is given it. An empty [label] is a device the
 *  platform gives no name. */
public data class SettingsDevice(
    val id: String,
    val label: String,
    val kind: SettingsDeviceKind,
)

/** What is chosen now; `null` is the system default. */
public data class SettingsSelection(
    val audioInput: String? = null,
    val videoInput: String? = null,
    val audioOutput: String? = null,
)

public enum class SettingsSectionKind { MICROPHONE, CAMERA, SPEAKER }

/** One row of a section. An [id] of `null` is the system default. */
public data class SettingsOption(
    val id: String?,
    val label: String,
    val selected: Boolean,
)

public data class SettingsSection(
    val kind: SettingsSectionKind,
    val title: String,
    val options: List<SettingsOption>,
)

private fun deviceKindOf(kind: SettingsSectionKind): SettingsDeviceKind = when (kind) {
    SettingsSectionKind.MICROPHONE -> SettingsDeviceKind.AUDIO_INPUT
    SettingsSectionKind.CAMERA -> SettingsDeviceKind.VIDEO_INPUT
    SettingsSectionKind.SPEAKER -> SettingsDeviceKind.AUDIO_OUTPUT
}

private fun titleKeyOf(kind: SettingsSectionKind): String = when (kind) {
    SettingsSectionKind.MICROPHONE -> "call.settings.microphone"
    SettingsSectionKind.CAMERA -> "call.control.camera"
    SettingsSectionKind.SPEAKER -> "call.control.speaker"
}

private fun chosenOf(kind: SettingsSectionKind, selection: SettingsSelection): String? = when (kind) {
    SettingsSectionKind.MICROPHONE -> selection.audioInput
    SettingsSectionKind.CAMERA -> selection.videoInput
    SettingsSectionKind.SPEAKER -> selection.audioOutput
}

/**
 * The sections of the sheet, in order: microphone, camera, speaker.
 *
 * A device with no name is numbered. A stored choice the platform no longer
 * reports (a headset was unplugged) reads as the system default, which is
 * what the call is in fact using. An entry with an empty id, or the reserved
 * id `default`, is dropped: the first row already is the system default.
 *
 * @param canSelectAudioOutput the app can move the call's sound to a chosen
 *   output; without it the speaker section is absent.
 */
public fun callSettingsSections(
    devices: List<SettingsDevice>,
    selection: SettingsSelection,
    canSelectAudioOutput: Boolean,
    strings: Strings = Localization.strings.value,
): List<SettingsSection> = buildList {
    for (kind in SettingsSectionKind.values()) {
        if (kind == SettingsSectionKind.SPEAKER && !canSelectAudioOutput) continue
        val listed = devices.filter { it.kind == deviceKindOf(kind) && it.id.isNotEmpty() && it.id != "default" }
        if (listed.isEmpty()) continue
        val stored = chosenOf(kind, selection)
        val chosen = stored?.takeIf { id -> listed.any { it.id == id } }
        add(
            SettingsSection(
                kind = kind,
                title = strings[titleKeyOf(kind)],
                options = buildList {
                    add(
                        SettingsOption(
                            id = null,
                            label = strings["call.settings.system_default"],
                            selected = chosen == null,
                        ),
                    )
                    listed.forEachIndexed { index, device ->
                        add(
                            SettingsOption(
                                id = device.id,
                                label = device.label.trim().ifEmpty {
                                    strings.format("call.settings.unnamed_device", mapOf("n" to (index + 1).toString()))
                                },
                                selected = chosen == device.id,
                            ),
                        )
                    }
                },
            ),
        )
    }
}

/**
 * The devices of one kind that are worth a section on this platform.
 *
 * Where the platform reports a single device there is nothing to choose: the
 * section would be the system default and the one thing the default already
 * is. A phone has one microphone until a headset is plugged in, and that is
 * the usual case, so the microphone section is usually absent.
 */
public fun choosableDevices(devices: List<SettingsDevice>): List<SettingsDevice> =
    if (devices.size < 2) emptyList() else devices

/**
 * The devices the sheet offers on this platform, from what the audio router
 * and the camera enumerator report.
 *
 * A device is named by what the platform calls it. Where the platform has no
 * name worth showing, this device's own microphone and loudspeaker take the
 * catalog's words for them; anything else is left without a name and the
 * sheet numbers it ([callSettingsSections]). A camera has no name on this
 * platform at all.
 *
 * @param cameras the cameras' names, the default one first.
 */
public fun callSettingsDevices(
    inputs: List<CallAudioInput>,
    cameras: List<String>,
    outputs: List<CallAudioOutput>,
    strings: Strings = Localization.strings.value,
): List<SettingsDevice> = buildList {
    addAll(
        choosableDevices(
            inputs.map { input ->
                SettingsDevice(
                    id = input.id.toString(),
                    label = input.name.ifEmpty {
                        if (input.kind == CallInputKind.BUILT_IN) strings["call.settings.microphone"] else ""
                    },
                    kind = SettingsDeviceKind.AUDIO_INPUT,
                )
            },
        ),
    )
    addAll(
        choosableDevices(
            cameras.map { name -> SettingsDevice(id = name, label = "", kind = SettingsDeviceKind.VIDEO_INPUT) },
        ),
    )
    addAll(
        choosableDevices(
            outputs.map { output ->
                SettingsDevice(
                    id = output.id.toString(),
                    label = output.name.ifEmpty {
                        if (output.kind == CallOutputKind.LOUDSPEAKER) strings["call.control.speaker"] else ""
                    },
                    kind = SettingsDeviceKind.AUDIO_OUTPUT,
                )
            },
        ),
    )
}

/**
 * What the person chose in the call's Settings sheet, for one call.
 *
 * Held beside the call rather than by the call screen, which is destroyed and
 * rebuilt whenever the call is put away and brought back: a choice "holds for
 * the call" only if it outlives the screen it was made on.
 */
public class CallDeviceSelection {
    private val mutableCamera = MutableStateFlow<String?>(null)
    private val mutableAudioOutput = MutableStateFlow<Int?>(null)
    private val mutableAudioInput = MutableStateFlow<Int?>(null)

    /** The camera's name, or `null` for the default (the front camera). */
    public val camera: StateFlow<String?> = mutableCamera.asStateFlow()

    /** The output's platform id, or `null` for the call's own rule
     *  ([callOutputTarget]). */
    public val audioOutput: StateFlow<Int?> = mutableAudioOutput.asStateFlow()

    /** The microphone's platform id, or `null` for the one the platform
     *  picks. */
    public val audioInput: StateFlow<Int?> = mutableAudioInput.asStateFlow()

    public fun selectCamera(name: String?) {
        mutableCamera.value = name
    }

    public fun selectAudioOutput(id: Int?) {
        mutableAudioOutput.value = id
    }

    public fun selectAudioInput(id: Int?) {
        mutableAudioInput.value = id
    }

    /** A new call starts from the defaults. */
    public fun reset() {
        mutableCamera.value = null
        mutableAudioOutput.value = null
        mutableAudioInput.value = null
    }
}
