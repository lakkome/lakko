// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

/*
 * Which keys a contact request goes out to, as pure decisions.
 *
 * A request sealed to keys the caller already holds (an invite link's
 * capability, a search result's bundle) runs no identity fetch of its own.
 * For a STRANGER (no pin) that is right. For an account this device has
 * pinned, a key that is not the pin sends the published identity through
 * the fail-closed gate first, where a proven rotation re-pins and an attested
 * reset is accepted silently (ADR-0065 R.2); a key that is then still neither
 * the pin nor a proven predecessor of it is refused. Old keys do not come
 * back. Mirror of `apps/web/src/contacts/request-target.ts`.
 */

public enum class ContactRequestGateVerdict {
    /** Nothing stands in the way: no pin (a stranger), or the key is the pin. */
    SEND,

    /** A pinned account is about to be reached under a key that is NOT the
     *  pin: the published identity must pass the fail-closed gate first. */
    CHECK_IDENTITY,
}

/**
 * What to do with a contact request addressed to [recipientKeyB64], given the
 * key this device has pinned for that account (`null` = no pin).
 */
public fun contactRequestGate(pinnedKeyB64: String?, recipientKeyB64: String): ContactRequestGateVerdict =
    if (pinnedKeyB64 == null || pinnedKeyB64 == recipientKeyB64) {
        ContactRequestGateVerdict.SEND
    } else {
        ContactRequestGateVerdict.CHECK_IDENTITY
    }

/** Which keys a contact request goes out to once the recipient's published
 *  identity has passed the fail-closed gate (ADR-0066 section 3.5a rule 1). */
public enum class RequestTarget {
    /** The keys in hand are the published ones. */
    AS_SUPPLIED,

    /** The key in hand is a PROVEN predecessor of the published one: seal to
     *  the published keys and send to the current mailbox. */
    CURRENT,

    /** The key in hand is neither: nothing is sent. */
    REFUSE,
}

/** Pure. */
public fun requestTargetAfterFetch(
    suppliedKeyB64: String,
    publishedKeyB64: String,
    /** Whether the published key is the proven successor of the supplied one. */
    publishedIsProvenSuccessor: Boolean,
): RequestTarget = when {
    suppliedKeyB64 == publishedKeyB64 -> RequestTarget.AS_SUPPLIED
    publishedIsProvenSuccessor -> RequestTarget.CURRENT
    else -> RequestTarget.REFUSE
}
