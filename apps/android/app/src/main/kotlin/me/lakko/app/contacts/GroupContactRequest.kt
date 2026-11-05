// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import java.security.MessageDigest

/*
 * `docs/ux/who-can-find-me.md` section 6.4 — a contact request sent inside a
 * group both people are in. The pure halves: which way a request goes, and
 * what the person's device keeps of one that arrived. Host-testable; the FFI
 * calls are passed in.
 */

/** Which way `Message` (and every other way of asking a known person) goes. */
public enum class RequestRoute {
    /** A grant this device holds still reaches the person: today's path. */
    GRANT,

    /** Both are current members of a group (not a one-to-one): inside it. */
    GROUP,

    /** Neither: nothing is sent (`contacts.detail.cannot_message`). */
    NONE,
}

/** Section 6.4, the order: a working grant, else a shared group, else none. */
public fun requestRoute(grantReaches: Boolean, sharedGroupHex: String?): RequestRoute = when {
    grantReaches -> RequestRoute.GRANT
    sharedGroupHex != null -> RequestRoute.GROUP
    else -> RequestRoute.NONE
}

/**
 * Whether a request to the account [recipientAccountIdB64] is still waiting in
 * Sent: pressing Message again sends nothing new until it is answered or
 * removed (section 6.4, "Message pressed again").
 */
public fun requestWaiting(sent: List<SentContactRequest>, recipientAccountIdB64: String): Boolean =
    sent.any { it.recipientAccountIdB64 == recipientAccountIdB64 }

/**
 * Open a request that arrived inside a group: [check] is
 * `checkGroupContactRequest` with this account's X25519 secret (it verifies
 * the request is from [senderKey], the member who posted it, and returns
 * `null` when it is not for this account); [toRequest] reads the verified
 * wire as an ordinary request. `null` = drop silently. A refusal [check]
 * throws propagates to the caller, which logs its code only.
 */
internal fun openGroupContactRequest(
    envelope: ByteArray,
    senderKey: ByteArray,
    check: (envelope: ByteArray, senderKey: ByteArray) -> ByteArray?,
    toRequest: (wire: ByteArray) -> IncomingContactRequest?,
): IncomingContactRequest? {
    val wire = check(envelope, senderKey) ?: return null
    val request = toRequest(wire) ?: return null
    // Defence in depth: the core already checked the request names the
    // member who posted it.
    if (!request.fromPubkey.contentEquals(senderKey)) return null
    return request
}

/**
 * Whether an opened group request is shown (section 6.4, "When the person
 * does not want it"): the person's own setting lets the members of that group
 * find them, or the sender is already their contact. Otherwise dropped
 * without a trace.
 */
internal fun groupRequestShown(allowsGroup: Boolean, senderIsContact: Boolean): Boolean =
    allowsGroup || senderIsContact

/**
 * The pending-row id of a request that arrived as DS row [dsMessageId] of
 * group [groupIdHex]: negative, stable, and apart from the mailbox's ids
 * (a group journal numbers its rows on its own).
 */
internal fun groupRequestMessageId(groupIdHex: String, dsMessageId: Long): Long {
    val digest = MessageDigest.getInstance("SHA-256")
        .digest("lakko-android/group-contact-request/${groupIdHex.lowercase()}/$dsMessageId".toByteArray(Charsets.UTF_8))
    var value = 0L
    for (i in 0 until 8) value = (value shl 8) or (digest[i].toLong() and 0xFF)
    val magnitude = value and Long.MAX_VALUE
    return if (magnitude == 0L) -1L else -magnitude
}

/** The refusal code of a failed opening (`requestInvalid`, `senderMismatch`,
 *  a layout code), never the message text, which may carry more. */
internal fun groupRequestErrorCode(error: Throwable): String {
    val message = error.message ?: return error.javaClass.simpleName
    val match = Regex("group contact request ([A-Za-z]+)").find(message)
    return match?.groupValues?.get(1) ?: error.javaClass.simpleName
}

/** What `Message` on a known person ended in (section 6.4). */
public sealed interface MessageOutcome {
    /** A one-to-one stands (founded through a grant): open it. */
    public data class Opened(val groupIdHex: String) : MessageOutcome

    /** A request went inside a shared group, or one is already waiting. */
    public data object RequestSent : MessageOutcome

    /** No grant and no shared group: nothing was sent. */
    public data object CannotMessage : MessageOutcome

    /** The request could not be sent (network): Message works again. */
    public data object RequestFailed : MessageOutcome
}

/**
 * Section 6.4, the order Message runs for a known person with no chat on this
 * device, with every effect passed in:
 *
 * 0. [waiting]: a request to them is already in Sent; nothing new is sent.
 * 1. [grantReaches]: a grant this device holds opens their inbox; the
 *    one-to-one is founded ([found], which throws on failure).
 * 2. Else [sharedGroup]: the request goes inside it ([sendInGroup]).
 * 3. Else nothing is sent.
 *
 * A grant check that fails (network) is [MessageOutcome.RequestFailed].
 */
public suspend fun runMessageOrder(
    waiting: Boolean,
    grantReaches: suspend () -> Boolean,
    sharedGroup: () -> String?,
    found: suspend () -> String,
    sendInGroup: suspend (groupIdHex: String) -> Boolean,
    noteFailure: (String) -> Unit = {},
): MessageOutcome {
    if (waiting) return MessageOutcome.RequestSent
    val reaches = try {
        grantReaches()
    } catch (e: kotlinx.coroutines.CancellationException) {
        throw e
    } catch (e: Throwable) {
        noteFailure("grant check failed (${e.javaClass.simpleName})")
        return MessageOutcome.RequestFailed
    }
    val shared = if (reaches) null else sharedGroup()
    return when (requestRoute(reaches, shared)) {
        RequestRoute.GRANT -> MessageOutcome.Opened(found())
        RequestRoute.GROUP -> if (sendInGroup(shared!!)) MessageOutcome.RequestSent else MessageOutcome.RequestFailed
        RequestRoute.NONE -> MessageOutcome.CannotMessage
    }
}
