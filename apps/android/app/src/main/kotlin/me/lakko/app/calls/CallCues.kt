// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * What a call sounds like (`docs/ux/calls.md` section 3.4): the outgoing tone
 * and the two short cues.
 *
 * ## The defect this closes
 *
 * The call screen looped the outgoing tone for every call that was not yet
 * connected. So a person who started a group call alone, or joined one from a
 * link, heard a phone ringing although nobody was being called (found live
 * 2026-10-02), and the device that had just ANSWERED a one-to-one call rang
 * too, through the seconds of connecting.
 *
 * ## The rule
 *
 * Whether the tone plays is the shared rule
 * `lakko-ui-rules::call_group::ringback_plays`, asked through
 * [GroupCallRules]: only a one-to-one call this device placed, and only until
 * it is answered. What is here is this client's side: which phase the rule is
 * asked about ([ringbackShouldPlay]) and when the two short cues play
 * ([CallCueTracker]).
 *
 * Pure and Android-free, so the wiring is unit-tested on the host JVM with a
 * fake [GroupCallRules]; the rule itself is tested in the core.
 */

/**
 * Whether the outgoing tone loops for the call on screen.
 *
 * [groupCall] is `true` for a call in a conversation that runs over the SFU
 * and for a call started from or joined through a link. Both ring nobody: the
 * person who starts one is simply in it, and a person who joins is joining,
 * not calling.
 */
public fun ringbackShouldPlay(
    rules: GroupCallRules,
    groupCall: Boolean,
    phase: CallUiState.CallPhase,
): Boolean = rules.ringbackPlays(groupCall, callBarPhaseOf(phase))

/** One of the two short sounds. */
public enum class CallCue {
    /** The call connected, or someone joined it. */
    JOIN,

    /** Someone left, or the call ended. */
    LEAVE,
}

/**
 * Decides when the two short cues play: each on the change, and only on the
 * change.
 *
 * Held beside the call (in [CallUiState], or by the one-to-one overlay), not
 * by the call screen. The screen is destroyed and rebuilt whenever the call
 * is put away and brought back (`docs/ux/calls.md` section 4.5), so a screen
 * that kept this itself played the connected sound again on every return.
 *
 * Not thread-safe: the call screen calls it from the main thread.
 */
public class CallCueTracker {
    private enum class Kind { CALLING, CONNECTING, CONNECTED, RECONNECTING, ENDED }

    private var lastPhase: Kind? = null
    private var lastRemoteIds: List<String> = emptyList()

    /**
     * A call screen has come up showing this call.
     *
     * The first screen of a call records nothing: the phase it then reports is
     * a change, so a screen that first appears on a connected call plays the
     * connected cue once. A screen that comes BACK takes the call as it finds
     * it, silently: whatever happened while the call was put away is not
     * played late as if it were happening now.
     */
    public fun mount(phase: CallUiState.CallPhase, remoteIds: List<String>) {
        if (lastPhase == null) return
        lastPhase = kindOf(phase)
        lastRemoteIds = remoteIds
    }

    /** The cue for the call being in [phase] now, or `null` when the phase is
     *  the one already heard. */
    public fun onPhase(phase: CallUiState.CallPhase): CallCue? {
        val kind = kindOf(phase)
        if (kind == lastPhase) return null
        lastPhase = kind
        return when (kind) {
            Kind.CONNECTED -> CallCue.JOIN
            Kind.ENDED -> CallCue.LEAVE
            Kind.CALLING, Kind.CONNECTING, Kind.RECONNECTING -> null
        }
    }

    /**
     * The cues for the other people now in the call: one [CallCue.JOIN] for
     * each arrival and one [CallCue.LEAVE] for each departure since the last
     * reading. Nothing before the call is connected, and nothing for the
     * people who were already there when this device connected: the connected
     * cue covers them.
     */
    public fun onRoster(remoteIds: List<String>, connected: Boolean): List<CallCue> {
        val previous = lastRemoteIds
        lastRemoteIds = remoteIds
        if (!connected || previous.isEmpty()) return emptyList()
        val cues = ArrayList<CallCue>()
        for (id in remoteIds) if (id !in previous) cues.add(CallCue.JOIN)
        for (id in previous) if (id !in remoteIds) cues.add(CallCue.LEAVE)
        return cues
    }

    /** A new call. */
    public fun reset() {
        lastPhase = null
        lastRemoteIds = emptyList()
    }

    private fun kindOf(phase: CallUiState.CallPhase): Kind = when (phase) {
        is CallUiState.CallPhase.Calling -> Kind.CALLING
        is CallUiState.CallPhase.Connecting -> Kind.CONNECTING
        is CallUiState.CallPhase.Connected -> Kind.CONNECTED
        is CallUiState.CallPhase.Reconnecting -> Kind.RECONNECTING
        is CallUiState.CallPhase.Ended -> Kind.ENDED
    }
}
