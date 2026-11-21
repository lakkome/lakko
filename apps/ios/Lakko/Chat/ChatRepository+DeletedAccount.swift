// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0098 (`docs/ux/deleted-account.md`) — the chat side of a deleted
// account: posting the statement when this account is deleted, accepting one
// another person's device posted, and, on an owner's or admin's device,
// proposing the removal of that person from the group.

extension ChatRepository {

    // MARK: - Send (the person who deletes)

    /// Post the account-deleted statement in every one-to-one and group this
    /// device holds (never a channel, never the DeviceGroup: neither is in
    /// ``groups``). Called only after `DELETE /account` succeeded and before
    /// the wipe (``AccountDeletionRun``). Best effort: a conversation that
    /// cannot be reached is skipped; the others learn it from the directory.
    /// Throws only when not one conversation took the statement, so the run
    /// retries the whole step.
    ///
    /// Signed by the account key the conversation leaves are bound under
    /// (``conversationAccountIdentity``: after an ADR-0074 rotation, the
    /// conversation key), which is the key every peer resolves this device's
    /// leaves to (C-68).
    public func announceAccountDeleted() async throws {
        guard let seed = conversationAccountIdentity?.ed25519Seed() else {
            throw ChatError.argument("no account identity")
        }
        let targets = groups.keys.sorted().filter { !membershipEnded(membershipStateNow(groupIdHex: $0)) }
        var sent = 0
        var failed = 0
        for groupIdHex in targets {
            guard let record = groups[groupIdHex] else { continue }
            do {
                let envelope = try composeAccountDeletedEnvelope(
                    accountSigningSeed: seed, groupId: record.groupId)
                try await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: envelope)
                sent += 1
            } catch {
                failed += 1
            }
        }
        if failed > 0 {
            NSLog("[account] WARN deletion notice not posted in %d of %d conversations", failed, targets.count)
        }
        if sent == 0 && failed > 0 {
            throw ChatError.argument("the deletion notice reached no conversation")
        }
    }

    // MARK: - Receive

    /// An account-deleted statement arrived in `groupIdHex` from the member
    /// whose account key resolved at receipt is `senderKey` (C-68). Kept only
    /// when its signature verifies under that key over this conversation's
    /// group id; anything else changes nothing. Synchronous.
    func receiveAccountDeleted(groupIdHex: String, envelope: Data, senderKey: MemberIdentity) {
        guard let record = groups[groupIdHex] else { return }
        Self.acceptAccountDeleted(
            envelope: envelope, groupId: record.groupId, groupIdHex: groupIdHex,
            senderKeyB64: senderKey, into: DeletedAccounts.shared)
    }

    /// The receive rule, apart from the live group: `true` (and recorded)
    /// only when the statement verifies under `senderKeyB64` over `groupId`.
    @discardableResult
    static func acceptAccountDeleted(
        envelope: Data, groupId: Data, groupIdHex: String, senderKeyB64: MemberIdentity,
        into deletions: DeletedAccounts
    ) -> Bool {
        guard let key = base64Decode(senderKeyB64),
              verifyAccountDeletedEnvelope(plaintext: envelope, groupId: groupId, senderAccountKey: key)
        else {
            NSLog("[deleted-account] statement refused")
            return false
        }
        deletions.noteStatement(personKeyB64: senderKeyB64, groupIdHex: groupIdHex)
        return true
    }

    /// Wire the deletion facts to the chat layer: a change (a statement
    /// recorded, or a directory answer that changed what a deletion does)
    /// refreshes the background name mirror and proposes the group removals
    /// now owed in EVERY group this device holds, whichever conversation the
    /// statement arrived in (the account is gone, not only its leaf there).
    /// A removal is owed only on a statement AND a directory `404` after it
    /// (``DeletedAccountRules/owesRemoval(_:groupIdHex:)``).
    func observeDeletedAccounts() {
        DeletedAccounts.shared.onDeletionChanged = { [weak self] _ in
            guard let self else { return }
            self.mirrorCallPeerNames()
            for groupIdHex in self.groups.keys {
                self.proposeDeletedMemberRemovalsSoon(groupIdHex: groupIdHex)
            }
        }
    }

    // MARK: - Groups (ADR-0098 section 5)

    /// No second try of one removal within this long (chosen, not measured).
    nonisolated static let deletedRemovalRetryMs: Int64 = 60_000

    /// Whether this device should propose removing `personKey` from a group
    /// whose role table is `roles` and where this device's role is `ownRole`:
    /// only an owner or an admin does, never for the group's owner (C-63),
    /// never in a one-to-one, and only for a person still in the roster.
    nonisolated static func shouldProposeRemoval(
        ownRole: Role?, roles: RoleTable?, personKey: Data, inRoster: Bool, oneToOne: Bool
    ) -> Bool {
        guard !oneToOne, inRoster, let roles, roles.owner != personKey else { return false }
        switch ownRole {
        case .owner?, .admin?: return true
        default: return false
        }
    }

    /// What this device does about one deleted person in one group.
    enum DeletedRemovalPlan: Equatable {
        /// Propose the removal naming this leaf (the remove flow removes every
        /// leaf of the account, ADR-0050).
        case propose(leafIndex: UInt32)
        /// Nothing now: not ours to remove here, or a removal of every one of
        /// their leaves is already pending. Asked again on a later poll.
        case wait
        /// Nothing is ever owed here: the person is not in the roster, is the
        /// owner (C-63), or this is a one-to-one.
        case settled
    }

    /// The plan for one deleted person in one group. Pure.
    nonisolated static func deletedRemovalPlan(
        ownRole: Role?, roles: RoleTable?, personKey: Data, personLeaves: [UInt32],
        pendingRemoveLeaves: Set<UInt32>, oneToOne: Bool
    ) -> DeletedRemovalPlan {
        guard let first = personLeaves.first else { return .settled }
        if oneToOne || roles?.owner == personKey { return .settled }
        guard shouldProposeRemoval(
            ownRole: ownRole, roles: roles, personKey: personKey, inRoster: true, oneToOne: oneToOne)
        else { return .wait }
        if personLeaves.allSatisfy(pendingRemoveLeaves.contains) { return .wait }
        return .propose(leafIndex: first)
    }

    /// Propose, in `groupIdHex`, the removal of every person whose deletion
    /// both a verified statement and a directory `404` confirm, once each,
    /// when this device may. A
    /// removal that could not be sent is tried again on a later poll; one that
    /// went, or one that is never owed here, is recorded.
    func proposeDeletedMemberRemovalsSoon(groupIdHex: String) {
        let owed = DeletedAccounts.shared.removalsOwed(groupIdHex: groupIdHex)
        guard !owed.isEmpty, let record = groups[groupIdHex],
              !membershipEnded(membershipStateNow(groupIdHex: groupIdHex)) else { return }
        let roster = (try? record.group.members()) ?? []
        let roles = groupRoles(groupIdHex: groupIdHex)
        let own = ownRole(groupIdHex: groupIdHex)
        let oneToOne = isOneToOne(groupIdHex: groupIdHex)
        let pendingRemoveLeaves: Set<UInt32> = Set(((try? record.group.pendingProposals()) ?? []).compactMap {
            guard $0.stored, case .remove(let leaf) = $0.kind else { return nil }
            return leaf
        })
        for personKeyB64 in owed {
            guard let personKey = base64Decode(personKeyB64) else { continue }
            let plan = Self.deletedRemovalPlan(
                ownRole: own, roles: roles, personKey: personKey,
                personLeaves: roster.filter { PersonKey.of($0) == personKey }.map(\.leafIndex),
                pendingRemoveLeaves: pendingRemoveLeaves, oneToOne: oneToOne)
            let leaf: UInt32
            switch plan {
            case .settled:
                DeletedAccounts.shared.noteRemovalProposed(personKeyB64: personKeyB64, groupIdHex: groupIdHex)
                continue
            case .wait:
                continue
            case .propose(let leafIndex):
                leaf = leafIndex
            }
            let attemptKey = "\(groupIdHex)|\(personKeyB64)"
            let now = Self.nowMs()
            guard !deletedRemovalsInFlight.contains(attemptKey),
                  now - (deletedRemovalLastTryMs[attemptKey] ?? Int64.min / 2) >= Self.deletedRemovalRetryMs
            else { continue }
            deletedRemovalsInFlight.insert(attemptKey)
            deletedRemovalLastTryMs[attemptKey] = now
            let bound = binding.capture()
            Task { @MainActor [weak self] in
                defer { self?.deletedRemovalsInFlight.remove(attemptKey) }
                guard let self else { return }
                do {
                    _ = try await self.removeMemberFromGroup(groupIdHex: groupIdHex, leafIndex: leaf)
                    guard bound.isCurrent else { return }
                    DeletedAccounts.shared.noteRemovalProposed(
                        personKeyB64: personKeyB64, groupIdHex: groupIdHex)
                } catch {
                    NSLog("[deleted-account] removal not proposed; tried again later")
                }
            }
        }
    }
}
