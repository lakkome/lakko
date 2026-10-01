// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0045 S4a — the pending-membership-request surface, as
/// `docs/ux/proposal-based-membership.md` specifies it. iOS mirror of the web
/// `chat/pending-requests.test.ts`.
///
/// ## Which of these discriminate (measured by RUNNING the break, quoted
/// verbatim — not predicted)
///
/// Deleting the `guard record.stored else { continue }` line from
/// `foldPendingRequests`:
///   - `testARefusedProposalIsNotAPendingRequest` FAILS with
///     `XCTAssertEqual failed: ("1") is not equal to ("0") - the receipt gate
///     REFUSED it, so no sweep will ever commit it and it is not pending`.
///
/// Making `requestState` return `.pending` for an unobserved request
/// (`guard let observedAtMs else { return .pending }`):
///   - `testAnUnobservedRequestRendersImmediately` FAILS with
///     `XCTAssertEqual failed: ("pending") is not equal to ("waiting") - a
///     request restored from a snapshot is by definition older than any window`.
@MainActor
final class PendingRequestsTests: XCTestCase {

    private let account = "acct-1"
    private let group = "group-1"

    private func makeStore() -> PendingRequestStore {
        PendingRequestStore(defaults: UserDefaults(suiteName: "pending-\(UUID().uuidString)")!)
    }

    private func addRecord(key: UInt8 = 0xAA, senderLeaf: UInt32? = 0, stored: Bool = true)
        -> ProposalRecord
    {
        ProposalRecord(
            senderLeaf: senderLeaf,
            kind: .add(signatureKey: Data(repeating: key, count: 32)),
            stored: stored)
    }

    private func removeRecord(targetLeaf: UInt32, senderLeaf: UInt32? = 0) -> ProposalRecord {
        ProposalRecord(senderLeaf: senderLeaf, kind: .remove(targetLeaf: targetLeaf), stored: true)
    }

    // MARK: - The quiet window

    /// The owner is rank 0 and sweeps its own request immediately, so in the
    /// common case the request lands before anyone could look. A spinner that
    /// appears and vanishes in 300 ms is worse than no spinner: it teaches the
    /// user that membership is slow when it is not.
    func testARequestInsideTheQuietWindowRendersNothing() {
        XCTAssertEqual(requestState(observedAtMs: 1_000, nowMs: 1_100), .pending)
        XCTAssertEqual(
            requestState(observedAtMs: 1_000, nowMs: 1_000 + pendingQuietWindowMs), .waiting)
    }

    /// An UNOBSERVED request — restored from a snapshot rather than seen to
    /// land — renders immediately: it has necessarily been pending since before
    /// this session started, so it is by definition past any window.
    func testAnUnobservedRequestRendersImmediately() {
        XCTAssertEqual(
            requestState(observedAtMs: nil, nowMs: 0), .waiting,
            "a request restored from a snapshot is by definition older than any window"
        )
    }

    // MARK: - The fold

    /// A record the receipt gate refused is a refusal reported as DATA rather
    /// than as a throw — a proposal advances no state, so dropping it unstored
    /// IS the refusal — and it must not surface as a pending request.
    func testARefusedProposalIsNotAPendingRequest() {
        let (requests, liveKeys) = foldPendingRequests(
            records: [addRecord(stored: false)], observations: [:], nowMs: 0)
        XCTAssertEqual(
            requests.count, 0,
            "the receipt gate REFUSED it, so no sweep will ever commit it and it is not pending"
        )
        XCTAssertEqual(liveKeys, [])
    }

    /// The key is derived from what the request IS, never from a leaf index
    /// where a signature key is available: OpenMLS reuses a vacated leaf for the
    /// next Add, so a leaf-keyed record can silently come to name someone else.
    func testAnAddIsKeyedOnTheSignatureKeyNotTheLeaf() {
        XCTAssertEqual(
            proposalKey(addRecord(key: 0x01, senderLeaf: 3)),
            proposalKey(addRecord(key: 0x01, senderLeaf: 9)),
            "the same prospective member requested twice is ONE request, whoever asked"
        )
        XCTAssertNotEqual(
            proposalKey(addRecord(key: 0x01)), proposalKey(addRecord(key: 0x02)))
    }

    /// Two role changes are indistinguishable on the wire: `classify` discards
    /// the `GroupContextExtensions` payload, so a received role-change request
    /// is only describable at group level (UX §11.5).
    func testEveryRoleChangeCollapsesToOneKey() {
        let a = ProposalRecord(senderLeaf: 1, kind: .roleChange, stored: true)
        let b = ProposalRecord(senderLeaf: 7, kind: .roleChange, stored: true)
        XCTAssertEqual(proposalKey(a), proposalKey(b))
    }

    // MARK: - No optimistic update

    /// A member with a pending removal STAYS in the roster, annotated. Dropping
    /// the row would assert they are gone while they are still cryptographically
    /// in the group — still receiving every message, still able to decrypt.
    func testAPendingRemovalAnnotatesTheRowRatherThanDroppingIt() {
        let (requests, _) = foldPendingRequests(
            records: [removeRecord(targetLeaf: 4)],
            observations: ["remove:4": RequestObservation(atMs: 0, own: false)],
            nowMs: pendingQuietWindowMs)
        XCTAssertEqual(
            rowAnnotation(requests, leafIndex: 4, ownLeaf: 9), "Waiting to be removed.")
        XCTAssertNil(
            rowAnnotation(requests, leafIndex: 5, ownLeaf: 9),
            "an untargeted member must carry no annotation")
    }

    /// `leave` is the self-remove that is how leaving works, and it names no
    /// target on the wire — `ProposalKind.leave` "structurally targets the
    /// sender's own leaf and can name no other", so the sender leaf IS it.
    func testALeaveIsAttributedToItsSenderLeaf() {
        let leave = ProposalRecord(senderLeaf: 4, kind: .leave, stored: true)
        let (requests, _) = foldPendingRequests(
            records: [leave],
            observations: ["leave:4": RequestObservation(atMs: 0, own: true)],
            nowMs: pendingQuietWindowMs)
        XCTAssertEqual(rowAnnotation(requests, leafIndex: 4, ownLeaf: 4), "Waiting to leave.")
    }

    /// A person who has been *requested* is not a member and must not appear in
    /// the members list — the same no-optimistic-update rule in reverse.
    func testAPendingAddGetsItsOwnSectionRatherThanARosterRow() {
        let (requests, _) = foldPendingRequests(
            records: [addRecord()],
            observations: ["add:\(base64Encode(Data(repeating: 0xAA, count: 32)))":
                RequestObservation(atMs: 0, own: false)],
            nowMs: pendingQuietWindowMs)
        XCTAssertEqual(waitingToBeAdded(requests).count, 1)
        XCTAssertNil(
            rowAnnotation(requests, leafIndex: 0, ownLeaf: nil),
            "an add targets nobody in the roster, so it annotates no row"
        )
    }

    // MARK: - Visibility (UX §10)

    /// The pending set is a work queue and only owner/admins have a verb over
    /// it. Telling a plain member "Bob is waiting to be removed" tells them
    /// something they cannot act on, about someone else's standing — and in a
    /// group where a removal is a safety action, advertising it is a hazard.
    func testAPlainMemberSeesOnlyTheirOwnRequests() {
        let mine = PendingRequest(
            key: "leave:4", kind: .leave, addSignatureKeyB64: nil, targetLeaf: 4,
            senderLeaf: 4, observedAtMs: 0, own: true, state: .waiting)
        let theirs = PendingRequest(
            key: "remove:7", kind: .remove, addSignatureKeyB64: nil, targetLeaf: 7,
            senderLeaf: 0, observedAtMs: 0, own: false, state: .waiting)
        XCTAssertEqual(visibleRequests([mine, theirs], canManageMembers: true).count, 2)
        XCTAssertEqual(
            visibleRequests([mine, theirs], canManageMembers: false).map(\.key), ["leave:4"],
            "the own-request exception is non-negotiable: without it a self-remove is an "
            + "action with no feedback"
        )
    }

    // MARK: - The notice (UX §5.1)

    func testTheNoticeCountsOnlyWaitingRequestsAndSingularises() {
        let waiting = PendingRequest(
            key: "a", kind: .add, addSignatureKeyB64: nil, targetLeaf: nil, senderLeaf: 0,
            observedAtMs: 0, own: false, state: .waiting)
        var quiet = waiting
        quiet.key = "b"
        quiet.state = .pending

        XCTAssertNil(pendingNotice([quiet]), "nothing past the window renders no notice at all")
        XCTAssertEqual(
            pendingNotice([waiting, quiet])?.count, "1 membership change is waiting to apply.")
        var second = waiting
        second.key = "c"
        XCTAssertEqual(
            pendingNotice([waiting, second])?.count, "2 membership changes are waiting to apply.")
    }

    /// ONE liveness line, not two. ADR-0045 decision 2 gives a waiting request
    /// no deadline, so there is no moment at which the client learns something
    /// new to say: the sentence must be true at ten seconds and at ten days, it
    /// must state INACTION (the only thing the client can observe), and it must
    /// name nobody.
    func testTheLivenessLineNamesNobodyAndPromisesNoDeadline() {
        let waiting = PendingRequest(
            key: "a", kind: .add, addSignatureKeyB64: nil, targetLeaf: nil, senderLeaf: 0,
            observedAtMs: 0, own: false, state: .waiting)
        let line = pendingNotice([waiting])?.liveness ?? ""
        XCTAssertTrue(line.contains("automatically"))
        for forbidden in ["minute", "second", "soon", "shortly", "offline", "stalled", "failed"] {
            XCTAssertFalse(
                line.lowercased().contains(forbidden),
                "the notice must not imply a deadline or diagnose an absent committer, found "
                + "'\(forbidden)' in: \(line)"
            )
        }
    }

    // MARK: - The observation store

    /// The FIRST observation of a key wins: a re-broadcast or a re-request must
    /// not reset its age, or a request that keeps being re-sent would never
    /// mature past the quiet window and would stay invisible forever — exactly
    /// the wait the notice exists to surface.
    func testTheFirstObservationWinsButOwnIsUpgraded() {
        let store = makeStore()
        store.record(accountIdB64: account, groupIdB64: group, key: "k", atMs: 5_000, own: false)
        store.record(accountIdB64: account, groupIdB64: group, key: "k", atMs: 9_000, own: true)
        XCTAssertEqual(
            store.load(accountIdB64: account, groupIdB64: group)["k"],
            RequestObservation(atMs: 5_000, own: true),
            "the age is the first sighting; `own` is upgraded so arrival order cannot lose the "
            + "UX §10 own-request exception"
        )
    }

    func testPruneDropsAnythingNoLongerInTheCoresPendingSet() {
        let store = makeStore()
        store.record(accountIdB64: account, groupIdB64: group, key: "gone", atMs: 1, own: false)
        store.record(accountIdB64: account, groupIdB64: group, key: "live", atMs: 1, own: false)
        store.prune(accountIdB64: account, groupIdB64: group, liveKeys: ["live"])
        XCTAssertEqual(
            Array(store.load(accountIdB64: account, groupIdB64: group).keys), ["live"])
    }

    // MARK: - ADR-0050 2.5a — the intent store

    /// Every kind round-trips through the plist row, epoch included.
    func testEveryIntentKindRoundTrips() {
        let store = makeStore()
        let requests: [MembershipIntent.Request] = [
            .add(personKeyB64: "UA==", accountIdB64: "QQ==", leafKeysB64: ["TDE=", "TDI="]),
            .approveJoin(joinerKeyB64: "Sg=="),
            .remove(personKeyB64: "Ug=="),
            .setAdmin(personKeyB64: "Uw==", makeAdmin: false),
            .transferOwnership(toPersonKeyB64: "VA==", outgoing: .member),
            .leave(ownPersonKeyB64: "Tw=="),
            .ownDeviceAdd(deviceKeyB64: "RA=="),
            .ownDeviceRemove(deviceKeyB64: "RQ=="),
        ]
        for (i, request) in requests.enumerated() {
            store.recordIntent(
                accountIdB64: account, groupIdB64: group,
                MembershipIntent(request: request, epoch: UInt64(10 + i)))
        }
        let loaded = store.intents(accountIdB64: account, groupIdB64: group)
        XCTAssertEqual(loaded.count, requests.count)
        for (i, request) in requests.enumerated() {
            XCTAssertTrue(
                loaded.contains(MembershipIntent(request: request, epoch: UInt64(10 + i))),
                "lost \(request)")
        }
    }

    /// The prune of the observation store runs on every fold and drops a
    /// request the moment it leaves the pending set — which is exactly the
    /// moment a superseded request's intent is needed. It must not touch it.
    func testTheObservationPruneLeavesIntentsAlone() {
        let store = makeStore()
        let intent = MembershipIntent(request: .remove(personKeyB64: "Ug=="), epoch: 4)
        store.recordIntent(accountIdB64: account, groupIdB64: group, intent)
        store.prune(accountIdB64: account, groupIdB64: group, liveKeys: [])
        XCTAssertEqual(store.intents(accountIdB64: account, groupIdB64: group), [intent])
    }

    /// One slot per subject and kind: a demote after a promote of the same
    /// member REPLACES it (the latest ask is the intent); a promote of another
    /// member sits beside it.
    func testALaterAskAboutTheSameSubjectReplacesTheEarlierOne() {
        let store = makeStore()
        store.recordIntent(
            accountIdB64: account, groupIdB64: group,
            MembershipIntent(request: .setAdmin(personKeyB64: "Qg==", makeAdmin: true), epoch: 1))
        store.recordIntent(
            accountIdB64: account, groupIdB64: group,
            MembershipIntent(request: .setAdmin(personKeyB64: "Qg==", makeAdmin: false), epoch: 2))
        store.recordIntent(
            accountIdB64: account, groupIdB64: group,
            MembershipIntent(request: .setAdmin(personKeyB64: "Qw==", makeAdmin: true), epoch: 2))
        let loaded = store.intents(accountIdB64: account, groupIdB64: group)
        XCTAssertEqual(loaded.count, 2)
        XCTAssertTrue(loaded.contains(
            MembershipIntent(request: .setAdmin(personKeyB64: "Qg==", makeAdmin: false), epoch: 2)))
    }

    func testIntentsAreRemovedPerSlotAndClearedPerAccount() {
        let store = makeStore()
        let a = MembershipIntent(request: .leave(ownPersonKeyB64: "Tw=="), epoch: 1)
        let b = MembershipIntent(request: .remove(personKeyB64: "Ug=="), epoch: 1)
        store.recordIntent(accountIdB64: account, groupIdB64: group, a)
        store.recordIntent(accountIdB64: account, groupIdB64: group, b)
        store.recordIntent(accountIdB64: "other", groupIdB64: group, a)
        store.removeIntent(accountIdB64: account, groupIdB64: group, slot: a.slot)
        XCTAssertEqual(store.intents(accountIdB64: account, groupIdB64: group), [b])
        store.clearForAccount(accountIdB64: account)
        XCTAssertTrue(store.intents(accountIdB64: account, groupIdB64: group).isEmpty)
        XCTAssertEqual(store.intents(accountIdB64: "other", groupIdB64: group), [a])
    }
}
