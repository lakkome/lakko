// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073 "Phase 2 revised", R.6 and R.7 cases 1 and 3 - the ORDER in
// which a device continues a conversation that cannot be repaired in place,
// as one function over an injected port.
//
// iOS mirror of `apps/web/src/chat/continuity-runtime.ts`, statement for
// statement; the cases of `ContinuityRuntimeTests` are the web ones.
//
// `SuccessorClaimRules` holds the rules (who is eligible, which claim wins)
// and `ResetRepair` the plans (who is added, which roles are restored).
// This file is the sequence between them, and the sequence is the point:
//
//   1. create the successor locally and claim its DS journal; nothing is
//      sent to anyone;
//   2. post the claim into the OLD group and note its DS message id;
//   3. wait until the old journal has been read up to that id; an EARLIER
//      eligible claim means this device lost: discard, adopt the earlier one;
//   4. only the winner builds: it delivers the Welcomes it prepared, records
//      the lineage, carries the name, adds the remaining people, restores
//      the roles and (decision) the owner, and re-sends what was sent into
//      the dead session (decision).
//
// "Nothing is sent before the claim is confirmed" is what keeps a lost
// claim harmless; a lost claim AFTER Welcomes went out would leave people in
// a group nobody continues in (R.14), and this order is the only thing that
// prevents it. So it lives here, behind a port a test can drive.
//
// Every step is idempotent and the pass is re-entered from the top at each
// trigger (a poll tail, an accepted reset, a start): a crash at any point
// resumes from the claim state the store holds.

/// What the pass reads off one group, live.
public struct ContinuityFacts: Equatable, Sendable {
    public let kind: ContinuityKind
    /// The PERSON keys of the roster, base64.
    public let rosterPersonKeysB64: [String]
    /// `nil` = no role table.
    public let ownerKeyB64: String?
    public let adminKeysB64: [String]
    /// This device's membership is live.
    public let active: Bool

    public init(
        kind: ContinuityKind, rosterPersonKeysB64: [String], ownerKeyB64: String?,
        adminKeysB64: [String], active: Bool
    ) {
        self.kind = kind
        self.rosterPersonKeysB64 = rosterPersonKeysB64
        self.ownerKeyB64 = ownerKeyB64
        self.adminKeysB64 = adminKeysB64
        self.active = active
    }
}

/// Everything the pass does to the world.
@MainActor
public protocol ContinuityPort: AnyObject {
    func ownPersonKeyB64() -> String?
    /// Every key an accepted reset retired -> its account id (base64).
    func supersededKeys() -> [String: String]
    /// The key this device now trusts for an account, or `nil`.
    func currentKeyOf(accountIdB64: String) -> String?
    /// `nil` when the group is not held or cannot be read.
    func facts(groupIdB64: String) -> ContinuityFacts?
    func claimState(oldGroupIdB64: String) -> StoredGroupClosed
    /// How far this device has read the old group's journal.
    func cursorOf(oldGroupIdB64: String) -> Int64?
    /// Whether the new identity publishes at least one device (R.8).
    func newIdentityHasDevices(accountIdB64: String) async throws -> Bool
    /// Step 1: create the successor of `oldGroupIdB64` locally, add the new
    /// identity's devices and this account's other devices in ONE commit,
    /// claim its DS journal, and keep the Welcome for step 4. Sends nothing.
    /// Returns the successor's id.
    func prepareSuccessor(oldGroupIdB64: String, newIdentityAccountIdB64: String) async throws -> String
    /// Step 2: post the claim; returns the DS message id. The own-claim
    /// record is written before the publish.
    func postClaim(oldGroupIdB64: String, successorGroupIdB64: String) async throws -> Int64
    /// Forget an own claim whose publish never produced a row.
    func abandonOwnClaim(oldGroupIdB64: String) async throws
    /// A lost claim: drop the local successor (never announced).
    func discardSuccessor(successorGroupIdB64: String) async throws
    /// Step 3 decided: record the conversation's successor, clear the own
    /// claim.
    func settle(oldGroupIdB64: String, winner: SuccessorClaim) async throws
    /// The lineage register `successor -> conversation of the old group`.
    /// Idempotent (write-once).
    func recordLineage(oldGroupIdB64: String, successorGroupIdB64: String) async throws
    /// Step 4a: make the prepared successor a held conversation and deliver
    /// the Welcomes. Idempotent: `false` when the successor could not be
    /// brought up (the next pass tries again).
    func deliverSuccessor(oldGroupIdB64: String, successorGroupIdB64: String) async throws -> Bool
    /// Step 4b: re-send the group name into the successor with its ORIGINAL
    /// stamp (ADR-0051). Idempotent.
    func carryName(oldGroupIdB64: String, successorGroupIdB64: String) async throws
    /// Step 4c: add one person of the old roster (by the key the old roster
    /// names) to the successor. `false` when they could not be resolved or
    /// reached; the completeness re-check retries.
    func addPerson(successorGroupIdB64: String, personKeyB64: String) async throws -> Bool
    /// Whether the completeness re-check of `successorGroupIdB64` is due
    /// (the re-admission arm's interval, per successor); stamps it when so.
    func recheckDue(successorGroupIdB64: String) -> Bool
    /// Step 4d: set the successor's admin set (one role change).
    func setAdmins(successorGroupIdB64: String, adminKeysB64: [String]) async throws
    /// Step 4e: propose the ownership transfer to the new identity, the
    /// founder staying an admin when `stayAdmin`.
    func transferOwnership(successorGroupIdB64: String, newOwnerKeyB64: String, stayAdmin: Bool) async throws
    /// R.7 case 1 / D5: send again, into the successor, this account's
    /// messages sent in the old one-to-one after `signedAtMs`. Idempotent
    /// (records that it ran).
    func resendOwnMessages(oldGroupIdB64: String, successorGroupIdB64: String, accountIdB64: String) async throws
}

/// What one pass did, for the caller's log and the tests.
public enum ContinuityStep: Equatable, Sendable {
    /// Nothing to continue (repairable, no accepted reset, not held).
    case nothing
    /// Another device's claim is the conversation's successor: adopted.
    case adopted
    /// The new identity publishes no device yet (R.8): retry later.
    case waitingForDevices
    /// This device is not eligible to found.
    case notEligible
    /// Steps 1 and 2 ran: the claim is posted.
    case claimed
    /// The old journal has not been read up to the own claim yet.
    case waitingForJournal
    /// An earlier eligible claim won: the local successor was discarded.
    case lost
    /// This device won and built (or continued building) the successor.
    case built
    /// A moved admin or owner that did not found the successor added people
    /// the successor still lacked.
    case rechecked
    /// A step failed; the next pass resumes.
    case failed
}

@MainActor
public enum ContinuityRuntime {

    /// One pass over one OLD group. Never throws.
    public static func continueConversation(port: ContinuityPort, oldGroupIdB64: String) async -> ContinuityStep {
        do {
            return try await run(port, oldGroupIdB64)
        } catch {
            NSLog("[continuity] pass failed; resumed at the next trigger: %@", String(describing: error))
            return .failed
        }
    }

    private static func run(_ port: ContinuityPort, _ oldGroupIdB64: String) async throws -> ContinuityStep {
        guard let facts = port.facts(groupIdB64: oldGroupIdB64) else { return .nothing }
        let own = port.ownPersonKeyB64()
        let superseded = port.supersededKeys()
        if superseded.isEmpty { return .nothing }
        let supersededSet = Set(superseded.keys)
        guard let unrepairableKeyB64 = SuccessorClaimRules.unrepairableBy(
            kind: facts.kind, rosterPersonKeysB64: facts.rosterPersonKeysB64, ownPersonKeyB64: own,
            ownerKeyB64: facts.ownerKeyB64, supersededKeys: supersededSet)
        else { return .nothing }
        guard let accountIdB64 = superseded[unrepairableKeyB64] else { return .nothing }
        let state = port.claimState(oldGroupIdB64: oldGroupIdB64)

        // An own claim is undecided: step 3.
        if let ownClaim = state.own {
            guard let ownClaimMessageId = ownClaim.messageId else {
                // The publish never reported an id (a crash or a failure
                // mid-send). Posting again is harmless: both rows name the
                // same successor.
                _ = try await port.postClaim(
                    oldGroupIdB64: oldGroupIdB64, successorGroupIdB64: ownClaim.successorGroupIdB64)
                return .claimed
            }
            guard let cursor = port.cursorOf(oldGroupIdB64: oldGroupIdB64), cursor >= ownClaimMessageId else {
                return .waitingForJournal
            }
            let outcome = SuccessorClaimRules.claimOutcome(
                ownClaimMessageId: ownClaimMessageId, first: state.accepted)
            if case .lost(let earlier) = outcome {
                try await port.discardSuccessor(successorGroupIdB64: ownClaim.successorGroupIdB64)
                try await port.settle(oldGroupIdB64: oldGroupIdB64, winner: earlier)
                try await port.recordLineage(
                    oldGroupIdB64: oldGroupIdB64, successorGroupIdB64: earlier.successorGroupIdB64)
                return .lost
            }
            guard let own else { return .failed }
            let winner = SuccessorClaim(
                messageId: ownClaimMessageId,
                successorGroupIdB64: ownClaim.successorGroupIdB64,
                senderKeyB64: own,
                senderWasAdmin: facts.adminKeysB64.contains(own),
                adminKeysAtReceiptB64: facts.adminKeysB64)
            try await port.settle(oldGroupIdB64: oldGroupIdB64, winner: winner)
            return try await build(
                port, oldGroupIdB64: oldGroupIdB64, winner: winner, old: facts, own: own,
                accountIdB64: accountIdB64, unrepairableKeyB64: unrepairableKeyB64)
        }

        // A claim is held: the conversation's successor is decided.
        if let accepted = state.accepted {
            try await port.recordLineage(
                oldGroupIdB64: oldGroupIdB64, successorGroupIdB64: accepted.successorGroupIdB64)
            if let own, accepted.senderKeyB64 == own {
                // This account founded it (this device, or another of the
                // person's): whoever holds the successor keeps building it.
                return try await build(
                    port, oldGroupIdB64: oldGroupIdB64, winner: accepted, old: facts, own: own,
                    accountIdB64: accountIdB64, unrepairableKeyB64: unrepairableKeyB64)
            }
            // R.7 case 3 "the completeness re-check": ANY moved admin or
            // owner, not only the founding account, adds whoever of the old
            // roster the successor still lacks, at the re-admission arm's
            // cadence.
            if let own, facts.kind == .group {
                return try await recheckCompleteness(
                    port, successorGroupIdB64: accepted.successorGroupIdB64, old: facts, own: own)
            }
            return .adopted
        }

        // Steps 1 and 2.
        guard facts.active, let own else { return .notEligible }
        // Eligibility first (no network), then the one directory read (R.8).
        let eligible = SuccessorClaimRules.shouldFoundSuccessor(
            kind: facts.kind, unrepairableKeyB64: unrepairableKeyB64, held: state.accepted,
            ownClaimPosted: false, membershipEnded: !facts.active, ownPersonKeyB64: own,
            ownIsAdmin: facts.adminKeysB64.contains(own), adminKeysB64: facts.adminKeysB64,
            supersededKeys: supersededSet, newIdentityHasDevices: true)
        if !eligible { return .notEligible }
        if !(try await port.newIdentityHasDevices(accountIdB64: accountIdB64)) { return .waitingForDevices }
        // The claim state is read again after the awaits above: a claim that
        // landed meanwhile means this device stands down (R.6 "to avoid
        // wasted claims a device re-reads the journal head before step 1").
        let again = port.claimState(oldGroupIdB64: oldGroupIdB64)
        if again.accepted != nil || again.own != nil { return .adopted }
        let successorGroupIdB64 = try await port.prepareSuccessor(
            oldGroupIdB64: oldGroupIdB64, newIdentityAccountIdB64: accountIdB64)
        do {
            _ = try await port.postClaim(oldGroupIdB64: oldGroupIdB64, successorGroupIdB64: successorGroupIdB64)
        } catch {
            // Nothing was announced: the local successor is dropped and the
            // next trigger starts over.
            try await port.abandonOwnClaim(oldGroupIdB64: oldGroupIdB64)
            try await port.discardSuccessor(successorGroupIdB64: successorGroupIdB64)
            throw error
        }
        return .claimed
    }

    /// Whether a device that MOVED to the successor runs the completeness
    /// re-check there (R.7 case 3: "any moved admin or owner").
    ///
    /// | holds the successor, membership live | own role in the successor | re-checks |
    /// |--------------------------------------|---------------------------|-----------|
    /// | no                                   | -                         | no        |
    /// | yes                                  | owner or admin            | yes       |
    /// | yes                                  | member                    | no        |
    ///
    /// A plain member's `Add` of another account is refused by the core, so
    /// it is not sent.
    public nonisolated static func mayRecheckCompleteness(
        successor: ContinuityFacts?, ownPersonKeyB64: String
    ) -> Bool {
        guard let successor, successor.active else { return false }
        if !successor.rosterPersonKeysB64.contains(ownPersonKeyB64) { return false }
        return successor.ownerKeyB64 == ownPersonKeyB64 || successor.adminKeysB64.contains(ownPersonKeyB64)
    }

    /// The re-check of a device that did not found the successor.
    private static func recheckCompleteness(
        _ port: ContinuityPort, successorGroupIdB64: String, old: ContinuityFacts, own: String
    ) async throws -> ContinuityStep {
        let found = port.facts(groupIdB64: successorGroupIdB64)
        guard mayRecheckCompleteness(successor: found, ownPersonKeyB64: own), let successor = found else {
            return .adopted
        }
        let missing = ResetRepair.successorMissingPeople(
            oldRosterPersonKeysB64: old.rosterPersonKeysB64,
            successorPersonKeysB64: successor.rosterPersonKeysB64,
            supersededKeysB64: Set(port.supersededKeys().keys)
        ).filter { $0 != own }
        if missing.isEmpty { return .adopted }
        if !port.recheckDue(successorGroupIdB64: successorGroupIdB64) { return .adopted }
        for personKeyB64 in missing {
            _ = try await port.addPerson(successorGroupIdB64: successorGroupIdB64, personKeyB64: personKeyB64)
        }
        return .rechecked
    }

    /// Step 4, for the winner. Every sub-step is idempotent.
    private static func build(
        _ port: ContinuityPort, oldGroupIdB64: String, winner: SuccessorClaim, old: ContinuityFacts,
        own: String, accountIdB64: String, unrepairableKeyB64: String
    ) async throws -> ContinuityStep {
        let successorGroupIdB64 = winner.successorGroupIdB64
        try await port.recordLineage(oldGroupIdB64: oldGroupIdB64, successorGroupIdB64: successorGroupIdB64)
        if !(try await port.deliverSuccessor(
            oldGroupIdB64: oldGroupIdB64, successorGroupIdB64: successorGroupIdB64)) {
            return .failed
        }
        let superseded = Set(port.supersededKeys().keys)
        if old.kind == .chat {
            try await port.resendOwnMessages(
                oldGroupIdB64: oldGroupIdB64, successorGroupIdB64: successorGroupIdB64,
                accountIdB64: accountIdB64)
            return .built
        }
        // R.7 case 3: the same people, the same roles and the same owner.
        try await port.carryName(oldGroupIdB64: oldGroupIdB64, successorGroupIdB64: successorGroupIdB64)
        guard let successor = port.facts(groupIdB64: successorGroupIdB64) else { return .failed }
        let missing = ResetRepair.successorMissingPeople(
            oldRosterPersonKeysB64: old.rosterPersonKeysB64,
            successorPersonKeysB64: successor.rosterPersonKeysB64,
            supersededKeysB64: superseded
        ).filter { $0 != own }
        for personKeyB64 in missing {
            // A person who cannot be resolved or reached keeps writing in
            // the old group; the completeness re-check (this same pass,
            // later) tries again.
            _ = try await port.addPerson(successorGroupIdB64: successorGroupIdB64, personKeyB64: personKeyB64)
        }
        let after = port.facts(groupIdB64: successorGroupIdB64) ?? successor
        // Roles are restored by the successor's OWNER only (a role change is
        // owner-only), which is this account until the transfer below lands.
        if after.ownerKeyB64 != own { return .built }
        let wantedAdmins = ResetRepair.successorAdmins(
            oldAdminKeysB64: old.adminKeysB64, successorPersonKeysB64: after.rosterPersonKeysB64,
            successorOwnerKeyB64: own, supersededKeysB64: superseded)
        let have = Set(after.adminKeysB64)
        if wantedAdmins.contains(where: { !have.contains($0) }) {
            var union: [String] = []
            for key in after.adminKeysB64 + wantedAdmins where !union.contains(key) {
                union.append(key)
            }
            try await port.setAdmins(successorGroupIdB64: successorGroupIdB64, adminKeysB64: union)
            // One role change per sweep (C-65): the transfer waits for the
            // next pass.
            return .built
        }
        let newOwnerKeyB64 = port.currentKeyOf(accountIdB64: accountIdB64)
        if ResetRepair.restoreOwnershipToResetIdentity(),
           old.ownerKeyB64 == unrepairableKeyB64,
           let newOwnerKeyB64,
           after.rosterPersonKeysB64.contains(newOwnerKeyB64) {
            try await port.transferOwnership(
                successorGroupIdB64: successorGroupIdB64, newOwnerKeyB64: newOwnerKeyB64,
                stayAdmin: old.adminKeysB64.contains(own))
        }
        return .built
    }
}
