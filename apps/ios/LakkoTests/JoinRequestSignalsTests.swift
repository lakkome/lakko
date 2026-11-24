// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// `docs/ux/group-and-channel-privacy.md` section 8.1 — the pure rules of the
/// request-to-join signals (``JoinRequestSignals``) and their per-account
/// store (``JoinRequestSignalStore``).
@MainActor
final class JoinRequestSignalsTests: XCTestCase {

    private let anna = "YW5uYQ=="
    private let boris = "Ym9yaXM="

    // MARK: - Role gate

    func testOnlyAnOwnerOrAdminWhoseMembershipStandsGetsTheSignals() {
        XCTAssertTrue(JoinRequestSignals.signalsApply(role: .owner, membershipEnded: false))
        XCTAssertTrue(JoinRequestSignals.signalsApply(role: .admin, membershipEnded: false))
        XCTAssertFalse(JoinRequestSignals.signalsApply(role: .member, membershipEnded: false))
        XCTAssertFalse(JoinRequestSignals.signalsApply(role: nil, membershipEnded: false))
        XCTAssertFalse(JoinRequestSignals.signalsApply(role: .owner, membershipEnded: true))
        XCTAssertFalse(JoinRequestSignals.signalsApply(role: .admin, membershipEnded: true))
    }

    // MARK: - The bar

    func testTheBarIsGoneWithNoRequestOrForAMember() {
        XCTAssertNil(JoinRequestSignals.banner(role: .owner, membershipEnded: false, pendingCount: 0))
        XCTAssertNil(JoinRequestSignals.banner(role: .member, membershipEnded: false, pendingCount: 3))
        XCTAssertNil(JoinRequestSignals.banner(role: .admin, membershipEnded: true, pendingCount: 3))
    }

    func testTheBarCountsAndReadsFullAtTheHeadingThreshold() {
        let one = JoinRequestSignals.banner(role: .owner, membershipEnded: false, pendingCount: 1)
        XCTAssertEqual(one, .count(1))
        XCTAssertEqual(one?.key, "chat.join_requests.banner")
        let three = JoinRequestSignals.banner(role: .admin, membershipEnded: false, pendingCount: 3)
        XCTAssertEqual(three, .count(3))
        XCTAssertEqual(three?.key, "chat.join_requests.banner")
        XCTAssertEqual(
            JoinRequestSignals.banner(role: .owner, membershipEnded: false, pendingCount: 63), .count(63))
        let full = JoinRequestSignals.banner(role: .owner, membershipEnded: false, pendingCount: 64)
        XCTAssertEqual(full, .full)
        XCTAssertEqual(full?.key, "chat.join_requests.banner_full")
        XCTAssertEqual(
            JoinRequestSignals.banner(role: .owner, membershipEnded: false, pendingCount: 200), .full)
    }

    func testTheBarTextUsesThePluralForms() {
        XCTAssertFalse(JoinRequestSignals.Banner.count(1).text.isEmpty)
        XCTAssertNotEqual(JoinRequestSignals.Banner.count(1).text, JoinRequestSignals.Banner.count(2).text)
        XCTAssertFalse(JoinRequestSignals.Banner.full.text.isEmpty)
    }

    // MARK: - Arrival

    func testANewRequestNotifiesOnceAndAnAskAgainIsSilent() {
        let first = JoinRequestSignals.arrival(
            joinerKeyB64: anna, sets: .init(), openAndVisible: false, muted: false)
        XCTAssertEqual(first.arrival, .notify)
        XCTAssertEqual(first.sets.announced, [anna])
        XCTAssertEqual(first.sets.unshown, [anna])

        let again = JoinRequestSignals.arrival(
            joinerKeyB64: anna, sets: first.sets, openAndVisible: false, muted: false)
        XCTAssertEqual(again.arrival, .alreadyAnnounced)
        XCTAssertEqual(again.sets, first.sets)
    }

    func testNoNotificationWhileTheConversationIsInFront() {
        let decision = JoinRequestSignals.arrival(
            joinerKeyB64: anna, sets: .init(), openAndVisible: true, muted: false)
        XCTAssertEqual(decision.arrival, .shownByBar)
        XCTAssertEqual(decision.sets.announced, [anna])
        XCTAssertTrue(decision.sets.unshown.isEmpty, "the bar shows it: not unshown")

        // Announced by the bar: closing the conversation later does not
        // announce it a second time.
        let later = JoinRequestSignals.arrival(
            joinerKeyB64: anna, sets: decision.sets, openAndVisible: false, muted: false)
        XCTAssertEqual(later.arrival, .alreadyAnnounced)
    }

    func testAMutedGroupCountsTheRequestButRaisesNoNotification() {
        let decision = JoinRequestSignals.arrival(
            joinerKeyB64: anna, sets: .init(), openAndVisible: false, muted: true)
        XCTAssertEqual(decision.arrival, .unshownQuiet)
        XCTAssertEqual(decision.sets.unshown, [anna])
        XCTAssertEqual(decision.sets.announced, [anna])
    }

    // MARK: - Prune and the bar drawn

    func testPruneKeepsOnlyTheRequestsStillWaiting() {
        let sets = JoinRequestSignals.Sets(announced: [anna, boris], unshown: [anna, boris])
        let pruned = JoinRequestSignals.pruned(sets, pending: [boris])
        XCTAssertEqual(pruned.announced, [boris])
        XCTAssertEqual(pruned.unshown, [boris])

        // A request answered and asked again later is a new request.
        let afterLeave = JoinRequestSignals.pruned(sets, pending: [])
        XCTAssertTrue(afterLeave.isEmpty)
        let reasked = JoinRequestSignals.arrival(
            joinerKeyB64: anna, sets: afterLeave, openAndVisible: false, muted: false)
        XCTAssertEqual(reasked.arrival, .notify)
    }

    func testDrawingTheBarClearsTheUnshownSetOnly() {
        let sets = JoinRequestSignals.Sets(announced: [anna, boris], unshown: [anna])
        let drawn = JoinRequestSignals.barDrawn(sets)
        XCTAssertEqual(drawn.announced, [anna, boris])
        XCTAssertTrue(drawn.unshown.isEmpty)
    }

    // MARK: - The chat-list row

    func testUnshownRequestsStillWaitingAddToTheRowsUnread() {
        let sets = JoinRequestSignals.Sets(announced: [anna, boris], unshown: [anna, boris])
        XCTAssertEqual(
            JoinRequestSignals.unshownCount(sets: sets, pending: [anna, boris], signalsApply: true), 2)
        XCTAssertEqual(
            JoinRequestSignals.unshownCount(sets: sets, pending: [anna], signalsApply: true), 1)
        XCTAssertEqual(
            JoinRequestSignals.unshownCount(sets: sets, pending: [anna, boris], signalsApply: false), 0,
            "a member, or a person whose membership ended, sees no signal")
        XCTAssertEqual(JoinRequestSignals.unshownCount(sets: nil, pending: [anna], signalsApply: true), 0)

        XCTAssertEqual(JoinRequestSignals.rowUnread(messages: 3, unshown: 2), 5)
        XCTAssertEqual(JoinRequestSignals.rowUnread(messages: 0, unshown: 1), 1)
        XCTAssertEqual(JoinRequestSignals.rowUnread(messages: 4, unshown: 0), 4)
    }

    // MARK: - The notification

    func testTheBodyNamesTheJoinerOrFallsBackToSomeone() {
        let named = JoinRequestSignals.notificationBody(name: "Anna", groupTitle: "Hikers")
        XCTAssertTrue(named.contains("Anna"))
        XCTAssertTrue(named.contains("Hikers"))
        let unnamed = JoinRequestSignals.notificationBody(name: "  ", groupTitle: "Hikers")
        XCTAssertFalse(unnamed.contains("Anna"))
        XCTAssertTrue(unnamed.contains("Hikers"))
        XCTAssertEqual(unnamed, JoinRequestSignals.notificationBody(name: nil, groupTitle: "Hikers"))
    }

    func testTheNameIsTheAccountsThenTheCarriedThenNone() {
        let contact = ResolvedDisplayName(
            name: "Anna K", source: .localRename, verifiedUsername: nil, fingerprint: "",
            theirName: nil, usernameNotVerified: false)
        let placeholder = ResolvedDisplayName(
            name: "Someone", source: .placeholder, verifiedUsername: nil, fingerprint: "",
            theirName: nil, usernameNotVerified: false)
        XCTAssertEqual(
            JoinRequestSignals.requestName(accountKeyName: contact, carried: "Ann", leafName: nil), "Anna K")
        XCTAssertEqual(
            JoinRequestSignals.requestName(accountKeyName: placeholder, carried: "Ann", leafName: nil), "Ann")
        XCTAssertNil(
            JoinRequestSignals.requestName(accountKeyName: placeholder, carried: nil, leafName: placeholder))
    }

    func testOneNotificationIdentifierPerGroupAndJoiner() {
        let a = JoinRequestSignals.notificationIdentifier(conversationIdHex: "ABCD", joinerKeyB64: anna)
        XCTAssertEqual(a, JoinRequestSignals.notificationIdentifier(conversationIdHex: "abcd", joinerKeyB64: anna))
        XCTAssertNotEqual(a, JoinRequestSignals.notificationIdentifier(conversationIdHex: "abcd", joinerKeyB64: boris))
        XCTAssertNotEqual(a, JoinRequestSignals.notificationIdentifier(conversationIdHex: "abce", joinerKeyB64: anna))
    }

    // MARK: - The store

    func testTheStoreKeepsSetsPerGroupAndRefusesASealedAccount() throws {
        let suite = "lakko.tests.join-request-signals.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let account = "join-signals-account"
        AccountStoreGate.open(account)
        let store = JoinRequestSignalStore(defaults: defaults)
        let sets = JoinRequestSignals.Sets(announced: [anna], unshown: [anna])
        store.save(accountIdB64: account, groupIdHex: "AA11", sets: sets)
        XCTAssertEqual(store.sets(accountIdB64: account, groupIdHex: "aa11"), sets)
        XCTAssertEqual(store.sets(accountIdB64: account, groupIdHex: "bb22"), .init())

        store.forgetGroup(accountIdB64: account, groupIdHex: "aa11")
        XCTAssertTrue(store.load(accountIdB64: account).isEmpty)
        XCTAssertNil(defaults.data(forKey: JoinRequestSignalStore.key(accountIdB64: account)))

        AccountStoreGate.seal(account)
        store.save(accountIdB64: account, groupIdHex: "aa11", sets: sets)
        XCTAssertTrue(store.load(accountIdB64: account).isEmpty, "no write after the session ended")
    }
}
