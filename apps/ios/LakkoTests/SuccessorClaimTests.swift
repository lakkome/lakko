// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0073 R.6 - claim, then build: the pure rules of `SuccessorClaimRules`.
/// The cases are those of `apps/web/src/chat/successor-claim.test.ts`.
final class SuccessorClaimTests: XCTestCase {

    private typealias Rules = SuccessorClaimRules

    private let old = "old-anna"
    private let me = "me"
    private let bob = "bob"
    private let carol = "carol"
    private let superseded: Set<String> = ["old-anna"]

    private func claim(
        messageId: Int64 = 10, successor: String = "S1", sender: String = "me",
        wasAdmin: Bool = false, admins: [String] = []
    ) -> SuccessorClaim {
        SuccessorClaim(
            messageId: messageId, successorGroupIdB64: successor, senderKeyB64: sender,
            senderWasAdmin: wasAdmin, adminKeysAtReceiptB64: admins)
    }

    // MARK: - unrepairableBy (R.6 (i))

    func testAOneToOneWhoseOtherPersonWasSuperseded() {
        XCTAssertEqual(
            Rules.unrepairableBy(
                kind: .chat, rosterPersonKeysB64: [me, old], ownPersonKeyB64: me,
                ownerKeyB64: me, supersededKeys: superseded),
            old)
    }

    func testAGroupWhoseOwnerWasSuperseded() {
        XCTAssertEqual(
            Rules.unrepairableBy(
                kind: .group, rosterPersonKeysB64: [me, old, bob], ownPersonKeyB64: me,
                ownerKeyB64: old, supersededKeys: superseded),
            old)
    }

    func testAGroupWhereTheSupersededKeyIsOnlyAMemberIsRepairable() {
        XCTAssertNil(
            Rules.unrepairableBy(
                kind: .group, rosterPersonKeysB64: [me, old, bob], ownPersonKeyB64: me,
                ownerKeyB64: me, supersededKeys: superseded))
    }

    func testNothingAcceptedOrNoRoleTableIsNil() {
        XCTAssertNil(
            Rules.unrepairableBy(
                kind: .chat, rosterPersonKeysB64: [me, bob], ownPersonKeyB64: me,
                ownerKeyB64: nil, supersededKeys: superseded))
        XCTAssertNil(
            Rules.unrepairableBy(
                kind: .group, rosterPersonKeysB64: [me, old], ownPersonKeyB64: me,
                ownerKeyB64: nil, supersededKeys: superseded))
    }

    // MARK: - claimEligible (R.6)

    private func groupEligible(sender: String, wasAdmin: Bool, admins: [String]) -> Bool {
        Rules.claimEligible(
            kind: .group, senderKeyB64: sender, senderWasAdmin: wasAdmin, adminKeysB64: admins,
            supersededKeyB64: old, supersededKeys: superseded, survivingKeyB64: nil)
    }

    private func chatEligible(sender: String) -> Bool {
        Rules.claimEligible(
            kind: .chat, senderKeyB64: sender, senderWasAdmin: false, adminKeysB64: [],
            supersededKeyB64: old, supersededKeys: superseded, survivingKeyB64: me)
    }

    func testASupersededKeyIsNeverEligible() {
        XCTAssertFalse(groupEligible(sender: old, wasAdmin: true, admins: [old]))
        XCTAssertFalse(chatEligible(sender: old))
    }

    func testOneToOneAnyDeviceOfTheSurvivingAccountAndNobodyElse() {
        XCTAssertTrue(chatEligible(sender: me))
        XCTAssertFalse(chatEligible(sender: bob))
    }

    func testGroupAnAdminOtherThanTheSupersededOwner() {
        XCTAssertTrue(groupEligible(sender: bob, wasAdmin: true, admins: [bob]))
    }

    func testGroupAPlainMemberIsNotEligibleWhileAnotherAdminExists() {
        XCTAssertFalse(groupEligible(sender: carol, wasAdmin: false, admins: [bob]))
    }

    func testGroupWithNoAdminButTheSupersededOwnerAnyMemberIs() {
        XCTAssertTrue(groupEligible(sender: carol, wasAdmin: false, admins: []))
        XCTAssertTrue(groupEligible(sender: carol, wasAdmin: false, admins: [old]))
    }

    // MARK: - claimVerdict (the receive rule)

    private func verdict(
        kind: ContinuityKind = .chat, claim: SuccessorClaim, held: SuccessorClaim? = nil,
        unrepairable: String? = "old-anna", surviving: String? = "me"
    ) -> Rules.ClaimVerdict {
        Rules.claimVerdict(
            kind: kind, groupIdB64: "G", claim: claim, held: held, unrepairableKeyB64: unrepairable,
            supersededKeys: superseded, survivingKeyB64: surviving)
    }

    func testAcceptsTheFirstEligibleClaim() {
        XCTAssertEqual(verdict(claim: claim()), .accept)
    }

    func testIgnoresEveryLaterClaimForGood() {
        XCTAssertEqual(
            verdict(claim: claim(messageId: 11, successor: "S2"), held: claim(messageId: 10)),
            .ignoreLater)
        XCTAssertEqual(verdict(claim: claim(messageId: 10), held: claim(messageId: 10)), .ignoreLater)
    }

    func testAnEarlierClaimJudgedLateStillWinsTheJournalsOrder() {
        XCTAssertEqual(
            verdict(claim: claim(messageId: 7, successor: "S0"), held: claim(messageId: 10)),
            .accept)
    }

    func testKeepsAClaimWhileThisDeviceHasNoVerdictOfItsOwn() {
        XCTAssertEqual(verdict(claim: claim(), unrepairable: nil), .keepPending)
    }

    func testNeverKeepsOrAcceptsAClaimFromTheSupersededIdentitysLeaves() {
        XCTAssertEqual(verdict(claim: claim(sender: old)), .dropIneligible)
        XCTAssertEqual(verdict(claim: claim(sender: old), unrepairable: nil), .dropIneligible)
    }

    func testDropsAClaimFromAPlainMemberOfAGroupThatHasAnotherAdmin() {
        XCTAssertEqual(
            verdict(kind: .group, claim: claim(sender: carol, admins: [bob]), surviving: nil),
            .dropIneligible)
    }

    func testDropsAnEnvelopeThatNamesNoSuccessorOrThisVeryGroup() {
        XCTAssertEqual(verdict(claim: claim(successor: "")), .dropMalformed)
        XCTAssertEqual(verdict(claim: claim(successor: "G")), .dropMalformed)
    }

    // MARK: - firstEligibleClaim / claimOutcome (step 3)

    private var journalClaims: [SuccessorClaim] {
        [
            claim(messageId: 30, successor: "S-me", sender: me, wasAdmin: true, admins: [me, bob]),
            claim(messageId: 12, successor: "S-old", sender: old, wasAdmin: true, admins: [me, bob]),
            claim(messageId: 20, successor: "S-bob", sender: bob, wasAdmin: true, admins: [me, bob]),
            claim(messageId: 15, successor: "S-carol", sender: carol, wasAdmin: false, admins: [me, bob]),
        ]
    }

    private func first(_ claims: [SuccessorClaim], unrepairable: String? = "old-anna") -> SuccessorClaim? {
        Rules.firstEligibleClaim(
            kind: .group, groupIdB64: "G", claims: claims, unrepairableKeyB64: unrepairable,
            supersededKeys: superseded, survivingKeyB64: nil)
    }

    func testTheFirstEligibleClaimInJournalOrderSkippingTheSupersededSenderAndThePlainMember() {
        XCTAssertEqual(first(journalClaims)?.successorGroupIdB64, "S-bob")
    }

    func testAFounderThatFindsAnEarlierEligibleClaimLostAndAdoptsIt() {
        XCTAssertEqual(
            Rules.claimOutcome(ownClaimMessageId: 30, first: first(journalClaims)),
            .lost(winner: journalClaims[2]))
    }

    func testAFounderWhoseClaimIsTheFirstEligibleOneWon() {
        let own = first([journalClaims[0], journalClaims[1]])
        XCTAssertEqual(own?.successorGroupIdB64, "S-me")
        XCTAssertEqual(Rules.claimOutcome(ownClaimMessageId: 30, first: own), .won)
        XCTAssertEqual(Rules.claimOutcome(ownClaimMessageId: 30, first: nil), .won)
    }

    func testWithoutAVerdictNothingIsEligible() {
        XCTAssertNil(first(journalClaims, unrepairable: nil))
    }

    // MARK: - shouldFoundSuccessor (steps 1 and 2)

    private func founds(
        kind: ContinuityKind = .chat, unrepairable: String? = "old-anna", held: SuccessorClaim? = nil,
        ownClaimPosted: Bool = false, membershipEnded: Bool = false, own: String? = "me",
        ownIsAdmin: Bool = false, admins: [String] = [], newIdentityHasDevices: Bool = true
    ) -> Bool {
        Rules.shouldFoundSuccessor(
            kind: kind, unrepairableKeyB64: unrepairable, held: held, ownClaimPosted: ownClaimPosted,
            membershipEnded: membershipEnded, ownPersonKeyB64: own, ownIsAdmin: ownIsAdmin,
            adminKeysB64: admins, supersededKeys: superseded, newIdentityHasDevices: newIdentityHasDevices)
    }

    func testTheSurvivingSideOfAOneToOneFounds() {
        XCTAssertTrue(founds())
    }

    func testStandsDownWhenAnEligibleClaimIsAlreadyHeld() {
        XCTAssertFalse(founds(held: claim()))
    }

    func testDoesNotClaimTwice() {
        XCTAssertFalse(founds(ownClaimPosted: true))
    }

    func testWaitsWhileTheNewIdentityPublishesNoDevice() {
        XCTAssertFalse(founds(newIdentityHasDevices: false))
    }

    func testDoesNothingForARepairableGroupAnEndedMembershipOrNoIdentity() {
        XCTAssertFalse(founds(unrepairable: nil))
        XCTAssertFalse(founds(membershipEnded: true))
        XCTAssertFalse(founds(own: nil))
    }

    func testInAnOwnedGroupOnlyAnAdminFoundsWhileAnotherAdminExists() {
        XCTAssertFalse(founds(kind: .group, admins: [bob]))
        XCTAssertTrue(founds(kind: .group, ownIsAdmin: true, admins: [me, bob]))
        XCTAssertTrue(founds(kind: .group, admins: []))
    }
}
