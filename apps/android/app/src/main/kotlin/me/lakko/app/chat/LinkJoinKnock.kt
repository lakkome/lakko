// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0053 section 4 / client contract point 6 — how a non-member asks to
 * join a claimed journal.
 *
 * The joiner no longer reads the group journal at all: a claimed journal
 * answers only its members. It knocks twice, each knock spending its own
 * `ds-knock` anonymous token (ADR-0054):
 *
 *   1. an EMPTY knock — a probe that stores nothing and returns the group's
 *      latest registered access epoch, which IS the group's epoch;
 *   2. the attribution (advisory, ADR-0028 OQ2) and the external-join proposal
 *      built for that epoch, in that order.
 *
 * The Welcome then arrives in the joiner's own device mailbox, so the wait is
 * on the mailbox receive path, never on the journal.
 *
 * The same sequence serves the channel co-publisher joiner against a
 * PublisherGroup journal. Pure over injected effects, so the order is
 * host-testable.
 */

/** A probe knock learned no epoch: the group is unclaimed, i.e. its owner has
 *  not opened it since the update. Nothing was stored. */
public class GroupNotAcceptingRequests :
    IllegalStateException("the group has no registered epoch yet; nothing was sent")

/**
 * Run the two knocks. [knock] posts its payloads (spending a fresh token on
 * every call; a refused token is retried inside the client) and returns the
 * epoch the server reports; [buildProposal] builds the external-join proposal
 * for that epoch (and makes its KeyPackage durable); [buildAttribution] is
 * best effort — `null` when it cannot be composed, and the request goes
 * without it.
 *
 * @return the epoch the proposal targets.
 * @throws GroupNotAcceptingRequests when the probe returns no epoch; no
 *   proposal is built and nothing is stored.
 */
public suspend fun runLinkJoinKnock(
    knock: suspend (payloads: List<ByteArray>) -> ULong?,
    buildProposal: suspend (epoch: ULong) -> ByteArray,
    buildAttribution: () -> ByteArray?,
): ULong {
    val epoch = knock(emptyList()) ?: throw GroupNotAcceptingRequests()
    val proposal = buildProposal(epoch)
    val attribution = try {
        buildAttribution()
    } catch (_: Throwable) {
        null
    }
    knock(listOfNotNull(attribution, proposal))
    return epoch
}

/** What the joiner reads when the group cannot take requests yet. */
public const val GROUP_NOT_ACCEPTING_REQUESTS: String =
    "This group isn't taking requests yet. Try again later."

/** The join screen after the joiner's device joined the group. */
public const val JOIN_SUCCESS_COPY: String = "You joined the group."

/** The join screen's button that opens the joined group. */
public const val JOIN_OPEN_CHAT: String = "Open chat"

/** Every join failure that is neither [GROUP_NOT_ACCEPTING_REQUESTS] nor a
 *  wrong device clock. The raw cause is logged, never shown. */
public const val JOIN_FAILED_COPY: String =
    "Your request didn't go through. Check your connection and try again."

/** The join screen's button that re-runs a failed request. */
public const val JOIN_TRY_AGAIN: String = "Try again"

/**
 * The one sentence a failed join request shows, from anywhere in [error]'s
 * cause chain: [GROUP_NOT_ACCEPTING_REQUESTS] when the probe learned no epoch,
 * [me.lakko.app.ds.DS_CLOCK_SKEW_COPY] when a proof refusal is explained by the
 * device clock, [me.lakko.app.discovery.INVITE_RATE_LIMITED_COPY] when the
 * ledger consume answered `429`, [me.lakko.app.anon.ACTION_ALLOWANCE_USED_COPY]
 * when the day's allowance of invite-consume or knock tokens is used up,
 * [JOIN_FAILED_COPY] for everything else.
 * Never a raw message.
 */
public fun joinFailureCopy(error: Throwable): String {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is GroupNotAcceptingRequests) return GROUP_NOT_ACCEPTING_REQUESTS
        if (cause is me.lakko.app.discovery.InviteLinkDamaged || cause is GroupInviteLinkDamaged) {
            return me.lakko.app.discovery.INVITE_LINK_DAMAGED_COPY
        }
        if (cause is me.lakko.app.anon.AnonTokenQuotaExhausted) {
            return me.lakko.app.anon.ACTION_ALLOWANCE_USED_COPY
        }
        if (cause is me.lakko.app.ds.DsProofRefusedError && cause.clockSkewed) {
            return me.lakko.app.ds.DS_CLOCK_SKEW_COPY
        }
        cause = cause.cause
    }
    // The ledger consume answered 429: the link is fine, the network's budget
    // is spent. Never "link invalid".
    if (me.lakko.app.discovery.isDiscoveryRateLimited(error)) {
        return me.lakko.app.discovery.INVITE_RATE_LIMITED_COPY
    }
    return JOIN_FAILED_COPY
}

/**
 * Whether a join failure showing [copy] offers "Try again": the general
 * failure ([JOIN_FAILED_COPY]) and the rate-limited one (its sentence already
 * says to try again in a minute). "Isn't taking requests yet" and the clock sentence
 * ask the person to wait or fix the device first, so they offer no immediate
 * retry.
 */
public fun joinFailureOffersRetry(copy: String): Boolean =
    copy == JOIN_FAILED_COPY || copy == me.lakko.app.discovery.INVITE_RATE_LIMITED_COPY

/**
 * ADR-0053 — the label of one pending join request in the members sheet.
 *
 * The request is keyed on the joiner's DEVICE leaf key while contacts and
 * profiles are keyed on the ACCOUNT key, so the name is looked up under
 * [accountKey] (from the joiner's verified device binding) first; a joiner
 * this device knows nothing about under that key keeps the existing label of
 * the leaf key ([fallbackLabel]).
 */
public fun pendingJoinLabel(
    accountKey: ByteArray?,
    leafKey: ByteArray,
    knownName: (ByteArray) -> String?,
    fallbackLabel: (ByteArray) -> String,
): String {
    val named = accountKey?.let(knownName)
    return named ?: fallbackLabel(leafKey)
}
