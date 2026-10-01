// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

/*
 * ADR-0060 section 3 - the pure rules behind channel member cards. The
 * Android twin of `apps/web/src/channels/member-card-rules.ts`.
 *
 * A member card is the member's profile (v3) sealed under the channel's
 * current epoch seed and uploaded under the subscriber key it registered.
 * What the rules here decide:
 *
 * - what the OWN card says: the name always; the `@username` and the
 *   `channel` reach grant only when the "who can find me" setting lets this
 *   channel's members find the owner (UX section 6.2);
 * - when to upload it again: no card yet, a newer epoch (a newcomer holds
 *   only current seeds), or different content at the current epoch;
 * - what to do with a grant READ from someone else's card: keep it, withdraw
 *   a grant the person no longer hands out, or leave the store alone.
 */

/** What the own card carries. */
public data class CardContent(
    val displayName: String?,
    /** Only with the grant (UX 6.2). */
    val username: String?,
    /** Whether the `channel` reach grant rides in the card. */
    val includeGrant: Boolean,
)

/** Decide the own card's fields from the profile and the setting. */
public fun cardContent(
    displayName: String?,
    username: String?,
    /** `allowsScope(setting, Channel(id))`; `false` with no setting loaded. */
    channelAllowed: Boolean,
    /** A reach session is bound (it holds the seed grants derive from). */
    reachAvailable: Boolean,
): CardContent {
    val includeGrant = channelAllowed && reachAvailable
    return CardContent(
        displayName = displayName?.trim()?.takeIf { it.isNotEmpty() },
        username = if (includeGrant) username else null,
        includeGrant = includeGrant,
    )
}

/** A card this device uploaded, as far as re-upload decisions need. */
public data class UploadedCard(val epoch: UInt, val fingerprint: String)

/** A stable description of the card's content: two cards with the same
 *  fingerprint carry the same profile and the same grant version. */
public fun cardFingerprint(content: CardContent, updatedAtMs: Long, grantVersion: UInt?): String =
    listOf(
        content.displayName ?: "",
        content.username ?: "",
        if (content.includeGrant) grantVersion?.toString() ?: "" else "-",
        updatedAtMs.toString(),
    ).joinToString("\u0001")

/**
 * Whether the own card must be uploaded (again) for [epoch]: nothing uploaded
 * yet, a newer epoch, or the same epoch with different content. An older
 * epoch than the one already uploaded never is (the server keeps the newest).
 */
public fun cardUploadDue(last: UploadedCard?, epoch: UInt, fingerprint: String): Boolean = when {
    last == null -> true
    epoch > last.epoch -> true
    epoch < last.epoch -> false
    else -> fingerprint != last.fingerprint
}

/** What to do with the grant state of one card read from the member list. */
public enum class CardGrantDecision {
    /** File the card's `channel` grant under the sender's account key. */
    FILE,

    /** The card carries no `channel` grant: withdraw one held from before. */
    WITHDRAW,

    /** Leave the store alone. */
    SKIP,
}

/**
 * Decide the grant outcome of one card. A grant is filed only after the reach
 * check showed it is registered on the author's mailbox: a grant another
 * member copied or made up would otherwise be filed under someone else's key.
 * A refused or unanswered check leaves what is held as it is.
 */
public fun cardGrantDecision(
    isOwnCard: Boolean,
    /** The author's account key hex, `null` when it could not be resolved. */
    senderAccountKeyHex: String?,
    ownAccountKeyHex: String?,
    carriesChannelGrant: Boolean,
    /** The reach check accepted the grant; `null` when it did not run or
     *  could not answer (offline). */
    grantVerified: Boolean?,
): CardGrantDecision {
    if (isOwnCard) return CardGrantDecision.SKIP
    val sender = senderAccountKeyHex ?: return CardGrantDecision.SKIP
    if (ownAccountKeyHex != null && sender == ownAccountKeyHex) return CardGrantDecision.SKIP
    if (!carriesChannelGrant) return CardGrantDecision.WITHDRAW
    return if (grantVerified == true) CardGrantDecision.FILE else CardGrantDecision.SKIP
}

/** The label a member row shows. */
public fun memberRowName(displayName: String?, isSelf: Boolean): String {
    if (isSelf) return "You"
    return displayName?.trim()?.takeIf { it.isNotEmpty() } ?: "Member"
}
