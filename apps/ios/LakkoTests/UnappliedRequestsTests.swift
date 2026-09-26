// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0045 S7 residual 1 + 2 — iOS mirror of the web reference
/// `apps/web/src/chat/unapplied-requests.test.ts` and the two residual-2 helpers
/// in `PendingRequests.swift`.
///
/// A SwiftUI view cannot be asserted on, which is the same split
/// `PendingRequestsTests` and `MembershipEndedTests` already use: every rule
/// lives in the plain module.
///
/// The measured discrimination evidence for each rule (each probe was RUN, the
/// failure observed, and the probe reverted) is recorded beside the test.
final class UnappliedRequestsTests: XCTestCase {

    private let account = "acct"
    private let group = "grp"
    private let joiner = "am9pbmVyLWtleQ=="
    private let contact = "Y29udGFjdC1rZXk="

    private var store: UnappliedRequestStore!
    private var defaults: UserDefaults!
    private var suiteName: String!

    override func setUp() {
        super.setUp()
        // ABORT on the first failure: a later assertion running against a
        // fixture that already failed reports a second, derived failure that
        // hides the first.
        continueAfterFailure = false
        suiteName = "unapplied-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
        store = UnappliedRequestStore(defaults: defaults)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private func own(joinRequest: Bool = false) -> RequestObservation {
        RequestObservation(atMs: 1_000, own: true, joinRequest: joinRequest)
    }

    // MARK: - Detection

    /// The whole point. The approving admin's own `add:` observation is gone
    /// from the core's live pending set and the joiner is not in the roster, so
    /// the request died without a trace anywhere else.
    ///
    /// DISCRIMINATION (RUN, then reverted): with the roster check inverted
    /// (`roster.contains` -> `!roster.contains`) two tests went red, this one
    /// with:
    ///
    ///     XCTAssertEqual failed: ("0") is not equal to ("1")
    func testReportsAnOwnAddThatLeftThePendingSetWithoutReachingTheRoster() {
        let out = unappliedAddRequests(
            observations: ["add:\(joiner)": own(joinRequest: true)],
            liveKeys: [],
            rosterKeysB64: [])
        XCTAssertEqual(out.count, 1)
        XCTAssertEqual(
            out.first,
            UnappliedAdd(addSignatureKeyB64: joiner, viaJoinRequest: true))
    }

    /// The single most important false positive to exclude: the request WORKED.
    ///
    /// DISCRIMINATION (RUN, then reverted): dropping the roster membership test
    /// entirely made this red:
    ///
    ///     XCTAssertEqual failed: ("1") is not equal to ("0")
    func testSaysNothingWhenTheAddLanded() {
        XCTAssertEqual(
            unappliedAddRequests(
                observations: ["add:\(joiner)": own(joinRequest: true)],
                liveKeys: [],
                rosterKeysB64: [joiner]
            ).count,
            0)
    }

    func testSaysNothingWhileTheRequestIsStillPending() {
        XCTAssertEqual(
            unappliedAddRequests(
                observations: ["add:\(joiner)": own()],
                liveKeys: ["add:\(joiner)"],
                rosterKeysB64: []
            ).count,
            0)
    }

    /// UX §10 — a member is never told about someone else's request.
    ///
    /// DISCRIMINATION (RUN, then reverted): with the `own` guard removed:
    ///
    ///     XCTAssertEqual failed: ("1") is not equal to ("0")
    func testIgnoresARequestThisDeviceDidNotMake() {
        XCTAssertEqual(
            unappliedAddRequests(
                observations: ["add:\(joiner)": RequestObservation(atMs: 1_000, own: false)],
                liveKeys: [],
                rosterKeysB64: []
            ).count,
            0)
    }

    /// The deliberate scope limit, pinned so nobody widens it by accident.
    ///
    /// `remove:` and `leave:` name a LEAF and OpenMLS hands a vacated leaf to
    /// the next `Add`, so a roster test on one can be wrong in both directions;
    /// a `role-change` carries no payload at all. All three are also VISIBLE
    /// when they fail — the person you tried to remove is still in the roster,
    /// and a group you failed to leave is still in your list. The add is the
    /// only request whose failure leaves no trace, and the only one whose
    /// effect test is keyed on a signature key rather than a leaf.
    func testClaimsNothingAboutARemoveALeaveOrARoleChange() {
        XCTAssertEqual(
            unappliedAddRequests(
                observations: [
                    "remove:4": own(),
                    "leave:2": own(),
                    "role-change": own(),
                    "update:3": own(),
                ],
                liveKeys: [],
                rosterKeysB64: []
            ).count,
            0)
    }

    /// A torn roster read is UNKNOWN, never "nobody is in the group" — the same
    /// fail-closed direction `membershipRotationDecision` takes with the
    /// identical signal.
    ///
    /// DISCRIMINATION (RUN, then reverted): with the `nil` guard replaced by
    /// `rosterKeysB64 ?? []`:
    ///
    ///     XCTAssertEqual failed: ("1") is not equal to ("0")
    func testClaimsNothingWhenTheRosterCouldNotBeRead() {
        XCTAssertEqual(
            unappliedAddRequests(
                observations: ["add:\(joiner)": own(joinRequest: true)],
                liveKeys: [],
                rosterKeysB64: nil
            ).count,
            0)
    }

    // MARK: - Copy

    /// DISCRIMINATION (RUN, then reverted): with the join line replaced by the
    /// UX spec §7 draft `This request expired before it could be applied. Ask
    /// them to request to join again.`:
    ///
    ///     XCTAssertEqual failed: ("This request expired before it could be
    ///     applied. Ask them to request to join again.") is not equal to
    ///     ("Not applied. Ask them to request to join again.")
    func testIsExactlyTheseTwoStrings() {
        XCTAssertEqual(
            unappliedLine(UnappliedAdd(addSignatureKeyB64: joiner, viaJoinRequest: true)),
            "Not applied. Ask them to request to join again.")
        XCTAssertEqual(
            unappliedLine(UnappliedAdd(addSignatureKeyB64: contact, viaJoinRequest: false)),
            "Not added. You can add them again.")
    }

    /// The rule the whole item turns on, asserted independently of the literals
    /// so it survives a re-wording: state that it did not apply, never why. The
    /// core reports no reason for an exclusion, so neither may the UI.
    ///
    /// DISCRIMINATION (RUN, then reverted): with the same §7 draft substitution
    /// this went red on the join line — which is the finding, because "expired"
    /// is a REASON this client cannot establish.
    func testNeverStatesAReason() {
        let banned = ["expired", "refused", "rejected", "invalid", "failed", "error", "because"]
        let strings = [
            unappliedLine(UnappliedAdd(addSignatureKeyB64: joiner, viaJoinRequest: true)),
            unappliedLine(UnappliedAdd(addSignatureKeyB64: contact, viaJoinRequest: false)),
            unappliedExplanation,
            joinWaitPrimary,
            joinWaitSecondary,
            joinRequestAgain,
        ]
        for s in strings {
            for word in banned {
                XCTAssertFalse(
                    s.lowercased().contains(word), "states a reason (\(word)): \(s)")
            }
        }
    }

    func testTheHeadingCounts() {
        XCTAssertEqual(unappliedHeading(2), "Not applied (2)")
    }

    /// The joiner cannot be TOLD anything, so what it gets is the truth about
    /// the wait plus the one repair available to it.
    ///
    /// DISCRIMINATION (RUN, then reverted): with `joinWaitSecondary` reverted to
    /// the pre-change copy `Waiting for an admin to approve, then for the change
    /// to be applied…`:
    ///
    ///     XCTAssertTrue failed - the joiner is not told the wait is unbounded
    func testTheJoinerIsToldTheWaitIsUnboundedAndOfferedTheOneRepair() {
        XCTAssertEqual(
            joinWaitPrimary,
            "Request sent. An admin approves it, then the change is applied to the group.")
        XCTAssertTrue(
            joinWaitSecondary.contains("time limit"),
            "the joiner is not told the wait is unbounded")
        XCTAssertTrue(joinWaitSecondary.contains("send the request again"))
        XCTAssertEqual(joinRequestAgain, "Request again")
    }

    // MARK: - The store

    func testRoundTripsAndIsIdempotent() {
        let item = UnappliedAdd(addSignatureKeyB64: joiner, viaJoinRequest: true)
        store.record(accountIdB64: account, groupIdB64: group, items: [item])
        store.record(accountIdB64: account, groupIdB64: group, items: [item])
        XCTAssertEqual(store.load(accountIdB64: account, groupIdB64: group), [item])
    }

    /// DISCRIMINATION (RUN, then reverted): with `dismiss` an early `return`,
    /// this and the resurrection test both went red:
    ///
    ///     XCTAssertEqual failed: ("2") is not equal to ("1")
    func testDismissesOneRowWithoutTouchingTheOther() {
        store.record(
            accountIdB64: account, groupIdB64: group,
            items: [
                UnappliedAdd(addSignatureKeyB64: joiner, viaJoinRequest: true),
                UnappliedAdd(addSignatureKeyB64: contact, viaJoinRequest: false),
            ])
        store.dismiss(accountIdB64: account, groupIdB64: group, addSignatureKeyB64: joiner)
        let left = store.load(accountIdB64: account, groupIdB64: group)
        XCTAssertEqual(left.count, 1)
        XCTAssertEqual(left.first?.addSignatureKeyB64, contact)
    }

    /// A dismissed row must STAY dismissed: the detection re-runs on every
    /// fold, and re-recording what the user has already read would make the
    /// notice un-dismissable.
    ///
    /// DISCRIMINATION (RUN, then reverted): with `record` overwriting a
    /// dismissed row instead of honouring the tombstone:
    ///
    ///     XCTAssertEqual failed: ("1") is not equal to ("0")
    func testDoesNotResurrectADismissedRow() {
        let item = UnappliedAdd(addSignatureKeyB64: joiner, viaJoinRequest: true)
        store.record(accountIdB64: account, groupIdB64: group, items: [item])
        store.dismiss(accountIdB64: account, groupIdB64: group, addSignatureKeyB64: joiner)
        store.record(accountIdB64: account, groupIdB64: group, items: [item])
        XCTAssertEqual(store.load(accountIdB64: account, groupIdB64: group).count, 0)
    }

    func testIsScopedPerAccountAndPerGroup() {
        store.record(
            accountIdB64: account, groupIdB64: group,
            items: [UnappliedAdd(addSignatureKeyB64: joiner, viaJoinRequest: true)])
        XCTAssertEqual(store.load(accountIdB64: account, groupIdB64: "other").count, 0)
        XCTAssertEqual(store.load(accountIdB64: "other", groupIdB64: group).count, 0)
    }

    func testClearsOneAccountAndLeavesTheOthersAlone() {
        store.record(
            accountIdB64: account, groupIdB64: group,
            items: [UnappliedAdd(addSignatureKeyB64: joiner, viaJoinRequest: true)])
        store.record(
            accountIdB64: account, groupIdB64: "g2",
            items: [UnappliedAdd(addSignatureKeyB64: contact, viaJoinRequest: false)])
        store.record(
            accountIdB64: "other", groupIdB64: group,
            items: [UnappliedAdd(addSignatureKeyB64: contact, viaJoinRequest: false)])
        store.clearForAccount(accountIdB64: account)
        XCTAssertEqual(store.load(accountIdB64: account, groupIdB64: group).count, 0)
        XCTAssertEqual(store.load(accountIdB64: account, groupIdB64: "g2").count, 0)
        XCTAssertEqual(store.load(accountIdB64: "other", groupIdB64: group).count, 1)
    }

    func testClearsOneGroup() {
        store.record(
            accountIdB64: account, groupIdB64: group,
            items: [UnappliedAdd(addSignatureKeyB64: joiner, viaJoinRequest: true)])
        store.clearForGroup(accountIdB64: account, groupIdB64: group)
        XCTAssertEqual(store.load(accountIdB64: account, groupIdB64: group).count, 0)
    }

    /// The observation record's new flag must survive the plist round-trip, or
    /// the copy silently degrades to the wrong action after a relaunch.
    func testTheJoinRequestFlagSurvivesTheObservationRoundTrip() throws {
        let suite = "unapplied-obs-\(UUID().uuidString)"
        let d = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { d.removePersistentDomain(forName: suite) }
        let observations = PendingRequestStore(defaults: d)
        observations.record(
            accountIdB64: account, groupIdB64: group, key: "add:\(joiner)",
            atMs: 1_000, own: true, joinRequest: true)
        XCTAssertEqual(
            observations.load(accountIdB64: account, groupIdB64: group)["add:\(joiner)"]?
                .joinRequest,
            true)
        // Sticky-true: the receive loop observes our own approval landing back
        // off the journal, and THAT observation knows nothing about where the
        // request came from. Losing the flag to arrival order would silently
        // downgrade the copy to the wrong action.
        observations.record(
            accountIdB64: account, groupIdB64: group, key: "add:\(joiner)",
            atMs: 2_000, own: false)
        XCTAssertEqual(
            observations.load(accountIdB64: account, groupIdB64: group)["add:\(joiner)"]?
                .joinRequest,
            true)
    }

    // MARK: - Residual 2 — the queue an approve grows

    /// DISCRIMINATION (RUN, then reverted): with the threshold changed from
    /// `< 2` to `< 1` (in the same pass as the `of 64` probe below, which is why
    /// the observed string carries both changes):
    ///
    ///     XCTAssertNil failed: "1 of 64 changes are now waiting to apply."
    func testTheQueueLineStaysSilentForOneChangeAndCountsFromTwo() {
        XCTAssertNil(waitingQueueLine(0))
        XCTAssertNil(waitingQueueLine(1))
        XCTAssertEqual(waitingQueueLine(2), "2 changes are now waiting to apply.")
        XCTAssertEqual(waitingQueueLine(64), "64 changes are now waiting to apply.")
    }

    /// It states what IS, never a limit — the cap is `pub(crate)` in `lakko-mls`
    /// and on neither FFI surface, so this layer has no number to compare
    /// against and must not imply one.
    ///
    /// DISCRIMINATION (RUN, then reverted): with the line changed to
    /// `\(n) of 64 changes are now waiting to apply.`:
    ///
    ///     XCTAssertFalse failed - implies a limit (of 64): 2 of 64 changes are
    ///     now waiting to apply.
    func testTheQueueLineNeverImpliesALimitItCannotRead() {
        for n in [2, 10, 64, 100] {
            let line = waitingQueueLine(n) ?? ""
            for word in ["limit", "maximum", "of 64", "full", "cap"] {
                XCTAssertFalse(line.contains(word), "implies a limit (\(word)): \(line)")
            }
        }
    }
}
