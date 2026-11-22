// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0073 "Phase 2 revised", R.5 - one conversation over several groups, as
// the repository answers it. `Lineage.swift` is the pure model; this binds it
// to the groups this device holds, the way `use-lineage.ts` does on web.
//
// Person-facing stores (drafts, pin / mute / archive, the read position,
// folder refs, the route) take the CONVERSATION id; protocol stores take the
// group id. A caller that needs both asks here.

extension ChatRepository {
    /// The lineage registers changed (a fold on this device, or one another
    /// device of the account sent): rebuild the conversation list.
    func applyLineage(_ entries: [LineageEntry]) {
        let next = ConversationLineage.build(entries)
        guard next != conversationLineage else { return }
        conversationLineage = next
        state = state.with(threads: buildThreadList())
        lineageDidChange()
        foldDuplicateOneToOnes()
    }

    /// One person, one one-to-one row (``DuplicateOneToOne``): a one-to-one
    /// the other person founded continues an older one this account founded
    /// with them, which their device never opened. Recorded in the lineage,
    /// so the list shows one row and the old journal is not polled. Re-run
    /// whenever the held groups or the lineage move; writing the same
    /// register again changes nothing.
    func foldDuplicateOneToOnes() {
        guard let account = currentAccountIdB64, lineageReady() else { return }
        let hexes = groups.keys.sorted()
        // Base scenario 9 — when this device first held each group.
        let since = ConversationHeldSince.record(
            accountIdB64: account, groupHexes: hexes,
            nowMs: Int64(Date().timeIntervalSince1970 * 1000))
        let held: [DuplicateOneToOne.Held] = hexes.map { hex in
            DuplicateOneToOne.Held(
                groupHex: hex,
                conversationHex: conversationLineage.conversationOf(hex),
                isOneToOne: isOneToOne(groupIdHex: hex),
                people: (members(groupIdHex: hex) ?? []).map { base64Encode($0.signaturePublicKey) },
                founderKeyB64: founderKeyB64(groupIdHex: hex),
                heldSinceMs: since[hex])
        }
        let own = conversationAccountIdentity.map { base64Encode($0.signaturePublicKey()) }
        for fold in DuplicateOneToOne.folds(held: held, ownKeyB64: own) {
            guard let successor = Self.hexToBytes(fold.successorHex),
                  let root = Self.hexToBytes(fold.rootHex) else { continue }
            let writer = lineageWriter
            Task { @MainActor in _ = await writer(successor, root) }
        }
    }

    /// Who founded a group: the role table's owner, else the person at leaf
    /// 0; `nil` when neither can be read.
    func founderKeyB64(groupIdHex: String) -> String? {
        guard let record = groups[groupIdHex] else { return nil }
        if let owner = (try? record.group.groupRoles())??.owner, owner.count == 32 {
            return base64Encode(owner)
        }
        guard let roster = try? record.group.members(),
              let first = roster.first(where: { $0.leafIndex == 0 }) else { return nil }
        return base64Encode(PersonKey.of(first))
    }

    /// The per-account continuity bookkeeping (moved-at stamps, re-send and
    /// push-retired marks).
    var continuityStore: ContinuityStore { ContinuityStore(ownerAccountIdB64: currentAccountIdB64) }

    /// Standard base64 of a held group's id, from its hex.
    func groupIdB64(ofHex groupIdHex: String) -> String? {
        groups[groupIdHex].map { base64Encode($0.groupId) }
    }

    /// The hex a group id (standard base64) is keyed under.
    func groupIdHex(ofB64 groupIdB64: String) -> String? {
        base64Decode(groupIdB64).map { DsClient.pseudonymHex($0) }
    }

    /// One plan per conversation this device holds.
    func conversationPlans() -> [LineageRules.ConversationPlan] {
        let held = groups.keys.sorted().map { hex in
            LineageRules.HeldGroup(
                groupHex: hex,
                active: !membershipEnded(membershipStateNow(groupIdHex: hex)))
        }
        return LineageRules.planConversations(held, lineage: conversationLineage)
    }

    /// The plan of the conversation a group id names: the id of the
    /// conversation itself or of ANY of its groups (a route minted before the
    /// move still opens the conversation). `nil` when this device holds none
    /// of it.
    func conversationPlan(ofGroup groupIdHex: String) -> LineageRules.ConversationPlan? {
        let conversation = conversationLineage.conversationOf(groupIdHex)
        return conversationPlans().first { $0.conversationHex == conversation }
    }

    /// The conversation id of a group, hex: the key of every person-facing
    /// store.
    public func conversationIdHex(ofGroup groupIdHex: String) -> String {
        conversationLineage.conversationOf(groupIdHex)
    }

    /// The group a conversation currently is on this device (R.5 "the
    /// head"), named by the conversation id or by any of its groups. Falls
    /// back to the id itself when this device holds none of the conversation.
    public func headGroupIdHex(of idHex: String) -> String {
        conversationPlan(ofGroup: idHex)?.headHex ?? idHex
    }

    /// The index of a group in its conversation's segments.
    public func segmentIndex(ofGroup groupIdHex: String) -> Int {
        conversationLineage.segmentIndexOf(groupIdHex)
    }

    /// Whether `groupIdHex` is an OLDER segment of its conversation on this
    /// device (not the head).
    func isOlderSegment(groupIdHex: String) -> Bool {
        guard let plan = conversationPlan(ofGroup: groupIdHex) else { return false }
        guard let at = plan.heldSegments.firstIndex(of: groupIdHex),
              let headAt = plan.heldSegments.firstIndex(of: plan.headHex) else { return false }
        return at < headAt
    }

    /// R.5 "the old segment after the move" - whether a group's journal is
    /// polled now (``LineageRules/oldSegmentPolled(isHead:oneToOne:movedAtMs:nowMs:)``).
    func journalPolled(groupIdHex: String) -> Bool {
        guard let plan = conversationPlan(ofGroup: groupIdHex) else { return true }
        let isHead = plan.headHex == groupIdHex
        if isHead { return true }
        // A successor this device holds beyond its head is not an old
        // segment: it is polled like any group.
        guard isOlderSegment(groupIdHex: groupIdHex) else { return true }
        return LineageRules.oldSegmentPolled(
            isHead: false,
            oneToOne: isOneToOne(groupIdHex: groupIdHex),
            movedAtMs: groupIdB64(ofHex: groupIdHex).flatMap {
                continuityStore.conversationMovedAt(oldGroupIdB64: $0)
            },
            nowMs: Self.nowMs())
    }

    /// R.5 "push routing ... the old group's subscription is retired": once
    /// this device is in the group that continues a conversation, its push
    /// subscription for the OLDER group is retired. Once per old group;
    /// best-effort like every push teardown.
    func retireMovedPush(groupIdHex: String) async {
        guard currentAccountIdB64 != nil, let oldB64 = groupIdB64(ofHex: groupIdHex) else { return }
        let plan = conversationPlan(ofGroup: groupIdHex)
        let headHex = plan?.headHex
        guard LineageRules.retiresOldSegmentPush(
            groupHex: groupIdHex,
            headHex: headHex,
            headActive: headHex.map { !membershipEnded(membershipStateNow(groupIdHex: $0)) } ?? false,
            alreadyRetired: continuityStore.wasPushRetired(oldGroupIdB64: oldB64))
        else { return }
        guard isOlderSegment(groupIdHex: groupIdHex) else { return }
        if await retireOwnPushSubscription(groupIdHex: groupIdHex) {
            continuityStore.markPushRetired(oldGroupIdB64: oldB64)
        }
    }

    // MARK: - Pins across segments

    /// The older segments of the conversation `headGroupIdHex` is the head
    /// of, oldest first, as this device holds them.
    func olderSegments(headGroupIdHex: String) -> [String] {
        guard let plan = conversationPlan(ofGroup: headGroupIdHex),
              let headAt = plan.heldSegments.firstIndex(of: plan.headHex) else { return [] }
        return Array(plan.heldSegments.prefix(headAt))
    }

    /// The pins of every segment of a conversation this device holds, each
    /// under the id its row is drawn under
    /// (``LineageViews/pinsAcrossSegments(_:rowId:)``).
    func pinsAcrossSegments(headGroupIdHex: String) -> LineageViews.SpannedPins<StoredSharedPin> {
        let older = olderSegments(headGroupIdHex: headGroupIdHex)
        var segments: [LineageViews.SegmentPins<StoredSharedPin>] = []
        for (index, hex) in older.enumerated() {
            segments.append(LineageViews.SegmentPins(
                distanceFromHead: older.count - index,
                shared: sharedPins(groupIdHex: hex),
                personal: personalPins(groupIdHex: hex)))
        }
        segments.append(LineageViews.SegmentPins(
            distanceFromHead: 0,
            shared: sharedPins(groupIdHex: headGroupIdHex),
            personal: personalPins(groupIdHex: headGroupIdHex)))
        return LineageViews.pinsAcrossSegments(segments)
    }

    /// A shared pin of an OLDER segment, cleared on this device alone: an
    /// older segment is read-only on the wire, so nothing is published. The
    /// unpin is STORED with a fresh stamp, so a late re-send of the old pin
    /// cannot bring it back.
    func clearSharedPinLocally(groupIdHex: String, messageId: Int64) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        sharedPinStore.record(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, messageId: messageId,
            value: StoredSharedPin(pinned: false, updatedAtMs: UInt64(max(0, Self.nowMs()))))
        notePinsChanged()
    }

    /// The lineage changed: a conversation may have a new current group, so
    /// what was held for it can go out and its old push subscription can be
    /// retired.
    func lineageDidChange() {
        mirrorLineage()
        continueAllSoon(force: true)
    }

    /// Mirror `successor group -> conversation` into the App Group for the
    /// notification extension (`ConversationLineageMirror`): the groups this
    /// device holds that continue another conversation.
    func mirrorLineage() {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        var map: [String: String] = [:]
        for hex in groups.keys {
            let conversation = conversationLineage.conversationOf(hex)
            if conversation != hex { map[hex] = conversation }
        }
        ConversationLineageMirror.replaceAll(accountIdB64: accountIdB64, conversationOfSuccessor: map)
        mirrorContinuityFacts()
    }

    /// Mirror what the notification extension needs to stay silent where
    /// this app is (`ContinuityMirror`): the keys accepted resets retired,
    /// this account's own person key, and the segment index of each held
    /// group that continues a conversation. Written at bootstrap, when the
    /// lineage changes and when a reset is accepted.
    func mirrorContinuityFacts() {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        var indexes: [String: Int] = [:]
        var own: String?
        for (hex, record) in groups {
            let index = conversationLineage.segmentIndexOf(hex)
            if index > 0 { indexes[hex.lowercased()] = index }
            if own == nil { own = AuthorIdentity.own(group: record.group) }
        }
        ContinuityMirror.replace(
            accountIdB64: accountIdB64,
            row: ContinuityMirror.Row(
                supersededKeysB64: Array(supersededKeyMap().keys),
                ownPersonKeyB64: own,
                segmentIndexOfGroupHex: indexes))
    }
}
