// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0051 §1-§3 on iOS: the receive-side verdict matrix, the per-account
/// store, the receive arm's classification against a REAL FFI-composed
/// envelope, the title precedence, and the growth re-send's timing.
final class GroupNameTests: XCTestCase {

    private let owner = Data(repeating: 0x01, count: 32)
    private let admin = Data(repeating: 0x02, count: 32)
    private let member = Data(repeating: 0x03, count: 32)

    private func table() -> PushGroupRouting.SecretAuthority {
        .roleTable(owner: owner, admins: [admin])
    }

    private func classify(
        name: String = "Trio",
        stamp: UInt64 = 20,
        stored: StoredGroupName? = StoredGroupName(name: "Old", updatedAtMs: 10),
        sender: Data?,
        authority: PushGroupRouting.SecretAuthority
    ) -> GroupNameVerdict {
        GroupNameGate.classify(
            incomingName: name, incomingUpdatedAtMs: stamp, stored: stored,
            senderKey: sender, authority: authority)
    }

    // MARK: - Verdict matrix (ADR-0051 §2)

    func testOwnerAndAdminSetANewerNameAndAMemberIsRefused() {
        XCTAssertEqual(classify(sender: owner, authority: table()), .accept)
        XCTAssertEqual(classify(sender: admin, authority: table()), .accept)
        XCTAssertEqual(classify(sender: member, authority: table()), .rejectNotAuthorized)
    }

    func testAnUnresolvableSenderOrAnUnreadableTableIsRefusedFailClosed() {
        XCTAssertEqual(classify(sender: nil, authority: table()), .rejectUnverifiable)
        XCTAssertEqual(classify(sender: Data(), authority: table()), .rejectUnverifiable)
        XCTAssertEqual(classify(sender: owner, authority: .unreadable), .rejectUnverifiable)
    }

    func testALegacyGroupWithNoRoleTableAcceptsFromAnyMember() {
        XCTAssertEqual(classify(sender: member, authority: .noRoleTable), .accept)
        XCTAssertEqual(classify(sender: nil, authority: .noRoleTable), .accept)
    }

    func testOnlyAStrictlyNewerStampWins() {
        XCTAssertEqual(
            classify(stamp: 10, sender: owner, authority: table()), .rejectStale,
            "equal is not newer — a growth re-send of an older name never clobbers")
        XCTAssertEqual(classify(stamp: 9, sender: owner, authority: table()), .rejectStale)
        XCTAssertEqual(classify(stamp: 11, sender: owner, authority: table()), .accept)
        XCTAssertEqual(
            classify(stored: nil, sender: admin, authority: table()), .accept,
            "nothing stored: the first name lands")
    }

    func testAnIdenticalNameAndStampIsAcceptedFromAnyoneBeforeAuthority() {
        let stored = StoredGroupName(name: "Trio", updatedAtMs: 20)
        XCTAssertEqual(classify(stored: stored, sender: member, authority: table()), .acceptIdentical)
        XCTAssertEqual(classify(stored: stored, sender: nil, authority: .unreadable), .acceptIdentical)
        XCTAssertEqual(
            classify(name: "Trio ", stored: stored, sender: member, authority: table()),
            .rejectNotAuthorized, "identical means byte-identical")
    }

    func testAnEmptyNameClearsThroughTheSameGate() {
        XCTAssertEqual(classify(name: "", sender: admin, authority: table()), .accept)
        XCTAssertEqual(classify(name: "", sender: member, authority: table()), .rejectNotAuthorized)
    }

    func testTheOrderIsTheCoreRule() {
        XCTAssertTrue(groupNameIsNewer(incomingUpdatedAtMs: 2, storedUpdatedAtMs: 1))
        XCTAssertFalse(groupNameIsNewer(incomingUpdatedAtMs: 1, storedUpdatedAtMs: 1))
        XCTAssertFalse(groupNameIsNewer(incomingUpdatedAtMs: 0, storedUpdatedAtMs: 1))
    }

    // MARK: - Store

    func testTheStoreIsPerAccountSurvivesAReloadAndClears() {
        let suite = "GroupNameTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = GroupNameStore(defaults: defaults)
        XCTAssertNil(store.load(accountIdB64: "a", groupIdHex: "g1"))

        store.record(accountIdB64: "a", groupIdHex: "g1", value: StoredGroupName(name: "Trio", updatedAtMs: 5))
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g1"), StoredGroupName(name: "Trio", updatedAtMs: 5))
        XCTAssertNil(store.load(accountIdB64: "b", groupIdHex: "g1"), "namespaced by account")

        let reloaded = GroupNameStore(defaults: UserDefaults(suiteName: suite)!)
        XCTAssertEqual(reloaded.load(accountIdB64: "a", groupIdHex: "g1")?.name, "Trio")

        store.record(accountIdB64: "a", groupIdHex: "g1", value: StoredGroupName(name: "", updatedAtMs: 6))
        XCTAssertNil(store.load(accountIdB64: "a", groupIdHex: "g1")?.displayName, "a cleared name has no display name")
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdHex: "g1")?.updatedAtMs, 6, "but its stamp still orders")

        store.remove(accountIdB64: "a", groupIdHex: "g1")
        XCTAssertNil(store.load(accountIdB64: "a", groupIdHex: "g1"))
        store.record(accountIdB64: "a", groupIdHex: "g2", value: StoredGroupName(name: "X", updatedAtMs: 1))
        store.clear(accountIdB64: "a")
        XCTAssertNil(store.load(accountIdB64: "a", groupIdHex: "g2"))
    }

    // MARK: - Title precedence (ADR-0051 §5)

    func testTheSharedNameWinsElseTheMembers() {
        XCTAssertEqual(
            ConversationTitle.resolvedGroupTitle(sharedName: "Trio", otherMemberNames: ["Anna", "Ivan"]), "Trio")
        XCTAssertEqual(
            ConversationTitle.resolvedGroupTitle(sharedName: "  ", otherMemberNames: ["Anna", "Ivan"])
                .withoutIsolates,
            "Anna, Ivan")
        XCTAssertEqual(ConversationTitle.resolvedGroupTitle(sharedName: nil, otherMemberNames: [nil]), "Group")
    }

    // MARK: - Receive arm (a REAL envelope through the real chain)

    private func classifyEvent(_ plaintext: Data, senderLeaf: UInt32? = 3) -> IncomingChatEvent {
        ChatClient.toEvent(
            messageId: 9,
            createdAt: "2026-09-17T00:00:00Z",
            processed: .application(data: plaintext, senderLeaf: senderLeaf, senderAccount: nil),
            resolveSender: { "member-\($0)" }
        )
    }

    func testAGroupNameEnvelopeIsClassifiedWithItsSenderLeafNeverAsChat() throws {
        let bytes = try composeGroupNameEnvelope(groupName: FfiGroupName(name: "Trio", updatedAtMs: 42))
        guard case .groupName(let messageId, let name, let stamp, let leaf, _) = classifyEvent(bytes) else {
            return XCTFail("a group-name envelope must classify as .groupName")
        }
        XCTAssertEqual(messageId, 9)
        XCTAssertEqual(name, "Trio")
        XCTAssertEqual(stamp, 42)
        XCTAssertEqual(leaf, 3, "the leaf rides onward for the role gate; it is never a resolved key here")
        guard case .groupName(_, let cleared, _, .none, _) = classifyEvent(
            try composeGroupNameEnvelope(groupName: FfiGroupName(name: "", updatedAtMs: 43)), senderLeaf: nil)
        else { return XCTFail("the clear envelope is still an envelope, unattributable or not") }
        XCTAssertEqual(cleared, "")
    }

    func testWithoutTheArmTheNameWouldBeDroppedUnread() throws {
        let bytes = try composeGroupNameEnvelope(groupName: FfiGroupName(name: "Trio", updatedAtMs: 1))
        let parsed = parseBodyEnvelope(bytes: bytes)
        // Since 2026-09-29 the parser drops any `lakko/` plaintext as a service
        // envelope (it used to degrade it to a chat body reading
        // `lakko/group-name/v1` plus binary). Without the arm the envelope would
        // therefore never render, and never reach its receive handler either.
        XCTAssertEqual(parsed.kind, .serviceEnvelope)
        XCTAssertEqual(parsed.text, "")
    }

    func testTheComposerRefusesWhatTheDecoderRefuses() {
        XCTAssertThrowsError(
            try composeGroupNameEnvelope(groupName: FfiGroupName(name: "two\nlines", updatedAtMs: 1)))
        XCTAssertThrowsError(
            try composeGroupNameEnvelope(
                groupName: FfiGroupName(name: String(repeating: "n", count: 65), updatedAtMs: 1)))
    }

    // MARK: - Growth re-send timing

    @MainActor
    private final class Harness {
        var sent: [(String, Data)] = []
        var envelopes: [String: Data] = ["g": Data([1, 2, 3])]
        var now: Int64 = 1_000_000
        var jitter: Int64 = 0
        var slept: [Int64] = []
        var failNext = false

        func ports() -> GroupNamePublisher.Ports {
            GroupNamePublisher.Ports(
                send: { [unowned self] hex, bytes in
                    if self.failNext { self.failNext = false; throw ChatError.argument("offline") }
                    self.sent.append((hex, bytes))
                },
                envelope: { [unowned self] hex in self.envelopes[hex] },
                nowMs: { [unowned self] in self.now },
                jitterMs: { [unowned self] in self.jitter },
                sleep: { [unowned self] ms in self.slept.append(ms) }
            )
        }
    }

    @MainActor
    func testGrowthReSendsTheStoredEnvelopeOnlyForGroupsWithOne() async {
        let h = Harness()
        let publisher = GroupNamePublisher(ports: h.ports())
        await publisher.noteGrowth(groupIdHex: "g")?.value
        await publisher.noteGrowth(groupIdHex: "nameless")?.value
        XCTAssertEqual(h.sent.count, 1)
        XCTAssertEqual(h.sent.first?.1, Data([1, 2, 3]), "the STORED envelope, stamp and all")
        XCTAssertEqual(h.slept, [0, 0], "no recent publish: only the jitter, which is zero here")
    }

    @MainActor
    func testGrowthSoonAfterAPublishWaitsOutTheThrottleAndASendInBetweenSkipsIt() async {
        let h = Harness()
        h.jitter = 500
        let publisher = GroupNamePublisher(ports: h.ports())
        publisher.notePublished(groupIdHex: "g")
        h.now += 10_000
        await publisher.noteGrowth(groupIdHex: "g")?.value
        XCTAssertEqual(h.slept, [ProfilePublisher.minIntervalMs - 10_000 + 500])
        XCTAssertEqual(h.sent.count, 1)

        // A rename (notePublished) after the growth was noted makes the
        // scheduled re-send redundant: it was sealed at an epoch that already
        // includes the newcomer.
        let h2 = Harness()
        let p2 = GroupNamePublisher(ports: h2.ports())
        let task = p2.noteGrowth(groupIdHex: "g")
        p2.notePublished(groupIdHex: "g")
        await task?.value
        XCTAssertTrue(h2.sent.isEmpty)
    }

    @MainActor
    func testAFailedReSendIsNotCountedAsPublished() async {
        let h = Harness()
        h.failNext = true
        let publisher = GroupNamePublisher(ports: h.ports())
        await publisher.noteGrowth(groupIdHex: "g")?.value
        XCTAssertTrue(h.sent.isEmpty)
        await publisher.noteGrowth(groupIdHex: "g")?.value
        XCTAssertEqual(h.sent.count, 1, "the next growth re-sends without a throttle from the failure")
        XCTAssertEqual(h.slept, [0, 0])
    }
}
