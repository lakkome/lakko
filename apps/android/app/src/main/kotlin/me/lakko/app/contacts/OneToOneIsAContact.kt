// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import java.security.MessageDigest

/**
 * The one rule behind the Contacts tab: a person this account has a
 * one-to-one conversation with IS a contact, full stop.
 *
 * Until 2026-09-26 a contact row was written only by Accept and by an incoming
 * ContactAck, while the conversation itself arrives by another event (the MLS
 * Welcome, or a second device being admitted to the account's conversations),
 * so any path that delivered the conversation without the acknowledgement left
 * a named chat, an empty Contacts tab and a conversation info sheet reading
 * "Not in your contacts" - seen live on a second device.
 *
 * This is the pure half of [ContactsRepository.adoptOneToOnePeer]: the accepted
 * list after adopting the peer, or `null` when it is already complete.
 *
 *  - No accepted row holds [signatureKey]: append one. The row carries a
 *    synthetic request, because no mailbox message ever existed - the id is
 *    [syntheticContactMessageId] (stable, negative, per key) and the pseudonym
 *    is zero like the invite-link path's.
 *  - A row exists with no recorded `groupIdHex`: fill it in, keeping the
 *    request (its id, its account id) and the display name.
 *  - A row exists with a recorded chat: nothing to do, `null`.
 *
 * Never overwrites a recorded chat, a display name or an account id.
 */
public fun adoptingOneToOnePeer(
    accepted: List<AcceptedContact>,
    signatureKey: ByteArray,
    accountId: ByteArray?,
    groupIdHex: String,
    nowMs: ULong,
    createdAt: String,
): List<AcceptedContact>? {
    val index = accepted.indexOfFirst { it.request.fromPubkey.contentEquals(signatureKey) }
    if (index >= 0) {
        val old = accepted[index]
        if (old.groupIdHex != null) return null
        return accepted.toMutableList().also { rows ->
            rows[index] = old.copy(groupIdHex = groupIdHex)
        }
    }
    val synthetic = IncomingContactRequest(
        messageId = syntheticContactMessageId(signatureKey),
        fromPubkey = signatureKey,
        fromAccountId = accountId,
        fromPseudonym = ByteArray(32),
        validAfterMs = nowMs,
        intro = null,
        createdAt = createdAt,
    )
    return accepted + AcceptedContact(request = synthetic, groupIdHex = groupIdHex)
}

/**
 * A stable, negative row id for a contact that has no mailbox message: the
 * first eight bytes of SHA-256 over the key, sign-forced negative. Negative
 * because the mailbox assigns positive ids, so the two spaces never meet, and
 * stable so the row keeps its identity (rename, remove) across launches.
 */
public fun syntheticContactMessageId(signatureKey: ByteArray): Long {
    val digest = MessageDigest.getInstance("SHA-256").digest(signatureKey)
    var value = 0L
    for (i in 0 until 8) {
        value = (value shl 8) or (digest[i].toLong() and 0xFF)
    }
    val magnitude = value and Long.MAX_VALUE
    return if (magnitude == 0L) -1L else -magnitude
}
