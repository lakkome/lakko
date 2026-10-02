// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * Where a call's sound comes out (`docs/ux/calls.md` section 4.1).
 *
 * There is no Speaker control in a call. The route is a rule, and this is the
 * rule; the person who wants something else chooses an output in the call's
 * Settings sheet (`CallSettings.kt`), and that choice holds for the call.
 *
 *  - A group call, and any call with video, plays on the loudspeaker, the way
 *    a meeting does on a desk; a headset or a Bluetooth device that is
 *    connected takes the sound over by itself.
 *  - A one-to-one voice call plays on the earpiece, as a phone call does.
 *
 * [CallAudioRouter] reads the devices and applies the answer.
 *
 * Pure and Android-free, so the rule is unit-tested on the host JVM.
 */

/** An output the call's sound can be sent to. */
public enum class CallOutputKind {
    /** The small speaker held to the ear. */
    EARPIECE,

    /** The device's own loudspeaker. */
    LOUDSPEAKER,

    /** A headset or headphones on a wire. */
    WIRED,

    /** A Bluetooth headset, earbuds or hearing aid. */
    BLUETOOTH,
}

/** One output that is there now. */
public data class CallAudioOutput(
    /** The platform's id for the device, stable while it is connected. */
    val id: Int,
    val kind: CallOutputKind,
    /** The name the platform gives it, or empty when it gives none worth
     *  showing (a built-in output is named after the phone). */
    val name: String,
)

/**
 * Whether the call's default output is the loudspeaker rather than the
 * earpiece: a group call, or any call with a picture on the screen. Nobody
 * holds a phone to their ear while looking at it.
 */
public fun callPlaysOnLoudspeaker(groupCall: Boolean, videoPresent: Boolean): Boolean =
    groupCall || videoPresent

/**
 * The output a loudspeaker call uses by itself, given the kinds [available],
 * or `null` when there is nothing to choose.
 *
 * Something the person put on wins over the device's own speakers: a wired
 * headset first (plugging one in is the more deliberate act), then Bluetooth.
 * With neither, the loudspeaker.
 */
public fun groupCallOutput(available: Set<CallOutputKind>): CallOutputKind? = when {
    CallOutputKind.WIRED in available -> CallOutputKind.WIRED
    CallOutputKind.BLUETOOTH in available -> CallOutputKind.BLUETOOTH
    CallOutputKind.LOUDSPEAKER in available -> CallOutputKind.LOUDSPEAKER
    CallOutputKind.EARPIECE in available -> CallOutputKind.EARPIECE
    else -> null
}

/**
 * The output the call is sent to, or `null` to leave the platform's default.
 *
 * The person's own choice ([selectedId], from the Settings sheet) while that
 * device is still there; a headset that was unplugged falls back to the rule.
 * Otherwise the rule: [groupCallOutput] for a loudspeaker call
 * ([loudspeaker]), the earpiece for a one-to-one voice call.
 */
public fun callOutputTarget(
    available: List<CallAudioOutput>,
    selectedId: Int?,
    loudspeaker: Boolean,
): CallAudioOutput? {
    if (selectedId != null) {
        available.firstOrNull { it.id == selectedId }?.let { return it }
    }
    val kind = if (loudspeaker) {
        groupCallOutput(available.map { it.kind }.toSet())
    } else {
        CallOutputKind.EARPIECE
    }
    return available.firstOrNull { it.kind == kind }
}

/**
 * The outputs a person can choose between where the platform has no way to
 * name a device to route to (below Android 12).
 *
 * There the app has two switches, the loudspeaker and Bluetooth, and with
 * both off the platform itself picks a wired headset when one is plugged in
 * and the earpiece otherwise. So of those two only the one the platform would
 * pick is offered: a row for the earpiece while a headset is plugged in would
 * be a choice the app cannot honour.
 */
public fun legacySelectableOutputs(available: List<CallAudioOutput>): List<CallAudioOutput> {
    val wired = available.any { it.kind == CallOutputKind.WIRED }
    return available.filter { it.kind != CallOutputKind.EARPIECE || !wired }
}

/** A microphone the call's sound can be taken from. */
public enum class CallInputKind {
    /** The device's own microphone. */
    BUILT_IN,

    /** The microphone of a headset on a wire. */
    WIRED,
}

/** One microphone that is there now. */
public data class CallAudioInput(
    /** The platform's id for the device, stable while it is connected. */
    val id: Int,
    val kind: CallInputKind,
    /** The name the platform gives it, or empty when it gives none worth
     *  showing. */
    val name: String,
)

/**
 * The microphones a person can choose between.
 *
 * A phone reports its own microphone several times over, once for each
 * capsule (bottom, back, front); which of those is used is the platform's
 * business, so they are one row. A Bluetooth headset's microphone is not
 * listed: it works only while the call is routed to that headset, and then
 * the platform already uses it.
 */
public fun selectableInputs(available: List<CallAudioInput>): List<CallAudioInput> {
    val builtIn = available.firstOrNull { it.kind == CallInputKind.BUILT_IN }
    return listOfNotNull(builtIn) + available.filter { it.kind != CallInputKind.BUILT_IN }
}
