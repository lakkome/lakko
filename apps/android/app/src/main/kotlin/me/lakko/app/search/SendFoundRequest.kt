// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import kotlin.coroutines.cancellation.CancellationException
import me.lakko.app.l10n.UiText

/*
 * Sending the contact request to a person a search found. The Android twin
 * of `apps/web/src/search/send-request.ts`.
 *
 * - A person found through a grant is reach-checked again first: the setting
 *   may have narrowed since the search. When the row's grant is refused, every
 *   other grant this device holds for the person is tried, read fresh and in
 *   the usual order, and the send goes with the first that passes. None passing
 *   is [SendOutcome.ReachRefused], never sent; a check that could not be made
 *   is a failure, not a refusal.
 * - A person in Everyone with a verified username is sent by username (the
 *   username path resolves them at discovery).
 * - Anyone else goes by the identity bundle path with the key cross-checked
 *   here: the key the identity bundle publishes for the account id must be
 *   the key the person was found as (or the key that one provably rotated
 *   to), or nothing is sent.
 */

/** How a send ended. */
public sealed interface SendOutcome {
    public data object Sent : SendOutcome
    public data object Refused : SendOutcome

    /** Refused, and the reason is that the Delivery Service no longer takes
     *  this grant: the row must not offer it again. Reads as [Refused]. */
    public data object ReachRefused : SendOutcome

    public data class Failed(val message: UiText) : SendOutcome
}

/** The identity bundle of an account. */
public class IdentityKeys(public val signaturePub: ByteArray, public val x25519Pub: ByteArray)

/** What a send asks the outside world. */
public interface SendDeps {
    /** The DS reach check of the person's account mailbox with a grant. */
    public suspend fun reachCheck(identityPk: ByteArray, grantWireB64: String): Boolean

    /** Every grant this device holds for the person now, in the order they
     *  are tried. Read fresh: a new one may have arrived since the row. */
    public fun heldGrantWires(identityPk: ByteArray): List<String> = emptyList()

    /** The identity bundle of an account, or `null`. */
    public suspend fun identityFetch(accountId: ByteArray): IdentityKeys?

    /** ADR-0074 section 3.5a - whether [foundKey] is a PROVEN predecessor
     *  of [publishedKey] ([PeopleSearchDeps.isProvenPredecessor]). */
    public fun isProvenPredecessor(foundKey: ByteArray, publishedKey: ByteArray): Boolean = false

    /** [bindingB64] (ADR-0078) is the mailbox binding a public record
     *  carried, for a person found in Everyone. */
    public suspend fun sendByUsername(username: String, intro: ByteArray, grantWireB64: String?, bindingB64: String?)

    public suspend fun sendToBundle(
        accountId: ByteArray,
        signaturePub: ByteArray,
        x25519Pub: ByteArray,
        intro: ByteArray,
        grantWireB64: String?,
        bindingB64: String?,
    )

    /** Whether a send failure means the person can no longer be reached this
     *  way (the username no longer resolves: they left Everyone). */
    public fun isRefusal(error: Throwable): Boolean

    /** Why a request was refused, a fixed reason code and nothing about the
     *  person: every refusal reads alike on screen, and the log is the only
     *  place that tells them apart. */
    public fun noteRefusal(reason: String) {}

    /** The sentence for any other failure, rendered from its kind. */
    public fun errorMessage(error: Throwable): UiText
}

private fun hexToBytes(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    return try {
        ByteArray(hex.length / 2) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
    } catch (_: NumberFormatException) {
        null
    }
}

/** The first grant that still reaches the person: the row's, else any other
 *  held for them. `null` when every one was refused. Throws what the check
 *  throws: a check that could not be made is no refusal. */
private suspend fun reachingGrant(key: ByteArray, rowGrant: String, deps: SendDeps): String? {
    if (deps.reachCheck(key, rowGrant)) return rowGrant
    for (wire in deps.heldGrantWires(key)) {
        if (wire != rowGrant && deps.reachCheck(key, wire)) return wire
    }
    return null
}

/** Send the request. Never throws (a cancellation is let through). */
public suspend fun sendFoundRequest(person: PersonResult, intro: ByteArray, deps: SendDeps): SendOutcome {
    val accountIdHex = person.accountIdHex
    if (!canAddContact(person) || accountIdHex == null) {
        deps.noteRefusal("cannot-add")
        return SendOutcome.Refused
    }
    return try {
        val key = java.util.Base64.getDecoder().decode(person.keyB64)
        var grant: String? = null
        if (person.reach == PersonReach.GRANT) {
            val rowGrant = person.grantWireB64
            if (rowGrant == null) {
                deps.noteRefusal("no-grant")
                return SendOutcome.Refused
            }
            grant = reachingGrant(key, rowGrant, deps)
            if (grant == null) {
                deps.noteRefusal("reach-check-refused")
                return SendOutcome.ReachRefused
            }
        }
        val username = person.username
        if (person.reach == PersonReach.OPEN && username != null) {
            deps.sendByUsername(username, intro, grant, person.mailboxBindingB64)
            return SendOutcome.Sent
        }
        val accountId = hexToBytes(accountIdHex) ?: run {
            deps.noteRefusal("bad-account-id")
            return SendOutcome.Refused
        }
        val bundle = deps.identityFetch(accountId)
        // No bundle, or a bundle for another key: the person as found cannot
        // be reached under this account id. Nothing is sent. A bundle whose
        // key the found key PROVABLY rotated to is the same person
        // (ADR-0074 3.5a): the request is sealed to the bundle's keys.
        if (bundle == null) {
            deps.noteRefusal("no-bundle")
            return SendOutcome.Refused
        }
        if (!foundKeyIsPublished(key, bundle.signaturePub, deps::isProvenPredecessor)) {
            deps.noteRefusal("key-not-published")
            return SendOutcome.Refused
        }
        deps.sendToBundle(accountId, bundle.signaturePub, bundle.x25519Pub, intro, grant, person.mailboxBindingB64)
        SendOutcome.Sent
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (err: Throwable) {
        if (deps.isRefusal(err)) {
            deps.noteRefusal("send-refused")
            SendOutcome.Refused
        } else {
            SendOutcome.Failed(deps.errorMessage(err))
        }
    }
}
