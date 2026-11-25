// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0052 decision 3 / `docs/ux/message-actions.md` sections 4 and 4.1 on
/// iOS: the receive verdict matrix, the two stores, the bar's order and
/// advance, the sheet's rows by standing and state, the receive arm's
/// classification of a REAL FFI-composed envelope, and the growth re-send.
final class PinnedMessagesTests: XCTestCase {

    private let owner = Data(repeating: 0x01, count: 32)
    private let admin = Data(repeating: 0x02, count: 32)
    private let member = Data(repeating: 0x03, count: 32)

    private func table() -> PushGroupRouting.SecretAuthority {
        .roleTable(owner: owner, admins: [admin])
    }

    private func classify(
        pinned: Bool = true,
        stamp: UInt64 = 20,
        stored: StoredSharedPin? = StoredSharedPin(pinned: false, updatedAtMs: 10),
        sender: Data?,
        authority: PushGroupRouting.SecretAuthority,
        oneToOne: Bool = false
    ) -> PinVerdict {
        PinGate.classify(
            incomingPinned: pinned, incomingUpdatedAtMs: stamp, stored: stored,
            senderKey: sender, authority: authority, oneToOne: oneToOne)
    }

    private func message(_ id: Int64, deleted: Bool = false, text: String = "hello", ack: ChatMessage.Ack = .sent)
        -> ChatMessage
    {
        ChatMessage(
            messageId: id, direction: .incoming, text: text, createdAt: "2026-09-26T00:00:00Z",
            authorKeyB64: "k", deleted: deleted, ack: ack)
    }

    // MARK: - Verdict matrix (section 4.1 "What a receiver does")

    func testOwnerAndAdminPinAndAMemberIsRefusedInAGroup() {
        XCTAssertEqual(classify(sender: owner, authority: table()), .accept)
        XCTAssertEqual(classify(sender: admin, authority: table()), .accept)
        XCTAssertEqual(classify(sender: member, authority: table()), .rejectNotAuthorized)
    }

    func testEitherParticipantOfAOneToOneMayPinAndClear() {
        // The one-to-one carries a table with one owner, and it does not
        // decide: the other participant is a plain member of it.
        XCTAssertEqual(classify(sender: member, authority: table(), oneToOne: true), .accept)
        XCTAssertEqual(classify(pinned: false, sender: member, authority: table(), oneToOne: true), .accept)
        XCTAssertEqual(
            classify(sender: nil, authority: table(), oneToOne: true), .rejectUnverifiable,
            "a sender the roster cannot name is not a participant")
    }

    /// ADR-0062 — a two-person GROUP is not a one-to-one: its plain member
    /// may not pin for everyone. What decides is the kind, never the count.
    func testATwoPersonGroupFollowsTheRoleTable() {
        XCTAssertEqual(classify(sender: member, authority: table(), oneToOne: false), .rejectNotAuthorized)
        XCTAssertEqual(classify(sender: owner, authority: table(), oneToOne: false), .accept)
        XCTAssertFalse(PinGate.maySetShared(oneToOne: false, ownRole: .member, membershipEnded: false))
    }

    func testAnUnresolvableSenderOrAnUnreadableTableIsRefusedFailClosed() {
        XCTAssertEqual(classify(sender: nil, authority: table()), .rejectUnverifiable)
        XCTAssertEqual(classify(sender: Data(), authority: table()), .rejectUnverifiable)
        XCTAssertEqual(classify(sender: owner, authority: .unreadable), .rejectUnverifiable)
    }

    func testAGroupWithNoRoleTableAcceptsFromAnyMember() {
        XCTAssertEqual(classify(sender: member, authority: .noRoleTable), .accept)
    }

    func testOnlyAStrictlyNewerStampWinsPerMessage() {
        XCTAssertEqual(
            classify(stamp: 10, sender: owner, authority: table()), .rejectStale,
            "equal is not newer: a re-send is not an update")
        XCTAssertEqual(classify(stamp: 9, sender: owner, authority: table()), .rejectStale)
        XCTAssertEqual(classify(stamp: 11, sender: owner, authority: table()), .accept)
        XCTAssertEqual(classify(stored: nil, sender: admin, authority: table()), .accept)
    }

    /// The reason an unpin is stored rather than deleted: a pin that arrives
    /// AFTER a later unpin (a slow journal, a growth re-send of the original
    /// pin) must never bring the pin back.
    func testAStalePinAfterAnUnpinIsDropped() {
        let unpinnedAt30 = StoredSharedPin(pinned: false, updatedAtMs: 30)
        XCTAssertEqual(
            classify(pinned: true, stamp: 20, stored: unpinnedAt30, sender: owner, authority: table()),
            .rejectStale)
        XCTAssertEqual(
            classify(pinned: true, stamp: 30, stored: unpinnedAt30, sender: owner, authority: table()),
            .rejectStale, "same stamp, other action: still not newer")
        XCTAssertEqual(
            classify(pinned: true, stamp: 31, stored: unpinnedAt30, sender: owner, authority: table()),
            .accept)
    }

    func testAnIdenticalActionAndStampIsAcceptedFromAnyoneBeforeAuthority() {
        let stored = StoredSharedPin(pinned: true, updatedAtMs: 20)
        XCTAssertEqual(classify(stored: stored, sender: member, authority: table()), .acceptIdentical)
        XCTAssertEqual(classify(stored: stored, sender: nil, authority: .unreadable), .acceptIdentical)
    }

    func testTheOrderIsTheCoreRule() {
        XCTAssertTrue(pinIsNewer(incomingUpdatedAtMs: 2, storedUpdatedAtMs: 1))
        XCTAssertFalse(pinIsNewer(incomingUpdatedAtMs: 1, storedUpdatedAtMs: 1))
        XCTAssertFalse(pinIsNewer(incomingUpdatedAtMs: 0, storedUpdatedAtMs: 1))
    }

    // MARK: - Local standing (section 4.1 "The sheet")

    func testWhoMaySetASharedPin() {
        XCTAssertTrue(PinGate.maySetShared(oneToOne: true, ownRole: .member, membershipEnded: false))
        XCTAssertTrue(PinGate.maySetShared(oneToOne: false, ownRole: .owner, membershipEnded: false))
        XCTAssertTrue(PinGate.maySetShared(oneToOne: false, ownRole: .admin, membershipEnded: false))
        XCTAssertTrue(PinGate.maySetShared(oneToOne: false, ownRole: nil, membershipEnded: false), "no table: open")
        XCTAssertFalse(PinGate.maySetShared(oneToOne: false, ownRole: .member, membershipEnded: false))
        XCTAssertFalse(PinGate.maySetShared(oneToOne: true, ownRole: .member, membershipEnded: true))
    }

    // MARK: - Stores

    func testTheSharedStoreKeepsUnpinsIsPerAccountAndWipesWithTheConversation() {
        let suite = "PinnedMessagesTests.shared.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = SharedPinStore(defaults: defaults)
        XCTAssertTrue(store.load(accountIdB64: "a", groupIdHex: "g1").isEmpty)

        store.record(accountIdB64: "a", groupIdHex: "g1", messageId: 5, value: StoredSharedPin(pinned: true, updatedAtMs: 1))
        store.record(accountIdB64: "a", groupIdHex: "g1", messageId: 6, value: StoredSharedPin(pinned: false, updatedAtMs: 2))
        let rows = store.load(accountIdB64: "a", groupIdHex: "g1")
        XCTAssertEqual(rows[5], StoredSharedPin(pinned: true, updatedAtMs: 1))
        XCTAssertEqual(rows[6], StoredSharedPin(pinned: false, updatedAtMs: 2), "an unpin is a row, not an absence")
        XCTAssertTrue(store.load(accountIdB64: "b", groupIdHex: "g1").isEmpty, "namespaced by account")

        let reloaded = SharedPinStore(defaults: UserDefaults(suiteName: suite)!)
        XCTAssertEqual(reloaded.load(accountIdB64: "a", groupIdHex: "g1").count, 2)

        store.remove(accountIdB64: "a", groupIdHex: "g1", messageId: 5)
        XCTAssertNil(store.load(accountIdB64: "a", groupIdHex: "g1")[5])
        store.record(accountIdB64: "a", groupIdHex: "g2", messageId: 1, value: StoredSharedPin(pinned: true, updatedAtMs: 9))
        store.removeGroup(accountIdB64: "a", groupIdHex: "g1")
        XCTAssertTrue(store.load(accountIdB64: "a", groupIdHex: "g1").isEmpty)
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g2").count, 1, "another conversation is untouched")
        store.clear(accountIdB64: "a")
        XCTAssertTrue(store.load(accountIdB64: "a", groupIdHex: "g2").isEmpty)
    }

    func testThePersonalStoreIsASetKeyedOnConversationAndMessage() {
        let suite = "PinnedMessagesTests.personal.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = PersonalPinStore(defaults: defaults)
        store.set(accountIdB64: "a", groupIdHex: "g1", messageId: 5, pinnedAtMs: 100)
        store.set(accountIdB64: "a", groupIdHex: "g2", messageId: 5, pinnedAtMs: 200)
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g1"), [5: 100])
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g2"), [5: 200], "the same id in another chat is another pin")
        store.remove(accountIdB64: "a", groupIdHex: "g1", messageId: 5)
        XCTAssertTrue(store.load(accountIdB64: "a", groupIdHex: "g1").isEmpty)
        store.removeGroup(accountIdB64: "a", groupIdHex: "g2")
        XCTAssertTrue(store.load(accountIdB64: "a", groupIdHex: "g2").isEmpty)
    }

    // MARK: - The bar (section 4)

    func testTheBarListsSharedBeforePersonalNewestFirstAndOnlyHeldMessages() {
        let held = [message(1), message(2), message(3), message(4), message(5, deleted: true)]
        let shared: [Int64: StoredSharedPin] = [
            1: StoredSharedPin(pinned: true, updatedAtMs: 10),
            3: StoredSharedPin(pinned: true, updatedAtMs: 30),
            2: StoredSharedPin(pinned: false, updatedAtMs: 40),   // unpinned: not shown
            5: StoredSharedPin(pinned: true, updatedAtMs: 50),    // deleted: not shown
            9: StoredSharedPin(pinned: true, updatedAtMs: 60),    // not held: not shown
        ]
        let personal: [Int64: UInt64] = [4: 5, 3: 99, 5: 7]
        let entries = PinnedBar.entries(shared: shared, personal: personal, held: held)
        XCTAssertEqual(entries.map(\.messageId), [3, 1, 4])
        XCTAssertEqual(entries.map(\.scope), [.shared, .shared, .personal])
        XCTAssertEqual(entries[0].scope, .shared, "pinned in both scopes shows once, as shared")
    }

    func testTheBarSkipsRowsThatHaveNotLandedAndSystemRows() {
        let pending = message(0, ack: .pending)
        let system = ChatMessage(messageId: 7, direction: .system, text: "[mls commit]", createdAt: "x")
        let entries = PinnedBar.entries(
            shared: [7: StoredSharedPin(pinned: true, updatedAtMs: 1)], personal: [0: 1],
            held: [pending, system])
        XCTAssertTrue(entries.isEmpty)
    }

    func testATapAdvancesToTheNextOlderPinAndWrapsPastTheOldest() {
        XCTAssertEqual(PinnedBar.advanced(from: 0, count: 3), 1)
        XCTAssertEqual(PinnedBar.advanced(from: 1, count: 3), 2)
        XCTAssertEqual(PinnedBar.advanced(from: 2, count: 3), 0, "past the oldest it wraps to the newest")
        XCTAssertEqual(PinnedBar.advanced(from: 0, count: 1), 0)
        XCTAssertEqual(PinnedBar.advanced(from: 0, count: 0), 0)
        XCTAssertEqual(PinnedBar.clamped(5, count: 3), 0, "a list that shrank under the index shows the newest")
        XCTAssertEqual(PinnedBar.clamped(2, count: 3), 2)
    }

    func testADeletedMessageTakesItsSharedRecordWithIt() {
        let shared: [Int64: StoredSharedPin] = [
            1: StoredSharedPin(pinned: true, updatedAtMs: 1),
            2: StoredSharedPin(pinned: false, updatedAtMs: 2),
            3: StoredSharedPin(pinned: true, updatedAtMs: 3),
        ]
        let held = [message(1, deleted: true), message(2, deleted: true), message(4)]
        XCTAssertEqual(PinnedBar.recordsToDrop(shared: shared, held: held), [1, 2])
        XCTAssertFalse(
            PinnedBar.recordsToDrop(shared: shared, held: held).contains(3),
            "a message this device does not hold is absent, not deleted")
    }

    func testTheIndicatorWindowKeepsTheShownPinVisible() {
        XCTAssertEqual(PinnedStackIndicator.window(index: 0, count: 3, segments: 4), 0)
        XCTAssertEqual(PinnedStackIndicator.window(index: 5, count: 9, segments: 4), 2)
        XCTAssertEqual(PinnedStackIndicator.window(index: 8, count: 9, segments: 4), 5)
        XCTAssertEqual(PinnedBar.positionLabel(index: 1, count: 3).withoutIsolates, "Pinned message 2 of 3")
        XCTAssertEqual(PinnedBar.positionLabel(index: 0, count: 1), "Pinned message")
    }

    // MARK: - The sheet (section 4.1)

    func testTheSheetOffersForMeAlwaysAndForEveryoneOnlyWithStanding() {
        let admin = PinSheet.options(sharedPinned: false, personalPinned: false, maySetShared: true, oneToOne: false)
        XCTAssertEqual(admin.map(\.choice), [.pinForMe, .pinForEveryone])
        XCTAssertEqual(admin.map(\.title), ["Pin for me", "Pin for everyone"])
        XCTAssertEqual(admin.map(\.subtitle), ["Only on this device", "Every member sees it"])
        XCTAssertNil(PinSheet.actsAtOnce(admin), "a pin always asks")

        let member = PinSheet.options(sharedPinned: false, personalPinned: false, maySetShared: false, oneToOne: false)
        XCTAssertEqual(member.map(\.choice), [.pinForMe], "one row and no explanation")

        let oneToOne = PinSheet.options(sharedPinned: false, personalPinned: false, maySetShared: true, oneToOne: true)
        XCTAssertEqual(oneToOne.map(\.title), ["Pin for me", "Pin for both"])
        XCTAssertEqual(oneToOne.map(\.subtitle), ["Only on this device", "You both see it"],
                       "a one-to-one has two people, not members")
    }

    func testUnpinActsAtOnceInOneScopeAndAsksInTwo() {
        let onlyMine = PinSheet.options(sharedPinned: false, personalPinned: true, maySetShared: true, oneToOne: false)
        XCTAssertEqual(PinSheet.actsAtOnce(onlyMine), .unpinForMe)
        let onlyShared = PinSheet.options(sharedPinned: true, personalPinned: false, maySetShared: true, oneToOne: false)
        XCTAssertEqual(PinSheet.actsAtOnce(onlyShared), .unpinForEveryone)
        let both = PinSheet.options(sharedPinned: true, personalPinned: true, maySetShared: true, oneToOne: false)
        XCTAssertEqual(both.map(\.choice), [.unpinForMe, .unpinForEveryone])
        XCTAssertEqual(both.map(\.title), ["Unpin for me", "Unpin for everyone"])
        XCTAssertNil(PinSheet.actsAtOnce(both))
        let bothOneToOne = PinSheet.options(sharedPinned: true, personalPinned: true, maySetShared: true, oneToOne: true)
        XCTAssertEqual(bothOneToOne.map(\.title), ["Unpin for me", "Unpin for both"])
        XCTAssertEqual(bothOneToOne.map(\.subtitle), ["Only on this device", "You both see it"])
    }

    /// The sentence the spec writes out: a plain member holding a message an
    /// admin pinned sees `Unpin for me` only when they also pinned it for
    /// themselves; otherwise the row is `Pin` and the sheet offers `Pin for
    /// me` alone.
    func testAPlainMemberCannotClearTheRoomsPin() {
        XCTAssertEqual(PinSheet.menuState(sharedPinned: true, personalPinned: false, maySetShared: false), .pin)
        XCTAssertEqual(
            PinSheet.options(sharedPinned: true, personalPinned: false, maySetShared: false, oneToOne: false)
                .map(\.choice),
            [.pinForMe])
        XCTAssertEqual(PinSheet.menuState(sharedPinned: true, personalPinned: true, maySetShared: false), .unpin)
        let options = PinSheet.options(sharedPinned: true, personalPinned: true, maySetShared: false, oneToOne: false)
        XCTAssertEqual(options.map(\.choice), [.unpinForMe])
        XCTAssertEqual(PinSheet.actsAtOnce(options), .unpinForMe)
    }

    func testTheMenuRowWord() {
        XCTAssertEqual(PinSheet.menuState(sharedPinned: false, personalPinned: false, maySetShared: true), .pin)
        XCTAssertEqual(PinSheet.menuState(sharedPinned: true, personalPinned: false, maySetShared: true), .unpin)
        XCTAssertEqual(PinSheet.menuState(sharedPinned: false, personalPinned: true, maySetShared: false), .unpin)
    }

    /// Section 2 — `Pin` sits with the actions that change this message, after
    /// the read-only pair (Copy, Select) and before Edit; `Unpin` takes the
    /// same slot. (Section 4.1's "after Forward" was where it sat before
    /// section 8 put Select in the card.)
    func testPinSitsWithTheChangingActionsInTheCard() {
        XCTAssertEqual(
            MessageMenuAction.offered(
                canReply: true, canForward: true, canEditOrDelete: true, hasText: true, pin: .pin,
                canSelect: true),
            [.reply, .forward, .copy, .select, .pin, .edit, .delete])
        XCTAssertEqual(
            MessageMenuAction.offered(canReply: true, canForward: true, canEditOrDelete: true, hasText: true, pin: .pin),
            [.reply, .forward, .copy, .pin, .edit, .delete])
        XCTAssertEqual(
            MessageMenuAction.offered(canReply: true, canForward: true, canEditOrDelete: false, hasText: false, pin: .unpin),
            [.reply, .forward, .unpin])
        XCTAssertEqual(
            MessageMenuAction.offered(canReply: true, canForward: true, canEditOrDelete: false, hasText: true),
            [.reply, .forward, .copy], "no state, no row")
    }

    // MARK: - Receive arm (a REAL envelope through the real chain)

    private func classifyEvent(_ plaintext: Data, senderLeaf: UInt32? = 3) -> IncomingChatEvent {
        ChatClient.toEvent(
            messageId: 9,
            createdAt: "2026-09-26T00:00:00Z",
            processed: .application(data: plaintext, senderLeaf: senderLeaf, senderAccount: nil),
            resolveSender: { "member-\($0)" }
        )
    }

    func testAPinEnvelopeIsClassifiedWithItsSenderLeafNeverAsChat() {
        let bytes = composePinEnvelope(op: FfiPinOp(messageId: 41, pinned: true, updatedAtMs: 77))
        guard case .pin(let messageId, let op, let leaf, _) = classifyEvent(bytes) else {
            return XCTFail("a pin envelope must classify as .pin")
        }
        XCTAssertEqual(messageId, 9)
        XCTAssertEqual(op, FfiPinOp(messageId: 41, pinned: true, updatedAtMs: 77))
        XCTAssertEqual(leaf, 3, "the leaf rides onward for the standing check; it is never a resolved key here")
        guard case .pin(_, let unpin, .none, _) = classifyEvent(
            composePinEnvelope(op: FfiPinOp(messageId: 41, pinned: false, updatedAtMs: 78)), senderLeaf: nil)
        else { return XCTFail("the unpin envelope is still an envelope, unattributable or not") }
        XCTAssertFalse(unpin.pinned)
    }

    func testWithoutTheArmThePinWouldBeDroppedUnread() {
        let bytes = composePinEnvelope(op: FfiPinOp(messageId: 1, pinned: true, updatedAtMs: 1))
        let parsed = parseBodyEnvelope(bytes: bytes)
        // Since 2026-09-29 the parser drops any `lakko/` plaintext as a service
        // envelope (it used to degrade it to a chat body reading
        // `lakko/pin/v1` plus binary). Without the arm the envelope would
        // therefore never render, and never reach its receive handler either.
        XCTAssertEqual(parsed.kind, .serviceEnvelope)
        XCTAssertEqual(parsed.text, "")
    }

    func testTheDecoderRoundTripsAndRefusesForeignBytes() throws {
        let op = FfiPinOp(messageId: -5, pinned: false, updatedAtMs: UInt64.max)
        XCTAssertEqual(try decodePinEnvelope(plaintext: composePinEnvelope(op: op)), op)
        XCTAssertThrowsError(try decodePinEnvelope(plaintext: Data("{\"v\":1}".utf8)))
        XCTAssertThrowsError(
            try decodePinEnvelope(
                plaintext: try composeGroupNameEnvelope(groupName: FfiGroupName(name: "x", updatedAtMs: 1))),
            "a group name of the same length is not a pin")
    }

    // MARK: - Growth re-send

    @MainActor
    private final class Harness {
        var sent: [(String, Data)] = []
        var envelopes: [String: [Data]] = ["g": [Data([1]), Data([2])]]
        var now: Int64 = 1_000_000
        var jitter: Int64 = 0
        var slept: [Int64] = []
        var failNext = false

        func ports() -> PinPublisher.Ports {
            PinPublisher.Ports(
                send: { [unowned self] hex, bytes in
                    if self.failNext { self.failNext = false; throw ChatError.argument("offline") }
                    self.sent.append((hex, bytes))
                },
                envelopes: { [unowned self] hex in self.envelopes[hex] ?? [] },
                nowMs: { [unowned self] in self.now },
                jitterMs: { [unowned self] in self.jitter },
                sleep: { [unowned self] ms in self.slept.append(ms) }
            )
        }
    }

    @MainActor
    func testGrowthReSendsEveryStoredPinVerbatimOnlyForGroupsWithOne() async {
        let h = Harness()
        let publisher = PinPublisher(ports: h.ports())
        await publisher.noteGrowth(groupIdHex: "g")?.value
        await publisher.noteGrowth(groupIdHex: "unpinned")?.value
        XCTAssertEqual(h.sent.map(\.1), [Data([1]), Data([2])], "the STORED envelopes, stamps and all")
        XCTAssertEqual(h.sent.map(\.0), ["g", "g"])
    }

    @MainActor
    func testAFailedReSendDoesNotStopTheRestAndTheNextGrowthTriesAgain() async {
        let h = Harness()
        let publisher = PinPublisher(ports: h.ports())
        h.failNext = true
        await publisher.noteGrowth(groupIdHex: "g")?.value
        XCTAssertEqual(h.sent.map(\.1), [Data([2])], "the first failed, the second still went")
        h.now += ProfilePublisher.minIntervalMs + 1
        await publisher.noteGrowth(groupIdHex: "g")?.value
        XCTAssertEqual(h.sent.count, 3)
    }

    @MainActor
    func testAPinSentAfterTheGrowthWasNotedSkipsTheScheduledReSend() async {
        let h = Harness()
        let publisher = PinPublisher(ports: h.ports())
        let task = publisher.noteGrowth(groupIdHex: "g")
        publisher.notePublished(groupIdHex: "g")
        await task?.value
        XCTAssertTrue(h.sent.isEmpty, "a pin that went out after the growth was sealed with the newcomer in")
    }
}
