// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073 "Phase 2 revised", R.6 - founding a successor by CLAIM, THEN
// BUILD, and the receive rule for the claim, as pure decisions.
//
// iOS mirror of `apps/web/src/chat/successor-claim.ts`; the tables are the
// same and so are the cases in `SuccessorClaimTests`.
//
// A one-to-one whose other person reset their keys, and a group whose owner
// did, cannot be repaired in place: the conversation continues in a NEW
// group behind the same row. Group ids are random, so two devices acting at
// once would found two successors; the claim makes it one:
//
//   1. an eligible device creates the successor locally and claims its DS
//      journal; nothing is sent to anyone;
//   2. it posts a `group_closed` envelope naming the successor into the OLD
//      group and notes the DS message id;
//   3. it reads the old journal up to that id: an EARLIER eligible claim
//      means it lost (discard, adopt the earlier one); otherwise it won;
//   4. only the winner builds (adds, Welcomes, name, roles).
//
// The envelope codec is unchanged (`lakko/group-closed/v1`); its only
// producer is now a claim. Journal order is the DS's arrival order, which
// every member reads identically; the caller holds the per-group lock over
// the read-decide-write (C-62 sub-invariant 5).
//
// Free of the core, of SwiftUI and of storage.

/// The kind of a conversation as the continuity rules read it. Its own tiny
/// type so these files depend on neither of the app's `ConversationKind`s.
public enum ContinuityKind: String, Codable, Equatable, Sendable {
    /// A one-to-one.
    case chat
    /// A group.
    case group
}

/// One claim as read off the old group's journal.
public struct SuccessorClaim: Codable, Equatable, Sendable {
    /// The DS message id of the row in the OLD group's journal: the order.
    public let messageId: Int64
    /// base64 of the successor group's id.
    public let successorGroupIdB64: String
    /// base64 of the sender's ACCOUNT key, resolved at receipt from the
    /// MLS-authenticated sender (C-68); never read from the envelope.
    public let senderKeyB64: String
    /// Whether the sender was an admin in the live role table at receipt.
    public let senderWasAdmin: Bool
    /// The admin keys of the live role table at receipt (base64).
    public let adminKeysAtReceiptB64: [String]

    public init(
        messageId: Int64, successorGroupIdB64: String, senderKeyB64: String,
        senderWasAdmin: Bool, adminKeysAtReceiptB64: [String]
    ) {
        self.messageId = messageId
        self.successorGroupIdB64 = successorGroupIdB64
        self.senderKeyB64 = senderKeyB64
        self.senderWasAdmin = senderWasAdmin
        self.adminKeysAtReceiptB64 = adminKeysAtReceiptB64
    }
}

/// The pure rules of claim-then-build.
public enum SuccessorClaimRules {

    /// The account whose accepted reset makes this group unrepairable (R.6
    /// (i)): the other participant of a one-to-one, or the owner of a group.
    /// `nil` when the group is repairable or nothing was accepted for it.
    ///
    /// | kind  | superseded key is ...                 | answer   |
    /// |-------|---------------------------------------|----------|
    /// | chat  | a person of the roster (not this one) | that key |
    /// | group | the role table's owner                | that key |
    /// | any   | anything else                         | `nil`    |
    ///
    /// `ownerKeyB64` is `nil` when the group has no role table.
    public static func unrepairableBy(
        kind: ContinuityKind, rosterPersonKeysB64: [String], ownPersonKeyB64: String?,
        ownerKeyB64: String?, supersededKeys: Set<String>
    ) -> String? {
        if kind == .chat {
            for key in rosterPersonKeysB64 where key != ownPersonKeyB64 && supersededKeys.contains(key) {
                return key
            }
            return nil
        }
        guard let ownerKeyB64 else { return nil }
        return supersededKeys.contains(ownerKeyB64) ? ownerKeyB64 : nil
    }

    /// R.6 "Eligible": who may found the successor, and whose claim counts.
    ///
    /// | kind  | sender                                                 | eligible |
    /// |-------|--------------------------------------------------------|----------|
    /// | any   | a superseded key                                       | no       |
    /// | chat  | the surviving account (any of its devices)             | yes      |
    /// | group | an admin other than the superseded owner               | yes      |
    /// | group | a member, when the table names no admin but that owner | yes      |
    /// | group | a member, when another admin exists                    | no       |
    ///
    /// `supersededKeyB64` is the key whose reset made the group unrepairable.
    /// `survivingKeyB64` is, for a one-to-one, the account that did not
    /// reset: the roster's other person as the judging device sees it.
    public static func claimEligible(
        kind: ContinuityKind, senderKeyB64: String, senderWasAdmin: Bool, adminKeysB64: [String],
        supersededKeyB64: String, supersededKeys: Set<String>, survivingKeyB64: String?
    ) -> Bool {
        if senderKeyB64 == supersededKeyB64 { return false }
        if supersededKeys.contains(senderKeyB64) { return false }
        if kind == .chat { return senderKeyB64 == survivingKeyB64 }
        let otherAdmins = adminKeysB64.filter { $0 != supersededKeyB64 && !supersededKeys.contains($0) }
        if !otherAdmins.isEmpty { return senderWasAdmin && otherAdmins.contains(senderKeyB64) }
        return true
    }

    /// What the receive rule does with an incoming claim.
    public enum ClaimVerdict: Equatable {
        /// The first eligible claim in the journal's order: the
        /// conversation's successor is the one it names.
        case accept
        /// A claim is already held: later claims are ignored for good.
        case ignoreLater
        /// The receiver has no accepted reset for this group YET: keep the
        /// claim and judge it again after the identity check the claim
        /// itself triggers.
        case keepPending
        /// The sender is not eligible (a plain member where an admin exists,
        /// the superseded identity's own leaves): dropped.
        case dropIneligible
        /// Names no successor, or names this very group: not a claim.
        case dropMalformed
    }

    /// The receive rule (R.6), replacing "owner or admin, strictly newer
    /// stamp wins":
    ///
    ///   (i)   the receiver has itself accepted a reset of the account that
    ///         makes this group unrepairable (`unrepairableBy`);
    ///   (ii)  the sender, resolved at receipt, is eligible (`claimEligible`);
    ///   (iii) it is the FIRST such claim in the journal's order.
    ///
    /// `held` is the claim already accepted for this group, if any. A claim
    /// with a LOWER message id than the held one (the receiver judged a
    /// pending claim late, or read the journal out of order) replaces it
    /// only through `firstEligibleClaim`, which the caller runs over
    /// everything it kept. `unrepairableKeyB64` is `unrepairableBy` at the
    /// moment of judging.
    public static func claimVerdict(
        kind: ContinuityKind, groupIdB64: String, claim: SuccessorClaim, held: SuccessorClaim?,
        unrepairableKeyB64: String?, supersededKeys: Set<String>, survivingKeyB64: String?
    ) -> ClaimVerdict {
        if claim.successorGroupIdB64.isEmpty || claim.successorGroupIdB64 == groupIdB64 {
            return .dropMalformed
        }
        // A superseded sender is never eligible, verdict or not: a removed
        // device that still holds the old keys can post here, and its claim
        // must not even wait in the pending list.
        if supersededKeys.contains(claim.senderKeyB64) { return .dropIneligible }
        guard let unrepairableKeyB64 else { return .keepPending }
        let eligible = claimEligible(
            kind: kind, senderKeyB64: claim.senderKeyB64, senderWasAdmin: claim.senderWasAdmin,
            adminKeysB64: claim.adminKeysAtReceiptB64, supersededKeyB64: unrepairableKeyB64,
            supersededKeys: supersededKeys, survivingKeyB64: survivingKeyB64)
        if !eligible { return .dropIneligible }
        if let held, held.messageId <= claim.messageId { return .ignoreLater }
        return .accept
    }

    /// The first eligible claim among `claims` in the journal's order, or
    /// `nil`. Used to re-judge the claims kept while the receiver had no
    /// verdict, and by a founder reading the old journal up to its own claim
    /// (step 3).
    public static func firstEligibleClaim(
        kind: ContinuityKind, groupIdB64: String, claims: [SuccessorClaim],
        unrepairableKeyB64: String?, supersededKeys: Set<String>, survivingKeyB64: String?
    ) -> SuccessorClaim? {
        for claim in inJournalOrder(claims) {
            let verdict = claimVerdict(
                kind: kind, groupIdB64: groupIdB64, claim: claim, held: nil,
                unrepairableKeyB64: unrepairableKeyB64, supersededKeys: supersededKeys,
                survivingKeyB64: survivingKeyB64)
            if verdict == .accept { return claim }
        }
        return nil
    }

    /// `claims` by ascending message id, equal ids keeping their given order
    /// (the stable sort the web twin gets from the language).
    static func inJournalOrder(_ claims: [SuccessorClaim]) -> [SuccessorClaim] {
        claims.enumerated()
            .sorted { a, b in
                a.element.messageId != b.element.messageId
                    ? a.element.messageId < b.element.messageId
                    : a.offset < b.offset
            }
            .map(\.element)
    }

    /// Step 3: how a founder's own claim ended.
    public enum ClaimOutcome: Equatable {
        /// No earlier eligible claim: this device builds the successor.
        case won
        /// An earlier eligible claim is in the journal: discard the local
        /// successor (never announced, no Welcome sent) and adopt that one.
        case lost(winner: SuccessorClaim)
    }

    /// Decide step 3 from the old journal's eligible claims up to and
    /// including the founder's own row.
    public static func claimOutcome(ownClaimMessageId: Int64, first: SuccessorClaim?) -> ClaimOutcome {
        guard let first, first.messageId < ownClaimMessageId else { return .won }
        return .lost(winner: first)
    }

    /// Whether THIS device founds a successor now (steps 1 and 2).
    ///
    /// | condition                                                     | founds |
    /// |---------------------------------------------------------------|--------|
    /// | the group is not unrepairable for an accepted reset           | no     |
    /// | an eligible claim is already held (stand down, adopt it)      | no     |
    /// | this device's own claim is in flight or already posted        | no     |
    /// | this device's membership ended                                | no     |
    /// | this device's account is not eligible                         | no     |
    /// | the new identity publishes no device yet (R.8, no KeyPackage) | no     |
    /// | otherwise                                                     | yes    |
    ///
    /// The last row is the R.8 rule: a founder that finds no published
    /// device for the accepted key does nothing and retries at the next
    /// trigger.
    public static func shouldFoundSuccessor(
        kind: ContinuityKind, unrepairableKeyB64: String?, held: SuccessorClaim?,
        ownClaimPosted: Bool, membershipEnded: Bool, ownPersonKeyB64: String?, ownIsAdmin: Bool,
        adminKeysB64: [String], supersededKeys: Set<String>, newIdentityHasDevices: Bool
    ) -> Bool {
        guard let unrepairableKeyB64 else { return false }
        if held != nil || ownClaimPosted || membershipEnded { return false }
        guard let ownPersonKeyB64 else { return false }
        if !newIdentityHasDevices { return false }
        return claimEligible(
            kind: kind, senderKeyB64: ownPersonKeyB64, senderWasAdmin: ownIsAdmin,
            adminKeysB64: adminKeysB64, supersededKeyB64: unrepairableKeyB64,
            supersededKeys: supersededKeys, survivingKeyB64: ownPersonKeyB64)
    }
}
