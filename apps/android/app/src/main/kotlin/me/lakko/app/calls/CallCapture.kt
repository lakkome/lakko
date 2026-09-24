// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * Whether a call is HOLDING the microphone and camera, so the composer's
 * voice / video-note recorder must stand aside.
 *
 * ## The defect this closes
 *
 * `docs/ux/media-messages.md` blocks recording while a call owns capture, which
 * is right: two things cannot have the microphone. What this screen asked was a
 * different question — "is there a call in THIS chat" — and the two part
 * company for exactly one state: an incoming call that is ringing and has not
 * been answered. Nothing is capturing then. The callee's microphone is not
 * opened until they accept, which is why [OneToOneCallHost] builds no peer
 * connection for a ringing offer.
 *
 * ## Why the question is app-wide, not per-chat
 *
 * "A call in this chat" was the same question as "a call is live" for as long
 * as a live call owned the whole screen: there was no other chat to be standing
 * in. `docs/ux/calls.md` section 4.5 collapses a call to a bar, so a person can
 * be reading chat B with a call running from chat A — and the microphone is
 * just as busy there. So the fact this file supplies is the app-wide call slot
 * ([callSlotOccupant] over [AppCallHost.occupant]), never a comparison of the
 * call's group id against the open chat's.
 *
 * With a call ringing quietly in a MUTED conversation (`docs/ux/calls.md`
 * section 2.1.1) the composer therefore refused the one reply that fits the
 * moment — a voice message saying they cannot talk. It was invisible until
 * those quiet surfaces landed: before them a ringing call took the screen over,
 * so a recorder disabled behind it could not be noticed.
 *
 * ## Where the rule lives
 *
 * `crates/lakko-ui-rules/src/capture.rs`, reached over UniFFI as
 * `uniffi.lakko_ffi_uniffi.callHoldsCapture`; web reaches the same code over
 * wasm. Nothing here decides the rule — this file maps the two facts this
 * client already holds into what the rule takes.
 *
 * The rule arrives as a parameter for the same reason
 * [me.lakko.app.calls.TEST_RING_WINDOW_MS]'s number does: this file is
 * deliberately free of Android and of `liblakko_ffi_uniffi.so` so the host JVM
 * suite can reach it, and a unit test does not load the native library, so
 * calling into the core from here would only prove the library is absent. The
 * one production caller is what reads it from the core.
 */

/**
 * The call in the slot is an incoming offer still being rung, which the person
 * has not accepted — the one state that owns no capture.
 *
 * Gated on the occupant being the 1x1 call, not only on that call's own state:
 * a group or link call holding the slot owns the microphone outright, and it
 * must not be freed by whatever a stale 1x1 slot happens to read. The app
 * admits one call at a time ([callSlotOccupant]), so the guard never fires in a
 * correct host — it just keeps the one failure that would matter on the
 * blocking side.
 *
 * Reads [oneToOneCallPresentation] rather than restating "ringing and not
 * accepted", so this and the surfaces that draw the ring cannot disagree about
 * whether a call has been answered. Both of its ringing verdicts count:
 * `RINGING_SCREEN` and `QUIET_OFFER` differ only in whether the person is
 * interrupted, and neither opens a microphone.
 *
 * An OUTGOING call is never reported here, even while it is only dialing: the
 * caller's microphone opens as the call is placed. That asymmetry is the rule's,
 * and it is why [incoming] is asked separately rather than folded into the
 * state.
 */
public fun unansweredIncomingRing(
    occupant: CallKind?,
    incoming: Boolean,
    accepted: Boolean,
    muted: Boolean,
    state: CallSession.WebCallState,
): Boolean =
    occupant == CallKind.ONE_TO_ONE &&
        incoming &&
        oneToOneCallPresentation(state, accepted, muted) != OneToOneCallPresentation.IN_CALL_SCREEN

/**
 * Whether the composer's recorder must stand aside, by the shared rule.
 *
 * [holdsCapture] is `uniffi.lakko_ffi_uniffi.callHoldsCapture`, passed in rather
 * than called here — see this file's header. It has no default so the call site
 * names the rule it is asking, and an empty slot ([occupant] `null`) is handed
 * over unchanged rather than short-circuited: the rule is what decides, and one
 * fewer branch here is one fewer place for this client to answer on its own.
 *
 * [occupant] is the ONE app-wide call slot, so `callIsLive` below means "a call
 * is live at all" and not "a call is live in the chat on screen" — a call
 * collapsed into the bar of `docs/ux/calls.md` section 4.5 holds the microphone
 * in every chat.
 */
public fun callOwnsCapture(
    occupant: CallKind?,
    call: OneToOneCall<*>?,
    state: CallSession.WebCallState,
    holdsCapture: (Boolean, Boolean) -> Boolean,
): Boolean = holdsCapture(
    // callIsLive
    occupant != null,
    unansweredIncomingRing(
        occupant = occupant,
        incoming = call?.incoming == true,
        accepted = call?.accepted == true,
        muted = call?.muted == true,
        state = state,
    ),
)
