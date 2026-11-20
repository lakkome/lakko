// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

/*
 * ADR-0074 section 3.7 (2026-11-20) — envelopes sealed to a member's account
 * after that member rotated its account keys.
 *
 * The roster names a rotated person by its CONVERSATION key (the oldest own
 * predecessor: every leaf binding is under it), while the directory serves
 * the CURRENT key and the current identity's X25519 key, plus the two-way
 * conversation link signed by both keys.
 *
 * Sender (the owner sealing a registrar seed, a root handover or a knock
 * seed): the envelope is ADDRESSED to the roster key and SEALED to the X25519
 * key of the bundle, and the bundle is accepted only by
 * [bundleServesRosterKey]. Receiver: the address is checked against the
 * conversation identity; the envelope opens with the conversation identity's
 * own X25519 key, else with the current identity's
 * ([openSealedToOwnAccount]).
 */

/**
 * Whether a fetched identity bundle is the bundle of the person the MLS
 * roster names by [rosterKey]: its served key IS the roster key, or its
 * [conversationLink] verifies against the served key and names exactly the
 * roster key as the conversation key. Anything else (no link, a link that
 * does not verify, a link naming another key, a malformed key) is refused.
 *
 * [verifyLink] is the core's `verifyConversationLink(link, trustedCurrentKey)`:
 * it answers the 32-byte conversation key and throws on anything wrong.
 */
public fun bundleServesRosterKey(
    servedKey: ByteArray,
    conversationLink: ByteArray?,
    rosterKey: ByteArray,
    verifyLink: (link: ByteArray, trustedCurrentKey: ByteArray) -> ByteArray,
): Boolean {
    if (servedKey.size != 32 || rosterKey.size != 32) return false
    if (servedKey.contentEquals(rosterKey)) return true
    if (conversationLink == null || conversationLink.isEmpty()) return false
    val conversationKey = try {
        verifyLink(conversationLink, servedKey)
    } catch (_: Throwable) {
        return false
    }
    return conversationKey.contentEquals(rosterKey)
}

/**
 * Open an envelope addressed to this account's conversation identity
 * [conversation]: first sealed to its own X25519 key ([openOwn]), then, when
 * the account rotated and the directory serves [current], sealed to the
 * current identity's X25519 key ([openSealedTo] with the conversation
 * identity as the addressee). `null` when neither opens.
 */
public fun <I : Any> openSealedToOwnAccount(
    conversation: I,
    current: I?,
    openOwn: (addressee: I) -> ByteArray,
    openSealedTo: (addressee: I, sealedTo: I) -> ByteArray,
): ByteArray? {
    try {
        return openOwn(conversation)
    } catch (_: Throwable) {
        // Not sealed to the conversation identity's X25519 key; try the
        // current identity's below.
    }
    if (current == null || current === conversation) return null
    return try {
        openSealedTo(conversation, current)
    } catch (_: Throwable) {
        null
    }
}
