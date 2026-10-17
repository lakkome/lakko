// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0079 (ordering of 2026-10-17) — the one-to-one a device founds for an
/// invite-link request it won, in three steps the caller records between:
///
///   1. ``prepareLinkOneToOne(recipientAccountId:recipientSignaturePubkey:)``
///      founds it in ONE commit adding the requester's devices AND every
///      other device of this account (ADR-0050 section 2), claims the journal
///      and persists the founder's state; nothing is sent. The caller records
///      the returned founding durably BEFORE step 3;
///   2. ``holdLinkOneToOne(groupIdB64:)`` makes it a held conversation
///      (after a restart, from the MLS snapshot step 1 persisted);
///   3. ``deliverLinkWelcome(_:)`` writes the Welcome to this account's own
///      mailbox FIRST and to the requester's only after that write
///      succeeded, so whenever the requester holds the Welcome every own
///      device's mailbox holds it too.
///
/// A retry after a failed publish or a restart runs 2 and 3 again on the
/// recorded founding (the receivers ignore a duplicate Welcome); nothing is
/// founded twice.
extension ChatRepository {

    func prepareLinkOneToOne(
        recipientAccountId: Data, recipientSignaturePubkey: Data
    ) async throws -> LinkClaims.Founded {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        memberAccountIdHints[recipientSignaturePubkey] = recipientAccountId
        let bound = binding.capture()
        // ADR-0078 — no verified mailbox for the requester: nothing is made.
        guard ReachCenter.shared.writePlan(toAccountKey: recipientSignaturePubkey) != nil else {
            throw ChatError.wrapped("no mailbox known for the recipient", underlying: MailboxUnknown())
        }
        let recipientDevices = try await expectedPersonDevices(
            accountId: recipientAccountId, expectedKey: recipientSignaturePubkey)
        guard !recipientDevices.isEmpty else {
            throw ChatError.argument("recipient has no KeyPackage uploaded yet")
        }
        try bound.require()
        let ownDevices = await ownOtherDevices(identity: identity)
        try bound.require()
        let devices = recipientDevices + ownDevices
        let group: Group
        let welcome: Data
        let joinSigner: DsProofSigner
        do {
            group = try Group.create(identity: identity, kind: .oneToOne)
            // ONE commit for every device (ADR-0050 2.6).
            welcome = try group.addMembersFromWire(
                identity: identity, keyPackages: devices.map { $0.keyPackage }).welcome
            joinSigner = try group.dsAccessSigner(identity: identity)
        } catch {
            throw ChatError.wrapped("addMembersFromWire failed", underlying: error)
        }
        let groupId = try group.groupId()
        do {
            try await JournalOwnership.claim(
                root: try journalRootSigner(groupId: groupId),
                journalId: groupId,
                epoch: joinSigner.epoch(),
                epochKey: joinSigner.verifyingKey(),
                ds: dsClient)
        } catch {
            throw ChatError.wrapped("DS journal claim failed", underlying: error)
        }
        try bound.require()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        if let accountIdB64 = currentAccountIdB64 {
            registrarSeeds.markRootHeld(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        }
        // The founder's state is on disk before the founding can be recorded
        // (and so before any Welcome can leave): the MLS snapshot and the
        // known-groups row, fail closed.
        _ = try persistFoundingOrThrow(groupIdHex: groupIdHex)
        dsSigners.observe(journal: groupIdHex, signer: joinSigner)
        let groupIdB64 = base64Encode(groupId)
        preparedLinkGroups[groupIdB64] = group
        var targets = [LinkClaims.FoundedTarget(
            accountKeyB64: base64Encode(recipientSignaturePubkey), isRecipient: true)]
        if !ownDevices.isEmpty, let own = ReachCenter.shared.ownAccountKey,
           !ReachCenter.shared.keyResolver.samePerson(own, recipientSignaturePubkey) {
            targets.append(LinkClaims.FoundedTarget(accountKeyB64: base64Encode(own), isRecipient: false))
        }
        return LinkClaims.Founded(
            groupIdB64: groupIdB64,
            welcomeB64: base64Encode(welcome),
            targets: LinkClaims.welcomeOrder(targets, isRecipient: \.isRecipient),
            delivered: false)
    }

    /// Make the founded one-to-one a held conversation. Returns its hex id,
    /// or `nil` when it cannot be brought up (the next pass tries again).
    func holdLinkOneToOne(groupIdB64: String) -> String? {
        guard let identity = canonicalIdentity, let groupId = base64Decode(groupIdB64) else { return nil }
        let groupIdHex = DsClient.pseudonymHex(groupId)
        if groups[groupIdHex] != nil {
            preparedLinkGroups[groupIdB64] = nil
            return groupIdHex
        }
        let group: Group
        if let live = preparedLinkGroups[groupIdB64] {
            group = live
        } else if let loaded = try? loadGroup(identity: identity, groupId: groupId) {
            // A restart between the founding and the delivery.
            group = loaded
        } else {
            return nil
        }
        preparedLinkGroups[groupIdB64] = nil
        groups[groupIdHex] = GroupRecord(group: group, groupId: groupId)
        mirrorLineage()
        claimedGroupsThisSession.insert(groupIdHex)
        persistKnownGroupBestEffort(groupIdHex: groupIdHex)
        persistSnapshotBestEffort()
        state = state.with(threads: buildThreadList())
        subscribeGroupUnderPushGid(groupIdHex: groupIdHex)
        reconcileJournalRegistrarsSoon(groupIdHex: groupIdHex)
        publishOwnProfileSoon(groupIdHex: groupIdHex)
        requestProfilesAfterJoin(groupIdHex: groupIdHex)
        return groupIdHex
    }

    /// Write the founded one-to-one's Welcome, in the recorded order (own
    /// mailbox first). A failure stops the loop: the requester is never
    /// written after a failed own write.
    func deliverLinkWelcome(_ founded: LinkClaims.Founded) async throws {
        guard let welcome = base64Decode(founded.welcomeB64) else {
            throw ChatError.argument("the founded Welcome is not held")
        }
        for target in founded.targets {
            guard let key = base64Decode(target.accountKeyB64) else { continue }
            try await deliverWelcome(
                welcome: welcome, toAccountKey: key,
                plan: ReachCenter.shared.writePlan(toAccountKey: key))
        }
    }

    /// Every held conversation with its kind and member account keys (hex):
    /// the input of ``LinkClaims/holdsOneToOneWith(_:requesterKeyHex:)``.
    func heldConversationsByAccount() -> [LinkClaims.HeldConversation] {
        groups.values.map { record in
            let oneToOne = (try? record.group.groupKind()) == .oneToOne
            let members = (try? record.group.members()) ?? []
            return LinkClaims.HeldConversation(
                oneToOne: oneToOne,
                memberAccountKeysHex: members.map {
                    LinkClaims.hex($0.accountKey.count == 32 ? $0.accountKey : $0.signaturePublicKey)
                })
        }
    }

    /// The hex id of a held one-to-one with the account whose key is
    /// `accountKey`, if any.
    func heldOneToOneHex(withAccountKey accountKey: Data) -> String? {
        let want = LinkClaims.hex(accountKey)
        return groups.first { _, record in
            LinkClaims.holdsOneToOneWith(
                [LinkClaims.HeldConversation(
                    oneToOne: (try? record.group.groupKind()) == .oneToOne,
                    memberAccountKeysHex: ((try? record.group.members()) ?? []).map {
                        LinkClaims.hex($0.accountKey.count == 32 ? $0.accountKey : $0.signaturePublicKey)
                    })],
                requesterKeyHex: want)
        }?.key
    }
}
