// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.UiText

/**
 * `Invite`, the one control the call screen gains
 * (`docs/ux/call-links.md` sections 3 and 5) — the Kotlin twin of the web
 * `apps/web/src/calls/in-call-invite.ts`.
 *
 * It opens a menu of the app's own with two items: **Copy link**, and
 * **Invite a contact**, which sends the invitation into a conversation the
 * person picks. Never the system's share sheet: a system sheet is a foreign
 * surface inside the call, different on every platform, and it offered a
 * dozen destinations where two are wanted.
 *
 * Both items hand out the link to THIS call — the same `call_id` the people
 * already in the call are using — so whoever follows it arrives here and not
 * in a new call. The first use in a call is what mints the capability
 * (ADR-0032), which costs a network round trip; later uses reuse the link
 * already minted. That includes a use that follows a failed send: the link
 * exists, only its delivery failed, so nothing is minted twice.
 *
 * A failure here never touches the call. Minting talks to the discovery
 * service and sending talks to the delivery service; the call runs over the
 * SFU and owes neither anything.
 *
 * Pure and Android-free, so every branch is unit-tested on the host JVM; the
 * clipboard arrives as an injected function.
 */

/** Where the link ended up. */
public sealed interface LinkDelivery {
    /** On the clipboard. */
    public data object Copied : LinkDelivery

    /** The clipboard was not reachable, so the link itself is shown to be
     *  copied by hand. */
    public data object Shown : LinkDelivery

    /** Sent as an invitation into a conversation; [name] is its title. */
    public data class Sent(val name: String) : LinkDelivery
}

/** What `Invite` is doing, from the moment the call starts. */
public sealed interface InCallInvite {
    /** Nothing minted yet in this call. */
    public data object Idle : InCallInvite

    /** Minting: the round trip to the ledger. */
    public data object Minting : InCallInvite

    /** A link exists for this call, and where the last use put it. */
    public data class Ready(val url: String, val delivery: LinkDelivery) : InCallInvite

    /** [url] is kept when the link was minted and only the delivery failed,
     *  so the next use does not mint again. */
    public data class Failed(val failure: CallInviteFailure, val url: String? = null) : InCallInvite
}

/** What choosing a menu item does now. */
public enum class InvitePress {
    /** Mint, then deliver. */
    MINT,

    /** Deliver the link already minted — no second round trip. */
    REOFFER,

    /** A mint is already in flight; a second one must not start. */
    IGNORE,
}

/** The link this call already has, if any. */
public fun mintedInviteUrl(state: InCallInvite): String? = when (state) {
    is InCallInvite.Ready -> state.url
    is InCallInvite.Failed -> state.url
    InCallInvite.Idle, InCallInvite.Minting -> null
}

/**
 * What the next use of `Invite` should do.
 *
 * A failed mint returns to [InvitePress.MINT]: section 5 says the failure
 * "can be tried again", and nothing about a failure makes the next attempt
 * less likely to work. A failed SEND does not: the link is there.
 */
public fun nextInvitePress(state: InCallInvite): InvitePress = when (state) {
    InCallInvite.Minting -> InvitePress.IGNORE
    is InCallInvite.Ready -> InvitePress.REOFFER
    InCallInvite.Idle -> InvitePress.MINT
    is InCallInvite.Failed -> if (state.url == null) InvitePress.MINT else InvitePress.REOFFER
}

/** Whether the control shows its working state. */
public fun inviteBusy(state: InCallInvite): Boolean = state is InCallInvite.Minting

/**
 * The line the call screen shows under the status row, or `null` when there
 * is nothing to say.
 */
public fun inviteStatusLine(state: InCallInvite): UiText? = when (state) {
    InCallInvite.Idle -> null
    InCallInvite.Minting -> UiText("call.invite.minting")
    is InCallInvite.Ready -> when (val delivery = state.delivery) {
        LinkDelivery.Copied -> UiText("call.invite.copied")
        LinkDelivery.Shown -> UiText("call.invite.shown", mapOf("url" to state.url))
        is LinkDelivery.Sent -> UiText("call.invite.sent", mapOf("name" to delivery.name))
    }
    is InCallInvite.Failed -> callInviteFailureMessage(state.failure)
}

/** How long a finished line stays up before the screen goes quiet again. */
public const val INVITE_STATUS_HOLD_MS: Long = 6_000L

/**
 * How long this state's line should stay, or `null` for "until something else
 * replaces it".
 *
 * [InCallInvite.Minting] has no timer: it ends when the mint does. A shown
 * link has none either — taking a link away while someone is copying it by
 * hand is the one case where a timer does harm.
 */
public fun inviteStatusHoldMs(state: InCallInvite): Long? = when {
    state is InCallInvite.Minting -> null
    state is InCallInvite.Ready && state.delivery is LinkDelivery.Shown -> null
    state is InCallInvite.Idle -> null
    else -> INVITE_STATUS_HOLD_MS
}

/**
 * Put the link on the clipboard, or in front of the person to copy by hand
 * when the clipboard is not reachable.
 */
public fun copyCallLink(url: String, copy: ((String) -> Unit)?): LinkDelivery {
    if (copy != null) {
        try {
            copy(url)
            return LinkDelivery.Copied
        } catch (_: Exception) {
            // Fall through to showing it.
        }
    }
    return LinkDelivery.Shown
}
