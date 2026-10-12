// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0065 R.6 / R.7 - claim, then build, driven through a fake port
/// (`ContinuityRuntime`). What these pin is the ORDER: nothing reaches anyone
/// before the claim is confirmed, a lost claim discards, and every step can
/// be re-entered. The cases are those of
/// `apps/web/src/chat/continuity-runtime.test.ts`.
@MainActor
final class ContinuityRuntimeTests: XCTestCase {

    private nonisolated static let me = "me"
    private nonisolated static let oldAnna = "anna-old"
    private nonisolated static let newAnna = "anna-new"
    private nonisolated static let bob = "bob"
    private nonisolated static let carol = "carol"
    private nonisolated static let annaAccount = "acct-anna"
    private nonisolated static let old = "G-old"

    private struct Offline: Error {}

    /// The fake world and the port over it; `log` records every effect.
    @MainActor
    private final class World: ContinuityPort {
        var factsByGroup: [String: ContinuityFacts] = [:]
        var claims: [String: StoredGroupClosed] = [:]
        var cursor: Int64? = 0
        var devices = true
        var log: [String] = []
        var nextClaimId: Int64 = 40
        var postFails = false
        var deliverFails = false
        var unreachable: Set<String> = []
        var recheckIsDue = true

        init(
            _ kind: ContinuityKind, roster: [String]? = nil, owner: String?? = nil,
            admins: [String]? = nil, active: Bool = true
        ) {
            let base: ContinuityFacts = kind == .chat
                ? ContinuityFacts(
                    kind: .chat, rosterPersonKeysB64: [me, oldAnna], ownerKeyB64: me,
                    adminKeysB64: [], active: true)
                : ContinuityFacts(
                    kind: .group, rosterPersonKeysB64: [oldAnna, me, bob, carol], ownerKeyB64: oldAnna,
                    adminKeysB64: [me, bob], active: true)
            factsByGroup[old] = ContinuityFacts(
                kind: kind,
                rosterPersonKeysB64: roster ?? base.rosterPersonKeysB64,
                ownerKeyB64: owner ?? base.ownerKeyB64,
                adminKeysB64: admins ?? base.adminKeysB64,
                active: active)
        }

        private func state(_ g: String) -> StoredGroupClosed { claims[g] ?? .noClaims }

        private func with(_ s: StoredGroupClosed, own: OwnClaim?) -> StoredGroupClosed {
            StoredGroupClosed(accepted: s.accepted, pending: s.pending, own: own, prepared: s.prepared)
        }

        func ownPersonKeyB64() -> String? { me }
        func supersededKeys() -> [String: String] { [oldAnna: annaAccount] }
        func currentKeyOf(accountIdB64: String) -> String? { accountIdB64 == annaAccount ? newAnna : nil }
        func facts(groupIdB64: String) -> ContinuityFacts? { factsByGroup[groupIdB64] }
        func claimState(oldGroupIdB64: String) -> StoredGroupClosed { state(oldGroupIdB64) }
        func cursorOf(oldGroupIdB64: String) -> Int64? { cursor }
        func newIdentityHasDevices(accountIdB64: String) async throws -> Bool { devices }

        func prepareSuccessor(oldGroupIdB64: String, newIdentityAccountIdB64: String) async throws -> String {
            log.append("prepare")
            return "S-mine"
        }

        func postClaim(oldGroupIdB64: String, successorGroupIdB64: String) async throws -> Int64 {
            claims[oldGroupIdB64] = with(
                state(oldGroupIdB64), own: OwnClaim(successorGroupIdB64: successorGroupIdB64, messageId: nil))
            if postFails { throw Offline() }
            let id = nextClaimId
            claims[oldGroupIdB64] = with(
                state(oldGroupIdB64), own: OwnClaim(successorGroupIdB64: successorGroupIdB64, messageId: id))
            log.append("claim:\(successorGroupIdB64)")
            return id
        }

        func abandonOwnClaim(oldGroupIdB64: String) async throws {
            claims[oldGroupIdB64] = with(state(oldGroupIdB64), own: nil)
            log.append("abandon")
        }

        func discardSuccessor(successorGroupIdB64: String) async throws {
            log.append("discard:\(successorGroupIdB64)")
        }

        func settle(oldGroupIdB64: String, winner: SuccessorClaim) async throws {
            claims[oldGroupIdB64] = StoredGroupClosed(accepted: winner, pending: [], own: nil)
            log.append("settle:\(winner.successorGroupIdB64)")
        }

        func recordLineage(oldGroupIdB64: String, successorGroupIdB64: String) async throws {
            if !log.contains("lineage:\(successorGroupIdB64)") { log.append("lineage:\(successorGroupIdB64)") }
        }

        func deliverSuccessor(oldGroupIdB64: String, successorGroupIdB64: String) async throws -> Bool {
            if deliverFails { return false }
            if factsByGroup[successorGroupIdB64] == nil {
                factsByGroup[successorGroupIdB64] = ContinuityFacts(
                    kind: factsByGroup[old]?.kind ?? .group, rosterPersonKeysB64: [me, newAnna],
                    ownerKeyB64: me, adminKeysB64: [], active: true)
                log.append("deliver:\(successorGroupIdB64)")
            }
            return true
        }

        func carryName(oldGroupIdB64: String, successorGroupIdB64: String) async throws {
            if !log.contains("name") { log.append("name") }
        }

        func addPerson(successorGroupIdB64: String, personKeyB64: String) async throws -> Bool {
            if unreachable.contains(personKeyB64) {
                log.append("add-failed:\(personKeyB64)")
                return false
            }
            guard let f = factsByGroup[successorGroupIdB64] else { return false }
            factsByGroup[successorGroupIdB64] = ContinuityFacts(
                kind: f.kind, rosterPersonKeysB64: f.rosterPersonKeysB64 + [personKeyB64],
                ownerKeyB64: f.ownerKeyB64, adminKeysB64: f.adminKeysB64, active: f.active)
            log.append("add:\(personKeyB64)")
            return true
        }

        func recheckDue(successorGroupIdB64: String) -> Bool { recheckIsDue }

        func setAdmins(successorGroupIdB64: String, adminKeysB64: [String]) async throws {
            guard let f = factsByGroup[successorGroupIdB64] else { return }
            factsByGroup[successorGroupIdB64] = ContinuityFacts(
                kind: f.kind, rosterPersonKeysB64: f.rosterPersonKeysB64, ownerKeyB64: f.ownerKeyB64,
                adminKeysB64: adminKeysB64, active: f.active)
            log.append("admins:\(adminKeysB64.sorted().joined(separator: ","))")
        }

        func transferOwnership(
            successorGroupIdB64: String, newOwnerKeyB64: String, stayAdmin: Bool
        ) async throws {
            guard let f = factsByGroup[successorGroupIdB64] else { return }
            var admins = f.adminKeysB64
            if stayAdmin, !admins.contains(me) { admins.append(me) }
            factsByGroup[successorGroupIdB64] = ContinuityFacts(
                kind: f.kind, rosterPersonKeysB64: f.rosterPersonKeysB64, ownerKeyB64: newOwnerKeyB64,
                adminKeysB64: admins, active: f.active)
            log.append("transfer:\(newOwnerKeyB64):\(stayAdmin ? "admin" : "member")")
        }

        func resendOwnMessages(
            oldGroupIdB64: String, successorGroupIdB64: String, accountIdB64: String
        ) async throws {
            if !log.contains("resend:\(successorGroupIdB64)") { log.append("resend:\(successorGroupIdB64)") }
        }
    }

    private func pass(_ w: World) async -> ContinuityStep {
        await ContinuityRuntime.continueConversation(port: w, oldGroupIdB64: Self.old)
    }

    private func claim(_ messageId: Int64, _ successor: String, sender: String = me) -> SuccessorClaim {
        SuccessorClaim(
            messageId: messageId, successorGroupIdB64: successor, senderKeyB64: sender,
            senderWasAdmin: true, adminKeysAtReceiptB64: [Self.me, Self.bob])
    }

    private func held(_ c: SuccessorClaim) -> StoredGroupClosed {
        StoredGroupClosed(accepted: c, pending: [], own: nil)
    }

    // MARK: - A one-to-one whose other person reset (R.7 case 1)

    func testClaimsFirstAndSendsNothingUntilTheJournalConfirmsTheClaim() async {
        let w = World(.chat)
        let first = await pass(w)
        XCTAssertEqual(first, .claimed)
        XCTAssertEqual(w.log, ["prepare", "claim:S-mine"])
        // The old journal has not been read up to the claim yet.
        w.cursor = 39
        let second = await pass(w)
        XCTAssertEqual(second, .waitingForJournal)
        XCTAssertEqual(w.log, ["prepare", "claim:S-mine"])
    }

    func testAnUnreadCursorAlsoWaitsForTheJournal() async {
        let w = World(.chat)
        _ = await pass(w)
        w.cursor = nil
        let step = await pass(w)
        XCTAssertEqual(step, .waitingForJournal)
        XCTAssertEqual(w.log, ["prepare", "claim:S-mine"])
    }

    func testWinsWhenNoEarlierEligibleClaimIsInTheJournalThenBuildsAndResends() async {
        let w = World(.chat)
        _ = await pass(w)
        w.cursor = 40
        let step = await pass(w)
        XCTAssertEqual(step, .built)
        XCTAssertEqual(
            w.log,
            ["prepare", "claim:S-mine", "settle:S-mine", "lineage:S-mine", "deliver:S-mine", "resend:S-mine"])
        // A one-to-one has no name to carry, nobody to add, no roles.
        XCTAssertFalse(w.log.contains { $0.hasPrefix("add") || $0 == "name" || $0.hasPrefix("transfer") })
    }

    func testLosesToAnEarlierClaimOfThePersonsOtherDeviceDiscardsAndAdopts() async {
        let w = World(.chat)
        _ = await pass(w)
        // The other device's claim was earlier in the journal and was
        // accepted by the receive rule while this device read up to its own.
        let mine = w.claims[Self.old] ?? .noClaims
        w.claims[Self.old] = StoredGroupClosed(
            accepted: claim(37, "S-theirs"), pending: mine.pending, own: mine.own, prepared: mine.prepared)
        w.cursor = 40
        let step = await pass(w)
        XCTAssertEqual(step, .lost)
        XCTAssertEqual(
            w.log, ["prepare", "claim:S-mine", "discard:S-mine", "settle:S-theirs", "lineage:S-theirs"])
        // Nothing of the lost successor ever reached anyone.
        XCTAssertFalse(w.log.contains { $0.hasPrefix("deliver") })
        XCTAssertNotNil(w.claims[Self.old])
        XCTAssertNil(w.claims[Self.old]?.own)
    }

    func testStandsDownWhenAClaimIsAlreadyHeldAndRecordsTheLineage() async {
        let w = World(.chat)
        w.claims[Self.old] = held(claim(12, "S-theirs", sender: "someone-else"))
        let step = await pass(w)
        XCTAssertEqual(step, .adopted)
        XCTAssertEqual(w.log, ["lineage:S-theirs"])
    }

    func testWaitsWhileTheNewIdentityPublishesNoDevice() async {
        let w = World(.chat)
        w.devices = false
        let step = await pass(w)
        XCTAssertEqual(step, .waitingForDevices)
        XCTAssertEqual(w.log, [])
    }

    func testAClaimWhosePublishFailedLeavesNothingBehind() async {
        let w = World(.chat)
        w.postFails = true
        let failed = await pass(w)
        XCTAssertEqual(failed, .failed)
        XCTAssertEqual(w.log, ["prepare", "abandon", "discard:S-mine"])
        XCTAssertNotNil(w.claims[Self.old])
        XCTAssertNil(w.claims[Self.old]?.own)
        // The next trigger starts over.
        w.postFails = false
        let again = await pass(w)
        XCTAssertEqual(again, .claimed)
    }

    func testAClaimInterruptedBeforeItsIdWasRecordedIsPostedAgainSameSuccessor() async {
        let w = World(.chat)
        w.claims[Self.old] = StoredGroupClosed(
            accepted: nil, pending: [], own: OwnClaim(successorGroupIdB64: "S-mine", messageId: nil))
        let step = await pass(w)
        XCTAssertEqual(step, .claimed)
        XCTAssertEqual(w.log, ["claim:S-mine"])
    }

    func testAFailedDeliveryIsResumedByTheNextPass() async {
        let w = World(.chat)
        _ = await pass(w)
        w.cursor = 40
        w.deliverFails = true
        let failed = await pass(w)
        XCTAssertEqual(failed, .failed)
        w.deliverFails = false
        let built = await pass(w)
        XCTAssertEqual(built, .built)
        XCTAssertEqual(w.log.filter { $0 == "deliver:S-mine" }.count, 1)
        XCTAssertEqual(w.log.filter { $0 == "resend:S-mine" }.count, 1)
    }

    func testDoesNothingWhenNothingWasSupersededInThisConversation() async {
        let w = World(.chat, roster: [Self.me, Self.bob])
        let step = await pass(w)
        XCTAssertEqual(step, .nothing)
    }

    func testDoesNothingForAGroupThisDeviceDoesNotHold() async {
        let w = World(.chat)
        let step = await ContinuityRuntime.continueConversation(port: w, oldGroupIdB64: "G-unknown")
        XCTAssertEqual(step, .nothing)
        XCTAssertEqual(w.log, [])
    }

    func testAnEndedMembershipFoundsNothing() async {
        let w = World(.chat, active: false)
        let step = await pass(w)
        XCTAssertEqual(step, .notEligible)
        XCTAssertEqual(w.log, [])
    }

    // MARK: - A group whose owner reset (R.7 case 3)

    private func winAsAdmin(_ w: World) async {
        let step = await pass(w)
        XCTAssertEqual(step, .claimed)
        w.cursor = 40
    }

    func testEndsWithTheSamePeopleTheSameRolesAndTheSameOwner() async {
        let w = World(.group)
        await winAsAdmin(w)
        // Pass 1: deliver, name, the remaining people, the admin set.
        let one = await pass(w)
        XCTAssertEqual(one, .built)
        XCTAssertEqual(
            w.log,
            [
                "prepare", "claim:S-mine", "settle:S-mine", "lineage:S-mine", "deliver:S-mine", "name",
                "add:bob", "add:carol", "admins:bob",
            ])
        // Pass 2 (one role change per sweep): the owner is restored, and the
        // founder, an admin before, stays one.
        let two = await pass(w)
        XCTAssertEqual(two, .built)
        XCTAssertEqual(w.log.last, "transfer:anna-new:admin")
        let successor = w.factsByGroup["S-mine"]
        XCTAssertEqual(
            successor?.rosterPersonKeysB64.sorted(),
            [Self.bob, Self.carol, Self.me, Self.newAnna].sorted())
        XCTAssertEqual(successor?.ownerKeyB64, Self.newAnna)
        XCTAssertEqual(successor?.adminKeysB64.sorted(), [Self.bob, Self.me].sorted())
        // Pass 3: nothing left to do; no step repeats.
        let before = w.log.count
        let three = await pass(w)
        XCTAssertEqual(three, .built)
        XCTAssertEqual(w.log.count, before)
    }

    func testKeepsAddingAMemberItCouldNotReach() async {
        let w = World(.group)
        w.unreachable.insert(Self.carol)
        await winAsAdmin(w)
        _ = await pass(w)
        XCTAssertTrue(w.log.contains("add-failed:carol"))
        XCTAssertEqual(w.factsByGroup["S-mine"]?.rosterPersonKeysB64.contains(Self.carol), false)
        w.unreachable.removeAll()
        _ = await pass(w)
        XCTAssertTrue(w.log.contains("add:carol"))
    }

    func testAPlainMemberDoesNotFoundWhileAnotherAdminExists() async {
        let w = World(.group, admins: [Self.bob])
        let step = await pass(w)
        XCTAssertEqual(step, .notEligible)
        XCTAssertEqual(w.log, [])
    }

    func testAMemberThatAdoptedAnotherAdminsSuccessorBuildsNothing() async {
        let w = World(.group, admins: [Self.bob])
        w.claims[Self.old] = held(claim(9, "S-bob", sender: Self.bob))
        let step = await pass(w)
        XCTAssertEqual(step, .adopted)
        XCTAssertEqual(w.log, ["lineage:S-bob"])
    }

    func testAGroupWhereTheSupersededKeyIsOnlyAMemberIsNotContinuedHere() async {
        let w = World(.group, owner: .some(Self.me))
        let step = await pass(w)
        XCTAssertEqual(step, .nothing)
    }

    // MARK: - The completeness re-check of a moved admin that did not found

    private enum SuccessorRole { case admin, owner, member }

    /// Bob founded `S-bob`; this device (ME) adopted it and has joined it.
    private func adopted(_ role: SuccessorRole, successorRoster: [String]) -> World {
        let w = World(.group, admins: [Self.me, Self.bob])
        w.claims[Self.old] = held(claim(9, "S-bob", sender: Self.bob))
        w.factsByGroup["S-bob"] = ContinuityFacts(
            kind: .group, rosterPersonKeysB64: successorRoster,
            ownerKeyB64: role == .owner ? Self.me : Self.bob,
            adminKeysB64: role == .admin ? [Self.me] : [], active: true)
        return w
    }

    func testAMovedAdminAddsWhoeverOfTheOldRosterTheSuccessorLacks() async {
        let w = adopted(.admin, successorRoster: [Self.bob, Self.me, Self.newAnna])
        let step = await pass(w)
        XCTAssertEqual(step, .rechecked)
        XCTAssertTrue(w.log.contains("add:carol"))
        // Never the superseded key, never itself, nobody already there.
        XCTAssertEqual(w.log.filter { $0.hasPrefix("add:") }, ["add:carol"])
    }

    func testAMovedOwnerDoesTheSame() async {
        let w = adopted(.owner, successorRoster: [Self.bob, Self.me])
        let step = await pass(w)
        XCTAssertEqual(step, .rechecked)
        XCTAssertTrue(w.log.contains("add:carol"))
    }

    func testAMovedPlainMemberAddsNobody() async {
        let w = adopted(.member, successorRoster: [Self.bob, Self.me])
        let step = await pass(w)
        XCTAssertEqual(step, .adopted)
        XCTAssertFalse(w.log.contains { $0.hasPrefix("add") })
    }

    func testADeviceThatHasNotJoinedTheSuccessorYetAddsNobody() async {
        let w = adopted(.admin, successorRoster: [Self.bob])
        let step = await pass(w)
        XCTAssertEqual(step, .adopted)
        XCTAssertFalse(w.log.contains { $0.hasPrefix("add") })
    }

    func testRunsAtTheArmsCadenceAndNotAtAllWhenNobodyIsMissing() async {
        let w = adopted(.admin, successorRoster: [Self.bob, Self.me])
        w.recheckIsDue = false
        let notDue = await pass(w)
        XCTAssertEqual(notDue, .adopted)
        XCTAssertFalse(w.log.contains { $0.hasPrefix("add") })
        let full = adopted(.admin, successorRoster: [Self.bob, Self.me, Self.carol, Self.newAnna])
        let complete = await pass(full)
        XCTAssertEqual(complete, .adopted)
    }

    func testMayRecheckCompletenessTheTable() {
        func facts(
            roster: [String] = [Self.me, Self.bob], owner: String = Self.bob,
            admins: [String] = [], active: Bool = true
        ) -> ContinuityFacts {
            ContinuityFacts(
                kind: .group, rosterPersonKeysB64: roster, ownerKeyB64: owner, adminKeysB64: admins,
                active: active)
        }
        let me = Self.me
        XCTAssertFalse(ContinuityRuntime.mayRecheckCompleteness(successor: nil, ownPersonKeyB64: me))
        XCTAssertFalse(ContinuityRuntime.mayRecheckCompleteness(successor: facts(), ownPersonKeyB64: me))
        XCTAssertTrue(
            ContinuityRuntime.mayRecheckCompleteness(successor: facts(admins: [me]), ownPersonKeyB64: me))
        XCTAssertTrue(
            ContinuityRuntime.mayRecheckCompleteness(successor: facts(owner: me), ownPersonKeyB64: me))
        XCTAssertFalse(
            ContinuityRuntime.mayRecheckCompleteness(
                successor: facts(admins: [me], active: false), ownPersonKeyB64: me))
        XCTAssertFalse(
            ContinuityRuntime.mayRecheckCompleteness(
                successor: facts(roster: [Self.bob], admins: [me]), ownPersonKeyB64: me))
    }
}
