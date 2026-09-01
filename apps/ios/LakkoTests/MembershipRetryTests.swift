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

    /// A group of Alice (owner, leaf 0) and Bob (leaf 1), nothing pending.
    private func snapshot(
        roster: [RetryRosterMember]? = [
            RetryRosterMember(leafIndex: 0, signatureKeyB64: "QUxJQ0U="),
            RetryRosterMember(leafIndex: 1, signatureKeyB64: "Qk9C"),
        ],
        pendingKeys: [String] = [],
        pendingCount: Int = 0,
        roleTable: RetryRoleTable? = RetryRoleTable(ownerKeyB64: "QUxJQ0U=", adminKeysB64: []),
        membershipEnded: Bool = false
    ) -> RetryGroupSnapshot {
        RetryGroupSnapshot(
            roster: roster,
            pendingKeys: pendingKeys,
            pendingCount: pendingCount,
            roleTable: roleTable,
            membershipEnded: membershipEnded
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
}
