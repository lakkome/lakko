// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

/*
 * UX `docs/ux/group-and-channel-privacy.md` section 15.2 — what a publisher
 * invitation offers, on the card in the one-to-one and on the `/g/` page
 * alike. A request has no deadline and a refusal is never told: once sent it
 * waits; after a day the person may ask again, once per day (amended
 * 2026-11-12, it was seven days; the rule is `requestStillWaiting`); a
 * link that stopped working shows so in place of the button, while the
 * earlier request still lands if the owner approves it late.
 */

/** What the invitation shows in place of (or as) its button. */
public enum class PublisherInviteControl {
    /** This account already publishes there: `Open`. */
    OPEN,

    /** Nothing sent yet: `Accept`. */
    ACCEPT,

    /** A press is in flight: the button disabled, "Waiting for the channel
     *  owner to approve...". */
    SENDING,

    /** Sent under a day ago: the waiting line, no button. */
    WAITING,

    /** Sent a day ago or more: "No answer yet." and `Ask again`. */
    ASK_AGAIN,

    /** The link no longer works: "This invite link is invalid or has
     *  expired." instead of the button. */
    LINK_INVALID,
}

/** The wait before `Ask again` (UX section 15.2.2): the group request's own
 *  day, [me.lakko.app.chat.ASK_AGAIN_AFTER_MS], not a second copy. */
public const val PUBLISHER_ASK_AGAIN_AFTER_MS: Long = me.lakko.app.chat.ASK_AGAIN_AFTER_MS

/**
 * The control for a publisher invitation. [pendingSentAtMs]: when the
 * request this account sent for the channel was sent, `null` when none is
 * pending; [linkDead]: an Accept or Ask again found the link gone.
 */
public fun publisherInviteControl(
    isPublisher: Boolean,
    pendingSentAtMs: Long?,
    nowMs: Long,
    sending: Boolean,
    linkDead: Boolean,
): PublisherInviteControl = when {
    sending -> PublisherInviteControl.SENDING
    isPublisher -> PublisherInviteControl.OPEN
    linkDead -> PublisherInviteControl.LINK_INVALID
    pendingSentAtMs == null -> PublisherInviteControl.ACCEPT
    me.lakko.app.chat.requestStillWaiting(pendingSentAtMs, nowMs) -> PublisherInviteControl.WAITING
    else -> PublisherInviteControl.ASK_AGAIN
}

/** Why an Accept or Ask again failed, for its line. */
public enum class PublisherAcceptFailure {
    /** The link is gone (revoked, expired, used up): [PublisherInviteControl.LINK_INVALID]. */
    LINK_DEAD,

    /** No network: "Could not reach Lakko. Check your connection and try again." */
    OFFLINE,

    /** Anything else: the join screen's own sentences (`joinFailureCopy`). */
    OTHER,
}

/** Classify [error] (its whole cause chain). */
public fun publisherAcceptFailure(error: Throwable): PublisherAcceptFailure {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is me.lakko.app.channels.CoPublisher.CoPublisherException.NotACoPublisherLink) {
            return PublisherAcceptFailure.LINK_DEAD
        }
        if (cause is me.lakko.app.ds.DsNetworkError ||
            cause is me.lakko.app.discovery.DiscoveryNetworkError ||
            cause is java.io.IOException
        ) {
            return PublisherAcceptFailure.OFFLINE
        }
        cause = cause.cause
    }
    return PublisherAcceptFailure.OTHER
}
