// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

/*
 * ADR-0073 "Phase 2 revised", R.4 - the CONTINUITY reach key, as it stands
 * after ADR-0078 phase 1.
 *
 * The key (`continuityReachSeed(account id, key generation)` in the core) let
 * a peer that ACCEPTED a reset write a Welcome into the reset account's new
 * mailbox, which the resetting device kept the key registered on. Under
 * ADR-0078 a mailbox is addressed through a binding the owner hands out, and
 * no peer can address that mailbox any more, so the registration was removed
 * (nothing here derives or registers the key) and the Welcome below is a
 * recorded no-op. The core function stays. Mirror of
 * `apps/web/src/reach/continuity.ts`.
 */

/**
 * ADR-0078 - the continuity Welcome is a recorded NO-OP.
 *
 * Before ADR-0078 a peer that accepted a reset wrote a Welcome into the reset
 * account's new mailbox, signed with the continuity key and addressed to a
 * mailbox derived from the identity key. A mailbox address is now a hash of a
 * mailbox key derived from the account seed, which reaches a writer only
 * through a binding the owner hands out, and there is no carrier yet for the
 * reset account's new binding (ADR-0078, open question). So nothing is
 * written: never to an address derived from the account id or the identity
 * key. Logged without identifiers when this device did accept a reset;
 * always `null`.
 */
public fun continuityWelcomeDest(acceptedResetKnown: Boolean): me.lakko.app.contacts.MailboxDest? {
    if (acceptedResetKnown) {
        try {
            android.util.Log.i(
                "Reach",
                "ADR-0078: continuity Welcome skipped; no carrier for the reset account's mailbox",
            )
        } catch (_: Throwable) {
            // Host JVM tests have no android.util.Log.
        }
    }
    return null
}

/** How a successor's Welcomes went ([deliverSuccessorWelcomes]). */
public enum class WelcomeDelivery {
    /** Every person's mailbox took the Welcome. */
    DELIVERED,

    /** At least one person's mailbox could not be written: no verified
     *  binding is held for them, or no grant they accept (the new identity of
     *  an accepted reset, ADR-0078 open question). The Welcome stays owed. */
    UNREACHABLE,
}

/**
 * Deliver a successor group's Welcome to each person in [targets], once per
 * mailbox. Every mailbox is named first: when [mailboxOf] cannot name one
 * (no verified binding is held for that person) NOTHING is written and the
 * result is [WelcomeDelivery.UNREACHABLE]; nothing is ever addressed to a
 * mailbox derived from the account id or the identity key. A write that
 * [isUnreachable] classifies (no grant accepted) stops the loop with the same
 * result; any other failure propagates. Never "delivered" for a person
 * nobody wrote to (2026-11-25: a successor counted delivered with its
 * Welcome skipped became the conversation and every message sent there was
 * read by nobody). Idempotent on the wire: a second delivery is a duplicate
 * the recipient's join ignores.
 */
public suspend fun deliverSuccessorWelcomes(
    targets: List<ByteArray>,
    mailboxOf: (ByteArray) -> ByteArray?,
    publish: suspend (mailbox: ByteArray) -> Unit,
    isUnreachable: (Throwable) -> Boolean = { it is me.lakko.app.contacts.MailboxUnknown },
): WelcomeDelivery {
    val mailboxes = ArrayList<ByteArray>(targets.size)
    for (accountKey in targets) {
        val mailbox = mailboxOf(accountKey)
        if (mailbox == null) {
            continuityWelcomeDest(acceptedResetKnown = true)
            return WelcomeDelivery.UNREACHABLE
        }
        mailboxes += mailbox
    }
    val written = HashSet<String>()
    for (mailbox in mailboxes) {
        if (!written.add(mailbox.joinToString("") { "%02x".format(it) })) continue
        try {
            publish(mailbox)
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            if (isUnreachable(cause)) return WelcomeDelivery.UNREACHABLE
            throw cause
        }
    }
    return WelcomeDelivery.DELIVERED
}
