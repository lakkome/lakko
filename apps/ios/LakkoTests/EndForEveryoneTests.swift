// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// Ending a group call for everyone (`docs/ux/calls.md` section 4.1.2): the
/// pure rule, the starter's person key on the group-call record, and the
/// repository's hand-over of a received `CallEnd`. The hosts' halves are in
/// `CallSlotHostTests.swift`, beside their harnesses.
///
/// Same cases as `apps/web/src/calls/end-for-everyone.test.ts`.
@MainActor
final class EndForEveryoneTests: XCTestCase {

    private let callA = Data(repeating: 0x0A, count: 16)
    private let callB = Data(repeating: 0x0B, count: 16)
    private let t0: Int64 = 1_800_000_000_000
    private let starter: MemberIdentity = "c3RhcnRlcg=="

    // MARK: - The reason on the wire

    /// Composed and parsed by the real core: a reason the core does not know
    /// reads back as `hungUp`, and the end would then take nobody out.
    func testTheReasonSurvivesTheCoresComposeAndParse() throws {
        let bytes = try composeCallSignaling(body: .end(callId: callA, reason: .endedForAll))
        XCTAssertEqual(try parseCallSignaling(plaintext: bytes), .end(callId: callA, reason: .endedForAll))
    }

    // MARK: - The menu

    func testEndOpensTheChoiceForTheStarterWhileSomeoneElseIsInTheCall() {
        XCTAssertTrue(EndForEveryone.endOpensMenu(isStarter: true, othersInCall: 1))
        XCTAssertTrue(EndForEveryone.endOpensMenu(isStarter: true, othersInCall: 5))
    }

    func testEndEndsAtOnceForTheStarterAloneInTheCall() {
        XCTAssertFalse(EndForEveryone.endOpensMenu(isStarter: true, othersInCall: 0))
    }

    func testTheChoiceNeverOpensForAnyoneElse() {
        XCTAssertFalse(EndForEveryone.endOpensMenu(isStarter: false, othersInCall: 3))
    }

    // MARK: - What a received end does

    private func received(
        _ reason: FfiCallEndReason, sender: MemberIdentity?, starter: MemberIdentity?
    ) -> EndForEveryone.ReceivedEnd {
        .init(reason: reason, senderPersonKey: sender, starterPersonKey: starter)
    }

    func testTheStartersEndForEveryoneTakesTheDeviceOut() {
        XCTAssertTrue(EndForEveryone.endsTheCallForMe(
            received(.endedForAll, sender: starter, starter: starter)))
    }

    func testTheSameMessageFromAnyoneElseIsIgnored() {
        XCTAssertFalse(EndForEveryone.endsTheCallForMe(
            received(.endedForAll, sender: "b3RoZXI=", starter: starter)))
    }

    func testAnOrdinaryEndIsIgnoredEvenFromTheStarter() {
        for reason in [FfiCallEndReason.hungUp, .busy, .declined, .timedOut, .error] {
            XCTAssertFalse(
                EndForEveryone.endsTheCallForMe(received(reason, sender: starter, starter: starter)),
                "\(reason)")
        }
    }

    func testNothingHappensWhenTheSenderOrTheStarterIsUnknown() {
        XCTAssertFalse(EndForEveryone.endsTheCallForMe(
            received(.endedForAll, sender: nil, starter: starter)))
        XCTAssertFalse(EndForEveryone.endsTheCallForMe(
            received(.endedForAll, sender: "", starter: "")))
        XCTAssertFalse(EndForEveryone.endsTheCallForMe(
            received(.endedForAll, sender: starter, starter: nil)))
    }

    // MARK: - The line for the others

    func testTheLineNamesTheStarterWhenTheDeviceCan() {
        // The formatter wraps a name in bidi isolates; the words are compared.
        XCTAssertEqual(
            l10nPlain(EndForEveryone.endedText(starterName: "Anna").text),
            "Anna ended the call for everyone")
    }

    func testTheLineSaysItWithoutANameOtherwise() {
        XCTAssertEqual(
            EndForEveryone.endedText(starterName: nil).text, "The call was ended for everyone")
        XCTAssertEqual(
            EndForEveryone.endedText(starterName: "  ").text,
            EndForEveryone.endedText(starterName: nil).text)
    }

    // MARK: - Link calls

    func testOnlyAnEndForEveryoneOfThisCallIsRecognised() throws {
        let ended = try composeCallSignaling(body: .end(callId: callA, reason: .endedForAll))
        XCTAssertTrue(EndForEveryone.isEndForEveryone(plaintext: ended, callId: callA))
        XCTAssertFalse(
            EndForEveryone.isEndForEveryone(plaintext: ended, callId: callB),
            "an end naming another call says nothing about this one")
        let hungUp = try composeCallSignaling(body: .end(callId: callA, reason: .hungUp))
        XCTAssertFalse(EndForEveryone.isEndForEveryone(plaintext: hungUp, callId: callA))
        let leave = try composeCallSignaling(
            body: .groupLeave(callId: callA, leafIndex: 1, reason: .endedForAll))
        XCTAssertFalse(EndForEveryone.isEndForEveryone(plaintext: leave, callId: callA))
        XCTAssertFalse(EndForEveryone.isEndForEveryone(plaintext: Data([0xFF, 0x00, 0x01]), callId: callA))
        XCTAssertFalse(EndForEveryone.isEndForEveryone(plaintext: Data(), callId: callA))
    }

    /// The owner is whoever holds the leaf whose key the signed capability
    /// names; a leaf index alone, another member, or no sender is nobody.
    func testTheLinkOwnerIsTheSenderWhoseLeafHoldsTheCapabilityKey() {
        let owner = String(repeating: "aa", count: 32)
        let guest = String(repeating: "bb", count: 32)
        let members = [
            ElectionMember(leafIndex: 0, signatureKeyHex: owner),
            ElectionMember(leafIndex: 1, signatureKeyHex: guest),
        ]
        XCTAssertTrue(EndForEveryone.sentByOwner(senderLeaf: 0, members: members, ownerKeyHex: owner))
        XCTAssertFalse(EndForEveryone.sentByOwner(senderLeaf: 1, members: members, ownerKeyHex: owner))
        XCTAssertFalse(EndForEveryone.sentByOwner(senderLeaf: nil, members: members, ownerKeyHex: owner))
        XCTAssertFalse(EndForEveryone.sentByOwner(senderLeaf: 7, members: members, ownerKeyHex: owner))
        XCTAssertFalse(EndForEveryone.sentByOwner(senderLeaf: 0, members: [], ownerKeyHex: owner))
        XCTAssertFalse(EndForEveryone.sentByOwner(senderLeaf: 0, members: members, ownerKeyHex: ""))
        // The owner left and its leaf was handed to someone else.
        let reused = [ElectionMember(leafIndex: 0, signatureKeyHex: guest)]
        XCTAssertFalse(EndForEveryone.sentByOwner(senderLeaf: 0, members: reused, ownerKeyHex: owner))
    }

    // MARK: - The starter on the record

    private func startSignal(_ callId: Data, leaf: UInt32) -> GroupCallSignal {
        .start(callId: callId, initiatorLeaf: leaf, createdAtMs: UInt64(t0), audio: true, video: false)
    }

    private func participant(_ leaf: UInt32) -> GroupCallParticipant {
        GroupCallParticipant(deviceKey: "member-\(leaf)", personKey: "person-\(leaf)")
    }

    /// The starter is the announce's MLS sender resolved to the person, not
    /// the leaf the body names.
    func testAStartRecordsItsSendersPersonAsTheStarter() throws {
        let record = try XCTUnwrap(GroupCallReducer.apply(
            startSignal(callA, leaf: 4), participant: participant(4), to: nil, rowCreatedAtMs: t0))
        XCTAssertEqual(record.initiatorPersonKey, "person-4")
        // A re-announce, and the joins that follow, keep it.
        let joined = try XCTUnwrap(GroupCallReducer.apply(
            .join(callId: callA, leafIndex: 5), participant: participant(5), to: record,
            rowCreatedAtMs: t0 + 1_000))
        XCTAssertEqual(joined.initiatorPersonKey, "person-4")
    }

    func testAStartWithNoResolvableSenderHasAnUnknownStarter() throws {
        let record = try XCTUnwrap(GroupCallReducer.apply(
            startSignal(callA, leaf: 4), participant: nil, to: nil, rowCreatedAtMs: t0))
        XCTAssertNil(record.initiatorPersonKey)
    }

    /// A device that missed the announce does not know who started the call,
    /// and the joiner it adopted the record from is not the starter.
    func testARecordAdoptedFromAJoinHasAnUnknownStarter() throws {
        let adopted = try XCTUnwrap(GroupCallReducer.apply(
            .join(callId: callA, leafIndex: 5), participant: participant(5), to: nil,
            rowCreatedAtMs: t0))
        XCTAssertNil(adopted.initiatorPersonKey)
        XCTAssertFalse(EndForEveryone.endsTheCallForMe(received(
            .endedForAll, sender: "person-5", starter: adopted.initiatorPersonKey)))
    }

    private func isolatedDefaults() -> UserDefaults {
        let name = "EndForEveryoneTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }

    func testTheStoreKeepsTheStarterThroughItsRoundTrip() throws {
        let store = GroupCallStore(defaults: isolatedDefaults())
        let record = try XCTUnwrap(GroupCallReducer.apply(
            startSignal(callA, leaf: 4), participant: participant(4), to: nil, rowCreatedAtMs: t0))
        store.save(accountIdB64: "acct", records: ["g": record])
        let loaded = try XCTUnwrap(store.load(accountIdB64: "acct", nowMs: t0 + 1_000)["g"])
        XCTAssertEqual(loaded.initiatorPersonKey, "person-4")
        XCTAssertEqual(loaded, record)
    }

    /// A record stored before the starter was kept reads as unknown, and an
    /// unknown starter can never end the call for everyone.
    func testAStoredRecordWithoutAStarterReadsAsUnknown() throws {
        let defaults = isolatedDefaults()
        let store = GroupCallStore(defaults: defaults)
        let record = try XCTUnwrap(GroupCallReducer.apply(
            startSignal(callA, leaf: 4), participant: participant(4), to: nil, rowCreatedAtMs: t0))
        let encoded = try JSONEncoder().encode(["g": record])
        var json = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: [String: Any]])
        XCTAssertNotNil(json["g"]?["initiatorPersonKey"], "the fixture carries the field to remove")
        json["g"]?.removeValue(forKey: "initiatorPersonKey")
        defaults.set(
            try JSONSerialization.data(withJSONObject: json), forKey: GroupCallStore.key("acct"))

        let loaded = try XCTUnwrap(store.load(accountIdB64: "acct", nowMs: t0 + 1_000)["g"])
        XCTAssertEqual(loaded.callId, callA, "the rest of the record still loads")
        XCTAssertEqual(loaded.initiatorLeaf, 4)
        XCTAssertNil(loaded.initiatorPersonKey)
        XCTAssertFalse(EndForEveryone.endsTheCallForMe(received(
            .endedForAll, sender: "person-4", starter: loaded.initiatorPersonKey)))
    }

    // MARK: - Through the repository

    private func route(
        _ repo: ChatRepository, _ body: FfiCallSignalingBody, id: Int64, group: String = "ab",
        leaf: UInt32?, device: MemberIdentity?, person: MemberIdentity?
    ) throws {
        repo.routeCallSignaling(
            groupIdHex: group, messageId: id, payload: try composeCallSignaling(body: body),
            createdAt: "2027-01-15T08:00:00Z", senderLeaf: leaf, senderDeviceKey: device,
            senderKey: person)
    }

    private func groupStart(_ callId: Data, leaf: UInt32) -> FfiCallSignalingBody {
        .groupStart(
            callId: callId, initiatorLeaf: leaf, createdAtMs: UInt64(t0),
            media: FfiCallMediaFlags(audio: true, video: false))
    }

    /// The repository hands the host the call's starter as it recorded it —
    /// read before the end closes the record — and the row's MLS sender, never
    /// anything the body says.
    func testTheRepositoryReportsAReceivedEndWithItsSenderAndTheRecordedStarter() throws {
        let repo = ChatRepository()
        var ends: [ReceivedGroupCallEnd] = []
        repo.groupCallEndSink = { ends.append($0) }

        try route(repo, groupStart(callA, leaf: 2), id: 1, leaf: 2, device: "member-2", person: "person-2")
        XCTAssertEqual(repo.groupCalls["ab"]?.initiatorPersonKey, "person-2")

        // Another member says "for everyone": the record closes, as for any
        // end, and what the host is told can take nobody out of the call.
        try route(
            repo, .end(callId: callA, reason: .endedForAll), id: 2,
            leaf: 3, device: "member-3", person: "person-3")
        XCTAssertNil(repo.groupCalls["ab"])
        XCTAssertEqual(ends, [ReceivedGroupCallEnd(
            groupIdHex: "ab", callId: callA,
            end: .init(reason: .endedForAll, senderPersonKey: "person-3", starterPersonKey: "person-2"))])
        XCTAssertFalse(EndForEveryone.endsTheCallForMe(ends[0].end))

        // The starter says it.
        try route(repo, groupStart(callB, leaf: 2), id: 3, leaf: 2, device: "member-2", person: "person-2")
        try route(
            repo, .end(callId: callB, reason: .endedForAll), id: 4,
            leaf: 2, device: "member-2", person: "person-2")
        XCTAssertEqual(ends.count, 2)
        XCTAssertEqual(ends[1].callId, callB)
        XCTAssertTrue(EndForEveryone.endsTheCallForMe(ends[1].end))
    }

    /// An end for a call this device holds no record of, or with a sender
    /// that did not resolve, names no starter or no sender: neither can end
    /// a call.
    func testAnEndWithNoRecordOrNoSenderCanNeverEndACall() throws {
        let repo = ChatRepository()
        var ends: [ReceivedGroupCallEnd] = []
        repo.groupCallEndSink = { ends.append($0) }

        try route(
            repo, .end(callId: callA, reason: .endedForAll), id: 1,
            leaf: 2, device: "member-2", person: "person-2")
        XCTAssertNil(ends.last?.end.starterPersonKey)
        XCTAssertFalse(EndForEveryone.endsTheCallForMe(try XCTUnwrap(ends.last).end))

        // The record names another call: its starter is not this call's.
        try route(repo, groupStart(callB, leaf: 2), id: 2, leaf: 2, device: "member-2", person: "person-2")
        try route(
            repo, .end(callId: callA, reason: .endedForAll), id: 3,
            leaf: 2, device: "member-2", person: "person-2")
        XCTAssertNil(ends.last?.end.starterPersonKey)
        XCTAssertEqual(repo.groupCalls["ab"]?.callId, callB, "an end for another call closes nothing")

        try route(
            repo, .end(callId: callB, reason: .endedForAll), id: 4,
            leaf: nil, device: nil, person: nil)
        XCTAssertNil(ends.last?.end.senderPersonKey)
        XCTAssertFalse(EndForEveryone.endsTheCallForMe(try XCTUnwrap(ends.last).end))
    }
}
