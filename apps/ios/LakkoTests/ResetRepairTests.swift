// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0065 R.4 / R.7 — the repair decisions, the wider re-admission lookup,
/// the add-member key check and the continuity key. The cases are the tables
/// of `apps/web/src/chat/reset-repair.test.ts`, `device-readmission.test.ts`,
/// `contacts/add-member-key.test.ts` and `reach/continuity.test.ts`.
final class ResetRepairTests: XCTestCase {

    private typealias Leaf = ResetRepair.PersonLeaf

    // MARK: - resetCase

    func testTheFourCases() {
        XCTAssertEqual(ResetRepair.resetCase(isOneToOne: true, ownerKeyB64: "old", supersededKeyB64: "old"), .oneToOne)
        XCTAssertEqual(ResetRepair.resetCase(isOneToOne: false, ownerKeyB64: nil, supersededKeyB64: "old"), .noRoleTable)
        XCTAssertEqual(ResetRepair.resetCase(isOneToOne: false, ownerKeyB64: "old", supersededKeyB64: "old"), .owned)
        XCTAssertEqual(ResetRepair.resetCase(isOneToOne: false, ownerKeyB64: "me", supersededKeyB64: "old"), .repair)
    }

    // MARK: - repairPlan

    private let roster = [
        Leaf(leafIndex: 0, personKeyB64: "me"), Leaf(leafIndex: 1, personKeyB64: "old"),
        Leaf(leafIndex: 2, personKeyB64: "old"), Leaf(leafIndex: 3, personKeyB64: "bob"),
    ]

    private func plan(
        role: ResetRepair.OwnRole?, roster: [Leaf]?, pending: [UInt32] = [],
        inFlight: Bool = false, ended: Bool = false
    ) -> ResetRepair.RepairPlan {
        ResetRepair.repairPlan(
            ownRole: role, roster: roster, supersededKeyB64: "old", pendingRemoveLeaves: pending,
            removeInFlight: inFlight, membershipEnded: ended)
    }

    func testAnOwnerOrAdminProposesTheRemovalAndMayAdd() {
        for role in [ResetRepair.OwnRole.owner, .admin] {
            XCTAssertEqual(plan(role: role, roster: roster), ResetRepair.RepairPlan(removeLeaf: 1, mayAdd: true))
        }
    }

    func testAPlainMemberAndNoRoleProposeNothing() {
        XCTAssertEqual(plan(role: .member, roster: roster), ResetRepair.RepairPlan(removeLeaf: nil, mayAdd: false))
        XCTAssertEqual(plan(role: nil, roster: roster), ResetRepair.RepairPlan(removeLeaf: nil, mayAdd: false))
        XCTAssertFalse(ResetRepair.mayAddResetIdentity(.member))
        XCTAssertFalse(ResetRepair.mayAddResetIdentity(nil))
        XCTAssertTrue(ResetRepair.mayAddResetIdentity(.admin))
    }

    func testAPendingRemoveIsNotProposedAgain() {
        XCTAssertEqual(plan(role: .admin, roster: roster, pending: [1]).removeLeaf, 2)
        XCTAssertNil(plan(role: .admin, roster: roster, pending: [1, 2]).removeLeaf)
        XCTAssertNil(plan(role: .admin, roster: roster, inFlight: true).removeLeaf)
        XCTAssertTrue(plan(role: .admin, roster: roster, inFlight: true).mayAdd)
    }

    func testATornRosterAndAnEndedMembershipProposeNothing() {
        XCTAssertEqual(plan(role: .owner, roster: nil), ResetRepair.RepairPlan(removeLeaf: nil, mayAdd: false))
        XCTAssertEqual(
            plan(role: .owner, roster: roster, ended: true), ResetRepair.RepairPlan(removeLeaf: nil, mayAdd: false))
    }

    func testWithTheOldLeavesGoneTheAddIsStillAllowed() {
        let after = [Leaf(leafIndex: 0, personKeyB64: "me")]
        XCTAssertEqual(plan(role: .admin, roster: after), ResetRepair.RepairPlan(removeLeaf: nil, mayAdd: true))
    }

    // MARK: - adminRestoreStep

    private func step(
        role: ResetRepair.OwnRole? = .owner, admins: [String], people: [String], pending: Bool = false
    ) -> ResetRepair.AdminRestoreStep? {
        ResetRepair.adminRestoreStep(
            ownRole: role, adminKeysB64: admins, supersededKeyB64: "old", newKeyB64: "new",
            rosterPersonKeysB64: people, roleChangePending: pending)
    }

    func testTheOwnerPromotesTheNewKeyThenDemotesTheOld() {
        XCTAssertEqual(step(admins: ["old"], people: ["me", "old", "new"]), .promote(keyB64: "new"))
        XCTAssertEqual(step(admins: ["old", "new"], people: ["me", "new"]), .demote(keyB64: "old"))
    }

    func testNothingIsProposedOtherwise() {
        XCTAssertNil(step(admins: ["old"], people: ["me", "old", "new"], pending: true))
        XCTAssertNil(step(admins: ["bob"], people: ["me", "new"]), "the old key was no admin")
        XCTAssertNil(step(admins: ["old"], people: ["me", "old"]), "the new identity is not a member yet")
        XCTAssertNil(step(role: .admin, admins: ["old"], people: ["me", "new"]), "owner only")
        XCTAssertNil(step(admins: ["new"], people: ["me", "new"]), "done")
    }

    // MARK: - Members changed

    func testMembersChangedIsSuppressedWhenThePeopleAreUnchanged() {
        XCTAssertTrue(ResetRepair.membersChangedSuppressed(peopleBefore: ["a", "b"], peopleAfter: ["b", "a"]))
        XCTAssertFalse(ResetRepair.membersChangedSuppressed(peopleBefore: ["a", "b"], peopleAfter: ["a"]))
        XCTAssertFalse(ResetRepair.membersChangedSuppressed(peopleBefore: nil, peopleAfter: ["a"]))
        XCTAssertEqual(
            ResetRepair.peopleByAccount(["old", "new", "bob"], accountOfKey: { $0 == "bob" ? nil : "anna" }),
            ["anna", "bob"])
    }

    private func swap(
        before: [String]?, after: [String]?, awaited: [String: Int64] = [:], now: Int64 = 1_000
    ) -> ResetRepair.SwapVerdict {
        ResetRepair.deviceSwapAcrossCommits(
            personKeysBefore: before, personKeysAfter: after,
            accountOfKey: { ["old": "anna", "new": "anna"][$0] },
            isSuperseded: { $0 == "old" }, awaited: awaited, nowMs: now)
    }

    func testADeviceSwapInOneCommitIsSuppressed() {
        XCTAssertEqual(
            swap(before: ["me", "old"], after: ["me", "new"]), ResetRepair.SwapVerdict(suppress: true, awaited: [:]))
    }

    func testADeviceSwapAcrossTwoCommitsIsSuppressedOnBoth() {
        let first = swap(before: ["me", "old"], after: ["me"])
        XCTAssertEqual(first, ResetRepair.SwapVerdict(suppress: true, awaited: ["anna": 1_000]))
        let second = swap(before: ["me"], after: ["me", "new"], awaited: first.awaited, now: 2_000)
        XCTAssertEqual(second, ResetRepair.SwapVerdict(suppress: true, awaited: [:]))
    }

    func testAPersonTrulyGoneOrNewKeepsTheRow() {
        XCTAssertFalse(swap(before: ["me", "bob"], after: ["me"]).suppress)
        XCTAssertFalse(swap(before: ["me"], after: ["me", "new"]).suppress, "nobody was awaited")
        XCTAssertFalse(swap(before: ["me", "new"], after: ["me"]).suppress, "a current key left")
        XCTAssertFalse(swap(before: ["me", "old"], after: ["me", "old"]).suppress, "no key changed")
        XCTAssertFalse(swap(before: nil, after: ["me"]).suppress)
    }

    func testAnAwaitedPersonIsForgottenPastTheWindow() {
        let late = swap(
            before: ["me"], after: ["me", "new"], awaited: ["anna": 0],
            now: ResetRepair.deviceSwapPairWindowMs + 1)
        XCTAssertFalse(late.suppress)
        XCTAssertTrue(late.awaited.isEmpty)
    }

    // MARK: - The two pending decisions, and what hangs on them

    func testThePendingDecisionsAreAtTheAdrDefault() {
        XCTAssertTrue(ResetRepair.restoreOwnershipToResetIdentity())
        XCTAssertTrue(ResetRepair.resendOwnMessagesAfterReset())
    }

    func testOnlyOwnResendableMessagesAfterTheResetAreSentAgain() {
        typealias M = ResetRepair.OldSegmentMessage
        let rows = [
            M(messageId: 9, own: true, sentAtMs: 200, resendable: true),
            M(messageId: 3, own: true, sentAtMs: 150, resendable: true),
            M(messageId: 4, own: true, sentAtMs: 50, resendable: true),
            M(messageId: 5, own: false, sentAtMs: 300, resendable: true),
            M(messageId: 6, own: true, sentAtMs: 300, resendable: false),
            M(messageId: 7, own: true, sentAtMs: nil, resendable: true),
            M(messageId: 8, own: true, sentAtMs: 100, resendable: true),
        ]
        XCTAssertEqual(ResetRepair.messagesToResend(rows, resetSignedAtMs: 100), [3, 9])
    }

    func testTheSuccessorOfAnOwnedGroup() {
        XCTAssertEqual(
            ResetRepair.successorPeople(
                oldRosterPersonKeysB64: ["me", "old", "bob", "bob", "eve"], supersededKeysB64: ["old"],
                ownPersonKeyB64: "me"),
            ["bob", "eve"])
        XCTAssertEqual(
            ResetRepair.successorMissingPeople(
                oldRosterPersonKeysB64: ["me", "old", "bob", "eve"], successorPersonKeysB64: ["me", "bob"],
                supersededKeysB64: ["old"]),
            ["eve"])
        XCTAssertEqual(
            ResetRepair.successorAdmins(
                oldAdminKeysB64: ["me", "old", "bob", "eve"], successorPersonKeysB64: ["me", "bob"],
                successorOwnerKeyB64: "me", supersededKeysB64: ["old"]),
            ["bob"])
    }

    func testARowFromASupersededSenderIsDropped() {
        XCTAssertTrue(ResetRepair.dropsSupersededSender("old", supersededKeys: ["old"]))
        XCTAssertFalse(ResetRepair.dropsSupersededSender("new", supersededKeys: ["old"]))
        XCTAssertFalse(ResetRepair.dropsSupersededSender(nil, supersededKeys: ["old"]))
    }

    // MARK: - The wider lookup

    private let anna = Data(repeating: 0xA1, count: 16)

    func testCandidatesComeFromAContactRowOrTheMembersOwnProfile() {
        let bob = Data(repeating: 0xB2, count: 16)
        let out = DeviceReadmission.readmissionCandidates(
            rosterPersonKeysB64: ["me", "old", "bob", "eve", "old"],
            ownPersonKeyB64: "me",
            contacts: [
                DeviceReadmission.LookupContact(signaturePubkeyB64: "new", previousKeysB64: ["old"], accountId: anna)
            ],
            profileAccountIdOf: { $0 == "bob" ? bob : ($0 == "eve" ? Data([1, 2]) : nil) })
        XCTAssertEqual(out, [
            DeviceReadmission.LookupCandidate(rosterKeyB64: "old", accountId: anna),
            DeviceReadmission.LookupCandidate(rosterKeyB64: "bob", accountId: bob),
        ])
    }

    func testLookupStanding() {
        let pin = PeerPinStore.AccountPubkeyPin(
            pinnedSignaturePubkeyB64: "new", rotationMarker: 0, supersededKeysB64: ["old"])
        func standing(_ key: String, pin: PeerPinStore.AccountPubkeyPin?, binding: VerifiedBinding? = nil)
            -> DeviceReadmission.LookupStanding {
            DeviceReadmission.lookupStanding(
                rosterKeyB64: key, pin: pin, provenPredecessorsB64: ["pred"], binding: binding)
        }
        XCTAssertEqual(standing("new", pin: pin), .samePerson)
        XCTAssertEqual(standing("pred", pin: pin), .samePerson)
        XCTAssertEqual(standing("old", pin: pin), .resetIdentity)
        XCTAssertEqual(standing("eve", pin: pin), .skip)
        let binding = VerifiedBinding(keyB64: "bob", keyGeneration: 1, verifiedAtMs: 0)
        XCTAssertEqual(standing("bob", pin: nil, binding: binding), .samePerson)
        XCTAssertEqual(standing("eve", pin: nil, binding: binding), .skip, "a claimed id is never followed")
        XCTAssertEqual(standing("bob", pin: nil), .skip)
    }

    // MARK: - The add-member key check

    func testOnlyDevicesBoundToTheExpectedKeyOrItsProvenChainAreAdded() {
        var proven = ProvenPredecessors()
        proven.record(oldKeyB64: "k1", newKeyB64: "k2", evidence: .verifiedProof)
        XCTAssertEqual(AddMemberKey.acceptableBoundKeys(expectedKeyB64: "k2", proven: proven), ["k1", "k2"])
        XCTAssertEqual(AddMemberKey.acceptableBoundKeys(expectedKeyB64: "k1", proven: proven), ["k1", "k2"])
        XCTAssertEqual(AddMemberKey.acceptableBoundKeys(expectedKeyB64: "other", proven: proven), ["other"])
        XCTAssertEqual(AddMemberKey.acceptableBoundKeys(expectedKeyB64: "", proven: proven), [])

        let devices = ["a:k2", "b:k1", "c:reset-retired", "d:"]
        let bound: (String) -> String? = { device in
            let key = String(device.split(separator: ":", omittingEmptySubsequences: false)[1])
            return key.isEmpty ? nil : key
        }
        XCTAssertEqual(
            AddMemberKey.devicesBoundTo(devices, acceptedKeysB64: ["k1", "k2"], boundKeyOf: bound),
            ["a:k2", "b:k1"])
        XCTAssertEqual(AddMemberKey.devicesBoundTo(devices, acceptedKeysB64: ["new"], boundKeyOf: bound), [])
        XCTAssertNil(AddMemberKey.boundKeyB64(keyPackage: Data([1, 2, 3])), "an unparseable KeyPackage is refused")
    }

    // MARK: - The continuity key

    func testTheContinuityKeyIsWantedUntilItsExpiry() {
        XCTAssertEqual(Continuity.keyTtlMs, 30 * 24 * 60 * 60 * 1000)
        let marker = Continuity.Marker(keyGeneration: 3, signedAtMs: 1_000)
        XCTAssertNil(Continuity.keyWanted(marker: nil, nowMs: 0))
        XCTAssertEqual(
            Continuity.keyWanted(marker: marker, nowMs: 2_000),
            Continuity.Wanted(keyGeneration: 3, expiresAtMs: 1_000 + Continuity.keyTtlMs))
        XCTAssertNil(Continuity.keyWanted(marker: marker, nowMs: 1_000 + Continuity.keyTtlMs))
        XCTAssertTrue(Continuity.keyUsable(signedAtMs: 1_000, nowMs: 1_000 + Continuity.keyTtlMs - 1))
        XCTAssertFalse(Continuity.keyUsable(signedAtMs: 1_000, nowMs: 1_000 + Continuity.keyTtlMs))
    }

    func testTheMarkerRoundTripsAndIsCleared() {
        let name = "continuity-tests-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        XCTAssertNil(Continuity.readMarker(accountIdB64: "me", defaults: defaults))
        let marker = Continuity.Marker(keyGeneration: 2, signedAtMs: 55)
        Continuity.writeMarker(marker, accountIdB64: "me", defaults: defaults)
        XCTAssertEqual(Continuity.readMarker(accountIdB64: "me", defaults: defaults), marker)
        XCTAssertNil(Continuity.readMarker(accountIdB64: "other", defaults: defaults))
        Continuity.clearMarker(accountIdB64: "me", defaults: defaults)
        XCTAssertNil(Continuity.readMarker(accountIdB64: "me", defaults: defaults))
    }

    func testTheSignedAtOfAnAttestationIsReadFromItsLayout() {
        var bytes = [UInt8](repeating: 0, count: 177)
        bytes[105...112] = [0, 0, 0, 0, 0, 0, 0x01, 0x02]
        XCTAssertEqual(Continuity.attestationSignedAtMs(Data(bytes)), 258)
        XCTAssertNil(Continuity.attestationSignedAtMs(Data(repeating: 0, count: 176)))
    }

    func testAPeerSignsWithTheContinuityKeyOnlyForAKeyAResetRepinnedAndInsideTheWindow() {
        let accountB64 = anna.base64EncodedString()
        let reset = PeerPinStore.AccountPubkeyPin(
            pinnedSignaturePubkeyB64: "new", rotationMarker: 0, acceptedKeyGeneration: 2, resetSignedAtMs: 1_000)
        let plain = PeerPinStore.AccountPubkeyPin(
            pinnedSignaturePubkeyB64: "bob", rotationMarker: 0, acceptedKeyGeneration: 1)
        let pins = [accountB64: reset, "Ym9i": plain]
        XCTAssertEqual(
            Continuity.auth(targetAccountKeyB64: "new", pins: pins, nowMs: 2_000),
            Continuity.Auth(accountId: anna, keyGeneration: 2))
        XCTAssertNil(Continuity.auth(targetAccountKeyB64: "bob", pins: pins, nowMs: 2_000), "never reset")
        XCTAssertNil(Continuity.auth(targetAccountKeyB64: "old", pins: pins, nowMs: 2_000), "a retired key")
        XCTAssertNil(
            Continuity.auth(targetAccountKeyB64: "new", pins: pins, nowMs: 1_000 + Continuity.keyTtlMs),
            "past the window")
    }

    func testTheContinuitySeedIsDerivedFromPublicValuesAndDiffersPerGeneration() throws {
        let a = try continuityReachSeed(accountId: anna, keyGeneration: 2)
        let b = try continuityReachSeed(accountId: anna, keyGeneration: 2)
        let c = try continuityReachSeed(accountId: anna, keyGeneration: 3)
        XCTAssertEqual(a.count, 32)
        XCTAssertEqual(a, b)
        XCTAssertNotEqual(a, c)
    }
}
