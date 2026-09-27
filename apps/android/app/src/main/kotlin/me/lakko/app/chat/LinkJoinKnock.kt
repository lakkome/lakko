// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0053 section 4 / client contract point 6 — how a non-member asks to
 * join a claimed journal.
 *
 * The joiner no longer reads the group journal at all: a claimed journal
 * answers only its members. It knocks twice, each knock paying its own proof
 * of work:
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
 * Run the two knocks. [knock] posts its payloads (after solving a fresh
 * challenge) and returns the epoch the server reports; [buildProposal] builds
 * the external-join proposal for that epoch (and makes its KeyPackage
 * durable); [buildAttribution] is best effort — `null` when it cannot be
 * composed, and the request goes without it.
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
    "This group is not accepting requests yet. Try again later."
