// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// The group-call announce contract (`GroupCallStart` / `GroupCallJoin` /
/// `GroupCallLeave` / `CallEnd` on the chat's MLS journal) — the reducer, the
/// liveness rule, the C-11 routing split, the banner decision, and the
/// first-byte sniffers that keep a group-call body off the timeline and out of
/// a notification preview.
///
/// The FFI-facing cases compose their fixtures with the REAL
/// `composeCallSignaling`, so the sniffer is checked against the bytes prost
/// actually emits rather than against a hand-typed tag.
@MainActor
final class GroupCallRecordTests: XCTestCase {

    // MARK: - Fixtures

    private let callA = Data(repeating: 0x0A, count: 16)
    private let callB = Data(repeating: 0x0B, count: 16)
    private let t0: Int64 = 1_800_000_000_000

    private func start(
        _ callId: Data,
        at createdAtMs: Int64,
        leaf: UInt32 = 1,
        video: Bool = false
    ) -> GroupCallSignal {
        .start(
            callId: callId,
            initiatorLeaf: leaf,
            createdAtMs: UInt64(createdAtMs),
            audio: true,
            video: video
        )
    }

    private func record(
        _ signals: [(GroupCallSignal, Int64)],
        from initial: OngoingGroupCall? = nil
    ) -> OngoingGroupCall? {
        signals.reduce(initial) { current, step in
            GroupCallReducer.apply(
                step.0, participant: step.0.claimedLeaf.map(participant), to: current,
                rowCreatedAtMs: step.1)
        }
    }

    /// The fixture DEVICE key of the leaf: in these reducer tests a leaf
    /// resolves to one fixed key, as it does within one epoch.
    private func member(_ leaf: UInt32) -> MemberIdentity { "member-\(leaf)" }

    /// The leaf as a participant: its device key, and a person of its own.
    private func participant(_ leaf: UInt32) -> GroupCallParticipant {
        GroupCallParticipant(deviceKey: member(leaf), personKey: "person-\(leaf)")
    }

    private func members(_ leaves: UInt32...) -> Set<MemberIdentity> { Set(leaves.map(member)) }

    // MARK: - Create / join / leave / end

    func testStartCreatesARecordWithNobodyJoined() throws {
        let r = try XCTUnwrap(record([(start(callA, at: t0, leaf: 4, video: true), t0)]))
        XCTAssertEqual(r.callId, callA)
        XCTAssertEqual(r.initiatorLeaf, 4)
        XCTAssertTrue(r.video)
        XCTAssertEqual(r.startedAtMs, t0)
        XCTAssertTrue(r.joined.isEmpty)
    }

    func testStartTimeIsBoundedByTheServerRowTime() throws {
        // A sender claiming a start an hour in the future of its DS row does
        // not get to keep its call "fresh" for an extra hour.
        let r = try XCTUnwrap(record([(start(callA, at: t0 + 3_600_000), t0)]))
        XCTAssertEqual(r.startedAtMs, t0)
        // A claimed start earlier than the row is kept as claimed.
        let early = try XCTUnwrap(record([(start(callA, at: t0 - 5_000), t0)]))
        XCTAssertEqual(early.startedAtMs, t0 - 5_000)
    }

    func testJoinAndLeaveUpdateJoinedLeavesForTheMatchingCall() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
            (.join(callId: callA, leafIndex: 2), t0 + 2_000),
            (.leave(callId: callA, leafIndex: 1), t0 + 3_000),
        ]))
        XCTAssertEqual(Set(r.joined.keys), members(2))
        XCTAssertEqual(r.lastActivityMs, t0 + 3_000)
    }

    func testJoinLeaveAndEndForAnotherCallIdAreDropped() throws {
        // C-12 — a body naming a call id the record does not track changes
        // nothing, in particular a stray CallEnd does not clear the banner.
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
            (.join(callId: callB, leafIndex: 7), t0 + 2_000),
            (.leave(callId: callB, leafIndex: 1), t0 + 3_000),
            (.end(callId: callB), t0 + 4_000),
        ]))
        XCTAssertEqual(r.callId, callA)
        XCTAssertEqual(Set(r.joined.keys), members(1))
    }

    /// CHANGED BEHAVIOUR, deliberately. This used to assert that a join with no
    /// record created nothing, and a live run showed what that cost: the Start
    /// is announced exactly once, so a device that was not polling in that
    /// moment could never learn the call existed, and every heartbeat from the
    /// people in it folded to a no-op. The record is now adopted from the
    /// heartbeat instead — see `GroupCallRecordSelectionTests` for what it does
    /// and does not claim, and for the protective half that survives.
    func testJoinWithNoRecordAdoptsTheCallItNames() throws {
        let r = try XCTUnwrap(record([(.join(callId: callA, leafIndex: 1), t0)]))
        XCTAssertEqual(r.callId, callA)
        XCTAssertEqual(Set(r.joined.keys), members(1))
        XCTAssertNil(r.initiatorLeaf, "an adopted record never saw the announce")
    }

    func testEndClearsTheMatchingRecord() {
        XCTAssertNil(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
            (.end(callId: callA), t0 + 2_000),
        ]))
    }

    func testReannouncingTheTrackedCallKeepsItsJoins() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 3), t0 + 1_000),
            (start(callA, at: t0 + 5_000), t0 + 5_000),
        ]))
        XCTAssertEqual(Set(r.joined.keys), members(3))
        XCTAssertEqual(r.startedAtMs, t0)
    }

    // MARK: - Liveness

    func testAnUnjoinedCallIsLiveForTheGraceWindowOnly() throws {
        let r = try XCTUnwrap(record([(start(callA, at: t0), t0)]))
        XCTAssertTrue(GroupCallReducer.isLive(r, nowMs: t0))
        XCTAssertTrue(GroupCallReducer.isLive(r, nowMs: t0 + GroupCallReducer.unjoinedGraceMs))
        XCTAssertFalse(GroupCallReducer.isLive(r, nowMs: t0 + GroupCallReducer.unjoinedGraceMs + 1))
    }

    /// A member that keeps its heartbeat going keeps the call live up to the
    /// max age, and not a millisecond past it.
    func testAHeartbeatingCallStaysLiveUntilTheMaxAge() throws {
        let beats = stride(
            from: t0 + 1_000, through: t0 + GroupCallReducer.maxLiveAgeMs + 60_000,
            by: Int(GroupCallReducer.heartbeatMs)
        ).map { (GroupCallSignal.join(callId: callA, leafIndex: 1), $0) }
        let r = try XCTUnwrap(record([(start(callA, at: t0), t0)] + beats))
        XCTAssertTrue(GroupCallReducer.isLive(r, nowMs: t0 + 2 * 60 * 60 * 1000))
        XCTAssertTrue(GroupCallReducer.isLive(r, nowMs: t0 + GroupCallReducer.maxLiveAgeMs))
        XCTAssertFalse(GroupCallReducer.isLive(r, nowMs: t0 + GroupCallReducer.maxLiveAgeMs + 1))
    }

    /// The live defect: a participant whose app was killed never publishes a
    /// Leave. Its join stops counting once it is older than the fresh window,
    /// and the call stops being live on its own — no new row needed.
    func testAMemberWhoseHeartbeatsStoppedStopsCountingAndTheGhostCallDies() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
        ]))
        let lastSeen = t0 + 1_000
        XCTAssertEqual(r.freshDevices(nowMs: lastSeen + GroupCallReducer.memberFreshMs), members(1))
        XCTAssertTrue(GroupCallReducer.isLive(r, nowMs: lastSeen + GroupCallReducer.memberFreshMs))
        XCTAssertEqual(r.freshDevices(nowMs: lastSeen + GroupCallReducer.memberFreshMs + 1), [])
        XCTAssertFalse(GroupCallReducer.isLive(r, nowMs: lastSeen + GroupCallReducer.memberFreshMs + 1))
        XCTAssertEqual(
            GroupCallAffordance.of(record: r, activeGroupCallId: nil, ownDeviceKey: nil, nowMs: lastSeen + GroupCallReducer.memberFreshMs + 1),
            .none)
    }

    /// Fresh and stale members side by side: only the fresh one is counted.
    func testTheCountIsFreshMembersOnly() throws {
        let now = t0 + 10 * 60_000
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),        // stale by `now`
            (.join(callId: callA, leafIndex: 2), now - 30_000),      // fresh
        ]))
        XCTAssertEqual(Set(r.joined.keys), members(1, 2), "a stale member is kept, just not counted")
        XCTAssertEqual(r.freshDevices(nowMs: now), members(2))
        XCTAssertEqual(
            GroupCallAffordance.of(record: r, activeGroupCallId: nil, ownDeviceKey: nil, nowMs: now),
            .joinable(callId: callA, joinedCount: 1, video: false))
    }

    /// A heartbeat advances a member's last-seen time; an older row (a replay,
    /// or a join that arrived late) never lowers it.
    func testAHeartbeatAdvancesLastSeenAndAnOlderRowNeverLowersIt() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
            (.join(callId: callA, leafIndex: 1), t0 + 61_000),
        ]))
        XCTAssertEqual(r.joined[member(1)]?.lastSeenMs, t0 + 61_000)
        let replay = try XCTUnwrap(record([(.join(callId: callA, leafIndex: 1), t0 + 5_000)], from: r))
        XCTAssertEqual(replay.joined[member(1)]?.lastSeenMs, t0 + 61_000)
    }

    func testALeaveRemovesTheMemberAndAnEndRemovesTheRecord() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
            (.join(callId: callA, leafIndex: 2), t0 + 2_000),
            (.leave(callId: callA, leafIndex: 1), t0 + 3_000),
        ]))
        XCTAssertNil(r.joined[member(1)])
        XCTAssertEqual(r.freshDevices(nowMs: t0 + 3_000), members(2))
        XCTAssertNil(record([(.end(callId: callA), t0 + 4_000)], from: r))
    }

    /// A join or leave whose MLS sender did not resolve to a signature key names
    /// nobody, and is dropped.
    func testAJoinOrLeaveWithAnUnresolvableSenderIsDropped() throws {
        let started = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
        ]))
        let join = GroupCallReducer.apply(
            .join(callId: callA, leafIndex: 2), participant: nil, to: started, rowCreatedAtMs: t0 + 2_000)
        XCTAssertEqual(join, started)
        let leave = GroupCallReducer.apply(
            .leave(callId: callA, leafIndex: 1), participant: nil, to: started, rowCreatedAtMs: t0 + 2_000)
        XCTAssertEqual(leave, started)
    }

    /// C-68: leaf 1 is removed and the next member added inherits leaf 1. The
    /// newcomer resolves to its OWN key, so it does not inherit the departed
    /// member's place in the call, and the departed member's stale entry does
    /// not keep counting for the newcomer.
    func testAMemberAddedIntoAVacatedLeafDoesNotInheritMembership() throws {
        var r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
        ]))
        // Bob (leaf 1) is removed from the group without leaving the call; Dave
        // is added into leaf 1 and leaves a call he never joined.
        r = try XCTUnwrap(GroupCallReducer.apply(
            .leave(callId: callA, leafIndex: 1), participant: GroupCallParticipant(deviceKey: "dave", personKey: "person-dave"), to: r, rowCreatedAtMs: t0 + 2_000))
        XCTAssertEqual(Set(r.joined.keys), members(1), "Dave's leave cannot remove Bob")
        r = try XCTUnwrap(GroupCallReducer.apply(
            .join(callId: callA, leafIndex: 1), participant: GroupCallParticipant(deviceKey: "dave", personKey: "person-dave"), to: r, rowCreatedAtMs: t0 + 3_000))
        XCTAssertEqual(Set(r.joined.keys), members(1).union(["dave"]))
        // Bob's heartbeats stopped with his removal: only Dave is still counted.
        let later = t0 + 1_000 + GroupCallReducer.memberFreshMs + 1
        XCTAssertEqual(r.freshDevices(nowMs: later), ["dave"])
    }

    /// Two devices of one person in the call: one device leaves, the other keeps
    /// the call live, and the banner still counts one person.
    func testTwoDevicesOfOnePersonCountOnceAndOneLeavingKeepsTheCall() throws {
        let phone = GroupCallParticipant(deviceKey: "alice-phone", personKey: "alice")
        let tablet = GroupCallParticipant(deviceKey: "alice-tablet", personKey: "alice")
        var r = try XCTUnwrap(record([(start(callA, at: t0), t0)]))
        r = try XCTUnwrap(GroupCallReducer.apply(
            .join(callId: callA, leafIndex: 1), participant: phone, to: r, rowCreatedAtMs: t0 + 1_000))
        r = try XCTUnwrap(GroupCallReducer.apply(
            .join(callId: callA, leafIndex: 2), participant: tablet, to: r, rowCreatedAtMs: t0 + 2_000))
        XCTAssertEqual(r.freshPeopleCount(nowMs: t0 + 3_000), 1)
        XCTAssertEqual(
            GroupCallAffordance.of(record: r, activeGroupCallId: nil, ownDeviceKey: nil, nowMs: t0 + 3_000),
            .joinable(callId: callA, joinedCount: 1, video: false))

        r = try XCTUnwrap(GroupCallReducer.apply(
            .leave(callId: callA, leafIndex: 1), participant: phone, to: r, rowCreatedAtMs: t0 + 4_000))
        let now = t0 + 5 * 60_000
        XCTAssertEqual(r.freshDevices(nowMs: t0 + 5_000), ["alice-tablet"])
        XCTAssertTrue(GroupCallReducer.isLive(r, nowMs: t0 + 5_000))
        XCTAssertEqual(
            GroupCallAffordance.of(record: r, activeGroupCallId: nil, ownDeviceKey: nil, nowMs: t0 + 5_000),
            .joinable(callId: callA, joinedCount: 1, video: false))
        // And, with no heartbeat from the tablet, it too ages out.
        XCTAssertFalse(GroupCallReducer.isLive(r, nowMs: now))

        // The leaving device's plan does not end the call its other device is in.
        let plan = GroupCallHostRules.planLeave(
            hungUp: true, joinPublished: true, initiator: false, callId: callA,
            ownDeviceKey: "alice-phone", record: r, nowMs: t0 + 5_000)
        XCTAssertEqual(plan.publishEnd, false)
    }

    /// After a relaunch mid-call this device is not in the call, and the entry
    /// it left behind is neither liveness nor a person in the count.
    func testOwnStaleEntryAfterRelaunchIsNotCountedOrLive() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 3), t0 + 120_000),
        ]))
        let now = t0 + 150_000
        XCTAssertEqual(
            GroupCallAffordance.of(record: r, activeGroupCallId: nil, ownDeviceKey: member(3), nowMs: now),
            .none, "only this device's own entry: no call to join")
        XCTAssertEqual(
            GroupCallAffordance.of(record: r, activeGroupCallId: nil, ownDeviceKey: member(4), nowMs: now),
            .joinable(callId: callA, joinedCount: 1, video: false))
        // With another fresh participant it is joinable, counted without this device.
        let withOther = try XCTUnwrap(record([(.join(callId: callA, leafIndex: 5), now)], from: r))
        XCTAssertEqual(
            GroupCallAffordance.of(record: withOther, activeGroupCallId: nil, ownDeviceKey: member(3), nowMs: now),
            .joinable(callId: callA, joinedCount: 1, video: false))
        // While in the call, the own entry counts as before.
        XCTAssertEqual(
            GroupCallAffordance.of(record: r, activeGroupCallId: callA, ownDeviceKey: member(3), nowMs: now),
            .inCall)
    }

    func testTheHeartbeatConstantsAreTheContractValues() {
        XCTAssertEqual(GroupCallReducer.heartbeatMs, 60_000)
        XCTAssertEqual(GroupCallReducer.memberFreshMs, 150_000)
    }

    func testTheLastLeaveEndsLivenessAfterTheGraceWindow() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
            (.leave(callId: callA, leafIndex: 1), t0 + 120_000),
        ]))
        XCTAssertFalse(GroupCallReducer.isLive(r, nowMs: t0 + 120_000))
    }

    func testTheWindowsAreTheContractValues() {
        XCTAssertEqual(GroupCallReducer.maxLiveAgeMs, 4 * 60 * 60 * 1000)
        XCTAssertEqual(GroupCallReducer.unjoinedGraceMs, 60 * 1000)
    }

    // MARK: - Persistence

    private func isolatedDefaults() -> UserDefaults {
        let name = "GroupCallStoreTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }

    func testTheStoreRoundTripsRecordsPerAccount() throws {
        let store = GroupCallStore(defaults: isolatedDefaults())
        let r = try XCTUnwrap(record([
            (start(callA, at: t0, leaf: 4, video: true), t0),
            (.join(callId: callA, leafIndex: 4), t0 + 1_000),
        ]))
        store.save(accountIdB64: "acct", records: ["g1": r])
        XCTAssertEqual(store.load(accountIdB64: "acct", nowMs: t0 + 2_000), ["g1": r])
        XCTAssertEqual(store.load(accountIdB64: "other", nowMs: t0 + 2_000), [:])
        store.clear(accountIdB64: "acct")
        XCTAssertEqual(store.load(accountIdB64: "acct", nowMs: t0 + 2_000), [:])
    }

    func testTheStoreDropsRecordsPastTheMaxAgeOnLoad() throws {
        let store = GroupCallStore(defaults: isolatedDefaults())
        let old = try XCTUnwrap(record([(start(callA, at: t0), t0)]))
        let recent = try XCTUnwrap(record([(start(callB, at: t0 + 3_600_000), t0 + 3_600_000)]))
        store.save(accountIdB64: "acct", records: ["old": old, "recent": recent])
        let loaded = store.load(accountIdB64: "acct", nowMs: t0 + GroupCallReducer.maxLiveAgeMs + 1)
        XCTAssertEqual(loaded, ["recent": recent])
    }

    /// After a relaunch a reloaded record shows the Join banner only while its
    /// participants are still heartbeating, never for a call that died.
    func testAReloadedRecordIsJoinableOnlyWhileItsMembersAreFresh() throws {
        let store = GroupCallStore(defaults: isolatedDefaults())
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 120_000),
        ]))
        store.save(accountIdB64: "acct", records: ["g": r])
        let reloaded = store.load(accountIdB64: "acct", nowMs: t0 + 180_000)["g"]
        XCTAssertEqual(
            GroupCallAffordance.of(record: reloaded, activeGroupCallId: nil, ownDeviceKey: nil, nowMs: t0 + 180_000),
            .joinable(callId: callA, joinedCount: 1, video: false))
        XCTAssertEqual(
            GroupCallAffordance.of(record: reloaded, activeGroupCallId: nil, ownDeviceKey: nil,
                nowMs: t0 + 120_000 + GroupCallReducer.memberFreshMs + 1),
            .none)
    }

    // MARK: - Two call ids

    func testOfTwoLiveCallsTheEarlierStartWinsInEitherArrivalOrder() throws {
        let earlyA = start(callB, at: t0)  // callB starts first
        let lateB = start(callA, at: t0 + 10_000)

        let oneWay = try XCTUnwrap(record([(earlyA, t0), (lateB, t0 + 10_000)]))
        let otherWay = try XCTUnwrap(record([(lateB, t0 + 10_000), (earlyA, t0 + 10_500)]))
        XCTAssertEqual(oneWay.callId, callB)
        XCTAssertEqual(otherWay.callId, callB)
    }

    func testOfTwoLiveCallsWithTheSameStartTheLowerCallIdHexWins() throws {
        let r1 = try XCTUnwrap(record([(start(callB, at: t0), t0), (start(callA, at: t0), t0 + 1)]))
        let r2 = try XCTUnwrap(record([(start(callA, at: t0), t0), (start(callB, at: t0), t0 + 1)]))
        XCTAssertEqual(r1.callId, callA)
        XCTAssertEqual(r2.callId, callA)
    }

    func testAStaleRecordIsReplacedByANewStart() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            // callA never got a join; two minutes later it is not live.
            (start(callB, at: t0 + 120_000), t0 + 120_000),
        ]))
        XCTAssertEqual(r.callId, callB)
    }

    func testAStaleStartDoesNotDisplaceALiveCall() throws {
        let r = try XCTUnwrap(record([
            (start(callB, at: t0), t0),
            (.join(callId: callB, leafIndex: 1), t0 + 1_000),
            // An old announce (claimed an hour back, so never live) for a lower
            // call id must not steal the record from the live call.
            (start(callA, at: t0 - 3_600_000), t0 + 2_000),
        ]))
        XCTAssertEqual(r.callId, callB)
        XCTAssertEqual(Set(r.joined.keys), members(1))
    }

    // MARK: - C-11 routing

    func testGroupControlBodiesNeverReachTheOneToOneSession() {
        let media = FfiCallMediaFlags(audio: true, video: false)
        let bodies: [FfiCallSignalingBody] = [
            .groupStart(callId: callA, initiatorLeaf: 1, createdAtMs: 1, media: media),
            .groupJoin(callId: callA, leafIndex: 1, joinedAtMs: 1),
            .groupLeave(callId: callA, leafIndex: 1, reason: .hungUp),
        ]
        for body in bodies {
            let route = CallSignalingRoute.of(body)
            XCTAssertFalse(route.forwardToOneToOne, "\(body) must not reach the 1x1 session")
            XCTAssertNotNil(route.groupSignal, "\(body) must update the group-call record")
        }
    }

    func testOneToOneBodiesAreForwardedAndCarryNoGroupSignal() {
        let media = FfiCallMediaFlags(audio: true, video: false)
        let bodies: [FfiCallSignalingBody] = [
            .offer(callId: callA, sdp: "v=0", createdAtMs: 1, media: media),
            .answer(callId: callA, sdp: "v=0"),
            .iceCandidate(callId: callA, candidate: "c", sdpMid: "0", sdpMLineIndex: 0),
        ]
        for body in bodies {
            let route = CallSignalingRoute.of(body)
            XCTAssertTrue(route.forwardToOneToOne)
            XCTAssertNil(route.groupSignal)
        }
    }

    func testCallEndIsBothTheOneToOneAndTheGroupTerminal() {
        let route = CallSignalingRoute.of(.end(callId: callA, reason: .hungUp))
        XCTAssertTrue(route.forwardToOneToOne)
        XCTAssertEqual(route.groupSignal, .end(callId: callA))
    }

    func testTheRepositoryFoldsGroupBodiesAndForwardsOnlyOneToOneBodies() throws {
        let repo = ChatRepository()
        let group = "ab"
        var forwarded: [FfiCallSignalingBody] = []
        repo.callSignalingSink = { row in forwarded.append(row.body) }

        let startBytes = try composeCallSignaling(
            body: .groupStart(
                callId: callA, initiatorLeaf: 2, createdAtMs: UInt64(t0),
                media: FfiCallMediaFlags(audio: true, video: true)))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 1, payload: startBytes, createdAt: "2027-01-15T08:00:00Z", senderLeaf: 2, senderDeviceKey: member(2), senderKey: "person-2")
        let joinBytes = try composeCallSignaling(
            body: .groupJoin(callId: callA, leafIndex: 2, joinedAtMs: UInt64(t0)))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 2, payload: joinBytes, createdAt: "2027-01-15T08:00:01Z", senderLeaf: 2, senderDeviceKey: member(2), senderKey: "person-2")

        XCTAssertTrue(forwarded.isEmpty, "group bodies reached the 1x1 sink: \(forwarded)")
        let r = try XCTUnwrap(repo.groupCalls[group])
        XCTAssertEqual(r.callId, callA)
        XCTAssertEqual(Set(r.joined.keys), members(2))

        let offerBytes = try composeCallSignaling(
            body: .offer(
                callId: callB, sdp: "v=0", createdAtMs: 1,
                media: FfiCallMediaFlags(audio: true, video: false)))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 3, payload: offerBytes, createdAt: "2027-01-15T08:00:02Z", senderLeaf: 3, senderDeviceKey: member(3), senderKey: "person-3")
        XCTAssertEqual(forwarded.count, 1)
        XCTAssertEqual(repo.groupCalls[group].map { Set($0.joined.keys) }, members(2))

        let endBytes = try composeCallSignaling(body: .end(callId: callA, reason: .hungUp))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 4, payload: endBytes, createdAt: "2027-01-15T08:00:03Z", senderLeaf: 3, senderDeviceKey: member(3), senderKey: "person-3")
        XCTAssertEqual(forwarded.count, 2)
        XCTAssertNil(repo.groupCalls[group])
    }

    func testAStartWithAnUnreadableRowTimeIsDropped() throws {
        let repo = ChatRepository()
        let startBytes = try composeCallSignaling(
            body: .groupStart(
                callId: callA, initiatorLeaf: 2, createdAtMs: UInt64(t0),
                media: FfiCallMediaFlags(audio: true, video: false)))
        repo.routeCallSignaling(
            groupIdHex: "ab", messageId: 5, payload: startBytes, createdAt: "not a time", senderLeaf: 2, senderDeviceKey: member(2), senderKey: "person-2")
        XCTAssertNil(repo.groupCalls["ab"])
    }

    // MARK: - A body speaks only for its MLS sender

    /// The claimed leaf of each group body is what the check compares.
    func testEachGroupBodyClaimsTheLeafItNames() {
        XCTAssertEqual(start(callA, at: t0, leaf: 4).claimedLeaf, 4)
        XCTAssertEqual(GroupCallSignal.join(callId: callA, leafIndex: 5).claimedLeaf, 5)
        XCTAssertEqual(GroupCallSignal.leave(callId: callA, leafIndex: 6).claimedLeaf, 6)
        XCTAssertNil(GroupCallSignal.end(callId: callA).claimedLeaf)
    }

    /// The pure rule: equal leaves pass, any other leaf or no sender refuses,
    /// `CallEnd` names no leaf and passes.
    func testTheLeafCheckTable() {
        XCTAssertTrue(start(callA, at: t0, leaf: 4).speaksFor(senderLeaf: 4))
        XCTAssertFalse(start(callA, at: t0, leaf: 4).speaksFor(senderLeaf: 5))
        XCTAssertFalse(start(callA, at: t0, leaf: 4).speaksFor(senderLeaf: nil))
        XCTAssertFalse(GroupCallSignal.join(callId: callA, leafIndex: 1).speaksFor(senderLeaf: 2))
        XCTAssertFalse(GroupCallSignal.leave(callId: callA, leafIndex: 1).speaksFor(senderLeaf: 2))
        XCTAssertTrue(GroupCallSignal.end(callId: callA).speaksFor(senderLeaf: nil))
    }

    /// Through the repository: a member cannot announce a call as someone
    /// else, join someone else into the roster, or leave them out of it. Each
    /// forged row is dropped; the honest rows around them still land.
    func testTheRepositoryDropsGroupBodiesWhoseLeafIsNotTheSender() throws {
        let repo = ChatRepository()
        let group = "cd"
        let forgedStart = try composeCallSignaling(
            body: .groupStart(
                callId: callA, initiatorLeaf: 2, createdAtMs: UInt64(t0),
                media: FfiCallMediaFlags(audio: true, video: false)))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 6, payload: forgedStart, createdAt: "2027-01-15T08:00:00Z", senderLeaf: 7, senderDeviceKey: member(7), senderKey: "person-7")
        XCTAssertNil(repo.groupCalls[group], "a start naming leaf 2, sent by leaf 7, must not create a call")
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 7, payload: forgedStart, createdAt: "2027-01-15T08:00:00Z", senderLeaf: nil, senderDeviceKey: nil, senderKey: nil)
        XCTAssertNil(repo.groupCalls[group], "a start with no member sender must not create a call")

        repo.routeCallSignaling(
            groupIdHex: group, messageId: 8, payload: forgedStart, createdAt: "2027-01-15T08:00:00Z", senderLeaf: 2, senderDeviceKey: member(2), senderKey: "person-2")
        XCTAssertEqual(repo.groupCalls[group]?.callId, callA)

        let forgedJoin = try composeCallSignaling(
            body: .groupJoin(callId: callA, leafIndex: 3, joinedAtMs: UInt64(t0)))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 9, payload: forgedJoin, createdAt: "2027-01-15T08:00:01Z", senderLeaf: 2, senderDeviceKey: member(2), senderKey: "person-2")
        XCTAssertEqual(repo.groupCalls[group]?.joined.isEmpty, true, "leaf 2 cannot join leaf 3 into the call")
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 10, payload: forgedJoin, createdAt: "2027-01-15T08:00:01Z", senderLeaf: 3, senderDeviceKey: member(3), senderKey: "person-3")
        XCTAssertEqual(repo.groupCalls[group].map { Set($0.joined.keys) }, members(3))

        let forgedLeave = try composeCallSignaling(
            body: .groupLeave(callId: callA, leafIndex: 3, reason: .hungUp))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 11, payload: forgedLeave, createdAt: "2027-01-15T08:00:02Z", senderLeaf: 2, senderDeviceKey: member(2), senderKey: "person-2")
        XCTAssertEqual(repo.groupCalls[group].map { Set($0.joined.keys) }, members(3), "leaf 2 cannot leave leaf 3 out of the call")
    }

    // MARK: - Banner decision

    func testALiveCallThisDeviceIsNotInIsJoinable() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0, video: true), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
            (.join(callId: callA, leafIndex: 2), t0 + 2_000),
        ]))
        XCTAssertEqual(
            GroupCallAffordance.of(record: r, activeGroupCallId: nil, ownDeviceKey: nil, nowMs: t0 + 10_000),
            .joinable(callId: callA, joinedCount: 2, video: true)
        )
        XCTAssertEqual(
            GroupCallAffordance.of(record: r, activeGroupCallId: callA, ownDeviceKey: nil, nowMs: t0 + 10_000),
            .inCall
        )
    }

    func testADeadRecordOffersNothing() throws {
        let r = try XCTUnwrap(record([(start(callA, at: t0), t0)]))
        XCTAssertEqual(
            GroupCallAffordance.of(record: r, activeGroupCallId: nil, ownDeviceKey: nil,
                nowMs: t0 + GroupCallReducer.unjoinedGraceMs + 1),
            .none
        )
        XCTAssertEqual(GroupCallAffordance.of(record: nil, activeGroupCallId: nil, ownDeviceKey: nil, nowMs: t0), .none)
    }

    // MARK: - Sniffers (timeline + notification)

    private func groupBodies() throws -> [(String, Data)] {
        [
            ("groupStart", try composeCallSignaling(
                body: .groupStart(
                    callId: callA, initiatorLeaf: 1, createdAtMs: UInt64(t0),
                    media: FfiCallMediaFlags(audio: true, video: false)))),
            ("groupJoin", try composeCallSignaling(
                body: .groupJoin(callId: callA, leafIndex: 1, joinedAtMs: UInt64(t0)))),
            ("groupLeave", try composeCallSignaling(
                body: .groupLeave(callId: callA, leafIndex: 1, reason: .hungUp))),
        ]
    }

    func testTheComposedGroupBodiesOpenWithTheExpectedTags() throws {
        let bodies = try groupBodies()
        XCTAssertEqual(bodies[0].1.first, 0x72)
        XCTAssertEqual(bodies[1].1.first, 0x7A)
        XCTAssertEqual(Array(bodies[2].1.prefix(2)), [0x82, 0x01])
    }

    func testGroupBodiesAreCallSignalingNeverAChatBubble() throws {
        for (name, bytes) in try groupBodies() {
            XCTAssertTrue(ChatClient.looksLikeCallSignaling(bytes), name)
            let event = ChatClient.toEvent(
                messageId: 1,
                createdAt: "2027-01-15T08:00:00Z",
                processed: .application(data: bytes, senderLeaf: 1, senderAccount: nil),
                resolveSender: { "member-\($0)" }
            )
            guard case .callSignaling = event else {
                return XCTFail("\(name) classified as \(event), not .callSignaling")
            }
        }
    }

    func testGroupBodiesAreNeverANotificationPreview() throws {
        for (name, bytes) in try groupBodies() {
            XCTAssertTrue(nseLooksLikeCallSignaling(bytes), name)
            XCTAssertTrue(
                nseIsServiceEnvelope(
                    bytes,
                    decodesAsPushRoutingSecret: { _ in false },
                    decodesAsCommitterBeacon: { _ in false },
                    committerBeaconEnvelopeLen: 27,
                    decodesAsProfile: { _ in false },
                    profileEnvelopeLens: 28...315,
                    decodesAsGroupName: { _ in false },
                    groupNameEnvelopeLens: 30...285,
                    decodesAsProfileRequest: { _ in false },
                    profileRequestEnvelopeLen: 26
                ),
                name
            )
        }
    }

    func testTheLeaveTagNeedsItsContinuationByte() {
        // 0x82 alone is not field 16; neither is 0x82 followed by another byte.
        for bytes in [Data([0x82]), Data([0x82, 0x02, 0x0A]), Data([0x80, 0x01])] {
            XCTAssertFalse(ChatClient.looksLikeCallSignaling(bytes), "\(Array(bytes))")
            XCTAssertFalse(nseLooksLikeCallSignaling(bytes), "\(Array(bytes))")
        }
    }

    func testLegacyPlainTextStartingWithRorZIsNotCallSignaling() {
        // 0x72 / 0x7A are ASCII `r` / `z`; the group tags must not swallow a
        // plain-text body that merely starts with one.
        for text in ["real message", "zebra crossing", "r", "z"] {
            let bytes = Data(text.utf8)
            XCTAssertFalse(ChatClient.looksLikeCallSignaling(bytes), text)
            XCTAssertFalse(nseLooksLikeCallSignaling(bytes), text)
        }
    }

    func testAGroupBodyWithWrongFramingIsNotCallSignaling() throws {
        for (name, bytes) in try groupBodies() {
            let truncated = bytes.dropLast()
            var extended = bytes
            extended.append(0x00)
            for candidate in [Data(truncated), extended] {
                XCTAssertFalse(ChatClient.looksLikeCallSignaling(candidate), name)
                XCTAssertFalse(nseLooksLikeCallSignaling(candidate), name)
            }
        }
    }

    func testAChatEnvelopeIsStillNotCallSignaling() throws {
        let chat = try composeEnvelope(text: "hello", attachments: [])
        XCTAssertFalse(ChatClient.looksLikeCallSignaling(chat))
        XCTAssertFalse(nseLooksLikeCallSignaling(chat))
    }
}

/// The record-permission gate every call entry point runs first.
final class MicrophonePermissionTests: XCTestCase {

    func testGrantedProceedsWithoutPrompting() async {
        var prompted = false
        let ok = await MicrophonePermission.ensureGranted(
            status: { .granted },
            request: { prompted = true; return false }
        )
        XCTAssertTrue(ok)
        XCTAssertFalse(prompted)
    }

    func testDeniedRefusesWithoutPrompting() async {
        var prompted = false
        let ok = await MicrophonePermission.ensureGranted(
            status: { .denied },
            request: { prompted = true; return true }
        )
        XCTAssertFalse(ok)
        XCTAssertFalse(prompted, "the system never re-prompts after a denial")
    }

    func testUndeterminedFollowsThePrompt() async {
        let granted = await MicrophonePermission.ensureGranted(
            status: { .undetermined }, request: { true })
        let refused = await MicrophonePermission.ensureGranted(
            status: { .undetermined }, request: { false })
        XCTAssertTrue(granted)
        XCTAssertFalse(refused)
    }
}

/// The "Create call link" form's offered choices and label rule.
final class CallLinkFormTests: XCTestCase {

    func testExpiryChoicesAreOneHourOneDayAndSevenDays() {
        XCTAssertEqual(CallLinkForm.expiryChoices, [.oneHour, .oneDay, .sevenDays])
        let now: UInt64 = 1_000_000
        XCTAssertEqual(CallInviteExpiryPreset.oneHour.toMs(nowMs: now), now + 3_600_000)
        XCTAssertEqual(CallInviteExpiryPreset.oneDay.toMs(nowMs: now), now + 86_400_000)
        XCTAssertEqual(CallInviteExpiryPreset.sevenDays.toMs(nowMs: now), now + 7 * 86_400_000)
        XCTAssertFalse(CallLinkForm.expiryChoices.contains(.never))
    }

    func testMaxUsesChoicesAreOneTenAndUnlimited() {
        XCTAssertEqual(CallLinkForm.maxUsesChoices.map { $0.toValue() }, [0, 10, UInt32.max])
    }

    func testTheLabelBudgetIsCountedInBytes() {
        XCTAssertNil(CallLinkForm.labelError(""))
        XCTAssertNil(CallLinkForm.labelError(String(repeating: "a", count: CallInviteUrl.labelMaxLen)))
        XCTAssertNotNil(CallLinkForm.labelError(String(repeating: "a", count: CallInviteUrl.labelMaxLen + 1)))
        // 33 two-byte characters = 66 bytes: over the budget at 33 characters.
        XCTAssertNotNil(CallLinkForm.labelError(String(repeating: "\u{00E9}", count: 33)))
    }
}

/// The age a log line reports for a record, and the structural drops that
/// decide whether a record exists at all.
///
/// These pin the two facts that make "no banner" ambiguous on a live run: a
/// Join for an untracked call id is dropped outright (so a device that missed
/// the Start can never be recovered by heartbeats), and an empty `joined` set
/// is never live however recent the Start was.
final class GroupCallObservabilityTests: XCTestCase {

    private let own = "own-device"
    private let peer = "peer-device"

    private func start(_ callId: Data, atMs: Int64) -> OngoingGroupCall? {
        GroupCallReducer.apply(
            .start(callId: callId, initiatorLeaf: 1, createdAtMs: UInt64(atMs), audio: true, video: true),
            participant: nil, to: nil, rowCreatedAtMs: atMs)
    }

    /// A heartbeat for a call this device never heard announced now builds the
    /// record, because the announce is never repeated and the alternative is a
    /// call that is invisible for its whole life. What it must not do is claim
    /// the things only the announce carries.
    func testAJoinForAnUntrackedCallAdoptsItWithoutInventingTheAnnounce() {
        let joined = GroupCallReducer.apply(
            .join(callId: Data([1, 2, 3, 4]), leafIndex: 2),
            participant: GroupCallParticipant(deviceKey: peer, personKey: peer),
            to: nil, rowCreatedAtMs: 1_000)
        XCTAssertEqual(joined?.callId, Data([1, 2, 3, 4]))
        XCTAssertNil(joined?.initiatorLeaf)
        XCTAssertFalse(joined?.mediaKnown ?? true)
    }

    /// A Start alone announces a call nobody has joined yet, and it counts as
    /// live for a grace window — the initiator's own Join lands a moment after
    /// its Start, and without the grace the announce would flicker. After the
    /// grace, liveness depends entirely on Join rows having folded.
    ///
    /// This is the window that decides what a joining device must have
    /// received: inside it, the Start is enough; outside it, a device that
    /// applied the Start but no Join shows nothing.
    func testAStartWithNoJoinIsLiveOnlyForTheGrace() {
        let record = start(Data([1, 2, 3, 4]), atMs: 1_000)
        XCTAssertNotNil(record)
        XCTAssertTrue(GroupCallReducer.isLive(record!, nowMs: 1_000))
        XCTAssertTrue(
            GroupCallReducer.isLive(
                record!, nowMs: 1_000 + GroupCallReducer.unjoinedGraceMs))
        XCTAssertFalse(
            GroupCallReducer.isLive(
                record!, nowMs: 1_000 + GroupCallReducer.unjoinedGraceMs + 1))
    }

    func testAJoinWhoseDeviceDidNotResolveIsDropped() {
        let record = start(Data([1, 2, 3, 4]), atMs: 1_000)
        let after = GroupCallReducer.apply(
            .join(callId: Data([1, 2, 3, 4]), leafIndex: 2),
            participant: nil, to: record, rowCreatedAtMs: 2_000)
        XCTAssertEqual(after?.joined.count, 0)
        // Past the unjoined grace there is nothing left to carry it.
        XCTAssertFalse(
            GroupCallReducer.isLive(
                after!, nowMs: 1_000 + GroupCallReducer.unjoinedGraceMs + 1))
    }

    /// What the log prints as `ageMs`. A record judged stale because of clock
    /// skew and one judged stale because everybody left produce the same
    /// verdict, and only this number separates them.
    func testTheReportedAgeIsNowMinusTheNewestJoin() {
        let record = start(Data([1, 2, 3, 4]), atMs: 1_000)
        let after = GroupCallReducer.apply(
            .join(callId: Data([1, 2, 3, 4]), leafIndex: 2),
            participant: GroupCallParticipant(deviceKey: peer, personKey: peer),
            to: record, rowCreatedAtMs: 10_000)
        XCTAssertEqual(after?.newestLastSeenMs(), 10_000)
        XCTAssertTrue(GroupCallReducer.isLive(after!, nowMs: 10_000 + 149_000))
        XCTAssertFalse(GroupCallReducer.isLive(after!, nowMs: 10_000 + 151_000))
    }

    /// This device's own leftover entry never keeps a call alive for itself.
    func testOwnDeviceIsIgnoredInTheReportedAge() {
        let record = start(Data([1, 2, 3, 4]), atMs: 1_000)
        let after = GroupCallReducer.apply(
            .join(callId: Data([1, 2, 3, 4]), leafIndex: 2),
            participant: GroupCallParticipant(deviceKey: own, personKey: own),
            to: record, rowCreatedAtMs: 10_000)
        XCTAssertEqual(after?.newestLastSeenMs(ignoringDevice: own), nil)
        XCTAssertFalse(
            GroupCallReducer.isLive(
                after!, nowMs: 1_000 + GroupCallReducer.unjoinedGraceMs + 1,
                ignoringDevice: own))
    }

    func testTheShortCallIdMatchesWhatTheServerLogs() {
        XCTAssertEqual(
            GroupCallSignalLog.shortId(Data([0xec, 0x9b, 0xbd, 0x0a, 0xff])), "ec9bbd0a")
    }
}

/// One chat holds one call record, and the choice between a dead call and a
/// live one is made here.
///
/// This is the defect a live run caught: a chat held the record of a call from
/// half an hour earlier, every heartbeat from the call actually running was
/// folded against it, the ids did not match, and the fold returned the old
/// record unchanged. No change meant no publish, no publish meant no recomputed
/// verdict, and the only judgement the screen ever made was that the old call
/// was not live — correct, and about the wrong call.
final class GroupCallRecordSelectionTests: XCTestCase {

    private let alive = Data([0xec, 0x9b, 0xbd, 0x0a])
    private let dead = Data([0xe9, 0xa1, 0xe7, 0x0d])
    private let peer = "peer-device"
    private let other = "other-device"

    private func started(_ callId: Data, atMs: Int64) -> OngoingGroupCall {
        GroupCallReducer.apply(
            .start(callId: callId, initiatorLeaf: 1, createdAtMs: UInt64(atMs), audio: true, video: true),
            participant: nil, to: nil, rowCreatedAtMs: atMs)!
    }

    private func join(
        _ callId: Data, device: String, to current: OngoingGroupCall?, atMs: Int64
    ) -> OngoingGroupCall? {
        GroupCallReducer.apply(
            .join(callId: callId, leafIndex: 2),
            participant: GroupCallParticipant(deviceKey: device, personKey: device),
            to: current, rowCreatedAtMs: atMs)
    }

    /// The headline: a chat holding a stale record, handed a heartbeat from a
    /// different call, ends up holding the live one.
    func testAStaleRecordGivesWayToTheCallThatIsActuallyRunning() {
        let stale = started(dead, atMs: 0)
        let nowMs = GroupCallReducer.memberFreshMs * 10
        XCTAssertFalse(GroupCallReducer.isLive(stale, nowMs: nowMs))

        let after = join(alive, device: peer, to: stale, atMs: nowMs)

        XCTAssertEqual(after?.callId, alive, "the live call must win the slot")
        XCTAssertTrue(GroupCallReducer.isLive(after!, nowMs: nowMs))
        XCTAssertEqual(after?.joined.count, 1)
    }

    /// The fold must actually CHANGE the record, because an unchanged record
    /// publishes nothing and nothing downstream re-reads the verdict. This is
    /// the second symptom of the same cause.
    func testTheFoldChangesTheRecordSoTheVerdictIsRecomputed() {
        let stale = started(dead, atMs: 0)
        let nowMs = GroupCallReducer.memberFreshMs * 10
        XCTAssertNotEqual(join(alive, device: peer, to: stale, atMs: nowMs), stale)
    }

    /// The protective half of the old rule survives: a call in progress is not
    /// displaced by a foreign call id, so a stale or fabricated id cannot take
    /// the slot from a call people are in.
    func testALiveRecordIsNotDisplacedByAForeignCallId() {
        let live = join(alive, device: peer, to: started(alive, atMs: 0), atMs: 1_000)
        XCTAssertTrue(GroupCallReducer.isLive(live!, nowMs: 1_000))

        let after = join(dead, device: other, to: live, atMs: 1_000)

        XCTAssertEqual(after?.callId, alive)
        XCTAssertEqual(after?.joined.count, 1)
    }

    /// A record adopted from a heartbeat says what it does not know: the
    /// announce is published once and never repeated, so a device that missed
    /// it cannot recover who started the call or what media it carries.
    func testAnAdoptedRecordDoesNotClaimWhatItNeverSaw() {
        let adopted = join(alive, device: peer, to: nil, atMs: 5_000)
        XCTAssertNotNil(adopted)
        XCTAssertNil(adopted?.initiatorLeaf)
        XCTAssertFalse(adopted!.mediaKnown)
        // The start is the earliest moment the call is PROVEN to have been
        // running, never a guess at when it began.
        XCTAssertEqual(adopted?.startedAtMs, 5_000)
    }

    /// An announced record keeps saying so.
    func testAnAnnouncedRecordKeepsItsInitiatorAndMedia() {
        let announced = started(alive, atMs: 1_000)
        XCTAssertEqual(announced.initiatorLeaf, 1)
        XCTAssertTrue(announced.mediaKnown)
        XCTAssertTrue(announced.video)
    }

    /// An adopted record cannot borrow the unjoined grace to look live on a
    /// call that started long ago — it is live because somebody is
    /// demonstrably in it, which is the stronger reason.
    func testAnAdoptedRecordIsLiveOnItsHeartbeatNotOnTheGrace() {
        let atMs = GroupCallReducer.memberFreshMs * 10
        let adopted = join(alive, device: peer, to: nil, atMs: atMs)!
        XCTAssertTrue(GroupCallReducer.isLive(adopted, nowMs: atMs))
        XCTAssertFalse(
            GroupCallReducer.isLive(adopted, nowMs: atMs + GroupCallReducer.memberFreshMs + 1))
    }

    /// A leave for some other call prunes a record that is already dead, and
    /// leaves a live one alone.
    func testALeaveForAnotherCallPrunesOnlyADeadRecord() {
        let stale = started(dead, atMs: 0)
        let nowMs = GroupCallReducer.memberFreshMs * 10
        let pruned = GroupCallReducer.apply(
            .leave(callId: alive, leafIndex: 2),
            participant: GroupCallParticipant(deviceKey: peer, personKey: peer),
            to: stale, rowCreatedAtMs: nowMs)
        XCTAssertNil(pruned)

        let live = join(alive, device: peer, to: started(alive, atMs: 0), atMs: 1_000)
        let kept = GroupCallReducer.apply(
            .leave(callId: dead, leafIndex: 2),
            participant: GroupCallParticipant(deviceKey: other, personKey: other),
            to: live, rowCreatedAtMs: 1_000)
        XCTAssertEqual(kept?.callId, alive)
    }

    /// An unresolved sender is still dropped (C-68): a leaf index is not an
    /// identity, and adoption must not be a way around that.
    func testAnUnresolvedSenderStillCannotCreateARecord() {
        let adopted = GroupCallReducer.apply(
            .join(callId: alive, leafIndex: 2),
            participant: nil, to: nil, rowCreatedAtMs: 5_000)
        XCTAssertNil(adopted)
    }
}
