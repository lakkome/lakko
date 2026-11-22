// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.flow.first

/*
 * ADR-0060 section 6, "one inbound mailbox per person" (amending ADR-0050
 * section 2.7) — a Welcome goes to the added leaf's ACCOUNT mailbox, one write
 * per added account, and every device of that account reads the one mailbox.
 * The pure half: which accounts one Welcome is written to, and whether a
 * Welcome that did not open was simply sealed to another device.
 */

/**
 * The distinct accounts one Welcome is delivered to, in first-seen order:
 * each added leaf resolves to the account key its device binding names
 * ([accountKeyOf], `null` when it names none — a Phase 1 leaf, whose account
 * key IS its own key, so the leaf key stands in). One write per account: the
 * account's devices all read the same mailbox.
 */
public fun welcomeAccounts(
    addedLeafKeys: List<ByteArray>,
    accountKeyOf: (ByteArray) -> ByteArray?,
): List<ByteArray> {
    val seen = LinkedHashMap<String, ByteArray>()
    for (leaf in addedLeafKeys) {
        val account = accountKeyOf(leaf)?.takeIf { it.isNotEmpty() } ?: leaf
        seen.putIfAbsent(account.joinToString("") { "%02x".format(it.toInt() and 0xff) }, account)
    }
    return seen.values.toList()
}

/**
 * Whether [error] (or anything in its cause chain) is OpenMLS refusing a
 * Welcome for which this device holds no matching KeyPackage — a Welcome for
 * another device of the same account, which shares the account mailbox. Not a
 * failure of this device: nothing to report, nothing to republish.
 */
public fun isWelcomeForAnotherDevice(error: Throwable): Boolean {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause.message?.contains(NO_MATCHING_KEY_PACKAGE) == true) return true
        cause = cause.cause
    }
    return false
}

/**
 * Whether [error] (or anything in its cause chain) is the core refusing a
 * Welcome because its group carries no role table, so it is not a conversation
 * (the typed `NoRoleTable` refusal of `Group.joinFromWelcome`, C-86): a
 * PublisherGroup or DeviceGroup Welcome that shares the account mailbox. Not a
 * failed conversation join: the channel and device sides take it, and the
 * conversation intake raises no banner for it. Typed, never matched by message.
 */
public fun isWelcomeNotAConversation(error: Throwable): Boolean {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is uniffi.lakko_ffi_uniffi.FfiException.MembershipNoRoleTable) return true
        cause = cause.cause
    }
    return false
}

/**
 * Whether a conversation join from the account mailbox that threw [error]
 * raises the "could not be opened on this device" banner: only a refusal of
 * this device's own conversation Welcome does. A Welcome sealed to another
 * device of the account, one kept for a group still held as active, one that
 * is not a conversation ([isWelcomeNotAConversation]) and a co-publisher's
 * Welcome while one is awaited ([awaitingCoPublishWelcome]) raise nothing.
 */
public fun welcomeFailureRaisesBanner(error: Throwable, awaitingCoPublishWelcome: Boolean): Boolean =
    !isChatLayerNotReady(error) &&
        !isWelcomeForAnotherDevice(error) &&
        groupStillActiveHex(error) == null &&
        !isWelcomeNotAConversation(error) &&
        !awaitingCoPublishWelcome

/** The OpenMLS `WelcomeError` variant the core formats into its message. */
internal const val NO_MATCHING_KEY_PACKAGE: String = "NoMatchingKeyPackage"

/** What became of one Welcome from the account mailbox, for the log line
 *  (no identifiers). */
public enum class WelcomeOutcome {
    /** Joined (or rejoined) its group. */
    JOINED,

    /** Sealed to another device of the account: no matching KeyPackage here. */
    FOR_ANOTHER_DEVICE,

    /** Refused for a group this device still holds as active: kept, tried again. */
    HELD_GROUP_STILL_ACTIVE,

    /** Not tried: this device's chat layer was not ready for the account
     *  (not bootstrapped yet, or another account). Kept, tried again. */
    HELD_CHAT_NOT_READY,

    /** Not a conversation (a PublisherGroup or DeviceGroup Welcome): not this
     *  intake's, nothing to report. */
    NOT_A_CONVERSATION,

    /** Refused for any other reason. */
    REFUSED,
}

/** The outcome of a join attempt that threw [error]. */
public fun welcomeOutcomeOf(error: Throwable): WelcomeOutcome = when {
    isChatLayerNotReady(error) -> WelcomeOutcome.HELD_CHAT_NOT_READY
    isWelcomeForAnotherDevice(error) -> WelcomeOutcome.FOR_ANOTHER_DEVICE
    groupStillActiveHex(error) != null -> WelcomeOutcome.HELD_GROUP_STILL_ACTIVE
    isWelcomeNotAConversation(error) -> WelcomeOutcome.NOT_A_CONVERSATION
    else -> WelcomeOutcome.REFUSED
}

/**
 * The chat layer cannot act for the signed-in account yet: [ChatRepository]
 * has not finished its bootstrap, or it holds another account. Nothing about
 * the input is wrong; the same call succeeds once the layer is ready, so a
 * Welcome refused with it is kept behind the inbox cursor, never consumed.
 */
public class ChatLayerNotReady(message: String) : IllegalStateException(message)

/** Whether [error] (or anything in its cause chain) is [ChatLayerNotReady].
 *  Typed, never matched by message. */
public fun isChatLayerNotReady(error: Throwable): Boolean {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is ChatLayerNotReady) return true
        cause = cause.cause
    }
    return false
}

/**
 * Whether the chat layer can join Welcomes for [wantedAccountB64]: it finished
 * its bootstrap ([bootstrapped]), holds an identity ([hasIdentity]) and that
 * bootstrap was for this account ([chatAccountB64]).
 */
public fun chatLayerReadyFor(
    bootstrapped: Boolean,
    chatAccountB64: String?,
    hasIdentity: Boolean,
    wantedAccountB64: String,
): Boolean = bootstrapped && hasIdentity && chatAccountB64 == wantedAccountB64

/**
 * Wait until [isReady] holds, re-checking on every emission of [changes],
 * for at most [timeoutMs]. `true` when it holds (at once, or within the
 * time); `false` when it did not. A [timeoutMs] of 0 or less does not wait.
 *
 * The inbox poll waits here before it joins Welcomes: a pass that met an
 * unready chat layer spends its long-poll budget waiting rather than
 * returning at once and re-reading the held rows in a tight loop.
 */
public suspend fun awaitChatLayerReady(
    isReady: () -> Boolean,
    changes: kotlinx.coroutines.flow.Flow<*>,
    timeoutMs: Long,
): Boolean {
    if (isReady()) return true
    if (timeoutMs <= 0) return false
    return kotlinx.coroutines.withTimeoutOrNull(timeoutMs) {
        changes.first { isReady() }
        true
    } ?: isReady()
}

/**
 * The inbox cursor after a pass that fetched up to [fetched]: never backward
 * from [current], and never past a Welcome this pass kept for later
 * ([heldWelcomeIds], DS message ids). The cursor stops just before the
 * earliest held Welcome, so the next pass reads it again; the rows after it
 * are read again too, and the steps that take them are idempotent.
 */
public fun inboxCursorAfterPass(current: Long, fetched: Long, heldWelcomeIds: Collection<Long>): Long {
    val earliestHeld = heldWelcomeIds.minOrNull() ?: return maxOf(current, fetched)
    return maxOf(current, minOf(fetched, earliestHeld - 1))
}
