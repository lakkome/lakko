// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * A muted conversation's call, offered quietly (`docs/ux/calls.md` section
 * 2.1.1).
 *
 * ## What this is for
 *
 * Muting takes away the interruption, not the call. The ringtone, the
 * vibration and the screen that takes over are all withheld
 * ([IncomingCallRinger] and [OneToOneCallPresentation.QUIET_OFFER] between
 * them), and what is left has to be answerable anyway — otherwise muting a
 * conversation quietly becomes half-blocking the person, which is exactly the
 * reading section 2.1.1 replaced.
 *
 * So the same ringing call is offered on two surfaces that interrupt nothing:
 *
 *  1. a panel at the top of that conversation, IN the layout, so nothing the
 *     person was reading moves or is covered;
 *  2. that chat's own row in the conversation list.
 *
 * Both read this one function, so they cannot disagree about whether there is
 * anything to offer. It is pure and Android-free, which is what lets the host
 * JVM suite assert it; the Compose side renders what it is handed.
 *
 * ## What it deliberately does not carry
 *
 * The caller's **name**. Both surfaces are drawn inside something that already
 * names the conversation — the chat screen has resolved its peer name for the
 * header, the list row has its title — so carrying a name here would mean the
 * offer could say one thing while the row above it said another.
 *
 * The ring **window**. Nothing here counts down, and no surface needs a timer:
 * the host's own RING timer ends a call nobody answered, which clears the slot
 * both surfaces read from, so they disappear exactly when the window runs out
 * and by the same reading that files the missed call.
 */
public data class QuietCallOffer(
    /** The conversation the call is offered in. */
    val groupIdHex: String,
    /** The offer asked for video, so `Accept` answers the call that was made. */
    val video: Boolean,
) {
    /**
     * Is this offer the one conversation [groupIdHex] should draw?
     *
     * The comparison lives here rather than at the two call sites because it is
     * the whole condition of the list-row affordance, and a row that got it
     * wrong would put Accept on somebody else's conversation. Case-insensitive:
     * both sides are hex spellings of the same group id produced by different
     * code paths, and a call offered in a conversation is not a place to find
     * out that one of them capitalises.
     */
    public fun isFor(groupIdHex: String): Boolean = this.groupIdHex.equals(groupIdHex, ignoreCase = true)
}

/**
 * The quiet offer for the call in the slot, or `null` when there is nothing to
 * offer quietly.
 *
 * Reads [oneToOneCallPresentation] rather than re-deriving the muted-and-still-
 * ringing condition, so the overlay's verdict and the quiet surfaces' verdict
 * are one reading: there is never a moment where the full-screen ring and a
 * panel both draw, or neither does.
 *
 * [incoming] is asked as well, and it is not redundant. An outgoing call has
 * nobody to accept and its `muted` flag is meaningless (nobody is being
 * interrupted by a call they placed), so an offer built from one would put
 * Decline and Accept on the person's own outgoing call if any future state ever
 * read `Ringing` for it.
 */
public fun quietCallOffer(
    groupIdHex: String,
    state: CallSession.WebCallState,
    incoming: Boolean,
    accepted: Boolean,
    muted: Boolean,
    video: Boolean,
): QuietCallOffer? {
    if (!incoming) return null
    if (oneToOneCallPresentation(state, accepted, muted) != OneToOneCallPresentation.QUIET_OFFER) return null
    return QuietCallOffer(groupIdHex = groupIdHex, video = video)
}

/**
 * The quiet offer for this call in [state].
 *
 * The mapping from a call to the six values above exists once, here, for the
 * same reason the rule does: two screens mapping it themselves is two chances
 * to pass `remoteOffersVideo` where `muted` belongs.
 */
public fun OneToOneCall<*>.quietOffer(state: CallSession.WebCallState): QuietCallOffer? = quietCallOffer(
    groupIdHex = groupIdHex,
    state = state,
    incoming = incoming,
    accepted = accepted,
    muted = muted,
    video = remoteOffersVideo,
)
