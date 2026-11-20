// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

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

    /** Not a conversation (a PublisherGroup or DeviceGroup Welcome): not this
     *  intake's, nothing to report. */
    NOT_A_CONVERSATION,

    /** Refused for any other reason. */
    REFUSED,
}

/** The outcome of a join attempt that threw [error]. */
public fun welcomeOutcomeOf(error: Throwable): WelcomeOutcome = when {
    isWelcomeForAnotherDevice(error) -> WelcomeOutcome.FOR_ANOTHER_DEVICE
    groupStillActiveHex(error) != null -> WelcomeOutcome.HELD_GROUP_STILL_ACTIVE
    isWelcomeNotAConversation(error) -> WelcomeOutcome.NOT_A_CONVERSATION
    else -> WelcomeOutcome.REFUSED
}
