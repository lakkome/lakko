// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.moderation.AccountStanding
import me.lakko.app.moderation.SuspensionMarks

/**
 * ADR-0099 sections 3 and 6 — what the inbox poll lets through of a fetched
 * batch of contact requests, BEFORE anything is stored or decided. Every
 * route runs through it: the inbox, a request carried inside a group, and a
 * request answering one of this account's invite links, which is otherwise
 * accepted with no card ([ContactsRepository] keeps it undecided right after
 * this and decides it later).
 *
 * - from a blocked account: dropped unseen;
 * - from an account the moderation log confirms suspended within
 *   [timeoutMs]: dropped unseen; a slow, failed or unknown check keeps it.
 */
internal suspend fun screenFetchedRequests(
    requests: List<IncomingContactRequest>,
    dropsBlocked: (accountKeyB64: String) -> Boolean = { me.lakko.app.moderation.dropsContactRequest(it) },
    timeoutMs: Long = SuspensionMarks.REQUEST_CHECK_TIMEOUT_MS,
    standingOf: suspend (accountKeyB64: String) -> AccountStanding = me.lakko.app.moderation.liveStanding,
): List<IncomingContactRequest> {
    val notBlocked = requests.filterNot { dropsBlocked(java.util.Base64.getEncoder().encodeToString(it.fromPubkey)) }
    return me.lakko.app.moderation.filterSuspendedRequests(notBlocked, { it.fromPubkey }, timeoutMs, standingOf)
}

/**
 * ADR-0099 step 4 / UX 3 — the held contact requests the Requests list shows:
 * a request kept from before its sender's suspension leaves the list once the
 * moderation log confirms it ([suspendedKeysB64] grows as answers arrive; an
 * unknown standing keeps the request).
 */
internal fun shownPendingRequests(
    pending: List<IncomingContactRequest>,
    suspendedKeysB64: Set<String>,
): List<IncomingContactRequest> =
    me.lakko.app.moderation.withoutSuspendedRows(pending, suspendedKeysB64) {
        java.util.Base64.getEncoder().encodeToString(it.fromPubkey)
    }

/**
 * ADR-0099 step 4 — whether an undecided request answering one of this
 * account's invite links is dropped when it comes to be decided: only on a
 * suspension the log confirms within [timeoutMs]. It may have been set aside
 * before the suspension was published, or while the check at fetch timed out.
 */
internal suspend fun heldLinkRequestDropsOnSuspension(
    request: IncomingContactRequest,
    timeoutMs: Long = SuspensionMarks.REQUEST_CHECK_TIMEOUT_MS,
    standingOf: suspend (accountKeyB64: String) -> AccountStanding = me.lakko.app.moderation.liveStanding,
): Boolean = me.lakko.app.moderation.suspendedWithin(request.fromPubkey, timeoutMs, standingOf)
