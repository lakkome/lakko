// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0045 UX §7 — the automatic re-send of a request a full queue refused.
/// iOS mirror of the web reference `apps/web/src/chat/membership-retry.test.ts`.
///
/// ## The port is REAL, and in three tests it deliberately is not
///
/// `membershipRefusalRetryMaySucceed` is the actual core. The inverted-port
/// tests are what discriminate *asking the core* from *keeping a list that
/// happens to agree with it*: a module that hard-coded `pendingSetFull` /
/// `roleChangePending` agrees with the real core on every input and would pass
/// every test that only ever uses the real core.
///
/// The discrimination evidence for each rule (each probe was RUN, the failure
/// observed, and the probe reverted) is recorded in `MembershipRetry.swift`.
final class MembershipRetryTests: XCTestCase {

    override func setUp() {
        super.setUp()
        // A failed assertion must ABORT this test rather than let the rest of
        // it run against a state the failure already invalidated.
        continueAfterFailure = false
        store.clear(scope: scope)
    }

    override func tearDown() {
        store.clear(scope: scope)
        super.tearDown()
    }

    private let real = membershipRefusalRetryMaySucceed
    private let store = MembershipRetryStore()
    private let scope = "acct-1 group-1"

    private let alice = "QUxJQ0U="
    private let bob = "Qk9C"
    private let carol = "Q0FST0w="

    /// Carol is NOT in the default roster, so an add of her is genuinely
    /// outstanding — an add fixture naming somebody the group already contains
    /// reads as `alreadyApplied` instead, which is a different test.
    private var addCarol: RetryableRequest {
        .add(subjectKeyB64: carol, subjectAccountIdB64: "Y2Fyb2wtYWNjdA==")
    }
    private var removeBob: RetryableRequest { .remove(subjectKeyB64: bob) }
    private var leaveAlice: RetryableRequest { .leave(subjectKeyB64: alice) }
    private var makeBobAdmin: RetryableRequest {
        .setAdmin(subjectKeyB64: bob, makeAdmin: true)
    }
    private var transferToBob: RetryableRequest {
        .transferOwnership(subjectKeyB64: bob, outgoing: .admin)
    }
    /// Carol asked to join via a `/g/` link and an admin's approve was refused.
    private var approveCarol: RetryableRequest { .approveJoin(subjectKeyB64: carol) }

    /// A group of Alice (owner, leaf 0) and Bob (leaf 1), nothing pending, with
    /// Carol's join request retained in the quarantine.
    private func snapshot(
        roster: [RetryRosterMember]? = [
            RetryRosterMember(leafIndex: 0, signatureKeyB64: "QUxJQ0U="),
            RetryRosterMember(leafIndex: 1, signatureKeyB64: "Qk9C"),
        ],
        pendingKeys: [String] = [],
        pendingCount: Int = 0,
        roleTable: RetryRoleTable? = RetryRoleTable(ownerKeyB64: "QUxJQ0U=", adminKeysB64: []),
        membershipEnded: Bool = false,
        retainedJoinKeys: [String]? = ["Q0FST0w="]
    ) -> RetryGroupSnapshot {
        RetryGroupSnapshot(
            roster: roster,
            pendingKeys: pendingKeys,
            pendingCount: pendingCount,
            roleTable: roleTable,
            membershipEnded: membershipEnded,
            retainedJoinKeys: retainedJoinKeys
        )
    }

    private func armed(_ request: RetryableRequest, _ count: Int = 64) -> ArmedRetry {
        ArmedRetry(request: request, reason: .pendingSetFull, pendingCountAtRefusal: count)
    }

    // MARK: - 1. Arming — the core decides, and only the core

    func testArmsOnEachOfTheCoresOwnRetryLaterReasons() {
        for reason in [FfiRefusalReason.pendingSetFull, .roleChangePending] {
            XCTAssertTrue(real(reason))
            let out = armedRetry(
                for: FfiError.MembershipPendingSetFull(message: "core prose"),
                request: addCarol,
                pendingCountAtRefusal: 64,
                reason: reason,
                retryMaySucceed: real
            )
            XCTAssertNotNil(out, "\(reason)")
            XCTAssertEqual(out?.pendingCountAtRefusal, 64)
        }
    }

    func testNeverArmsOnADoNotRetryOrNotPermittedReason() {
        for reason in allMembershipRefusalReasons where !real(reason) {
            XCTAssertNil(
                armedRetry(
                    for: FfiError.Group(message: "irrelevant"),
                    request: addCarol,
                    pendingCountAtRefusal: 64,
                    reason: reason,
                    retryMaySucceed: real
                ),
                "\(reason) must never arm"
            )
        }
    }

    func testNeverArmsOnAnErrorThatCarriesNoReasonAtAll() {
        // The whole production path: `membershipRefusalReason(of:)` returns nil
        // for a non-refusal, and this must then arm nothing.
        XCTAssertNil(
            armedRetry(
                for: ChatError.argument("socket closed"),
                request: addCarol,
                pendingCountAtRefusal: 64,
                retryMaySucceed: real
            )
        )
    }

    func testFindsTheReasonThroughTheWrapperTheRepositoryThrows() {
        let inner = FfiError.MembershipPendingSetFull(message: "core prose")
        let outer = ChatError.wrapped("addMemberToGroup failed", underlying: inner)
        XCTAssertNotNil(
            armedRetry(
                for: outer,
                request: addCarol,
                pendingCountAtRefusal: 64,
                retryMaySucceed: real
            )
        )
    }

    /// The probe: prove the module ASKS rather than keeping its own list. A
    /// hard-coded module agrees with the real core on every input and passes
    /// every test above.
    func testItFollowsThePortEvenWhenThePortIsWrong() {
        let invertedTrue: (FfiRefusalReason) -> Bool = { $0 == .addNotOwnerOrAdmin }
        XCTAssertNotNil(
            armedRetry(
                for: FfiError.MembershipAddNotOwnerOrAdmin(message: "x"),
                request: addCarol,
                pendingCountAtRefusal: 64,
                retryMaySucceed: invertedTrue
            ),
            "a port that says a permission refusal is retryable must be believed"
        )

        let invertedFalse: (FfiRefusalReason) -> Bool = { _ in false }
        XCTAssertNil(
            armedRetry(
                for: FfiError.MembershipPendingSetFull(message: "x"),
                request: addCarol,
                pendingCountAtRefusal: 64,
                retryMaySucceed: invertedFalse
            ),
            "a port that says pendingSetFull is NOT retryable must be believed"
        )
    }

    // MARK: - 2. The trigger — the queue drained, and never a clock

    func testWaitsWhileTheQueueHasNotDrained() {
        XCTAssertEqual(retryVerdict(armed(addCarol), snapshot(pendingCount: 64)), .wait)
    }

    func testWaitsWhenTheQueueHasGrown() {
        XCTAssertEqual(retryVerdict(armed(addCarol), snapshot(pendingCount: 70)), .wait)
    }

    func testFiresOnceTheQueueIsStrictlySmallerThanAtTheRefusal() {
        XCTAssertEqual(retryVerdict(armed(addCarol), snapshot(pendingCount: 63)), .fire)
        XCTAssertEqual(retryVerdict(armed(addCarol), snapshot(pendingCount: 0)), .fire)
    }

    func testCannotFireFromThePassageOfTimeAlone() {
        let a = armed(addCarol)
        let view = snapshot(pendingCount: 64)
        for _ in 0..<5 {
            XCTAssertEqual(retryVerdict(a, view), .wait)
        }
    }

    func testClaimsNothingOnATornRosterRead() {
        XCTAssertEqual(
            retryVerdict(armed(addCarol), snapshot(roster: nil, pendingCount: 0)),
            .wait
        )
    }

    // MARK: - 3. Stopping conditions — every one a state, none a count

    func testDropsAnAddOnceThePersonIsInTheRoster() {
        let view = snapshot(
            roster: [
                RetryRosterMember(leafIndex: 0, signatureKeyB64: alice),
                RetryRosterMember(leafIndex: 1, signatureKeyB64: bob),
                RetryRosterMember(leafIndex: 2, signatureKeyB64: carol),
            ],
            pendingCount: 0
        )
        XCTAssertEqual(retryVerdict(armed(addCarol), view), .drop(.alreadyApplied))
    }

    func testDropsARemoveOnceThePersonHasLeftTheRoster() {
        let view = snapshot(
            roster: [RetryRosterMember(leafIndex: 0, signatureKeyB64: alice)],
            pendingCount: 0
        )
        XCTAssertEqual(retryVerdict(armed(removeBob), view), .drop(.alreadyApplied))
    }

    func testDropsALeaveOnceOurOwnLeafHasGone() {
        let view = snapshot(
            roster: [RetryRosterMember(leafIndex: 1, signatureKeyB64: bob)],
            pendingCount: 0
        )
        XCTAssertEqual(retryVerdict(armed(leaveAlice), view), .drop(.alreadyApplied))
    }

    func testDropsAPromotionOnceTheLiveTableSaysIt() {
        let view = snapshot(
            pendingCount: 0,
            roleTable: RetryRoleTable(ownerKeyB64: alice, adminKeysB64: [bob])
        )
        XCTAssertEqual(retryVerdict(armed(makeBobAdmin), view), .drop(.alreadyApplied))
    }

    func testDropsADemotionOnceTheMemberIsNoLongerAnAdmin() {
        let demote = RetryableRequest.setAdmin(subjectKeyB64: bob, makeAdmin: false)
        XCTAssertEqual(
            retryVerdict(
                armed(demote),
                snapshot(pendingCount: 0, roleTable: RetryRoleTable(ownerKeyB64: alice, adminKeysB64: []))
            ),
            .drop(.alreadyApplied)
        )
        XCTAssertEqual(
            retryVerdict(
                armed(demote),
                snapshot(
                    pendingCount: 64,
                    roleTable: RetryRoleTable(ownerKeyB64: alice, adminKeysB64: [bob])
                )
            ),
            .wait
        )
    }

    func testDropsATransferOnceTheSuccessorHoldsTheGroup() {
        let view = snapshot(
            pendingCount: 0,
            roleTable: RetryRoleTable(ownerKeyB64: bob, adminKeysB64: [alice])
        )
        XCTAssertEqual(retryVerdict(armed(transferToBob), view), .drop(.alreadyApplied))
    }

    func testKeepsWaitingOnATransferTheLiveTableHasNotYetMade() {
        XCTAssertEqual(retryVerdict(armed(transferToBob), snapshot(pendingCount: 64)), .wait)
    }

    func testDropsOnceTheRequestIsItselfWaitingInThePendingSet() {
        XCTAssertEqual(
            retryVerdict(
                armed(addCarol),
                snapshot(pendingKeys: ["add:\(carol)"], pendingCount: 1)
            ),
            .drop(.alreadyWaiting)
        )
    }

    func testResolvesAPendingRemoveThroughTheRosterRatherThanAStoredLeaf() {
        XCTAssertEqual(
            retryVerdict(armed(removeBob), snapshot(pendingKeys: ["remove:1"], pendingCount: 1)),
            .drop(.alreadyWaiting)
        )
        // A pending remove of somebody ELSE's leaf is not ours.
        XCTAssertEqual(
            retryVerdict(armed(removeBob), snapshot(pendingKeys: ["remove:7"], pendingCount: 1)),
            .fire
        )
    }

    func testDropsARoleChangeAlreadyWaitingWhateverItSays() {
        XCTAssertEqual(
            retryVerdict(
                armed(makeBobAdmin),
                snapshot(pendingKeys: ["role-change"], pendingCount: 1)
            ),
            .drop(.alreadyWaiting)
        )
    }

    func testDropsEverythingOnceOurOwnMembershipHasEnded() {
        XCTAssertEqual(
            retryVerdict(armed(addCarol), snapshot(pendingCount: 0, membershipEnded: true)),
            .drop(.membershipEnded)
        )
    }

    func testDropsARoleChangeInAGroupThatHasLostItsRoleTable() {
        XCTAssertEqual(
            retryVerdict(armed(makeBobAdmin), snapshot(pendingCount: 0, roleTable: nil)),
            .drop(.noRoleTable)
        )
    }

    // MARK: - 3b. Approving an external join — the arm the core made possible

    func testArmsOnAQueueFullApproveRefusal() {
        let out = armedApproveJoinRetry(
            for: FfiError.MembershipPendingSetFull(message: "core prose"),
            subjectKeyB64: carol,
            pendingCountAtRefusal: 64,
            retained: true,
            retryMaySucceed: real
        )
        XCTAssertEqual(out?.request, approveCarol)
        XCTAssertEqual(out?.pendingCountAtRefusal, 64)
    }

    /// Every reason `approve_external_join` can produce besides the queue-full
    /// one, asserted against the CORE's own answer rather than a list written
    /// here — so a core that reclassified one fails this test instead of
    /// silently changing what the client does.
    func testNeverArmsOnAnApproveRefusalTheCoreDoesNotCallRetryable() {
        for reason: FfiRefusalReason in [
            .noRoleTable, .noSuchJoinRequest, .joinerAlreadyAMember,
            .addNotOwnerOrAdmin, .notAMember,
        ] {
            XCTAssertFalse(real(reason), "\(reason)")
            XCTAssertNil(
                armedApproveJoinRetry(
                    for: FfiError.Group(message: "irrelevant"),
                    subjectKeyB64: carol,
                    pendingCountAtRefusal: 64,
                    retained: true,
                    reason: reason,
                    retryMaySucceed: real
                ),
                "\(reason) must never arm"
            )
        }
    }

    /// The state before the core fix: `approve_external_join` flattened the
    /// typed error into prose, so this is what EVERY approve refusal looked
    /// like. An absent reason arms nothing, then and now.
    func testNeverArmsOnAnApproveThatFailedWithNoReasonAtAll() {
        XCTAssertNil(
            armedApproveJoinRetry(
                for: FfiError.Group(message: "external join approve refused: proposal refused"),
                subjectKeyB64: carol,
                pendingCountAtRefusal: 64,
                retained: true,
                retryMaySucceed: real
            )
        )
    }

    func testFindsTheApproveReasonThroughTheWrapperTheRepositoryThrows() {
        let inner = FfiError.MembershipPendingSetFull(message: "core prose")
        XCTAssertNotNil(
            armedApproveJoinRetry(
                for: ChatError.wrapped("approveExternalJoin failed", underlying: inner),
                subjectKeyB64: carol,
                pendingCountAtRefusal: 64,
                retained: true,
                retryMaySucceed: real
            )
        )
    }

    func testTheApproveArmFollowsThePortEvenWhenThePortIsWrong() {
        let invertedTrue: (FfiRefusalReason) -> Bool = { $0 == .noSuchJoinRequest }
        XCTAssertNotNil(
            armedApproveJoinRetry(
                for: FfiError.MembershipNoSuchJoinRequest(message: "x"),
                subjectKeyB64: carol,
                pendingCountAtRefusal: 64,
                retained: true,
                retryMaySucceed: invertedTrue
            ),
            "a port that says a missing-request refusal is retryable must be believed"
        )
        let invertedFalse: (FfiRefusalReason) -> Bool = { _ in false }
        XCTAssertNil(
            armedApproveJoinRetry(
                for: FfiError.MembershipPendingSetFull(message: "x"),
                subjectKeyB64: carol,
                pendingCountAtRefusal: 64,
                retained: true,
                retryMaySucceed: invertedFalse
            ),
            "a port that says pendingSetFull is NOT retryable must be believed"
        )
    }

    /// The rule this asserts is ``externalJoinAffordance(retained:)``'s, not
    /// one restated here: an unretained request has nothing for
    /// `approveExternalJoin` to look up, so a retry for it could only ever be
    /// refused. Written against the affordance rather than the literal `false`
    /// so a future change to what "actionable" means carries the retry with it.
    func testArmsExactlyWhenTheAffordanceSaysTheRequestIsActionable() {
        for retained in [true, false] {
            let actionable = externalJoinAffordance(retained: retained) == .actionable
            let out = armedApproveJoinRetry(
                for: FfiError.MembershipPendingSetFull(message: "x"),
                subjectKeyB64: carol,
                pendingCountAtRefusal: 64,
                retained: retained,
                retryMaySucceed: real
            )
            XCTAssertEqual(out != nil, actionable, "retained=\(retained)")
        }
    }

    func testKeepsTheApproveSlotApartFromAnAddOfTheSamePerson() {
        XCTAssertNotEqual(approveCarol.key, addCarol.key)
        XCTAssertTrue(approveCarol.key.contains(carol))
        store.arm(scope: scope, armed(approveCarol))
        store.arm(scope: scope, armed(addCarol))
        XCTAssertEqual(store.armed(scope: scope).count, 2)
        // A manual approve tap disarms the approve record and leaves the add.
        XCTAssertNotNil(store.claim(scope: scope, key: approveCarol.key))
        XCTAssertNil(store.claim(scope: scope, key: approveCarol.key))
        XCTAssertEqual(store.armed(scope: scope).count, 1)
    }

    func testTheApproveFiresOnceTheQueueDrainedAndNotBefore() {
        XCTAssertEqual(retryVerdict(armed(approveCarol), snapshot(pendingCount: 64)), .wait)
        XCTAssertEqual(retryVerdict(armed(approveCarol), snapshot(pendingCount: 63)), .fire)
    }

    func testDropsAnApproveOnceTheJoinerIsAMember() {
        let view = snapshot(
            roster: [
                RetryRosterMember(leafIndex: 0, signatureKeyB64: alice),
                RetryRosterMember(leafIndex: 1, signatureKeyB64: bob),
                RetryRosterMember(leafIndex: 2, signatureKeyB64: carol),
            ],
            pendingCount: 0,
            // The core reaps a joined request out of the quarantine, so this is
            // empty in reality; supplied non-empty here so the test can only
            // pass by reading the ROSTER.
            retainedJoinKeys: [carol]
        )
        XCTAssertEqual(retryVerdict(armed(approveCarol), view), .drop(.alreadyApplied))
    }

    /// An approved external join IS an ordinary admin-authored `Add` keyed on
    /// the joiner's signature key, so it lands in the pending set under exactly
    /// the key an add of the same person would.
    func testDropsAnApproveOnceAnAddForTheJoinerIsAlreadyWaiting() {
        XCTAssertEqual(
            retryVerdict(
                armed(approveCarol),
                snapshot(pendingKeys: ["add:\(carol)"], pendingCount: 1)
            ),
            .drop(.alreadyWaiting)
        )
    }

    /// Nothing else in the group changed: Carol is not a member, no Add is
    /// pending, the queue drained. The core simply no longer holds the request
    /// — anything that reloads the group empties the in-memory quarantine while
    /// this record survives — and re-issuing would be refused
    /// `noSuchJoinRequest`.
    func testDropsAnApproveOnceTheRequestHasLeftTheRetainedQuarantine() {
        XCTAssertEqual(
            retryVerdict(armed(approveCarol), snapshot(pendingCount: 0, retainedJoinKeys: [])),
            .drop(.joinRequestGone)
        )
        // Somebody ELSE's retained request is not ours.
        XCTAssertEqual(
            retryVerdict(armed(approveCarol), snapshot(pendingCount: 0, retainedJoinKeys: [bob])),
            .drop(.joinRequestGone)
        )
    }

    func testClaimsNothingOnATornReadOfTheRetainedSet() {
        XCTAssertEqual(
            retryVerdict(armed(approveCarol), snapshot(pendingCount: 0, retainedJoinKeys: nil)),
            .wait
        )
    }

    /// A torn or empty quarantine says nothing about an add, a removal or a
    /// role change, so it must not stop one.
    func testDoesNotReadTheRetainedSetForAnyOtherKindOfRequest() {
        for request in [addCarol, removeBob, makeBobAdmin] {
            XCTAssertEqual(
                retryVerdict(armed(request), snapshot(pendingCount: 0, retainedJoinKeys: nil)),
                .fire,
                "\(request)"
            )
            XCTAssertEqual(
                retryVerdict(armed(request), snapshot(pendingCount: 0, retainedJoinKeys: [])),
                .fire,
                "\(request)"
            )
        }
    }

    func testDropsAnApproveInAGroupThatHasLostItsRoleTable() {
        XCTAssertEqual(
            retryVerdict(armed(approveCarol), snapshot(pendingCount: 0, roleTable: nil)),
            .drop(.noRoleTable)
        )
    }

    func testDropsAnApproveOnceOurOwnMembershipHasEnded() {
        XCTAssertEqual(
            retryVerdict(armed(approveCarol), snapshot(pendingCount: 0, membershipEnded: true)),
            .drop(.membershipEnded)
        )
    }

    /// REAL threads, like the generic probe: the approve record goes through
    /// the same lock, and a serial queue would mask a broken one.
    func testExactlyOneThreadWinsTheApproveRecordAgainstAConcurrentManualTap() {
        for _ in 0..<200 {
            store.arm(scope: scope, armed(approveCarol))
            let winners = NSMutableArray()
            let barrier = NSLock()
            DispatchQueue.concurrentPerform(iterations: 8) { _ in
                if let got = self.store.claim(scope: self.scope, key: self.approveCarol.key) {
                    barrier.lock()
                    winners.add(got.request.key)
                    barrier.unlock()
                }
            }
            XCTAssertEqual(winners.count, 1, "exactly one caller may own the record")
        }
    }

    // MARK: - 4. Exactly-once

    func testHandsTheRecordToExactlyOneOfTwoClaims() {
        store.arm(scope: scope, armed(addCarol))
        XCTAssertNotNil(store.claim(scope: scope, key: addCarol.key))
        XCTAssertNil(store.claim(scope: scope, key: addCarol.key))
    }

    /// The claim runs under a real lock, so this drives it from REAL threads —
    /// a serial queue or a confined actor would mask a broken lock as
    /// thoroughly as no concurrency at all.
    func testExactlyOneOfManyConcurrentThreadsWinsTheClaim() {
        for _ in 0..<200 {
            store.arm(scope: scope, armed(addCarol))
            let winners = NSMutableArray()
            let barrier = NSLock()
            DispatchQueue.concurrentPerform(iterations: 8) { _ in
                if let got = self.store.claim(scope: self.scope, key: self.addCarol.key) {
                    barrier.lock()
                    winners.add(got.request.key)
                    barrier.unlock()
                }
            }
            XCTAssertEqual(winners.count, 1, "exactly one thread may own the record")
        }
    }

    func testHoldsAtMostOneRecordPerRequest() {
        store.arm(scope: scope, armed(addCarol, 64))
        store.arm(scope: scope, armed(addCarol, 12))
        XCTAssertEqual(store.armed(scope: scope).count, 1)
        XCTAssertEqual(store.armed(scope: scope).first?.pendingCountAtRefusal, 12)
    }

    func testAManualTapDisarmsTheRecord() {
        store.arm(scope: scope, armed(addCarol))
        XCTAssertNotNil(store.claim(scope: scope, key: addCarol.key))
        XCTAssertNil(store.claim(scope: scope, key: addCarol.key))
        XCTAssertTrue(store.armed(scope: scope).isEmpty)
    }

    func testKeepsDifferentRequestsApart() {
        store.arm(scope: scope, armed(addCarol))
        store.arm(scope: scope, armed(removeBob))
        XCTAssertEqual(store.armed(scope: scope).count, 2)
        _ = store.claim(scope: scope, key: addCarol.key)
        XCTAssertEqual(store.armed(scope: scope).count, 1)
    }

    func testScopesRecordsPerGroup() {
        store.arm(scope: scope, armed(addCarol))
        XCTAssertTrue(store.armed(scope: "acct-1 group-2").isEmpty)
        XCTAssertEqual(store.armed(scope: scope).count, 1)
    }

    func testSharesOneArmedSlotBetweenTheTwoRoleChangeShapes() {
        XCTAssertEqual(makeBobAdmin.key, transferToBob.key)
        store.arm(scope: scope, armed(makeBobAdmin))
        store.arm(scope: scope, armed(transferToBob))
        XCTAssertEqual(store.armed(scope: scope).count, 1)
        if case .transferOwnership = store.armed(scope: scope)[0].request {
            // the newer intent won
        } else {
            XCTFail("the newer role-change intent must supersede the older one")
        }
    }

    func testKeysARemoveOnTheSignatureKeyNeverOnAReusableLeaf() {
        XCTAssertTrue(removeBob.key.contains(bob))
        XCTAssertNotEqual(removeBob.key, "remove:1")
    }

    // MARK: - ADR-0050 2.5a — the superseded re-request

    // People: Alice (owner) on leaf 0; Bob on leaves 1 and 2 (two devices);
    // Carol is not a member. Person keys are account keys; leaf keys name
    // devices.
    private let personA = "QQ=="
    private let personB = "Qg=="
    private let personC = "Qw=="

    private func leaf(_ i: UInt32, _ leafKey: String, _ person: String) -> SupersededLeaf {
        SupersededLeaf(leafIndex: i, leafKeyB64: leafKey, personKeyB64: person)
    }

    private var defaultLeaves: [SupersededLeaf] {
        [leaf(0, "TEE=", personA), leaf(1, "TEIx", personB), leaf(2, "TEIy", personB)]
    }

    private func superseded(
        epoch: UInt64 = 6,
        leaves: [SupersededLeaf]?? = nil,
        roleTable: RetryRoleTable?? = nil,
        pendingKeys: [String] = [],
        membershipEnded: Bool = false,
        retainedJoinKeys: [String]? = []
    ) -> SupersededSnapshot {
        SupersededSnapshot(
            epoch: epoch,
            leaves: leaves ?? defaultLeaves,
            roleTable: roleTable ?? RetryRoleTable(ownerKeyB64: personA, adminKeysB64: []),
            pendingKeys: pendingKeys,
            membershipEnded: membershipEnded,
            retainedJoinKeys: retainedJoinKeys)
    }

    private func intent(_ request: MembershipIntent.Request, at epoch: UInt64 = 5)
        -> MembershipIntent
    {
        MembershipIntent(request: request, epoch: epoch)
    }

    // ---- add

    func testAnAddThatTheCommitDroppedIsReRequested() {
        let add = intent(.add(personKeyB64: personC, accountIdB64: "YWM=", leafKeysB64: ["TEM="]))
        XCTAssertEqual(supersededVerdict(add, superseded()), .reRequest)
    }

    /// Landed is read off the PERSON: Carol joined on a device other than the
    /// one the dropped proposal named, and that still answers the ask.
    func testAnAddLandsWhenThePersonIsInTheGroupOnAnyDevice() {
        let add = intent(.add(personKeyB64: personC, accountIdB64: "YWM=", leafKeysB64: ["TEM="]))
        let leaves = defaultLeaves + [leaf(3, "TEMy", personC)]
        XCTAssertEqual(supersededVerdict(add, superseded(leaves: leaves)), .landed)
        XCTAssertEqual(supersededResolution(.landed), .forget)
    }

    func testAnAddStillPendingIsKept() {
        let add = intent(.add(personKeyB64: personC, accountIdB64: "YWM=", leafKeysB64: ["TEM="]))
        XCTAssertEqual(supersededVerdict(add, superseded(pendingKeys: ["add:TEM="])), .keep)
    }

    /// No timer: with no commit since the request, nothing is superseded,
    /// whatever the pending set says.
    func testNothingIsSupersededWithoutACommit() {
        let add = intent(.add(personKeyB64: personC, accountIdB64: "YWM=", leafKeysB64: ["TEM="]))
        XCTAssertEqual(supersededVerdict(add, superseded(epoch: 5)), .keep)
        let remove = intent(.remove(personKeyB64: personB))
        XCTAssertEqual(supersededVerdict(remove, superseded(epoch: 5)), .keep)
    }

    // ---- remove, by signature key, through a reused leaf

    /// Bob left; leaf 1 now belongs to Carol. A leaf-keyed record would read
    /// "leaf 1 is still there" and re-request removing CAROL. Keyed on Bob's
    /// key, it is landed.
    func testARemovalLandsWhenTheSubjectLeftEvenThoughItsLeafWasReused() {
        let remove = intent(.remove(personKeyB64: personB))
        let leaves = [leaf(0, "TEE=", personA), leaf(1, "TEM=", personC)]
        XCTAssertEqual(supersededVerdict(remove, superseded(leaves: leaves)), .landed)
    }

    /// Bob is still here (now on leaves 3 and 4); the pending removal of leaf 1
    /// is someone else's request about whoever holds leaf 1 now. It is NOT
    /// ours, so ours is re-requested.
    func testAPendingRemovalOfAReusedLeafIsNotMistakenForOurs() {
        let remove = intent(.remove(personKeyB64: personB))
        let leaves = [
            leaf(0, "TEE=", personA), leaf(1, "TEM=", personC),
            leaf(3, "TEIx", personB), leaf(4, "TEIy", personB),
        ]
        XCTAssertEqual(
            supersededVerdict(remove, superseded(leaves: leaves, pendingKeys: ["remove:1"])),
            .reRequest)
        XCTAssertEqual(
            supersededVerdict(remove, superseded(leaves: leaves, pendingKeys: ["remove:4"])),
            .keep)
    }

    // ---- role change as a delta

    func testARoleChangeLandsWhenTheTableSaysWhatWasAsked() {
        let promote = intent(.setAdmin(personKeyB64: personB, makeAdmin: true))
        let table = RetryRoleTable(ownerKeyB64: personA, adminKeysB64: [personB])
        XCTAssertEqual(supersededVerdict(promote, superseded(roleTable: table)), .landed)
        let demote = intent(.setAdmin(personKeyB64: personB, makeAdmin: false))
        XCTAssertEqual(supersededVerdict(demote, superseded()), .landed)
    }

    /// Somebody else's change landed in between: the table now names Carol as
    /// admin. Our promote of Bob is re-requested, and the table it proposes is
    /// the CURRENT one plus our delta — it keeps the admin that landed
    /// meanwhile instead of re-proposing the table we saw.
    func testARoleChangeReRequestDoesNotRevertAChangeThatLandedMeanwhile() {
        let promote = intent(.setAdmin(personKeyB64: personB, makeAdmin: true))
        let table = RetryRoleTable(ownerKeyB64: personA, adminKeysB64: [personC])
        XCTAssertEqual(supersededVerdict(promote, superseded(roleTable: table)), .reRequest)
        let carol = Data([0x43])
        let bob = Data([0x42])
        XCTAssertEqual(
            adminsAfterToggle(current: [carol], subject: bob, makeAdmin: true), [carol, bob])
        XCTAssertEqual(
            adminsAfterToggle(current: [carol, bob], subject: bob, makeAdmin: false), [carol])
    }

    func testARoleChangeAboutSomeoneWhoLeftStopsSilently() {
        let promote = intent(.setAdmin(personKeyB64: personC, makeAdmin: true))
        XCTAssertEqual(supersededVerdict(promote, superseded()), .subjectGone)
        XCTAssertEqual(supersededResolution(.subjectGone), .forget)
        let transfer = intent(.transferOwnership(toPersonKeyB64: personC, outgoing: .admin))
        XCTAssertEqual(supersededVerdict(transfer, superseded()), .subjectGone)
    }

    func testARoleChangeWaitsWhileAnyRoleChangeIsPending() {
        let promote = intent(.setAdmin(personKeyB64: personB, makeAdmin: true))
        XCTAssertEqual(
            supersededVerdict(promote, superseded(pendingKeys: ["role-change"])), .keep)
    }

    func testATransferLandsWhenTheSubjectOwnsTheGroup() {
        let transfer = intent(.transferOwnership(toPersonKeyB64: personB, outgoing: .member))
        let table = RetryRoleTable(ownerKeyB64: personB, adminKeysB64: [])
        XCTAssertEqual(supersededVerdict(transfer, superseded(roleTable: table)), .landed)
        XCTAssertEqual(supersededVerdict(transfer, superseded()), .reRequest)
    }

    // ---- leave, and the end of our own membership

    func testALeaveLandsWhenOurMembershipEnded() {
        let leave = intent(.leave(ownPersonKeyB64: personB))
        XCTAssertEqual(supersededVerdict(leave, superseded(membershipEnded: true)), .landed)
        XCTAssertEqual(supersededVerdict(leave, superseded()), .reRequest)
        XCTAssertEqual(
            supersededVerdict(leave, superseded(pendingKeys: ["leave:2"])), .keep)
    }

    /// Every other request stops once we are out, and is listed.
    func testEveryOtherRequestStopsAndIsListedWhenOurMembershipEnded() {
        let remove = intent(.remove(personKeyB64: personB))
        XCTAssertEqual(
            supersededVerdict(remove, superseded(membershipEnded: true)), .membershipEnded)
        XCTAssertEqual(supersededResolution(.membershipEnded), .forgetAndList)
    }

    // ---- approved join

    func testAnApprovedJoinIsReApprovedOnlyWhileTheCoreHoldsTheRequest() {
        let approve = intent(.approveJoin(joinerKeyB64: "Sg=="))
        XCTAssertEqual(
            supersededVerdict(approve, superseded(retainedJoinKeys: ["Sg=="])), .reRequest)
        // The approve consumed the retained KeyPackage; nothing here can
        // re-derive it, so the row tells the admin to ask the joiner.
        XCTAssertEqual(
            supersededVerdict(approve, superseded(retainedJoinKeys: [])), .cannotReRequest)
        XCTAssertEqual(supersededResolution(.cannotReRequest), .forgetAndList)
        XCTAssertEqual(supersededVerdict(approve, superseded(retainedJoinKeys: nil)), .keep)
        let leaves = defaultLeaves + [leaf(5, "Sg==", "Sg==")]
        XCTAssertEqual(supersededVerdict(approve, superseded(leaves: leaves)), .landed)
    }

    // ---- own devices

    func testOwnDeviceRequestsAreJudgedOnTheDeviceKey() {
        let add = intent(.ownDeviceAdd(deviceKeyB64: "TEIz"))
        XCTAssertEqual(supersededVerdict(add, superseded()), .reRequest)
        XCTAssertEqual(
            supersededVerdict(add, superseded(leaves: defaultLeaves + [leaf(7, "TEIz", personB)])),
            .landed)
        let retire = intent(.ownDeviceRemove(deviceKeyB64: "TEIy"))
        XCTAssertEqual(supersededVerdict(retire, superseded()), .reRequest)
        XCTAssertEqual(
            supersededVerdict(retire, superseded(pendingKeys: ["remove:2"])), .keep)
        XCTAssertEqual(
            supersededVerdict(retire, superseded(leaves: [leaf(0, "TEE=", personA)])), .landed)
    }

    func testATornRosterClaimsNothing() {
        let remove = intent(.remove(personKeyB64: personB))
        XCTAssertEqual(supersededVerdict(remove, superseded(leaves: .some(nil))), .keep)
    }

    // ---- a failed re-request

    /// The authority is gone: the typed refusal's disposition stops it and it
    /// is listed. Queue full: left for the next merged commit, which is what
    /// drains a queue.
    func testAFailedReRequestStopsOnlyOnAnAuthorityRefusal() {
        let disposition = { membershipRefusalDisposition(reason: $0) }
        XCTAssertEqual(
            reRequestFailureResolution(
                FfiError.MembershipRemoveNotOwnerOrAdmin(message: "x"), disposition: disposition),
            .forgetAndList)
        XCTAssertEqual(
            reRequestFailureResolution(
                FfiError.MembershipNotOwner(message: "x"), disposition: disposition),
            .forgetAndList)
        XCTAssertEqual(
            reRequestFailureResolution(
                FfiError.MembershipPendingSetFull(message: "x"), disposition: disposition),
            .keep)
        XCTAssertEqual(
            reRequestFailureResolution(FfiError.Group(message: "x"), disposition: disposition),
            .keep)
        XCTAssertEqual(
            reRequestFailureResolution(
                ChatError.argument("network down"), disposition: disposition),
            .keep)
    }

    /// The module ASKS the disposition rather than naming reasons: inverting
    /// the port inverts the answer.
    func testAFailedReRequestFollowsTheDispositionPort() {
        XCTAssertEqual(
            reRequestFailureResolution(
                FfiError.MembershipPendingSetFull(message: "x"),
                disposition: { _ in .notPermitted }),
            .forgetAndList)
        XCTAssertEqual(
            reRequestFailureResolution(
                FfiError.MembershipNotOwner(message: "x"),
                disposition: { _ in .retryLater }),
            .keep)
    }

    // ---- the trigger

    /// Nothing fires without a commit: the same epoch twice evaluates once.
    func testTheTriggerFiresOnlyWhenTheEpochMoved() {
        var trigger = SupersededTrigger()
        XCTAssertTrue(trigger.take(groupIdHex: "g", epoch: 5), "the first observation of a session")
        XCTAssertFalse(trigger.take(groupIdHex: "g", epoch: 5))
        XCTAssertFalse(trigger.take(groupIdHex: "g", epoch: 5))
        XCTAssertTrue(trigger.take(groupIdHex: "g", epoch: 6))
        XCTAssertFalse(trigger.take(groupIdHex: "g", epoch: 6))
        XCTAssertTrue(trigger.take(groupIdHex: "other", epoch: 6), "per group")
    }

    // ---- knocks are read by role, not by rank

    /// Since 2.5a a plain member holds a committer rank while it has a device
    /// admission to sweep. Knock reading must not follow the rank: approving a
    /// join is still owner-or-admin, and the gate reads the role alone.
    func testKnocksAreReadByOwnersAndAdminsOnly() {
        XCTAssertTrue(ChatRepository.readsKnocks(role: .owner))
        XCTAssertTrue(ChatRepository.readsKnocks(role: .admin))
        XCTAssertFalse(ChatRepository.readsKnocks(role: .member))
        XCTAssertFalse(ChatRepository.readsKnocks(role: nil))
    }
}
