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

    // MARK: - ADR-0050 2.5a — rows from a stopped re-request

    /// The add-only detector is gone: a row is now made from the INTENT whose
    /// re-request stopped, and every intent kind maps to exactly one row kind
    /// keyed on its subject's signature key.
    func testEveryIntentKindMapsToOneRowAboutItsSubject() {
        let cases: [(MembershipIntent.Request, UnappliedKind, Bool)] = [
            (.add(personKeyB64: contact, accountIdB64: "YWNjdA==", leafKeysB64: []), .add, false),
            (.approveJoin(joinerKeyB64: joiner), .add, true),
            (.remove(personKeyB64: contact), .removal, false),
            (.setAdmin(personKeyB64: contact, makeAdmin: true), .makeAdmin, false),
            (.setAdmin(personKeyB64: contact, makeAdmin: false), .removeAdmin, false),
            (.transferOwnership(toPersonKeyB64: contact, outgoing: .admin), .ownershipTransfer, false),
            (.leave(ownPersonKeyB64: contact), .leave, false),
            (.ownDeviceAdd(deviceKeyB64: contact), .deviceAdd, false),
            (.ownDeviceRemove(deviceKeyB64: contact), .deviceRemoval, false),
        ]
        for (request, kind, viaJoin) in cases {
            let row = unappliedRow(for: MembershipIntent(request: request, epoch: 3))
            XCTAssertEqual(row.kind, kind)
            XCTAssertEqual(row.viaJoinRequest, viaJoin)
            XCTAssertEqual(row.addSignatureKeyB64, viaJoin ? joiner : contact)
        }
    }

    /// A removal and a role change about the SAME person are two rows: the row
    /// key carries the kind for every kind but the original add.
    func testRowsOfDifferentKindsAboutOnePersonDoNotOverwriteEachOther() {
        store.record(
            accountIdB64: account, groupIdB64: group,
            items: [
                UnappliedAdd(addSignatureKeyB64: contact, viaJoinRequest: false, kind: .removal),
                UnappliedAdd(addSignatureKeyB64: contact, viaJoinRequest: false, kind: .makeAdmin),
                UnappliedAdd(addSignatureKeyB64: contact, viaJoinRequest: false),
            ])
        let rows = store.load(accountIdB64: account, groupIdB64: group)
        XCTAssertEqual(Set(rows.map(\.kind)), [.add, .removal, .makeAdmin])
        XCTAssertTrue(rows.allSatisfy { $0.addSignatureKeyB64 == contact })
        XCTAssertEqual(
            Set(rows.map(\.rowKey)), [contact, "removal:\(contact)", "make-admin:\(contact)"])
        store.dismiss(
            accountIdB64: account, groupIdB64: group, addSignatureKeyB64: "removal:\(contact)")
        XCTAssertEqual(
            Set(store.load(accountIdB64: account, groupIdB64: group).map(\.kind)),
            [.add, .makeAdmin])
    }

    /// The new kinds state what did not happen and nothing more — no reason,
    /// and no "you can do it again", which is false when the row exists
    /// because the authority to do it is gone.
    func testTheNewKindsStateNoReasonAndOfferNoRedo() {
        let banned = ["expired", "refused", "rejected", "invalid", "failed", "error", "because",
                      "again"]
        let kinds: [UnappliedKind] = [
            .add, .removal, .makeAdmin, .removeAdmin, .ownershipTransfer, .leave, .deviceAdd,
            .deviceRemoval,
        ]
        for kind in kinds {
            let line = unappliedLine(
                UnappliedAdd(addSignatureKeyB64: contact, viaJoinRequest: false, kind: kind))
            XCTAssertFalse(line.isEmpty)
            for word in banned {
                XCTAssertFalse(line.lowercased().contains(word), "\(kind): \(line)")
            }
        }
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
            "Not added.")
        let lines: [(UnappliedKind, String)] = [
            (.removal, "Not removed."),
            (.makeAdmin, "Not made an admin."),
            (.removeAdmin, "Still an admin."),
            (.ownershipTransfer, "Ownership was not transferred."),
            (.leave, "Not applied. You are still a member."),
            (.deviceAdd, "Device not added to this group."),
            (.deviceRemoval, "Device not removed from this group."),
        ]
        for (kind, text) in lines {
            XCTAssertEqual(
                unappliedLine(UnappliedAdd(addSignatureKeyB64: contact, viaJoinRequest: false, kind: kind)),
                text)
        }
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
        ]
        for s in strings {
            for word in banned {
                XCTAssertFalse(
                    s.lowercased().contains(word), "states a reason (\(word)): \(s)")
            }
        }
    }

    func testTheHeadingCounts() {
        XCTAssertEqual(unappliedHeading(2).withoutIsolates, "Not applied (2)")
    }

    /// The joiner cannot be TOLD anything, so what it gets is the truth about
    /// the wait; the repair (`Ask again`) waits a day.
    func testTheJoinerIsToldTheWait() {
        XCTAssertEqual(
            joinWaitPrimary,
            "Request sent. An admin will see it when they next open Lakko.")
    }

    // MARK: - The invite page after `Request to join` (a day per group)

    private let day: Int64 = 86_400_000
    private let groupHex = String(repeating: "ab", count: 32)

    func testTheRuleIsOneDay() {
        XCTAssertEqual(RequestsSentStore.askAgainAfterMs, day)
        XCTAssertFalse(RequestsSentStore.mayAskAgain(sentAtMs: 0, nowMs: day - 1))
        XCTAssertTrue(RequestsSentStore.mayAskAgain(sentAtMs: 0, nowMs: day))
    }

    /// Under a day: the waiting line and no control; a day or more: "No
    /// answer yet." and `Ask again`.
    func testUnderADayWaitsADayOrMoreOffersAskAgain() {
        XCTAssertEqual(JoinGroupLandingViewModel.waitState(sentAtMs: 0, nowMs: 0), .waiting)
        XCTAssertEqual(JoinGroupLandingViewModel.waitState(sentAtMs: 0, nowMs: day - 1), .waiting)
        XCTAssertEqual(JoinGroupLandingViewModel.waitState(sentAtMs: 0, nowMs: day), .noAnswer)
        XCTAssertEqual(JoinGroupLandingViewModel.waitState(sentAtMs: 0, nowMs: 3 * day), .noAnswer)
        XCTAssertEqual(Localization.text("requests_sent.no_answer"), "No answer yet.")
        XCTAssertEqual(Localization.text("requests_sent.ask_again"), "Ask again")
    }

    /// Reopened inside the day for a group this device already asked to
    /// join: the waiting line with nothing to press, never `Request to join`.
    func testReopeningWithinTheDayShowsTheWaitNotTheRequestButton() {
        let now: Int64 = 10 * day
        XCTAssertEqual(
            JoinGroupLandingViewModel.stateOnOpen(
                groupIdHex: groupHex, requestSentAtMs: now - 60_000, groupHeld: false, nowMs: now),
            .waiting)
        XCTAssertEqual(
            JoinGroupLandingViewModel.stateOnOpen(
                groupIdHex: groupHex, requestSentAtMs: now - day, groupHeld: false, nowMs: now),
            .noAnswer)
        XCTAssertEqual(
            JoinGroupLandingViewModel.stateOnOpen(
                groupIdHex: groupHex, requestSentAtMs: nil, groupHeld: false, nowMs: now),
            .ready, "never asked")
    }

    /// The join landed: the page hands over to the group's chat at once.
    func testJoinedNavigatesToTheChat() {
        let joined = JoinGroupLandingViewModel.stateOnOpen(
            groupIdHex: groupHex, requestSentAtMs: 0, groupHeld: true, nowMs: 1)
        XCTAssertEqual(joined, .joined(groupIdHex: groupHex))
        XCTAssertEqual(JoinGroupLandingViewModel.chatToOpen(joined), groupHex)
        XCTAssertNil(JoinGroupLandingViewModel.chatToOpen(.waiting))
        XCTAssertNil(JoinGroupLandingViewModel.chatToOpen(.noAnswer))
        XCTAssertNil(JoinGroupLandingViewModel.chatToOpen(.ready))
    }

    /// The page reads the request this device recorded for the group.
    func testTheStoreFindsTheRecordedRequest() {
        let account = "joinday-\(UUID().uuidString)"
        defer { UserDefaults.standard.removeObject(forKey: RequestsSentStore.key(account)) }
        let groupId = Data(repeating: 0xab, count: 32)
        RequestsSentStore.record(
            groupId: groupId, name: "G", capabilityWire: Data([1]), blobId: Data([2]),
            accountIdB64: account, nowMs: 42)
        XCTAssertEqual(RequestsSentStore.entry(accountIdB64: account, groupIdHex: groupHex.uppercased())?.sentAtMs, 42)
        XCTAssertNil(RequestsSentStore.entry(
            accountIdB64: account, groupIdHex: String(repeating: "cd", count: 32)))
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
}

/// Seen live 2026-11-23: a member left a group and opened its invite link
/// again; the page showed "Request sent. An admin will see it..." and nothing
/// reached the admin, because the Requests sent entry of the request that had
/// led to the group outlived the membership, and the page took it for a
/// request waiting. The entry now goes when the group lands and when the
/// membership ends (``RequestsSentStore/entries(_:forgetting:upToMs:)``).
final class RequestsSentForgetTests: XCTestCase {

    private let group = String(repeating: "ab", count: 32)
    private let other = String(repeating: "cd", count: 32)

    private func entry(_ id: String, at: Int64) -> RequestsSentStore.Entry {
        RequestsSentStore.Entry(
            groupIdHex: id, name: nil, handle: nil, sentAtMs: at, capabilityB64: "", blobIdB64: "")
    }

    func testTheOldEntryMadeThePageWaitWithoutARequest() {
        let state = JoinGroupLandingViewModel.stateOnOpen(
            groupIdHex: group, requestSentAtMs: 1_000, groupHeld: false, nowMs: 2_000)
        XCTAssertEqual(state, .waiting)
    }

    func testAfterTheJoinAndTheLeaveThePageOffersRequestToJoin() {
        var entries = [entry(group, at: 1_000), entry(other, at: 1_500)]
        // The group lands at 2 000, then the person leaves at 3 000.
        entries = RequestsSentStore.entries(entries, forgetting: group, upToMs: 2_000)
        entries = RequestsSentStore.entries(entries, forgetting: group, upToMs: 3_000)
        let sent = entries.first { $0.groupIdHex == group }?.sentAtMs
        XCTAssertEqual(
            JoinGroupLandingViewModel.stateOnOpen(
                groupIdHex: group, requestSentAtMs: sent, groupHeld: false, nowMs: 4_000),
            .ready)
        XCTAssertEqual(entries, [entry(other, at: 1_500)])
    }

    func testARequestSentAfterTheCutoffStays() {
        let entries = [entry(group, at: 5_000)]
        XCTAssertEqual(RequestsSentStore.entries(entries, forgetting: group, upToMs: 4_000), entries)
    }

    func testTheIdIsMatchedWithoutCase() {
        XCTAssertTrue(
            RequestsSentStore.entries([entry(group, at: 1)], forgetting: group.uppercased(), upToMs: 1).isEmpty)
    }
}
