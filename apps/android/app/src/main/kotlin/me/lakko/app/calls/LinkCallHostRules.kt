// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * The call-link (`/c/<slug>`, ADR-0032) call as the app-level [LinkCallHost]
 * holds it — the decisions, kept pure so they can be unit-tested. The Kotlin
 * twin of `apps/web/src/calls/link-call-host.ts`.
 *
 * ## Why it moved
 *
 * `JoinCallViewModel` owned the [MultiJoinerCall]. The view model lived in the
 * activity's store, so leaving the landing kept the call running with no
 * screen able to show or end it, opening a second link left the first call
 * running inside a view model nothing rendered, and nothing stopped a link call
 * from starting while a 1x1 or group call was live. The host now owns the
 * orchestrator, the in-call surface, the controls and the hang-up; the landing
 * decodes the link, runs the guest sign-up, asks the host for the call and
 * shows the host's state for it.
 *
 * Nothing about the protocol changed: [MultiJoinerCall] still founds (owner) or
 * joins (joiner), elects the committer, dedups admissions, rekeys per
 * admission, checks the owner is in the joined group and times out a call with
 * no live committer. What is here is only where its outcome goes.
 */

/** Where a link call is, from this device's point of view. */
public enum class LinkCallPhase {
    /** Minting the call-admission token and connecting to the SFU. */
    STARTING,

    /** Joiner: signaling is open and the KeyPackage is out; no Welcome yet. */
    WAITING,

    /** In the call's MLS group with the seed derived (owner: at founding). */
    LIVE,

    /** The session closed or failed. */
    ENDED,

    /** Joiner: nobody admitted this device within the admission window. */
    DEAD,

    /** A setup error, or a Welcome into a group without the owner (refused). */
    ERROR,
}

/** What the orchestrator reported. */
public sealed interface LinkCallEvent {
    public data class SignalingOpen(val owner: Boolean) : LinkCallEvent

    public data object Joined : LinkCallEvent

    public data object Ended : LinkCallEvent

    public data object NoCommitter : LinkCallEvent

    public data object Error : LinkCallEvent
}

/** The phases in which the call holds the one call slot. */
public fun linkCallOccupiesSlot(phase: LinkCallPhase): Boolean =
    phase == LinkCallPhase.STARTING || phase == LinkCallPhase.WAITING || phase == LinkCallPhase.LIVE

/**
 * Fold one orchestrator report.
 *
 * `Ended` is a weak terminal: [MultiJoinerCall] tears its session down BEFORE it
 * reports why — an admission timeout closes the session (reported `Ended`) and
 * then reports `NoCommitter`; a refused redirect closes it and then reports
 * `Error`. The later, specific report replaces `Ended`. `DEAD` and `ERROR` are
 * final.
 */
public fun reduceLinkCallPhase(phase: LinkCallPhase, event: LinkCallEvent): LinkCallPhase {
    if (phase == LinkCallPhase.DEAD || phase == LinkCallPhase.ERROR) return phase
    return when (event) {
        is LinkCallEvent.SignalingOpen ->
            if (phase == LinkCallPhase.STARTING && !event.owner) LinkCallPhase.WAITING else phase
        LinkCallEvent.Joined ->
            if (phase == LinkCallPhase.STARTING || phase == LinkCallPhase.WAITING) LinkCallPhase.LIVE else phase
        LinkCallEvent.Ended -> if (linkCallOccupiesSlot(phase)) LinkCallPhase.ENDED else phase
        LinkCallEvent.NoCommitter -> if (phase == LinkCallPhase.LIVE) phase else LinkCallPhase.DEAD
        LinkCallEvent.Error -> LinkCallPhase.ERROR
    }
}

/** Whether a link call may start now. */
public sealed interface LinkCallStartDecision {
    public data object Start : LinkCallStartDecision

    /** This very call is already starting / waiting / live here: show it,
     *  never a second session for it. */
    public data object AlreadyInThisCall : LinkCallStartDecision

    public data class Busy(val message: String) : LinkCallStartDecision
}

/**
 * One call at a time, across all kinds.
 *
 * @param activeLinkCallIdHex the link call in the host's slot, when it holds the slot.
 */
public fun decideLinkCallStart(
    occupant: CallKind?,
    activeLinkCallIdHex: String?,
    requestedCallIdHex: String,
    verb: CallVerb,
): LinkCallStartDecision {
    if (occupant == null) return LinkCallStartDecision.Start
    if (occupant == CallKind.LINK && activeLinkCallIdHex == requestedCallIdHex) {
        return LinkCallStartDecision.AlreadyInThisCall
    }
    return LinkCallStartDecision.Busy(callBusyMessage(occupant, verb))
}

/** An action a link-call status line offers. */
public enum class LinkCallNoticeAction {
    /** Return to the link's landing. */
    OPEN,

    /** Leave the call. */
    CANCEL,

    /** Forget an outcome. */
    DISMISS,
}

/** A status line for a link call on a screen other than its landing. */
public data class LinkCallNotice(val text: String, val actions: List<LinkCallNoticeAction>)

/**
 * What to show about a link call away from its landing, or `null`.
 *
 * Nothing while the landing is showing the call itself, and nothing while the
 * call is live — the in-call screen covers every screen then. Before it is live
 * the user can move around (the admission wait can take a while), so they are
 * told it is still going and can cancel it; after it ends they are told why
 * once, since the landing that would have said so is gone. Same wording as the
 * web client.
 *
 * @param hasLanding this call came from a `/c/` link, so there is somewhere for
 *   [LinkCallNoticeAction.OPEN] to go. A call started from the Calls
 *   destination's `New call` (`docs/ux/call-links.md` section 2) has no
 *   landing, and offering to open one would be a button that goes nowhere.
 */
public fun linkCallNotice(
    phase: LinkCallPhase?,
    landingShowingIt: Boolean,
    errorMessage: String?,
    hasLanding: Boolean = true,
): LinkCallNotice? {
    if (phase == null || landingShowingIt) return null
    val waitingActions = buildList {
        if (hasLanding) add(LinkCallNoticeAction.OPEN)
        add(LinkCallNoticeAction.CANCEL)
    }
    return when (phase) {
        LinkCallPhase.LIVE -> null
        LinkCallPhase.STARTING -> LinkCallNotice("Connecting to the call…", waitingActions)
        LinkCallPhase.WAITING -> LinkCallNotice(
            "Waiting for the owner to admit you…",
            waitingActions,
        )
        LinkCallPhase.ENDED -> LinkCallNotice("The call has ended.", listOf(LinkCallNoticeAction.DISMISS))
        LinkCallPhase.DEAD -> LinkCallNotice(
            "This call has ended or isn't active.",
            listOf(LinkCallNoticeAction.DISMISS),
        )
        LinkCallPhase.ERROR -> LinkCallNotice(
            if (errorMessage == null) "The call failed." else "The call failed: $errorMessage",
            listOf(LinkCallNoticeAction.DISMISS),
        )
    }
}
