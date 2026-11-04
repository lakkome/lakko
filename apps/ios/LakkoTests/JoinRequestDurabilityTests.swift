// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// UX `group-and-channel-privacy.md` section 8 — what an admin's device keeps
/// about a join request must survive a restart: the rejection (the knock row
/// stays on the DS), and the co-publisher key an approve appoints.
@MainActor
final class JoinRequestDurabilityTests: XCTestCase {

    private var suite: String!
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suite = "JoinRequestDurabilityTests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suite)
        super.tearDown()
    }

    func testARejectionSurvivesAReopenAndCoversOnlyTheRowsItSaw() {
        let account = "join-durability", group = "ab12"
        GroupSettingsStore(defaults: defaults).noteRejected(
            accountIdB64: account, groupIdHex: group, joinerKeyB64: "joiner", throughRowId: 7)
        // A fresh store over the same defaults, as after a restart.
        let reopened = GroupSettingsStore(defaults: defaults)
        XCTAssertEqual(reopened.rejected(accountIdB64: account, groupIdHex: group), ["joiner": 7])
        // A later reject of the same joiner never moves the mark back.
        reopened.noteRejected(accountIdB64: account, groupIdHex: group, joinerKeyB64: "joiner", throughRowId: 3)
        XCTAssertEqual(reopened.rejected(accountIdB64: account, groupIdHex: group)["joiner"], 7)
        reopened.removeGroup(accountIdB64: account, groupIdHex: group)
        XCTAssertTrue(reopened.rejected(accountIdB64: account, groupIdHex: group).isEmpty)
    }

    func testTheKnockRowIdRidesWithTheRequestMeta() {
        let account = "join-durability", group = "ab12"
        let store = GroupSettingsStore(defaults: defaults)
        store.keepRequestMeta(
            accountIdB64: account, groupIdHex: group, joinerKeyB64: "joiner", askedAtMs: 1_000,
            inviterKey: nil, knockRowId: 42)
        let meta = GroupSettingsStore(defaults: defaults)
            .requestMeta(accountIdB64: account, groupIdHex: group)["joiner"]
        XCTAssertEqual(meta?.knockRowId, 42)
        XCTAssertEqual(meta?.askedAtMs, 1_000)
    }

    func testACoPublisherKeyReadBeforeARestartIsStillThereForTheApprove() async {
        let account = "join-durability-\(UUID().uuidString)"
        let groupId = Data(repeating: 9, count: 32), joiner = Data(repeating: 4, count: 32)
        let copublisher = Data(repeating: 5, count: 32)
        let binding = FfiChannelCoPublisherBinding(
            channelId: Data(repeating: 1, count: 16), accountPubkey: joiner, copublisherPubkey: copublisher)
        await CoPublisherKeyBook.shared.record(groupId: groupId, binding: binding, accountIdB64: account)
        // Nothing is held in memory: the book reads the per-account store.
        XCTAssertNotNil(UserDefaults.standard.dictionary(forKey: CoPublisherKeyBook.storeKey(account)))
        let found = await CoPublisherKeyBook.shared.key(groupId: groupId, joinerKey: joiner, accountIdB64: account)
        XCTAssertEqual(found, copublisher)
        await CoPublisherKeyBook.shared.forget(groupId: groupId, joinerKey: joiner, accountIdB64: account)
        let gone = await CoPublisherKeyBook.shared.key(groupId: groupId, joinerKey: joiner, accountIdB64: account)
        XCTAssertNil(gone)
        CoPublisherKeyBook.clear(accountIdB64: account)
    }
}

/// A message notification is titled with the conversation and, in a group,
/// names its sender.
final class NseMessageNotificationTests: XCTestCase {

    func testAGroupMessageIsTitledWithTheGroupAndNamesTheSender() {
        let shown = nseMessageNotification(
            conversationName: "Hiking club", senderName: "Anna", isGroup: true, text: "See you at 9")
        XCTAssertEqual(shown.title, "Hiking club")
        XCTAssertEqual(shown.body, "Anna: See you at 9")
    }

    func testAOneToOneMessageIsTitledWithThePersonAndCarriesTheTextAlone() {
        let shown = nseMessageNotification(
            conversationName: "Anna", senderName: "Anna", isGroup: false, text: "Photo")
        XCTAssertEqual(shown.title, "Anna")
        XCTAssertEqual(shown.body, "Photo")
    }

    func testAOneToOneWithNoChatNameIsTitledWithTheSender() {
        let shown = nseMessageNotification(conversationName: nil, senderName: "Anna", isGroup: false, text: "Hi")
        XCTAssertEqual(shown.title, "Anna")
        XCTAssertEqual(shown.body, "Hi")
    }

    func testUnknownNamesFallBackToLakkoAndNoPrefix() {
        let shown = nseMessageNotification(conversationName: nil, senderName: nil, isGroup: true, text: "Hi")
        XCTAssertEqual(shown.title, "Lakko")
        XCTAssertEqual(shown.body, "Hi")
        let blank = nseMessageNotification(conversationName: "", senderName: "  ", isGroup: true, text: "Hi")
        XCTAssertEqual(blank.title, "Lakko")
        XCTAssertEqual(blank.body, "Hi")
    }

    /// The reaction line carries the name it is handed.
    func testTheReactionLineCarriesTheName() {
        XCTAssertEqual(
            nseReactionBody(name: "VG3", emoji: "\u{1F44D}"), "VG3 reacted \u{1F44D} to your message")
    }

    /// A group member who is not a contact is mirrored under the account key
    /// the extension looks the reactor up by, not under the leaf key.
    func testGroupMembersAreMirroredUnderTheirAccountKey() {
        let leaf = Data(repeating: 1, count: 32)
        let account = Data(repeating: 2, count: 32)
        let unnamed = Data(repeating: 3, count: 32)
        let roster = [
            GroupMember(leafIndex: 0, signaturePublicKey: leaf, accountKey: account),
            GroupMember(leafIndex: 1, signaturePublicKey: unnamed, accountKey: Data()),
        ]
        let map = ChatRepository.personNameMap(rosters: [roster]) { key in
            key == account ? "VG3" : nil
        }
        XCTAssertEqual(map, [account.base64EncodedString(): "VG3", leaf.base64EncodedString(): "VG3"])
    }
}
