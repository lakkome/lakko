// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

/*
 * ADR-0073 R.7 case 1 / UX 16.6 - a contact request (or an `/i/` link) from
 * an account that is ALREADY a contact.
 *
 * A person who reset their keys lost their contacts and may find the other
 * person again before that person's device noticed anything. The device
 * never shows a request for someone who is already a contact: it runs the
 * R.2 check on the sender's account id and then answers the request itself
 * (the conversation's successor is the answer) or drops it. Nothing is shown
 * either way, and on a refusal nothing is sent to the keys that sent the
 * request. Mirror of `apps/web/src/contacts/known-contact-request.ts`.
 */

/** How the identity check of the sender's account ended. */
public enum class KnownRequestCheck {
    /** The gate passed: the published key is what this device now trusts. */
    CURRENT,

    /** The gate refused the published key ([SupersessionRefused]). */
    REFUSED,

    /** No verdict yet: the directory, the attest key or the key log could not
     *  be reached, or the account publishes nothing. */
    INCONCLUSIVE,
}

public enum class KnownRequestVerdict {
    /** The sender is not a contact: the ordinary request row. */
    SHOW,

    /** A contact, and the request is signed by the key this device trusts for
     *  the account: answered silently. No request row. */
    ANSWER,

    /** A contact, and the request is NOT from the trusted key: nothing is
     *  shown and nothing is sent. */
    DROP,

    /** A contact, and there is no verdict yet: the request waits, unshown. */
    WAIT,
}

/**
 * | sender is a contact | check          | request key is the trusted key | verdict  |
 * |---------------------|----------------|--------------------------------|----------|
 * | no                  | -              | -                              | `SHOW`   |
 * | yes                 | `REFUSED`      | -                              | `DROP`   |
 * | yes                 | `INCONCLUSIVE` | -                              | `WAIT`   |
 * | yes                 | `CURRENT`      | yes                            | `ANSWER` |
 * | yes                 | `CURRENT`      | no                             | `DROP`   |
 *
 * [trustedKeyB64] is the pin AFTER the check ran; `null` reads as "not the
 * trusted key".
 *
 * [senderIsContact] is [establishedContactOfAccount]: a contact row whose
 * account id came only from the peer's own profile is not enough.
 */
public fun knownContactRequestVerdict(
    senderIsContact: Boolean,
    check: KnownRequestCheck,
    requestKeyB64: String,
    trustedKeyB64: String?,
    /** Base scenario 7 (2026-11-22): whether this device holds a live
     *  one-to-one with them. The ack alone starts no chat, so a contact with
     *  none here is [KnownRequestVerdict.SHOW]n and Accept starts it, as a
     *  request through a group is (`docs/ux/who-can-find-me.md` 6.4). */
    holdsOneToOne: Boolean = true,
    /** Whether the request is no newer than the contact relationship
     *  ([requestPredatesContact]): read again after a lost mailbox cursor.
     *  Dropped instead of listed for want of a one-to-one. */
    predatesContact: Boolean = false,
    /** Whether another device of this account may still be admitting this
     *  one to the chat with them ([siblingMayAdmit]): the request waits,
     *  unshown, and is answered silently once the chat is here. */
    siblingMayAdmit: Boolean = false,
): KnownRequestVerdict = when {
    !senderIsContact -> KnownRequestVerdict.SHOW
    check == KnownRequestCheck.REFUSED -> KnownRequestVerdict.DROP
    check == KnownRequestCheck.INCONCLUSIVE -> KnownRequestVerdict.WAIT
    trustedKeyB64 != requestKeyB64 -> KnownRequestVerdict.DROP
    holdsOneToOne -> KnownRequestVerdict.ANSWER
    predatesContact -> KnownRequestVerdict.DROP
    siblingMayAdmit -> KnownRequestVerdict.WAIT
    else -> KnownRequestVerdict.SHOW
}

/** How long a request from a contact this device holds no chat with waits
 *  for another device of the account to admit this one (ADR-0050
 *  re-admission) before it is listed. Chosen, not measured. */
public const val SIBLING_ADMIT_WAIT_MS: Long = 15L * 60L * 1000L

/**
 * Whether another device of this account may still bring this one into the
 * chat with the person (base scenario 9): the account has another device and
 * the request ([requestCreatedAt], the DS stamp) is younger than
 * [SIBLING_ADMIT_WAIT_MS]. Unknown facts read as "no".
 */
public fun siblingMayAdmit(otherDeviceCount: Int?, requestCreatedAt: String, nowMs: Long): Boolean {
    if (otherDeviceCount == null || otherDeviceCount < 1) return false
    val at = runCatching { java.time.Instant.parse(requestCreatedAt).toEpochMilli() }
        .recoverCatching { java.time.OffsetDateTime.parse(requestCreatedAt).toInstant().toEpochMilli() }
        .getOrNull() ?: return false
    return nowMs - at < SIBLING_ADMIT_WAIT_MS
}

/**
 * Whether a request is no newer than the contact relationship: its DS stamp
 * ([requestCreatedAt], server-set) is at or before the contact row's own
 * stamp ([contactSince], the request it was made from, or the time the
 * one-to-one rule made it). A stamp that does not parse reads as newer: a
 * request is never dropped on a missing fact.
 */
public fun requestPredatesContact(requestCreatedAt: String, contactSince: String?): Boolean {
    fun parse(value: String): Long? = runCatching { java.time.Instant.parse(value).toEpochMilli() }
        .recoverCatching { java.time.OffsetDateTime.parse(value).toInstant().toEpochMilli() }
        .getOrNull()
    val at = parse(requestCreatedAt) ?: return false
    val since = contactSince?.let(::parse)?.takeIf { it > 0 } ?: return false
    return at <= since
}

/**
 * The "contact since" a contact carried to this device by the pairing state
 * bundle gets here (ISO-8601): the later of the time another device of the
 * account accepted them ([carriedAcceptedAtMs]) and the time the bundle
 * arrived ([arrivedAtMs]). On this device the person is a contact only since
 * then, so a request that was already in the account's mailbox predates the
 * contact here ([requestPredatesContact]): the device that held the contact
 * when it arrived handled it (answered silently, or listed it there), and this
 * one never asks again. Mirror of web `carriedContactSinceMs` and iOS
 * `StateBundle.accepted(_:arrivedAtMs:)`.
 */
public fun carriedContactSince(carriedAcceptedAtMs: Long, arrivedAtMs: Long): String =
    java.time.Instant.ofEpochMilli(maxOf(carriedAcceptedAtMs, arrivedAtMs, 0L)).toString()

/**
 * The contact row this device has ESTABLISHED for an account id, or `null`.
 *
 * A contact row can carry an account id that came only from the peer's own
 * profile, and a member can write any account id there. Such a row
 * establishes nothing: a member who claimed another account's id would
 * otherwise make that account's later, genuine request vanish. So a row
 * counts only when its key is one this device verified for the account
 * itself. The lookup is by the ACCOUNT, never by the key:
 *
 * | a row names the account | the pin names that row's key | a verified binding names that row's key | result   |
 * |-------------------------|------------------------------|-----------------------------------------|----------|
 * | no                      | -                            | -                                       | none     |
 * | yes                     | yes                          | any                                     | that row |
 * | yes                     | no                           | yes                                     | that row |
 * | yes                     | no                           | no                                      | none     |
 *
 * "The pin names the key" means the pinned key or one of its PROVEN
 * predecessors ([pinKeysB64]); a key a reset retired is not among them. Every
 * row naming the account is considered, so a claimed row cannot hide the real
 * one. Used by the known-contact request arm and for an `/i/` link from an
 * existing contact.
 */
public fun establishedContactOfAccount(
    rows: List<ContactRowKeys>,
    accountIdB64: String?,
    /** The pinned key and its proven predecessors; empty when not pinned. */
    pinKeysB64: List<String>,
    /** The key of the verified binding of the account, or `null`. */
    bindingKeyB64: String?,
): Int? {
    if (accountIdB64.isNullOrEmpty()) return null
    return rows.indexOfFirst { c ->
        c.accountIdB64 == accountIdB64 && c.keyB64.isNotEmpty() &&
            (c.keyB64 in pinKeysB64 || c.keyB64 == bindingKeyB64)
    }.takeIf { it >= 0 }
}

/**
 * Whether a contact row sits under the key that signed a request.
 *
 * A request reaches this code with its signature verified, so the sender
 * holds that key, and a row under the same key is that person whatever the
 * row says about their account: a row with no account id, or one no pin or
 * binding vouches for yet, is still a contact and must be answered. Treating
 * it as a stranger's request drops it silently (the inbox keeps no request
 * from a contact's key) and leaves the sender waiting for good. A member who
 * claimed someone else's account id gains nothing: the comparison is with the
 * key that signed.
 */
public fun isContactKey(requestKeyB64: String, rowKeysB64: List<String>): Boolean =
    requestKeyB64.isNotEmpty() && requestKeyB64 in rowKeysB64
