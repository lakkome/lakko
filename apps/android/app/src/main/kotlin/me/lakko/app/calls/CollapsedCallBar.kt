// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.Strings

import uniffi.lakko_ffi_uniffi.CallBarPhase
import uniffi.lakko_ffi_uniffi.CallBarStatus

/**
 * The collapsed call — `docs/ux/calls.md` section 4.5 — the decisions the
 * floating call window (`ui/call/CallMiniWindow.kt`) and the system
 * picture-in-picture window take from the shared `call_bar` rule: whether the
 * call can be put away, and the one thing the status line says. The window
 * replaced a bar across the top of the app on 2026-11-09; the rule kept its
 * name, and so do these functions, because the window replaces the bar's
 * picture and not its decisions (section 4.5.6).
 *
 * ## What is NOT here
 *
 * The two decisions that have no platform in them live in
 * `crates/lakko-ui-rules/src/call_bar.rs` and are reached over UniFFI:
 * [uniffi.lakko_ffi_uniffi.callBarVisible] and
 * [uniffi.lakko_ffi_uniffi.callBarStatus]. Neither is re-implemented here, and
 * the second one's shape is the reason: [CallBarStatus] is a sum type because
 * section 4 says the line carries the state **or** the running time and never
 * two at once, and three booleans in a composable are one careless branch from
 * breaking that.
 *
 * ## What IS here
 *
 * The two translations that are genuinely this client's: from the phase types
 * this app already has onto the one the rule speaks ([callBarPhaseOf]), and
 * from the rule's answer onto the words the call screen already uses
 * ([callBarStatusText]). Both are pure, so both are unit-tested on the host
 * JVM; the window's drawing is covered by the Robolectric suite
 * `CallMiniWindowUiTest`.
 */

/**
 * The 1x1 call's session state as the bar's rule sees it.
 *
 * Deliberately agrees with [CallUiState]'s own `mapSessionPhase`, which is what
 * the call screen reads, on every state that reaches a shown bar — the bar and
 * the screen must never disagree about what a call is doing. The two places it
 * is more precise than that mapping are both harmless and both deliberate:
 *
 *  - `Idle` is [CallBarPhase.PREPARING] rather than `Calling`. The rule answers
 *    `Calling...` for both, so the line reads the same; the phase is just the
 *    truer name for a call that is built and not yet on the wire.
 *  - `Ringing` splits on [accepted]. The screen has no need to: it reads
 *    `Connecting...` either way. The bar does, because an incoming call nobody
 *    has answered yet must have no bar at all (section 4.5: it is answered or
 *    declined, so there is nothing to put away), and only
 *    [CallBarPhase.RINGING] tells the rule that.
 *
 * `Ending` reports [CallBarPhase.ENDED], where the screen keeps reading
 * `Connected` so its own timer does not stop mid-hang-up. For the bar the
 * hang-up IS the end: section 4.5 says that when a collapsed call ends the bar
 * goes away and the conversation gets its usual row, so there is nothing left
 * for it to say.
 */
public fun callBarPhaseOf(state: CallSession.WebCallState, accepted: Boolean): CallBarPhase =
    when (state) {
        CallSession.WebCallState.Idle -> CallBarPhase.PREPARING
        CallSession.WebCallState.Outgoing -> CallBarPhase.CALLING
        CallSession.WebCallState.Ringing ->
            if (accepted) CallBarPhase.CONNECTING else CallBarPhase.RINGING
        CallSession.WebCallState.Connecting -> CallBarPhase.CONNECTING
        CallSession.WebCallState.Connected -> CallBarPhase.CONNECTED
        CallSession.WebCallState.Ending,
        CallSession.WebCallState.Done,
        -> CallBarPhase.ENDED
    }

/**
 * The phase of a call the app-level host drives through a [CallUiState] — the
 * group (SFU) call and the call-link call.
 *
 * One-to-one with the sealed class, so a phase added there is a compile error
 * here rather than a bar that silently reads `Calling...` forever.
 */
public fun callBarPhaseOf(phase: CallUiState.CallPhase): CallBarPhase = when (phase) {
    is CallUiState.CallPhase.Calling -> CallBarPhase.CALLING
    is CallUiState.CallPhase.Connecting -> CallBarPhase.CONNECTING
    is CallUiState.CallPhase.Connected -> CallBarPhase.CONNECTED
    is CallUiState.CallPhase.Reconnecting -> CallBarPhase.RECONNECTING
    is CallUiState.CallPhase.Ended -> CallBarPhase.ENDED
}

/**
 * [callBarPhaseOf] for a call that rings nobody: a call in a conversation that
 * runs over the SFU, or one started from or joined through a link.
 *
 * `docs/ux/calls.md` section 3.4: such a call is never `Calling...`. Its state
 * starts out as [CallUiState.CallPhase.Calling] only because that is what a
 * reset state holds for the instant before the host says otherwise, and the
 * bar must not read that instant as a phone ringing.
 */
public fun groupCallBarPhaseOf(phase: CallUiState.CallPhase): CallBarPhase =
    when (val barPhase = callBarPhaseOf(phase)) {
        CallBarPhase.CALLING -> CallBarPhase.CONNECTING
        else -> barPhase
    }

/**
 * The bar's one line, in the words section 4 gives it.
 *
 * Every arm goes through [callStatusText] — the call screen's own wording —
 * with exactly one of its flags set, rather than restating `Calling...` and its
 * siblings as literals here. Section 4.5 asks for "the words of the status
 * line", and a second copy of those words is a second thing to keep in step.
 *
 * The `when` is exhaustive over [CallBarStatus], which is the whole point of
 * the rule returning a sum type: there is no reachable way to render two of
 * these at once, and adding a fifth arm to the wire breaks the build here.
 */
public fun callBarStatusText(
    status: CallBarStatus,
    strings: Strings = Localization.strings.value,
): String = when (status) {
    CallBarStatus.Calling -> stateWord(strings, calling = true)
    CallBarStatus.Connecting -> stateWord(strings, connecting = true)
    CallBarStatus.Reconnecting -> stateWord(strings, reconnecting = true)
    // The rule carries milliseconds because "how long it has been is not a
    // presentation decision"; the seconds and the `m:ss` are.
    is CallBarStatus.Elapsed -> callStatusText(
        calling = false,
        connecting = false,
        connected = true,
        reconnecting = false,
        ended = false,
        elapsedSecs = (status.ms / MILLIS_PER_SECOND).toLong(),
        strings = strings,
    )
}

private const val MILLIS_PER_SECOND: ULong = 1_000uL

private fun stateWord(
    strings: Strings,
    calling: Boolean = false,
    connecting: Boolean = false,
    reconnecting: Boolean = false,
): String = callStatusText(
    calling = calling,
    connecting = connecting,
    connected = false,
    reconnecting = reconnecting,
    ended = false,
    elapsedSecs = null,
    strings = strings,
)

/**
 * The bar's one round mute button — section 4.5: "drawn on and off exactly as
 * the call screen draws it".
 *
 * Literally the call screen's own control: this picks the microphone arm out of
 * [callControlRow], so the state inversion that matters ("off is the loud
 * state" — a muted microphone is the solid, slashed one) and the spoken label
 * come from the one place that decides them. Only the palette differs, because
 * the bar sits on an app screen rather than on the call's own dark stage.
 *
 * [ringing] is the `Calling...` row of section 4.1's state table, where "only
 * End is active". The bar has no End, so on that row it has no live control at
 * all — the button is still drawn, at the same 40 percent the call screen uses,
 * so the person can see where mute will be.
 */
public fun callBarMicControl(micOn: Boolean, ringing: Boolean): CallControl =
    callControlRow(
        micOn = micOn,
        cameraOn = false,
        ringing = ringing,
    ).first { it.kind == CallControlKind.MICROPHONE }

/**
 * Everything the bar draws, resolved from whichever call holds the one slot.
 *
 * A single record rather than a per-kind composable, so the three kinds of call
 * cannot drift into three different bars. The per-kind part is only how these
 * fields are read, and that lives in the Compose adapter.
 *
 * @param who the same label the call screen shows — see [callScreenTitle],
 *   which is the function that produces it for both.
 * @param status the rule's answer, already carrying the state or the time.
 * @param showsLock section 4.5: a lock "only when the call screen would show
 *   its chip", decided by [showsCallEncryptionChip], the same predicate the
 *   chip itself is drawn from. The bar never carries the WORD: a claim about
 *   encryption belongs where the safety number can be read, one tap away.
 * @param cameraSending this device's own camera is on, "so a collapsed video
 *   call is never a camera you forgot about".
 * @param mic the mute button, from [callBarMicControl].
 */
public data class CallBarModel(
    val who: String,
    val status: CallBarStatus,
    val showsLock: Boolean,
    val cameraSending: Boolean,
    val mic: CallControl,
    /** The mic state the button will produce, so the press has one meaning. */
    val micOn: Boolean,
)
