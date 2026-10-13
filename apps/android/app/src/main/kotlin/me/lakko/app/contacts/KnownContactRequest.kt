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
): KnownRequestVerdict = when {
    !senderIsContact -> KnownRequestVerdict.SHOW
    check == KnownRequestCheck.REFUSED -> KnownRequestVerdict.DROP
    check == KnownRequestCheck.INCONCLUSIVE -> KnownRequestVerdict.WAIT
    trustedKeyB64 == requestKeyB64 -> KnownRequestVerdict.ANSWER
    else -> KnownRequestVerdict.DROP
}

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
