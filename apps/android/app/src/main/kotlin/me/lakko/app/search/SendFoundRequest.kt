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
 *   may have narrowed since the search. A refusal is "refused", never sent.
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
    public data class Failed(val message: UiText) : SendOutcome
}

/** The identity bundle of an account. */
public class IdentityKeys(public val signaturePub: ByteArray, public val x25519Pub: ByteArray)

/** What a send asks the outside world. */
public interface SendDeps {
    /** The DS reach check of the person's account mailbox with a grant. */
    public suspend fun reachCheck(identityPk: ByteArray, grantWireB64: String): Boolean

    /** The identity bundle of an account, or `null`. */
    public suspend fun identityFetch(accountId: ByteArray): IdentityKeys?

    /** ADR-0074 section 3.5a - whether [foundKey] is a PROVEN predecessor
     *  of [publishedKey] ([PeopleSearchDeps.isProvenPredecessor]). */
    public fun isProvenPredecessor(foundKey: ByteArray, publishedKey: ByteArray): Boolean = false

    public suspend fun sendByUsername(username: String, intro: ByteArray, grantWireB64: String?)

    public suspend fun sendToBundle(
        accountId: ByteArray,
        signaturePub: ByteArray,
        x25519Pub: ByteArray,
        intro: ByteArray,
        grantWireB64: String?,
    )

    /** Whether a send failure means the person can no longer be reached this
     *  way (the username no longer resolves: they left Everyone). */
    public fun isRefusal(error: Throwable): Boolean

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

/** Send the request. Never throws (a cancellation is let through). */
public suspend fun sendFoundRequest(person: PersonResult, intro: ByteArray, deps: SendDeps): SendOutcome {
    val accountIdHex = person.accountIdHex
    if (!canAddContact(person) || accountIdHex == null) return SendOutcome.Refused
    val grant = if (person.reach == PersonReach.GRANT) person.grantWireB64 else null
    return try {
        val key = java.util.Base64.getDecoder().decode(person.keyB64)
        if (person.reach == PersonReach.GRANT) {
            if (grant == null || !deps.reachCheck(key, grant)) return SendOutcome.Refused
        }
        val username = person.username
        if (person.reach == PersonReach.OPEN && username != null) {
            deps.sendByUsername(username, intro, grant)
            return SendOutcome.Sent
        }
        val accountId = hexToBytes(accountIdHex) ?: return SendOutcome.Refused
        val bundle = deps.identityFetch(accountId)
        // No bundle, or a bundle for another key: the person as found cannot
        // be reached under this account id. Nothing is sent. A bundle whose
        // key the found key PROVABLY rotated to is the same person
        // (ADR-0074 3.5a): the request is sealed to the bundle's keys.
        if (bundle == null || !foundKeyIsPublished(key, bundle.signaturePub, deps::isProvenPredecessor)) {
            return SendOutcome.Refused
        }
        deps.sendToBundle(accountId, bundle.signaturePub, bundle.x25519Pub, intro, grant)
        SendOutcome.Sent
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (err: Throwable) {
        if (deps.isRefusal(err)) SendOutcome.Refused else SendOutcome.Failed(deps.errorMessage(err))
    }
}
