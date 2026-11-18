// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0073 R.5 / R.8 - NOTHING FROM A SUPERSEDED SENDER ACTS.
 *
 * A key an accepted reset retired belongs to nobody this device trusts: a
 * device still holding it was removed from the account (or stolen). Its MLS
 * leaf stays in a conversation until the conversation is repaired or moves,
 * and until then it can still post. What it posts never acts here:
 *
 *   * a chat row or an op (edit, delete, reaction, typing, read receipt) is
 *     dropped wherever it arrives, before storage;
 *   * a call offer, and a control or service envelope (a profile, a group
 *     name, a pin, a registrar seed, a push routing secret, a beacon, a
 *     successor claim, ...) is dropped in a conversation that cannot be
 *     repaired in place for THAT reset, or that has already moved.
 *
 * The second rule is narrower on purpose. In a group that is repaired in
 * place the retired leaves are about to be removed by a proposal, and the
 * group's own role gates still judge every envelope; in a conversation whose
 * owner or other person is the retired key those gates would read the
 * retired key as the authority, which is exactly what must not happen.
 *
 * MLS frames are not touched: a commit or a proposal is the group's state
 * and is judged by the core. One function for the foreground poll and the
 * background push reader.
 */

/** What a received frame would do. */
public enum class SenderAct {
    /** A chat row or an op on one. */
    ROW,

    /** Call signalling (an offer rings the phone). */
    CALL,

    /** A control or service envelope. */
    CONTROL,
}

/**
 * | sender key is retired | act                | conversation                                   | dropped |
 * |-----------------------|--------------------|------------------------------------------------|---------|
 * | no, or not resolved   | any                | any                                            | no      |
 * | yes                   | `ROW`              | any                                            | yes     |
 * | yes                   | `CALL` / `CONTROL` | already moved                                  | yes     |
 * | yes                   | `CALL` / `CONTROL` | unrepairable because of this sender's reset    | yes     |
 * | yes                   | `CALL` / `CONTROL` | repaired in place, or unrepairable for another | no      |
 *
 * [unrepairableKeyB64] is the key whose accepted reset makes the
 * conversation unrepairable ([unrepairableBy]): the other person of a
 * one-to-one, or a group's owner; `null` when it is repairable.
 */
public fun supersededActDropped(
    act: SenderAct,
    senderKeyB64: String?,
    supersededKeys: Set<String>,
    unrepairableKeyB64: String?,
    moved: Boolean,
): Boolean {
    if (!dropsSupersededSender(senderKeyB64, supersededKeys)) return false
    return when (act) {
        SenderAct.ROW -> true
        SenderAct.CALL, SenderAct.CONTROL -> moved || unrepairableKeyB64 == senderKeyB64
    }
}

/**
 * What [event] would do and which leaf sent it, for [supersededActDropped];
 * `null` for a frame the rule never drops (an MLS commit or proposal, a
 * system row, an unrecognised envelope that is dropped anyway).
 *
 * The sender is the PERSON key resolved at receipt (C-68) wherever the event
 * carries one; the two envelopes that carry only the sender leaf are
 * resolved by [personKeyOfLeaf] against the roster the batch left.
 */
internal fun senderActOf(
    event: IncomingChatEvent,
    personKeyOfLeaf: (UInt?) -> MemberIdentity?,
): Pair<SenderAct, MemberIdentity?>? = when (event) {
    is IncomingChatEvent.Application -> SenderAct.ROW to event.senderKey
    is IncomingChatEvent.Op -> SenderAct.ROW to event.senderKey
    is IncomingChatEvent.ContentOp -> SenderAct.ROW to event.senderKey
    is IncomingChatEvent.OpenItems -> SenderAct.CONTROL to event.senderKey
    is IncomingChatEvent.Typing -> SenderAct.ROW to event.senderKey
    is IncomingChatEvent.ReadReceipt -> SenderAct.ROW to event.senderKey
    is IncomingChatEvent.CallSignaling -> SenderAct.CALL to (event.senderPersonKey ?: personKeyOfLeaf(event.senderLeaf))
    is IncomingChatEvent.Profile -> SenderAct.CONTROL to event.senderKey
    is IncomingChatEvent.GroupName -> SenderAct.CONTROL to event.senderKey
    is IncomingChatEvent.GroupClosed -> SenderAct.CONTROL to event.senderKey
    is IncomingChatEvent.Pin -> SenderAct.CONTROL to event.senderKey
    is IncomingChatEvent.SharedChannels -> SenderAct.CONTROL to event.senderKey
    is IncomingChatEvent.RegistrarSeed -> SenderAct.CONTROL to event.senderKey
    is IncomingChatEvent.ScopedDiscovery -> SenderAct.CONTROL to event.senderKey
    is IncomingChatEvent.GroupSetting -> SenderAct.CONTROL to event.senderKey
    is IncomingChatEvent.ProfileRequest -> SenderAct.CONTROL to event.senderKey
    is IncomingChatEvent.GroupContactRequest -> SenderAct.CONTROL to event.senderKey
    is IncomingChatEvent.PushRoutingSecret -> SenderAct.CONTROL to personKeyOfLeaf(event.senderLeaf)
    is IncomingChatEvent.CommitterBeacon -> SenderAct.CONTROL to personKeyOfLeaf(event.senderLeaf)
    is IncomingChatEvent.System,
    // ADR-0092 section 3.6 — a ballot names no sender by construction.
    is IncomingChatEvent.PollBallotRow,
    is IncomingChatEvent.ServiceEnvelope,
    is IncomingChatEvent.ExternalJoinRequest,
    is IncomingChatEvent.MembershipProposal,
    -> null
}

/** [events] without what a superseded sender posted ([supersededActDropped]).
 *  The same list when no key is retired: the common case costs nothing. */
internal fun withoutSupersededActs(
    events: List<IncomingChatEvent>,
    supersededKeys: Set<String>,
    unrepairableKeyB64: String?,
    moved: Boolean,
    personKeyOfLeaf: (UInt?) -> MemberIdentity?,
): List<IncomingChatEvent> {
    if (supersededKeys.isEmpty()) return events
    return events.filterNot { event ->
        val (act, sender) = senderActOf(event, personKeyOfLeaf) ?: return@filterNot false
        supersededActDropped(act, sender, supersededKeys, unrepairableKeyB64, moved)
    }
}
