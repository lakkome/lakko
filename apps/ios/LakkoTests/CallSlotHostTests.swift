// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// MARK: - The one call slot and its copy

final class CallSlotTests: XCTestCase {

    func testTheOccupantIsTheOneCallRunning() {
        XCTAssertNil(CallSlot.occupant(oneToOneInCall: false, groupCallActive: false, linkCallActive: false))
        XCTAssertEqual(CallSlot.occupant(oneToOneInCall: true, groupCallActive: false, linkCallActive: false), .oneToOne)
        XCTAssertEqual(CallSlot.occupant(oneToOneInCall: false, groupCallActive: true, linkCallActive: false), .group)
        XCTAssertEqual(CallSlot.occupant(oneToOneInCall: false, groupCallActive: false, linkCallActive: true), .link)
        // Never two in a correct app; the order only makes a bug deterministic.
        XCTAssertEqual(CallSlot.occupant(oneToOneInCall: true, groupCallActive: true, linkCallActive: true), .oneToOne)
        XCTAssertEqual(CallSlot.occupant(oneToOneInCall: false, groupCallActive: true, linkCallActive: true), .group)
    }

    /// Same strings as `apps/web/src/calls/call-slot.ts`.
    func testTheBusyMessageSaysWhatIsRunningAndWhatToDo() {
        XCTAssertEqual(
            CallSlot.busyMessage(.oneToOne, verb: .start),
            "You are already in a call. End it before you start another one.")
        XCTAssertEqual(
            CallSlot.busyMessage(.group, verb: .join),
            "You are already in a group call. End it before you join another one.")
        XCTAssertEqual(
            CallSlot.busyMessage(.link, verb: .start),
            "You are already in a call from a link. End it before you start another one.")
    }
}

// MARK: - Group call decisions

final class GroupCallHostRulesTests: XCTestCase {

    private let callA = Data(repeating: 0x0A, count: 16)
    private let callB = Data(repeating: 0x0B, count: 16)

    /// A record whose `joined` members were all last seen at 0, so every one is
    /// fresh at `now` (0) unless a test says otherwise.
    private func record(_ callId: Data, joined: Set<UInt32>, lastSeenMs: Int64 = 0) -> OngoingGroupCall {
        OngoingGroupCall(
            callId: callId, audio: true, video: false, initiatorLeaf: 1,
            startedAtMs: 0,
            joined: Dictionary(uniqueKeysWithValues: joined.map {
                ("member-\($0)", JoinedDevice(lastSeenMs: lastSeenMs, personKey: "person-\($0)"))
            }),
            lastActivityMs: 0)
    }

    func testLeavingAnnouncesALeaveOnlyAfterAJoin() {
        let plan = GroupCallHostRules.planLeave(
            hungUp: true, joinPublished: false, initiator: false,
            callId: callA, ownDeviceKey: "member-3", record: record(callA, joined: [1]), nowMs: 0)
        XCTAssertEqual(plan, .init(reason: .hungUp, publishLeave: false, publishEnd: false))
    }

    func testTheLastOneOutEndsTheCall() {
        let plan = GroupCallHostRules.planLeave(
            hungUp: true, joinPublished: true, initiator: false,
            callId: callA, ownDeviceKey: "member-3", record: record(callA, joined: [3]), nowMs: 0)
        XCTAssertEqual(plan, .init(reason: .hungUp, publishLeave: true, publishEnd: true))
    }

    func testSomeoneStillInKeepsTheCallAlive() {
        let plan = GroupCallHostRules.planLeave(
            hungUp: true, joinPublished: true, initiator: true,
            callId: callA, ownDeviceKey: "member-3", record: record(callA, joined: [3, 4]), nowMs: 0)
        XCTAssertEqual(plan, .init(reason: .hungUp, publishLeave: true, publishEnd: false))
    }

    /// A joiner whose connection failed before it joined must not end a call
    /// other members may be about to enter.
    func testAJoinerThatNeverJoinedNeverEndsTheCall() {
        let plan = GroupCallHostRules.planLeave(
            hungUp: false, joinPublished: false, initiator: false,
            callId: callA, ownDeviceKey: "member-3", record: nil, nowMs: 0)
        XCTAssertEqual(plan, .init(reason: .error, publishLeave: false, publishEnd: false))
    }

    func testAnInitiatorThatNeverConnectedStillEndsItsAnnouncedCall() {
        let plan = GroupCallHostRules.planLeave(
            hungUp: false, joinPublished: false, initiator: true,
            callId: callA, ownDeviceKey: "member-3", record: record(callA, joined: []), nowMs: 0)
        XCTAssertEqual(plan, .init(reason: .error, publishLeave: false, publishEnd: true))
    }

    /// The live defect's leave half: the only other member was killed mid-call
    /// and never left. Its stale entry must not stop the last real participant
    /// from ending the call.
    func testAStaleOtherMemberDoesNotKeepTheCallOpen() {
        let r = OngoingGroupCall(
            callId: callA, audio: true, video: false, initiatorLeaf: 1, startedAtMs: 0,
            joined: [
                "member-3": JoinedDevice(lastSeenMs: 200_000, personKey: "person-3"),
                "member-4": JoinedDevice(lastSeenMs: 0, personKey: "person-4"),
            ],
            lastActivityMs: 200_000)
        let stale = GroupCallHostRules.planLeave(
            hungUp: true, joinPublished: true, initiator: false,
            callId: callA, ownDeviceKey: "member-3", record: r, nowMs: 200_000)
        XCTAssertEqual(stale, .init(reason: .hungUp, publishLeave: true, publishEnd: true))
        let fresh = GroupCallHostRules.planLeave(
            hungUp: true, joinPublished: true, initiator: false,
            callId: callA, ownDeviceKey: "member-3", record: r,
            nowMs: GroupCallReducer.memberFreshMs)
        XCTAssertFalse(fresh.publishEnd, "member-4 is still fresh here")
    }

    /// An unresolvable own identity excludes nothing: the own fresh entry reads
    /// as someone else still in, which never ends a call wrongly.
    func testAnUnresolvedOwnMemberNeverEndsTheCall() {
        let plan = GroupCallHostRules.planLeave(
            hungUp: true, joinPublished: true, initiator: false,
            callId: callA, ownDeviceKey: nil, record: record(callA, joined: [3]), nowMs: 0)
        XCTAssertFalse(plan.publishEnd)
    }

    /// The record names a different call: this call lost the start tie-break,
    /// and this device knows nothing about who is in its own room.
    func testARecordForAnotherCallNeverEndsThisOne() {
        let plan = GroupCallHostRules.planLeave(
            hungUp: true, joinPublished: true, initiator: true,
            callId: callA, ownDeviceKey: "member-3", record: record(callB, joined: []), nowMs: 0)
        XCTAssertFalse(plan.publishEnd)
        XCTAssertTrue(plan.publishLeave)
    }

    func testAnEpochAdvanceOnTheCallsGroupRekeys() {
        XCTAssertEqual(
            GroupCallHostRules.decideEpochChange(
                callGroupIdHex: "g", settledGroupIdHex: "g", groupJoined: true,
                isActive: true, keyedEpoch: 5, currentEpoch: 6),
            .rekey)
        // No keyed epoch recorded yet: rekey to be safe (a no-op for an
        // unchanged seed).
        XCTAssertEqual(
            GroupCallHostRules.decideEpochChange(
                callGroupIdHex: "g", settledGroupIdHex: "g", groupJoined: true,
                isActive: nil, keyedEpoch: nil, currentEpoch: 6),
            .rekey)
    }

    func testNothingHappensOnAnotherGroupAnUnchangedOrUnreadableEpochOrWithoutACall() {
        XCTAssertEqual(
            GroupCallHostRules.decideEpochChange(
                callGroupIdHex: "g", settledGroupIdHex: "other", groupJoined: true,
                isActive: true, keyedEpoch: 5, currentEpoch: 9),
            .none)
        XCTAssertEqual(
            GroupCallHostRules.decideEpochChange(
                callGroupIdHex: "g", settledGroupIdHex: "g", groupJoined: true,
                isActive: true, keyedEpoch: 5, currentEpoch: 5),
            .none)
        XCTAssertEqual(
            GroupCallHostRules.decideEpochChange(
                callGroupIdHex: "g", settledGroupIdHex: "g", groupJoined: true,
                isActive: true, keyedEpoch: 5, currentEpoch: nil),
            .none, "an unreadable epoch keeps the current key rather than guessing")
        XCTAssertEqual(
            GroupCallHostRules.decideEpochChange(
                callGroupIdHex: nil, settledGroupIdHex: "g", groupJoined: true,
                isActive: true, keyedEpoch: 5, currentEpoch: 6),
            .none)
    }

    func testAGroupThisDeviceLeftEndsTheCallLocally() {
        XCTAssertEqual(
            GroupCallHostRules.decideEpochChange(
                callGroupIdHex: "g", settledGroupIdHex: "g", groupJoined: true,
                isActive: false, keyedEpoch: 5, currentEpoch: 6),
            .endLocally)
        XCTAssertEqual(
            GroupCallHostRules.decideEpochChange(
                callGroupIdHex: "g", settledGroupIdHex: "g", groupJoined: false,
                isActive: nil, keyedEpoch: 5, currentEpoch: nil),
            .endLocally)
    }

    func testTheChatCallButtons() {
        let joinable = GroupCallAffordance.joinable(callId: callA, joinedCount: 2, video: false)
        XCTAssertEqual(
            GroupCallHostRules.chatCallAction(
                membershipEnded: false, callInProgress: true, affordance: joinable, memberCount: 5),
            .unavailable, "a call running anywhere withdraws them, even a joinable one here")
        XCTAssertEqual(
            GroupCallHostRules.chatCallAction(
                membershipEnded: true, callInProgress: false, affordance: .none, memberCount: 5),
            .unavailable)
        XCTAssertEqual(
            GroupCallHostRules.chatCallAction(
                membershipEnded: false, callInProgress: false, affordance: joinable, memberCount: 5),
            .joinGroup(callId: callA))
        XCTAssertEqual(
            GroupCallHostRules.chatCallAction(
                membershipEnded: false, callInProgress: false, affordance: .none, memberCount: 3),
            .startGroup)
        XCTAssertEqual(
            GroupCallHostRules.chatCallAction(
                membershipEnded: false, callInProgress: false, affordance: .none, memberCount: 2),
            .startOneToOne)
        XCTAssertEqual(
            GroupCallHostRules.chatCallAction(
                membershipEnded: false, callInProgress: false, affordance: .none, memberCount: nil),
            .startOneToOne)
    }

    // MARK: Alone in the call (docs/ux/calls.md section 4.3)

    func testTheAloneEndArmsOnlyOnceARemoteWasSeenAndTheRosterIsEmpty() {
        XCTAssertEqual(GroupCallHostRules.aloneAction(remoteCount: 0, sawRemote: true, armed: false), .armEnd)
        // The person who started the call and is still waiting: nobody left.
        XCTAssertEqual(GroupCallHostRules.aloneAction(remoteCount: 0, sawRemote: false, armed: false), .none)
        XCTAssertEqual(GroupCallHostRules.aloneAction(remoteCount: 2, sawRemote: true, armed: false), .none)
    }

    func testTheRowIsWrittenOnlyForACallWhoseMediaFlowed() {
        XCTAssertNil(GroupCallHostRules.transcriptRow(initiator: true, connectedAtMs: nil, endedAtMs: 5_000))
        XCTAssertEqual(
            GroupCallHostRules.transcriptRow(initiator: true, connectedAtMs: 1_000, endedAtMs: 61_500),
            CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 60_500)))
        XCTAssertEqual(
            GroupCallHostRules.transcriptRow(initiator: false, connectedAtMs: 1_000, endedAtMs: 1_000),
            CallTranscriptRow(direction: .incoming, outcome: .answered(durationMs: 0)))
    }

    func testSomeoneComingBackDisarmsTheAloneEnd() {
        XCTAssertEqual(GroupCallHostRules.aloneAction(remoteCount: 1, sawRemote: true, armed: true), .disarm)
        XCTAssertEqual(GroupCallHostRules.aloneAction(remoteCount: 0, sawRemote: true, armed: true), .none)
        XCTAssertEqual(GroupCallHostRules.aloneEndMs, 5_000)
    }
}

// MARK: - Link call decisions

final class LinkCallHostRulesTests: XCTestCase {

    typealias Rules = LinkCallHostRules

    func testAJoinerWaitsThenGoesLive() {
        var p = Rules.reduce(.starting, .signalingOpen(owner: false))
        XCTAssertEqual(p, .waiting)
        p = Rules.reduce(p, .joined)
        XCTAssertEqual(p, .live)
        XCTAssertEqual(Rules.reduce(.live, .signalingOpen(owner: false)), .live)
    }

    func testTheOwnerIsLiveFromFounding() {
        let p = Rules.reduce(.starting, .joined)
        XCTAssertEqual(p, .live)
        XCTAssertEqual(Rules.reduce(p, .signalingOpen(owner: true)), .live)
    }

    /// `MultiJoinerCall` closes its session before it says why.
    func testTheSpecificReasonReplacesAPlainEnd() {
        XCTAssertEqual(Rules.reduce(Rules.reduce(.waiting, .ended), .noCommitter), .dead)
        XCTAssertEqual(Rules.reduce(Rules.reduce(.waiting, .ended), .error), .error)
        XCTAssertEqual(Rules.reduce(.dead, .ended), .dead)
        XCTAssertEqual(Rules.reduce(.error, .joined), .error)
        XCTAssertEqual(Rules.reduce(.live, .noCommitter), .live, "a live call has a committer")
        XCTAssertEqual(Rules.reduce(.ended, .joined), .ended)
    }

    func testOnlyARunningCallHoldsTheSlot() {
        XCTAssertTrue(Rules.occupiesSlot(.starting))
        XCTAssertTrue(Rules.occupiesSlot(.waiting))
        XCTAssertTrue(Rules.occupiesSlot(.live))
        XCTAssertFalse(Rules.occupiesSlot(.ended))
        XCTAssertFalse(Rules.occupiesSlot(.dead))
        XCTAssertFalse(Rules.occupiesSlot(.error))
    }

    func testStartDecision() {
        XCTAssertEqual(
            Rules.decideStart(occupant: nil, activeLinkCallIdHex: nil, requestedCallIdHex: "aa", verb: .join),
            .start)
        XCTAssertEqual(
            Rules.decideStart(occupant: .link, activeLinkCallIdHex: "aa", requestedCallIdHex: "aa", verb: .join),
            .alreadyInThisCall)
        XCTAssertEqual(
            Rules.decideStart(occupant: .link, activeLinkCallIdHex: "aa", requestedCallIdHex: "bb", verb: .join),
            .busy(message: "You are already in a call from a link. End it before you join another one."))
        XCTAssertEqual(
            Rules.decideStart(occupant: .group, activeLinkCallIdHex: nil, requestedCallIdHex: "aa", verb: .start),
            .busy(message: "You are already in a group call. End it before you start another one."))
    }

    func testTheNoticeAwayFromTheLanding() {
        XCTAssertNil(Rules.notice(phase: .waiting, landingShowing: true, errorMessage: nil))
        XCTAssertNil(Rules.notice(phase: .live, landingShowing: false, errorMessage: nil))
        XCTAssertNil(Rules.notice(phase: nil, landingShowing: false, errorMessage: nil))
        XCTAssertEqual(
            Rules.notice(phase: .waiting, landingShowing: false, errorMessage: nil),
            .init(text: "Waiting for the owner to admit you…", actions: [.open, .cancel]))
        XCTAssertEqual(
            Rules.notice(phase: .starting, landingShowing: false, errorMessage: nil),
            .init(text: "Connecting to the call…", actions: [.open, .cancel]))
        XCTAssertEqual(
            Rules.notice(phase: .dead, landingShowing: false, errorMessage: nil),
            .init(text: "This call has ended or isn't active.", actions: [.dismiss]))
        XCTAssertEqual(
            Rules.notice(phase: .error, landingShowing: false, errorMessage: "boom"),
            .init(text: "The call failed: boom", actions: [.dismiss]))
    }
}

// MARK: - The MLS chain's drain signal

@MainActor
final class GroupMlsLockTests: XCTestCase {

    /// A region that parks until released.
    final class Gate {
        private var continuation: CheckedContinuation<Void, Never>?
        private var released = false
        func wait() async {
            if released { return }
            await withCheckedContinuation { continuation = $0 }
        }
        func release() {
            released = true
            continuation?.resume()
            continuation = nil
        }
    }

    func testADrainIsReportedOnceAfterQueuedRegionsNeverBetweenThem() async throws {
        let lock = GroupMlsLock()
        var reports: [(group: String, step: Int)] = []
        var step = 0
        lock.observeIdle { reports.append(($0, step)) }
        let gate = Gate()

        let first = Task { @MainActor in
            try await lock.run(groupIdHex: "g") {
                step = 1
                await gate.wait()
            }
        }
        try await waitUntil { step == 1 }
        let second = Task { @MainActor in
            try await lock.run(groupIdHex: "g") { step = 2 }
        }
        await Task.yield()
        XCTAssertTrue(lock.isBusy(groupIdHex: "g"))
        XCTAssertTrue(reports.isEmpty, "nothing is reported while a region runs")

        gate.release()
        _ = try await first.value
        _ = try await second.value
        try await waitUntil { !reports.isEmpty }
        XCTAssertEqual(reports.map(\.group), ["g"], "one drain, after the last queued region")
        XCTAssertEqual(reports.map(\.step), [2], "never between the first region and the one queued behind it")
        XCTAssertFalse(lock.isBusy(groupIdHex: "g"))
    }

    func testAThrowingRegionStillDrainsAndGroupsAreIndependent() async {
        let lock = GroupMlsLock()
        var reports: [String] = []
        lock.observeIdle { reports.append($0) }
        struct Boom: Error {}
        do {
            try await lock.run(groupIdHex: "a") { throw Boom() }
            XCTFail("the region's error reaches its caller")
        } catch {}
        _ = try? await lock.run(groupIdHex: "b") { 1 }
        XCTAssertEqual(reports, ["a", "b"])
    }

    /// An observer may queue a new region: it starts a fresh chain rather than
    /// nesting in the one that drained, and reports again when it drains.
    func testAnObserverQueueingARegionStartsAFreshChain() async throws {
        let lock = GroupMlsLock()
        var reports = 0
        var ranFollowUp = false
        lock.observeIdle { group in
            reports += 1
            guard !ranFollowUp else { return }
            ranFollowUp = true
            Task { @MainActor in _ = try? await lock.run(groupIdHex: group) { () } }
        }
        _ = try await lock.run(groupIdHex: "g") { () }
        try await waitUntil { reports == 2 }
        XCTAssertFalse(lock.isBusy(groupIdHex: "g"))
    }

    func testARemovedObserverIsNotCalled() async throws {
        let lock = GroupMlsLock()
        var calls = 0
        let token = lock.observeIdle { _ in calls += 1 }
        lock.removeIdleObserver(token)
        _ = try await lock.run(groupIdHex: "g") { () }
        XCTAssertEqual(calls, 0)
    }
}

// MARK: - Fakes shared by the host tests

@MainActor
final class FakeGroupCallHandle: GroupCallHandle {
    var connects = 0
    var hangUps = 0
    var rekeys: [Data] = []
    var micGates: [Bool] = []
    var sessionState: SfuCallSession.State = .idle
    var lastInboundAtMs: Int64 = 0
    var callEndedByServer = false
    var connectError: Error?

    func connect() async throws {
        connects += 1
        if let connectError {
            sessionState = .failed
            throw connectError
        }
        sessionState = .connected
    }

    func hangUp() {
        hangUps += 1
        if sessionState != .failed { sessionState = .closed }
    }

    func rekey(newSeed: Data) { rekeys.append(newSeed) }
    func setMicrophoneTrackEnabled(_ on: Bool) { micGates.append(on) }
    func setCameraEnabled(_ on: Bool) {}
    func publishVideo() async throws {}
}

@MainActor
final class FakeLinkCallHandle: LinkCallHandle {
    var hangUps = 0
    var micGates: [Bool] = []
    var sessionState: SfuCallSession.State = .connected

    func hangUp() {
        hangUps += 1
        if sessionState != .failed { sessionState = .closed }
    }

    func setMicrophoneTrackEnabled(_ on: Bool) { micGates.append(on) }
    func setCameraEnabled(_ on: Bool) {}
    func publishVideo() async throws {}
}

@MainActor
func waitUntil(
    timeout: TimeInterval = 5,
    file: StaticString = #filePath,
    line: UInt = #line,
    _ condition: @MainActor () -> Bool
) async throws {
    let deadline = Date().addingTimeInterval(timeout)
    while !condition() {
        if Date() > deadline {
            XCTFail("condition not met in time", file: file, line: line)
            return
        }
        try await Task.sleep(nanoseconds: 5_000_000)
    }
}

// MARK: - The group call host

@MainActor
final class GroupCallHostTests: XCTestCase {

    private let group = "aa11"
    private let otherGroup = "bb22"
    private let callA = Data(repeating: 0x0A, count: 16)
    private let t0: Int64 = 1_800_000_000_000

    final class Harness {
        var epoch: UInt64? = 5
        var hasGroup = true
        var isActive: Bool? = true
        var record: OngoingGroupCall?
        var published: [FfiCallSignalingBody] = []
        var syncs = 0
        var handles: [FakeGroupCallHandle] = []
        var connectError: Error?
        var nowMs: Int64 = 0
        /// What the SFU's state route answers a probe; `nil` = unreachable.
        var sfuState: SfuCallState?
        var probes = 0
        /// Heartbeat sleeps the test has allowed to finish.
        var heartbeatTicks = 0
        /// Heartbeat sleeps started so far.
        var heartbeatSleeps = 0
        /// Alone-end sleeps the test has allowed to finish / started so far.
        var aloneTicks = 0
        var aloneSleeps = 0
        /// Conversation rows the host filed.
        var rows: [FinishedCall] = []
        /// The seed a given epoch derives, so a rekey is checkable.
        static func seed(_ epoch: UInt64?) -> Data {
            Data(repeating: UInt8(truncatingIfNeeded: epoch ?? 0), count: 32)
        }
    }

    private func makeHost(_ h: Harness) -> GroupCallHost {
        GroupCallHost(ports: GroupCallHost.Ports(
            deriveCallKey: { _, _ in h.epoch.map { (Harness.seed($0), UInt32(3)) } },
            epoch: { _ in h.epoch },
            hasGroup: { _ in h.hasGroup },
            isActive: { _ in h.isActive },
            membershipEnded: { _ in false },
            record: { _ in h.record },
            applySignal: { _, signal, atMs in
                h.record = GroupCallReducer.apply(
                    signal, participant: GroupCallParticipant(deviceKey: "own", personKey: "me"),
                    to: h.record, rowCreatedAtMs: atMs)
            },
            ownDeviceKey: { _ in "own" },
            sleepMs: { ms in
                // A heartbeat or alone-end sleep finishes only when the test
                // grants a tick; any other sleep is instant.
                if ms == GroupCallReducer.heartbeatMs {
                    h.heartbeatSleeps += 1
                    let granted = h.heartbeatSleeps
                    while !Task.isCancelled {
                        if h.heartbeatTicks >= granted { return true }
                        try? await Task.sleep(nanoseconds: 2_000_000)
                    }
                    return false
                }
                if ms == GroupCallHostRules.aloneEndMs {
                    h.aloneSleeps += 1
                    let granted = h.aloneSleeps
                    while !Task.isCancelled {
                        if h.aloneTicks >= granted { return true }
                        try? await Task.sleep(nanoseconds: 2_000_000)
                    }
                    return false
                }
                return true
            },
            publish: { _, bytes in
                if let body = try? parseCallSignaling(plaintext: bytes) { h.published.append(body) }
                return true
            },
            syncGroup: { _ in h.syncs += 1 },
            sessionToken: { "token" },
            sfuBaseUrl: { "wss://sfu.test" },
            microphoneGranted: { true },
            cameraGranted: { _ in true },
            mintJwt: { _, _ in "jwt" },
            makeHandle: { _, _, _ in
                let handle = FakeGroupCallHandle()
                handle.connectError = h.connectError
                h.handles.append(handle)
                return handle
            },
            conversationTitle: { _ in "Test group" },
            safetyNumber: { _ in nil },
            mintCallId: { Data(repeating: 0x0A, count: 16) },
            recordCall: { call in h.rows.append(call) },
            nowMs: { h.nowMs },
            sfuCallState: { _, _, _ in
                h.probes += 1
                return h.sfuState
            }
        ))
    }

    // MARK: - Asking the SFU whether an offered call is still there (section 4.3)

    /// The SFU says the call ended: the record is closed for the chat with an
    /// End this device publishes, though it was never in the call.
    func testAProbeAnsweredEndedClosesTheRecordForTheChat() async {
        let h = Harness()
        let host = makeHost(h)
        h.sfuState = .ended
        await host.probeCallState(groupIdHex: "g", callIdHex: String(repeating: "ab", count: 16))
        XCTAssertEqual(h.probes, 1)
        XCTAssertEqual(kinds(h.published), ["end"])
    }

    /// Any other answer, or none, changes nothing.
    func testAProbeAnsweredLiveOrNotAtAllChangesNothing() async {
        let h = Harness()
        let host = makeHost(h)
        for state in [SfuCallState.live, .closing, .unknown] {
            h.sfuState = state
            await host.probeCallState(groupIdHex: "g", callIdHex: String(repeating: "ab", count: 16))
        }
        h.sfuState = nil
        await host.probeCallState(groupIdHex: "g", callIdHex: String(repeating: "ab", count: 16))
        XCTAssertEqual(h.probes, 4)
        XCTAssertTrue(h.published.isEmpty)
    }

    private func startLiveCall(_ host: GroupCallHost, _ h: Harness) async throws -> FakeGroupCallHandle {
        h.nowMs = t0
        host.start(groupIdHex: group, video: false, joinCallId: nil)
        XCTAssertTrue(host.occupiesSlot, "the slot is claimed before the first suspension")
        try await waitUntil { host.activeCall?.joinPublished == true }
        await host.whenControlPublishesSettle()
        return try XCTUnwrap(h.handles.first)
    }

    private func kinds(_ bodies: [FfiCallSignalingBody]) -> [String] {
        bodies.map {
            switch $0 {
            case .groupStart: return "start"
            case .groupJoin: return "join"
            case .groupLeave: return "leave"
            case .end: return "end"
            default: return "other"
            }
        }
    }

    func testStartingAnnouncesConnectsAndJoins() async throws {
        let h = Harness()
        let host = makeHost(h)
        let handle = try await startLiveCall(host, h)
        XCTAssertEqual(handle.connects, 1)
        XCTAssertEqual(kinds(h.published), ["start", "join"])
        XCTAssertNotNil(host.presentation)
        XCTAssertEqual(host.activeCallId(groupIdHex: group), callA)
        XCTAssertNil(host.activeCallId(groupIdHex: otherGroup))
        XCTAssertEqual(host.activeCall?.keyedEpoch, 5)
        XCTAssertNil(host.startingGroupIdHex)
    }

    /// The defect: the rekey only ran after the open chat's poll. Here nothing
    /// but the MLS chain drains — no chat, no view model — and the key follows
    /// the epoch, once, after the last queued region.
    func testTheKeyFollowsTheEpochWhenTheChainDrainsNotBetweenRegions() async throws {
        let h = Harness()
        let host = makeHost(h)
        let lock = GroupMlsLock()
        lock.observeIdle { host.groupMlsSettled($0) }
        let handle = try await startLiveCall(host, h)
        XCTAssertTrue(handle.rekeys.isEmpty)

        let gate = GroupMlsLockTests.Gate()
        let first = Task { @MainActor in
            try await lock.run(groupIdHex: self.group) {
                h.epoch = 6 // a commit applied (or eager-merged, unpublished)
                await gate.wait()
            }
        }
        try await waitUntil { h.epoch == 6 }
        let second = Task { @MainActor in
            try await lock.run(groupIdHex: self.group) { h.epoch = 7 }
        }
        await Task.yield()
        XCTAssertTrue(handle.rekeys.isEmpty, "never read an epoch while a region is in flight")

        gate.release()
        _ = try await first.value
        _ = try await second.value
        try await waitUntil { !handle.rekeys.isEmpty }
        XCTAssertEqual(handle.rekeys, [Harness.seed(7)], "one rekey, at the epoch the last region left")
        XCTAssertEqual(host.activeCall?.keyedEpoch, 7)

        // A later drain with nothing changed, or a drain of another group, is not a rekey.
        _ = try await lock.run(groupIdHex: group) { () }
        h.epoch = 8
        _ = try await lock.run(groupIdHex: otherGroup) { () }
        XCTAssertEqual(handle.rekeys, [Harness.seed(7)])
    }

    func testAnUnreadableEpochKeepsTheCurrentKey() async throws {
        let h = Harness()
        let host = makeHost(h)
        let handle = try await startLiveCall(host, h)
        h.epoch = nil
        host.groupMlsSettled(group)
        XCTAssertTrue(handle.rekeys.isEmpty)
        XCTAssertEqual(handle.hangUps, 0)
    }

    /// Removed from the group: nothing can be published to it and there is no
    /// key to rotate to, so the call ends locally and says nothing.
    func testLeavingTheGroupEndsTheCallLocally() async throws {
        let h = Harness()
        let host = makeHost(h)
        let handle = try await startLiveCall(host, h)
        let publishedBefore = h.published.count
        h.isActive = false
        h.epoch = 6
        host.groupMlsSettled(group)
        XCTAssertEqual(handle.hangUps, 1)
        XCTAssertNil(host.activeCall)
        XCTAssertFalse(host.occupiesSlot)
        XCTAssertTrue(handle.rekeys.isEmpty)
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(h.published.count, publishedBefore, "no Leave or End to a group this device left")
    }

    /// The in-call End button only moves the screen to `ended`; the host turns
    /// that into a hang-up and the Leave / End the group is owed, in order.
    func testEndingFromTheCallScreenHangsUpAndTellsTheGroup() async throws {
        let h = Harness()
        let host = makeHost(h)
        let handle = try await startLiveCall(host, h)
        let ui = try XCTUnwrap(host.presentation)
        ui.endCall()
        try await waitUntil { handle.hangUps == 1 }
        try await waitUntil { self.kinds(h.published).last == "end" }
        XCTAssertEqual(kinds(h.published), ["start", "join", "leave", "end"])
        if case .groupLeave(_, _, let reason) = h.published[2] {
            XCTAssertEqual(reason, .hungUp)
        }
        XCTAssertNil(host.activeCall)
        XCTAssertNotNil(host.presentation, "the screen stays for its \"Call ended\" overlay")
        host.dismissPresentation()
        XCTAssertNil(host.presentation)
    }

    /// The participant heartbeat: every interval the live call re-publishes its
    /// own Join and advances its own last-seen time; hanging up stops it.
    func testTheLiveCallHeartbeatsItsJoinUntilItEnds() async throws {
        let h = Harness()
        let host = makeHost(h)
        _ = try await startLiveCall(host, h)
        XCTAssertEqual(kinds(h.published), ["start", "join"])
        try await waitUntil { h.heartbeatSleeps == 1 }
        XCTAssertEqual(h.record?.joined["own"]?.lastSeenMs, t0)

        h.nowMs = t0 + GroupCallReducer.heartbeatMs
        h.heartbeatTicks = 1
        try await waitUntil { self.kinds(h.published) == ["start", "join", "join"] }
        await host.whenControlPublishesSettle()
        XCTAssertEqual(h.record?.joined["own"]?.lastSeenMs, t0 + GroupCallReducer.heartbeatMs)
        if case .groupJoin(let callId, let leaf, _) = h.published[2] {
            XCTAssertEqual(callId, callA)
            XCTAssertEqual(leaf, 3)
        } else {
            XCTFail("the heartbeat is the same GroupCallJoin body")
        }

        try await waitUntil { h.heartbeatSleeps == 2 }
        host.leave()
        h.heartbeatTicks = 10
        try await waitUntil { self.kinds(h.published).last == "end" }
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(kinds(h.published), ["start", "join", "join", "leave", "end"],
                       "no heartbeat after the call ended")
        XCTAssertEqual(h.heartbeatSleeps, 2)
    }

    /// The mute button used to flip only the screen's state.
    func testTheMicButtonGatesTheTrack() async throws {
        let h = Harness()
        let host = makeHost(h)
        let handle = try await startLiveCall(host, h)
        let ui = try XCTUnwrap(host.presentation)
        ui.setParticipants([
            .init(id: "local", displayName: "You", isLocal: true, micOn: true, camOn: false,
                  handRaised: false, quality: .good, speaking: false, screenSharing: false),
        ])
        ui.setMic(false)
        try await waitUntil { handle.micGates.last == false }
        ui.setMic(true)
        try await waitUntil { handle.micGates.last == true }
    }

    func testAStartWhileAnotherCallHoldsTheSlotIsRefusedWithAReason() {
        let h = Harness()
        let host = makeHost(h)
        host.slotOccupant = { .link }
        host.start(groupIdHex: group, video: false, joinCallId: nil)
        XCTAssertFalse(host.occupiesSlot)
        XCTAssertEqual(
            host.callError,
            "You are already in a call from a link. End it before you start another one.")
        XCTAssertTrue(h.handles.isEmpty)
    }

    func testASecondTapWhileStartingIsRefused() async throws {
        let h = Harness()
        let host = makeHost(h)
        host.start(groupIdHex: group, video: false, joinCallId: nil)
        host.start(groupIdHex: group, video: false, joinCallId: nil)
        XCTAssertEqual(host.callError, "You are already in a group call. End it before you start another one.")
        try await waitUntil { host.activeCall?.joinPublished == true }
        XCTAssertEqual(h.handles.count, 1)
    }

    /// Joining a call whose End is already on the journal says so and connects
    /// nothing.
    func testJoiningAnEndedCallConnectsNothing() async throws {
        let h = Harness()
        h.nowMs = t0
        let host = makeHost(h)
        host.start(groupIdHex: group, video: false, joinCallId: callA)
        try await waitUntil { host.callNotice != nil }
        XCTAssertEqual(host.callNotice, "That group call has ended.")
        XCTAssertEqual(h.syncs, 1, "a join brings the group current first")
        XCTAssertTrue(h.handles.isEmpty)
        XCTAssertFalse(host.occupiesSlot)
    }

    func testAFailedConnectIsExplainedAndFreesTheSlot() async throws {
        let h = Harness()
        h.nowMs = t0
        h.connectError = Down()
        let host = makeHost(h)
        host.start(groupIdHex: group, video: false, joinCallId: nil)
        try await waitUntil { host.callError != nil }
        XCTAssertEqual(host.callError, "Could not connect to the call: down")
        XCTAssertFalse(host.occupiesSlot)
        XCTAssertNil(host.presentation, "the explanation is not hidden behind the call screen")
        // The initiator announced the call, so it owes the End.
        try await waitUntil { self.kinds(h.published).last == "end" }
        if case .end(_, let reason)? = h.published.last {
            XCTAssertEqual(reason, .error)
        }
    }

    // MARK: Alone in the call (docs/ux/calls.md section 4.3)

    private func participant(_ id: String, local: Bool) -> CallUiState.Participant {
        .init(id: id, displayName: local ? "You" : "Anna", isLocal: local, micOn: true, camOn: false,
              handRaised: false, quality: .good, speaking: false, screenSharing: false)
    }

    /// Everyone else left: after the grace the device hangs up as if the End
    /// button were tapped, so the group hears the Leave and, since nobody else
    /// is in the record, the End.
    func testTheCallEndsByItselfWhenEveryoneElseHasLeft() async throws {
        let h = Harness()
        let host = makeHost(h)
        let handle = try await startLiveCall(host, h)
        let ui = try XCTUnwrap(host.presentation)
        ui.setParticipants([participant("local", local: true), participant("peer-1", local: false)])
        ui.setParticipants([participant("local", local: true)])
        try await waitUntil { h.aloneSleeps == 1 }
        XCTAssertEqual(handle.hangUps, 0, "the grace has not run out")
        h.aloneTicks = 1
        try await waitUntil { handle.hangUps == 1 }
        XCTAssertNil(host.activeCall)
        // The departure is announced by a task the leave spawns; wait for it
        // rather than for the publish chain, which may not exist yet.
        try await waitUntil { Array(self.kinds(h.published).suffix(2)) == ["leave", "end"] }
    }

    /// `docs/ux/calls.md` section 6: the call this device started leaves an
    /// outgoing row with the time its media flowed, once.
    func testAFinishedGroupCallLeavesOneRowInTheConversation() async throws {
        let h = Harness()
        let host = makeHost(h)
        _ = try await startLiveCall(host, h)
        let ui = try XCTUnwrap(host.presentation)
        h.nowMs = t0 + 1_000
        ui.setPhase(.connected)
        try await waitUntil { host.activeCall?.connectedAtMs == self.t0 + 1_000 }
        h.nowMs = t0 + 75_000
        host.leave()
        host.dismissPresentation()
        XCTAssertEqual(h.rows.count, 1)
        XCTAssertEqual(h.rows.first?.groupIdHex, group)
        XCTAssertEqual(h.rows.first?.row, CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 74_000)))
    }

    func testAJoinThatNeverConnectedLeavesNoRow() async throws {
        let h = Harness()
        h.connectError = Down()
        let host = makeHost(h)
        host.start(groupIdHex: group, video: false, joinCallId: nil)
        try await waitUntil { host.activeCall == nil && host.callError != nil }
        XCTAssertTrue(h.rows.isEmpty)
    }

    func testSomeoneComingBackInsideTheGraceKeepsTheCall() async throws {
        let h = Harness()
        let host = makeHost(h)
        let handle = try await startLiveCall(host, h)
        let ui = try XCTUnwrap(host.presentation)
        ui.setParticipants([participant("local", local: true), participant("peer-1", local: false)])
        ui.setParticipants([participant("local", local: true)])
        try await waitUntil { h.aloneSleeps == 1 }
        ui.setParticipants([participant("local", local: true), participant("peer-2", local: false)])
        h.aloneTicks = 1
        try await Task.sleep(nanoseconds: 60_000_000)
        XCTAssertEqual(handle.hangUps, 0)
        XCTAssertNotNil(host.activeCall)
        // Leaving again re-arms from scratch.
        ui.setParticipants([participant("local", local: true)])
        try await waitUntil { h.aloneSleeps == 2 }
        XCTAssertEqual(handle.hangUps, 0)
    }

    /// The person who started the call and is still waiting for the first
    /// joiner is not alone in the sense of the rule: nobody left.
    func testACallThatNeverHadAnyoneElseWaitsForTheUser() async throws {
        let h = Harness()
        let host = makeHost(h)
        let handle = try await startLiveCall(host, h)
        let ui = try XCTUnwrap(host.presentation)
        ui.setParticipants([participant("local", local: true)])
        try await Task.sleep(nanoseconds: 60_000_000)
        XCTAssertEqual(h.aloneSleeps, 0)
        XCTAssertEqual(handle.hangUps, 0)
        XCTAssertNotNil(host.activeCall)
    }

    private struct Down: Error, LocalizedError {
        var errorDescription: String? { "down" }
    }
}

// MARK: - The link call host

@MainActor
final class LinkCallHostTests: XCTestCase {

    final class Harness {
        var hasSession = true
        var micGranted = true
        var handles: [FakeLinkCallHandle] = []
        var callbacks: [MultiJoinerCall.Callbacks] = []
        var startError: Error?
        /// The owner reports `onJoined` from inside `host`, before it returns.
        var joinedDuringStart = false
        /// `docs/ux/call-links.md` — the account that founds a `New call`.
        var ownIdentity: (accountId: Data, signatureKey: Data)? = (
            Data(repeating: 7, count: 16), Data(repeating: 8, count: 32))
        /// The call ids somebody pressed `Invite` for.
        var minted: [Data] = []
        var mintUrl = "https://lakko.me/c/minted"
    }

    private func makeHost(_ h: Harness) -> LinkCallHost {
        LinkCallHost(ports: LinkCallHost.Ports(
            session: { h.hasSession ? (Data(repeating: 1, count: 64), "token") : nil },
            sfuBaseUrl: { "wss://sfu.test" },
            microphoneGranted: { h.micGranted },
            cameraGranted: { _ in true },
            startCall: { request, _, _, _, _, callbacks in
                h.callbacks.append(callbacks)
                if let error = h.startError { throw error }
                if request.isOwner || h.joinedDuringStart { callbacks.onJoined?() }
                let handle = FakeLinkCallHandle()
                h.handles.append(handle)
                return handle
            },
            ownIdentity: { h.ownIdentity },
            mintInviteLink: { callId in
                h.minted.append(callId)
                return h.mintUrl
            }
        ))
    }

    private func request(_ callByte: UInt8, owner: Bool) -> LinkCallHost.Request {
        LinkCallHost.Request(
            slug: "slug-\(callByte)",
            callId: Data(repeating: callByte, count: 16),
            ownerAccountId: Data(repeating: 2, count: 16),
            ownerSignatureKey: Data(repeating: 3, count: 32),
            mediaVideo: false,
            isOwner: owner
        )
    }

    // MARK: - New call (`docs/ux/call-links.md` section 2)

    /// One press, one kind of call, and the person lands in it: nothing is
    /// asked, and the call is a group video call founded here.
    func testNewCallFoundsAVideoCallAndLandsInIt() async {
        let h = Harness()
        let host = makeHost(h)
        let outcome = await host.startNewCall()
        XCTAssertEqual(outcome, .started)
        XCTAssertEqual(host.slot?.phase, .live)
        XCTAssertEqual(host.slot?.isOwner, true)
        XCTAssertNil(host.slot?.slug, "a call started here was not opened from a link")
        XCTAssertNotNil(host.presentation, "the person lands in the call")
        XCTAssertEqual(host.slot?.callIdHex.count, 32, "a 16-byte call id")
    }

    /// Starting a call must stay instant, so no link is made until somebody
    /// asks for one.
    func testNewCallMakesNoLinkUntilInviteIsPressed() async {
        let h = Harness()
        let host = makeHost(h)
        _ = await host.startNewCall()
        XCTAssertEqual(h.minted, [], "no round trip, no anti-spam solve")
        guard let invite = host.invite else { return XCTFail("the founder can offer a link") }
        XCTAssertEqual(invite.state, .idle)

        await invite.press()
        XCTAssertEqual(h.minted.count, 1)
        XCTAssertEqual(invite.url, h.mintUrl)
        XCTAssertEqual(
            h.minted.first?.map { String(format: "%02x", $0) }.joined(),
            host.slot?.callIdHex,
            "the link leads into THIS call")

        // A second press re-offers the link the first one made.
        await invite.press()
        XCTAssertEqual(h.minted.count, 1)
    }

    /// A capability names its signer as the call's owner and ADR-0032 elects
    /// the committer from that owner, so a joiner's link would send arrivals
    /// to a call nobody is founding. It has no offer at all.
    func testAJoinerIsNeverOfferedALinkToSomebodyElsesCall() async {
        let h = Harness()
        let host = makeHost(h)
        _ = await host.start(request(0x0A, owner: false))
        XCTAssertNil(host.invite)
    }

    func testTheOfferGoesWithTheCall() async {
        let h = Harness()
        let host = makeHost(h)
        _ = await host.startNewCall()
        XCTAssertNotNil(host.invite)
        host.leave()
        XCTAssertNil(host.invite, "a new call starts with no link of its own")
    }

    func testNewCallRefusesWithoutASession() async {
        let h = Harness()
        h.ownIdentity = nil
        let host = makeHost(h)
        let outcome = await host.startNewCall()
        XCTAssertEqual(outcome, .failed(message: CallInviteOfferRules.signedOutMessage))
        XCTAssertNil(host.slot)
    }

    func testTheOwnerIsLiveAndPresentedOnceItStarts() async {
        let h = Harness()
        let host = makeHost(h)
        let outcome = await host.start(request(0x0A, owner: true))
        XCTAssertEqual(outcome, .started)
        XCTAssertEqual(host.slot?.phase, .live)
        XCTAssertNotNil(host.presentation)
        XCTAssertTrue(host.occupiesSlot)
    }

    func testAJoinerWaitsAndADeadCallFreesTheSlotAndHangsUp() async {
        let h = Harness()
        let host = makeHost(h)
        _ = await host.start(request(0x0A, owner: false))
        XCTAssertEqual(host.slot?.phase, .waiting)
        XCTAssertNil(host.presentation, "nothing to present before admission")

        // `MultiJoinerCall` closes the session, then says why.
        h.callbacks[0].onEnded?()
        h.callbacks[0].onNoCommitter?()
        XCTAssertEqual(host.slot?.phase, .dead)
        XCTAssertFalse(host.occupiesSlot)
        XCTAssertEqual(h.handles[0].hangUps, 1)
        XCTAssertEqual(host.notice?.text, "This call has ended or isn't active.")
        host.dismissOutcome()
        XCTAssertNil(host.slot)
    }

    func testOpeningTheSameLinkAgainNeverStartsASecondSession() async {
        let h = Harness()
        let host = makeHost(h)
        _ = await host.start(request(0x0A, owner: false))
        let again = await host.start(request(0x0A, owner: false))
        XCTAssertEqual(again, .alreadyInThisCall)
        let other = await host.start(request(0x0B, owner: false))
        XCTAssertEqual(
            other,
            .busy(message: "You are already in a call from a link. End it before you join another one."))
        XCTAssertEqual(h.handles.count, 1)
    }

    func testAStartWhileAGroupCallRunsIsRefused() async {
        let h = Harness()
        let host = makeHost(h)
        host.slotOccupant = { .group }
        let outcome = await host.start(request(0x0A, owner: true))
        XCTAssertEqual(
            outcome,
            .busy(message: "You are already in a group call. End it before you start another one."))
        XCTAssertNil(host.slot)
        XCTAssertTrue(h.callbacks.isEmpty)
    }

    func testADeniedMicrophoneIsAnOutcomeNotACall() async {
        let h = Harness()
        h.micGranted = false
        let host = makeHost(h)
        let outcome = await host.start(request(0x0A, owner: false))
        XCTAssertEqual(outcome, .failed(message: MicrophonePermission.deniedMessage))
        XCTAssertEqual(host.slot?.phase, .error)
        XCTAssertFalse(host.occupiesSlot)
        XCTAssertTrue(h.callbacks.isEmpty)
    }

    /// Leaving is the user's choice: no outcome, and a late report from the
    /// call that was left never resurrects the slot.
    func testLeavingIgnoresLateReports() async {
        let h = Harness()
        let host = makeHost(h)
        _ = await host.start(request(0x0A, owner: false))
        host.leave()
        XCTAssertNil(host.slot)
        XCTAssertEqual(h.handles[0].hangUps, 1)
        h.callbacks[0].onJoined?()
        h.callbacks[0].onError?(LinkCallHostError.identityUnavailable)
        XCTAssertNil(host.slot)
        XCTAssertNil(host.presentation)
    }

    /// The End button in the call screen: the session is still up, so the user
    /// is leaving.
    func testEndingFromTheCallScreenLeaves() async throws {
        let h = Harness()
        let host = makeHost(h)
        _ = await host.start(request(0x0A, owner: true))
        let ui = try XCTUnwrap(host.presentation)
        ui.endCall()
        try await waitUntil { host.slot == nil }
        XCTAssertEqual(h.handles[0].hangUps, 1)
        XCTAssertNotNil(host.presentation, "the screen stays for its \"Call ended\" overlay")
    }

    func testLiveCallMicGatesTheTrack() async throws {
        let h = Harness()
        let host = makeHost(h)
        _ = await host.start(request(0x0A, owner: true))
        let ui = try XCTUnwrap(host.presentation)
        ui.setParticipants([
            .init(id: "local", displayName: "You", isLocal: true, micOn: true, camOn: false,
                  handRaised: false, quality: .good, speaking: false, screenSharing: false),
        ])
        ui.setMic(false)
        try await waitUntil { h.handles[0].micGates.last == false }
    }

    func testTheNoticeHidesWhileTheLandingShows() async {
        let h = Harness()
        let host = makeHost(h)
        _ = await host.start(request(0x0A, owner: false))
        let landing = UUID()
        host.noteLanding(landing, visible: true)
        host.noteLanding(landing, visible: true)
        XCTAssertNil(host.notice)
        host.noteLanding(landing, visible: false)
        XCTAssertEqual(host.notice?.actions, [.open, .cancel], "a repeated appear is still one landing")
    }
}

// MARK: - One slot across the three hosts

@MainActor
final class CallSlotWiringTests: XCTestCase {

    private let t0: Int64 = 1_800_000_000_000

    private final class Published {
        var ends: [FfiCallEndReason] = []
    }

    private final class Timer: CallHostTimer {
        func cancel() {}
    }

    private func dsTime(_ ms: Int64) -> String {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
    }

    private func makeOneToOne(_ published: Published) -> CallHost {
        let now = t0
        return CallHost(ports: CallHost.Ports(
            publish: { _, bytes in
                if case .end(_, let reason)? = try? parseCallSignaling(plaintext: bytes) {
                    published.ends.append(reason)
                }
                return true
            },
            deriveCallKey: { _, _ in (Data(repeating: 0x42, count: 32), 0) },
            nowMs: { now },
            schedule: { _, _ in Timer() },
            microphoneGranted: { true },
            cameraGranted: { _ in true },
            peerName: { _ in nil },
            mintCallId: { Data(repeating: 0x0C, count: 16) }
        ))
    }

    private func makeLink() -> LinkCallHost {
        LinkCallHost(ports: LinkCallHost.Ports(
            session: { (Data(repeating: 1, count: 64), "token") },
            sfuBaseUrl: { "wss://sfu.test" },
            microphoneGranted: { true },
            cameraGranted: { _ in true },
            startCall: { _, _, _, _, _, _ in FakeLinkCallHandle() }
        ))
    }

    private func offerRow(_ callByte: UInt8, id: Int64) -> InboundCallRow {
        InboundCallRow(
            groupIdHex: "cc33", messageId: id, createdAt: dsTime(t0),
            body: .offer(
                callId: Data(repeating: callByte, count: 16), sdp: "v=0", createdAtMs: 1,
                media: FfiCallMediaFlags(audio: true, video: false)))
    }

    func testALiveLinkCallAnswersA1x1OfferBusyAndARinging1x1RefusesALinkCall() async {
        let published = Published()
        let oneToOne = makeOneToOne(published)
        let group = GroupCallHost.live(repository: ChatRepository(), sessionTokenProvider: { nil })
        let link = makeLink()
        CallSlotWiring.wire(oneToOne: oneToOne, group: group, link: link)

        let request = LinkCallHost.Request(
            slug: "s", callId: Data(repeating: 0x0A, count: 16),
            ownerAccountId: Data(repeating: 2, count: 16),
            ownerSignatureKey: Data(repeating: 3, count: 32), mediaVideo: false, isOwner: false)
        _ = await link.start(request)
        XCTAssertEqual(link.slot?.phase, .waiting)

        oneToOne.receive(offerRow(0x0B, id: 1))
        await oneToOne.whenPublishesSettle()
        XCTAssertNil(oneToOne.activeCall, "a 1x1 offer must not ring over a link call")
        XCTAssertEqual(published.ends, [.busy])

        link.leave()
        oneToOne.receive(offerRow(0x0B, id: 2))
        XCTAssertEqual(oneToOne.activeCall?.state, .ringing)

        let refused = await link.start(request)
        XCTAssertEqual(
            refused,
            .busy(message: "You are already in a call. End it before you join another one."))

        // The busy refusal's affordance ends the ringing call.
        link.endCurrentCall()
        XCTAssertEqual(oneToOne.activeCall?.state, .done)
    }
}

// MARK: - The repository reports its chain draining

/// The wiring half of the mid-call rekey: a region run by `ChatRepository`
/// itself — not a lock built by the test — reports its group when it settles.
@MainActor
final class ChatRepositoryMlsIdleTests: XCTestCase {

    func testALockedRepositoryRegionReportsItsGroupWhenItSettles() async throws {
        let seed = Data(repeating: 0x11, count: 32)
        let founder = try Identity.fromSeed(seed: seed)
        let other = try Identity.fromSeed(seed: Data(repeating: 0x55, count: 32))
        let created = try Group.create(identity: founder)
        _ = try created.addMemberFromWire(identity: founder, keyPackage: try other.createKeyPackage())
        let hex = DsClient.pseudonymHex(try created.groupId())
        let blob = try founder.snapshotStorage()

        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)
        let snapshotStore = IdentitySnapshotStore()
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: blob)
        defer { snapshotStore.deleteSnapshot(accountIdB64: accountIdB64) }
        let knownGroups = KnownGroupsStore(
            defaults: UserDefaults(suiteName: "mls-idle-\(UUID().uuidString)")!)
        knownGroups.add(accountIdB64: accountIdB64, groupIdHex: hex)
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("mls-idle-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: outboxDir) }

        let repo = ChatRepository(
            chatClient: ChatClient(),
            dsClient: DsClient(baseURL: "http://127.0.0.1:9"),
            discovery: DiscoveryClient(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroups,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir),
            pushRouting: PushGroupRouting(),
            beaconStore: BeaconObservationStore(
                defaults: UserDefaults(suiteName: "mls-idle-b-\(UUID().uuidString)")!),
            pendingRequestStore: PendingRequestStore(
                defaults: UserDefaults(suiteName: "mls-idle-p-\(UUID().uuidString)")!)
        )
        // Phase 1 fixture: the chats run on the account identity the snapshot
        // was taken under (ADR-0050 device identities are exercised elsewhere).
        repo.deviceIdentityInstaller = { _, _ in nil }
        repo.bootstrap(
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x22, count: 32),
                accountIdentitySeed: seed,
                sessionToken: "t",
                sessionExpiresAtMs: 0
            )
        )
        XCTAssertTrue(repo.hasGroup(groupIdHex: hex))

        var reports: [String] = []
        repo.observeGroupMlsIdle { reports.append($0) }
        // A 3-byte key fails inside the locked membership region, before any
        // network — the region still settles, and that is what is reported.
        _ = try? await repo.setMemberAdmin(
            groupIdHex: hex, memberSignaturePubkey: Data([1, 2, 3]), makeAdmin: true)
        XCTAssertEqual(reports, [hex])
    }
}
