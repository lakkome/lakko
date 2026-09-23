// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// The app-level 1x1 call host: an offer on ANY chat rings, each signaling row
/// is handled exactly once, stale offers stay silent, a second call is answered
/// busy, group-control bodies never reach the 1x1 session (C-11), and a
/// terminal body for another call or another group is dropped (C-12).
///
/// Fixtures compose their payloads with the real `composeCallSignaling`, and
/// sessions are real `CallSession`s: nothing here dials or accepts, so no
/// WebRTC peer connection is ever built.
@MainActor
final class CallHostTests: XCTestCase {

    // MARK: - Fixtures

    private let callA = Data(repeating: 0x0A, count: 16)
    private let callB = Data(repeating: 0x0B, count: 16)
    private let groupA = "aa11"
    private let groupB = "bb22"
    private let t0: Int64 = 1_800_000_000_000

    private final class FakeTimer: CallHostTimer {
        let afterMs: Int64
        let fire: @MainActor () -> Void
        var cancelled = false
        init(afterMs: Int64, fire: @escaping @MainActor () -> Void) {
            self.afterMs = afterMs
            self.fire = fire
        }
        func cancel() { cancelled = true }
    }

    private final class Harness {
        var nowMs: Int64
        var timers: [FakeTimer] = []
        var published: [(group: String, body: FfiCallSignalingBody)] = []
        var micGranted = true
        var camGranted = true
        /// `docs/ux/calls.md` section 6 — the rows the host filed.
        var recorded: [FinishedCall] = []
        /// `docs/ux/calls.md` sections 3.1 and 3.2 — the calls the host put on
        /// the wire, and with which media. A port rather than a real
        /// `session.dial`, so no peer connection is ever built here.
        var placed: [(audio: Bool, video: Bool)] = []
        var hasCamera = true
        /// `docs/ux/calls.md` section 2.1 — the conversations this device has
        /// muted, as the host's ``CallHost/Ports/isMuted`` port answers.
        var mutedGroups: Set<String> = []
        init(nowMs: Int64) { self.nowMs = nowMs }

        @MainActor
        func fireLiveTimers() {
            let live = timers.filter { !$0.cancelled }
            timers.removeAll()
            live.forEach { $0.fire() }
        }
    }

    private func makeHost(_ h: Harness) -> CallHost {
        CallHost(ports: CallHost.Ports(
            publish: { group, bytes in
                if let body = try? parseCallSignaling(plaintext: bytes) {
                    h.published.append((group, body))
                }
                return true
            },
            deriveCallKey: { _, _ in (Data(repeating: 0x42, count: 32), 0) },
            nowMs: { h.nowMs },
            schedule: { ms, fire in
                let timer = FakeTimer(afterMs: ms, fire: fire)
                h.timers.append(timer)
                return timer
            },
            microphoneGranted: { h.micGranted },
            cameraGranted: { video in !video || h.camGranted },
            peerName: { _ in "Alice" },
            mintCallId: { Data(repeating: 0x0C, count: 16) },
            recordCall: { call in h.recorded.append(call) },
            isMuted: { h.mutedGroups.contains($0) },
            hasCamera: { h.hasCamera },
            placeCall: { _, audio, video in h.placed.append((audio, video)) }
        ))
    }

    private func dsTime(_ ms: Int64) -> String {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
    }

    private func offer(_ callId: Data, video: Bool = false) -> FfiCallSignalingBody {
        .offer(callId: callId, sdp: "v=0", createdAtMs: 1, media: FfiCallMediaFlags(audio: true, video: video))
    }

    private func row(
        _ body: FfiCallSignalingBody,
        group: String,
        id: Int64,
        atMs: Int64
    ) -> InboundCallRow {
        InboundCallRow(groupIdHex: group, messageId: id, createdAt: dsTime(atMs), body: body)
    }

    private func endReasons(_ h: Harness) -> [(String, Data, FfiCallEndReason)] {
        h.published.compactMap { entry in
            if case .end(let cid, let reason) = entry.body { return (entry.group, cid, reason) }
            return nil
        }
    }

    // MARK: - Ringing from any chat

    /// Through the repository, exactly as a receive path delivers it, with no
    /// chat screen or chat view model in existence: the offer rings.
    func testAnOfferOnAChatThatIsNotOpenRingsThroughTheRepository() throws {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        let repo = ChatRepository()
        host.attach(to: repo)

        let bytes = try composeCallSignaling(body: offer(callA, video: true))
        repo.routeCallSignaling(
            groupIdHex: groupA, messageId: 7, payload: bytes, createdAt: dsTime(t0 - 2_000), senderLeaf: 1, senderDeviceKey: "device-1", senderKey: "member-1")

        XCTAssertEqual(host.activeCall?.state, .ringing)
        XCTAssertEqual(host.activeCallState, .ringing)
        XCTAssertEqual(host.activeGroupIdHex, groupA)
        XCTAssertEqual(host.activeCall?.callId, callA)
        XCTAssertTrue(host.incomingVideo)
        XCTAssertEqual(host.activePeerName, "Alice")
    }

    /// A chat view model built later (for any chat) must not take the feed
    /// back: that is the defect — the last-built view model owned the only slot
    /// and filtered to its own group.
    func testBuildingAChatViewModelDoesNotStealTheSignalingFeed() throws {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        let repo = ChatRepository()
        host.attach(to: repo)

        var model: ChatViewModel? = ChatViewModel(repository: repo, groupIdHex: groupB, callHost: host)
        XCTAssertNotNil(model)
        let bytes = try composeCallSignaling(body: offer(callA))
        repo.routeCallSignaling(
            groupIdHex: groupA, messageId: 1, payload: bytes, createdAt: dsTime(t0), senderLeaf: 1, senderDeviceKey: "device-1", senderKey: "member-1")
        XCTAssertEqual(host.activeCall?.state, .ringing, "an offer on another chat must still ring")

        // The view model going away (leaving the chat, a SwiftUI rebuild) does
        // not take the call with it.
        model = nil
        XCTAssertEqual(host.activeCall?.state, .ringing)
        XCTAssertEqual(host.activeGroupIdHex, groupA)
    }

    // MARK: - Exactly once

    /// A busy refusal is the observable effect: a re-fed row (a held cursor
    /// re-fetch, two receive paths overlapping) must not publish a second one.
    /// A genuinely new row for the same call is still answered.
    func testARowDeliveredTwiceIsHandledOnce() async {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0))
        let busyRow = row(offer(callB), group: groupB, id: 5, atMs: t0)
        host.receive(busyRow)
        host.receive(busyRow)
        await host.whenPublishesSettle()
        XCTAssertEqual(endReasons(h).count, 1, "a re-fed row must not answer busy twice")

        host.receive(row(offer(callB), group: groupB, id: 6, atMs: t0))
        await host.whenPublishesSettle()
        XCTAssertEqual(endReasons(h).count, 2)
    }

    /// The key is (group, message id): row 7 of one journal is not row 7 of
    /// another.
    func testTheSameMessageIdOnAnotherGroupIsADifferentRow() async {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.receive(row(offer(callA), group: groupA, id: 7, atMs: t0))
        host.receive(row(offer(callB), group: groupB, id: 7, atMs: t0))
        await host.whenPublishesSettle()
        let ends = endReasons(h)
        XCTAssertEqual(ends.count, 1)
        XCTAssertEqual(ends.first?.0, groupB)
        XCTAssertEqual(ends.first?.1, callB)
    }

    // MARK: - Stale offers

    func testAStaleOfferDoesNotRingAndIsNotAnsweredBusy() async {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        let window = Int64(CallHostRouting.ringWindowMs)
        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0 - window))
        XCTAssertNil(host.activeCall, "an offer at the ring window must not ring")

        host.receive(InboundCallRow(groupIdHex: groupA, messageId: 2, createdAt: "not a time", body: offer(callA)))
        XCTAssertNil(host.activeCall, "an offer whose age cannot be read must not ring")

        host.receive(row(offer(callB), group: groupB, id: 3, atMs: t0 - (window - 1_000)))
        XCTAssertEqual(host.activeCall?.state, .ringing, "an offer inside the window still rings")
        await host.whenPublishesSettle()
        XCTAssertTrue(endReasons(h).isEmpty)
    }

    /// The age is read off the DS row, and the boundary belongs to the caller
    /// giving up — at exactly the window this side must not still be offering
    /// an answer to a call nobody is on.
    func testTheStaleRuleReadsTheRowTimeNotTheLocalClockAlone() {
        let window = Int64(CallHostRouting.ringWindowMs)
        XCTAssertEqual(CallHostRouting.offerAgeMs(createdAt: dsTime(t0 - 5_000), nowMs: t0), 5_000)
        XCTAssertNil(CallHostRouting.offerAgeMs(createdAt: "", nowMs: t0))
        // A server clock reading ahead of this device yields zero, never a
        // negative age that would ring past the window.
        XCTAssertEqual(CallHostRouting.offerAgeMs(createdAt: dsTime(t0 + 9_000), nowMs: t0), 0)
        XCTAssertEqual(
            CallHostRouting.decide(
                row: row(offer(callA), group: groupA, id: 1, atMs: t0 - (window - 1)),
                nowMs: t0, active: nil, endedCallIds: [], otherCallBusy: false),
            .ring)
        XCTAssertEqual(
            CallHostRouting.decide(
                row: row(offer(callA), group: groupA, id: 1, atMs: t0 - window),
                nowMs: t0, active: nil, endedCallIds: [], otherCallBusy: false),
            .drop)
    }

    /// The window is the shared one, not a second copy of the number: it is a
    /// value the CALLER'S device has to agree with, and a drift means one side
    /// files a missed call while the other is still ringing.
    func testTheRingWindowIsTheSharedOne() {
        XCTAssertEqual(CallHostRouting.ringWindowMs, LakkoCore.callRingWindowMs())
        XCTAssertEqual(CallHostTiming().ringTimeoutMs, Int64(LakkoCore.callRingWindowMs()))
    }

    // MARK: - A muted conversation

    /// `docs/ux/calls.md` section 2.1. No session, no screen, no sound — and a
    /// history row all the same, because the person still gets to find out who
    /// tried to reach them.
    func testAMutedConversationRecordsTheCallWithoutRinging() async {
        let h = Harness(nowMs: t0)
        h.mutedGroups = [groupA]
        let host = makeHost(h)

        host.receive(row(offer(callA, video: true), group: groupA, id: 1, atMs: t0 - 2_000))

        XCTAssertNil(host.activeCall, "a muted conversation must not open a ringing session")
        XCTAssertEqual(h.recorded.count, 1)
        XCTAssertEqual(h.recorded.first?.groupIdHex, groupA)
        XCTAssertEqual(h.recorded.first?.row.direction, .incoming)
        XCTAssertEqual(h.recorded.first?.row.outcome, .missed)
        // Calling back repeats the mode the caller asked for.
        XCTAssertEqual(h.recorded.first?.video, true)
        // Filed at the moment it happened, by the server's stamp, not whenever
        // this device got round to reading the row.
        XCTAssertEqual(h.recorded.first?.startedAtMs, t0 - 2_000)

        await host.whenPublishesSettle()
        XCTAssertTrue(endReasons(h).isEmpty, "a muted call is not answered busy or declined")
    }

    /// Recorded ONCE. A journal is re-read on every catch-up, so a second
    /// delivery of the same offer must not file a second missed call.
    func testAMutedCallIsRecordedOnlyOnce() async {
        let h = Harness(nowMs: t0)
        h.mutedGroups = [groupA]
        let host = makeHost(h)

        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0 - 2_000))
        host.receive(row(offer(callA), group: groupA, id: 2, atMs: t0 - 1_000))

        XCTAssertEqual(h.recorded.count, 1)
        await host.whenPublishesSettle()
    }

    /// Muting one conversation silences that one. The next chat still rings,
    /// which is the difference between a mute and a broken ring path.
    func testAnUnmutedConversationStillRingsBesideAMutedOne() async {
        let h = Harness(nowMs: t0)
        h.mutedGroups = [groupA]
        let host = makeHost(h)

        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0 - 2_000))
        XCTAssertNil(host.activeCall)

        host.receive(row(offer(callB), group: groupB, id: 1, atMs: t0 - 2_000))
        XCTAssertEqual(host.activeCall?.state, .ringing)
        await host.whenPublishesSettle()
    }

    // MARK: - One call at a time

    func testASecondOfferWhileRingingIsAnsweredBusyToItsOwnGroup() async {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0))
        host.receive(row(offer(callB), group: groupB, id: 1, atMs: t0))
        await host.whenPublishesSettle()

        XCTAssertEqual(host.activeCall?.callId, callA, "the first call keeps the slot")
        XCTAssertEqual(host.activeCall?.state, .ringing)
        let ends = endReasons(h)
        XCTAssertEqual(ends.count, 1)
        XCTAssertEqual(ends.first?.0, groupB)
        XCTAssertEqual(ends.first?.1, callB)
        XCTAssertEqual(ends.first?.2, .busy)
    }

    func testAnOfferDuringAPreparedOutgoingCallIsAnsweredBusy() async throws {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.startOutgoingCall(groupIdHex: groupA)
        try await waitUntil { host.activeCall != nil }
        XCTAssertEqual(host.activeCallState, .idle)

        host.receive(row(offer(callB), group: groupB, id: 1, atMs: t0))
        await host.whenPublishesSettle()
        XCTAssertEqual(endReasons(h).map(\.2), [.busy])
        XCTAssertEqual(host.activeGroupIdHex, groupA)
    }

    func testAnOfferDuringAGroupOrLinkCallIsAnsweredBusy() async {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        var other: CallKind? = .group
        host.otherCallOccupant = { other }
        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0))
        await host.whenPublishesSettle()
        XCTAssertNil(host.activeCall)
        XCTAssertEqual(endReasons(h).map(\.2), [.busy])

        other = .link
        host.receive(row(offer(callB), group: groupB, id: 2, atMs: t0))
        await host.whenPublishesSettle()
        XCTAssertNil(host.activeCall, "a live call-link call holds the slot too")
        XCTAssertEqual(endReasons(h).map(\.2), [.busy, .busy])

        // The other call ending frees the slot.
        other = nil
        XCTAssertFalse(host.otherCallBusy)
        host.receive(row(offer(callB), group: groupB, id: 3, atMs: t0))
        XCTAssertEqual(host.activeCall?.state, .ringing)
    }

    func testStartingAOneToOneCallDuringAGroupCallIsRefusedWithAReason() {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.otherCallOccupant = { .group }
        host.startOutgoingCall(groupIdHex: groupA)
        XCTAssertNil(host.activeCall)
        XCTAssertFalse(host.callStarting)
        XCTAssertEqual(
            host.callError,
            "You are already in a group call. End it before you start another one.")
    }

    // MARK: - C-11 and C-12

    func testGroupControlBodiesNeverReachTheOneToOneSession() throws {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        let repo = ChatRepository()
        host.attach(to: repo)
        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0))

        // Through the repository: a group start for the ringing call id on the
        // same group folds into the group-call record and nowhere else.
        let start = try composeCallSignaling(
            body: .groupStart(callId: callA, initiatorLeaf: 1, createdAtMs: UInt64(t0),
                              media: FfiCallMediaFlags(audio: true, video: false)))
        repo.routeCallSignaling(
            groupIdHex: groupA, messageId: 2, payload: start, createdAt: dsTime(t0), senderLeaf: 1, senderDeviceKey: "device-1", senderKey: "member-1")
        XCTAssertEqual(host.activeCall?.state, .ringing)

        // And the pure rule drops them even if a caller mis-routed one.
        for body: FfiCallSignalingBody in [
            .groupStart(callId: callA, initiatorLeaf: 1, createdAtMs: 1, media: FfiCallMediaFlags(audio: true, video: false)),
            .groupJoin(callId: callA, leafIndex: 1, joinedAtMs: 1),
            .groupLeave(callId: callA, leafIndex: 1, reason: .hungUp),
        ] {
            XCTAssertEqual(
                CallHostRouting.decide(
                    row: row(body, group: groupA, id: 9, atMs: t0), nowMs: t0,
                    active: .init(callIdHex: CallHostRouting.hex(callA), groupIdHex: groupA),
                    endedCallIds: [], otherCallBusy: false),
                .drop)
        }
    }

    func testAnEndForAnotherCallOrFromAnotherGroupIsDropped() {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0))

        host.receive(row(.end(callId: callB, reason: .hungUp), group: groupA, id: 2, atMs: t0))
        XCTAssertEqual(host.activeCall?.state, .ringing, "C-12: another call id is not this call")

        host.receive(row(.end(callId: callA, reason: .hungUp), group: groupB, id: 3, atMs: t0))
        XCTAssertEqual(host.activeCall?.state, .ringing, "the same call id on another group's journal is not this call")

        host.receive(row(.end(callId: callA, reason: .hungUp), group: groupA, id: 4, atMs: t0))
        XCTAssertEqual(host.activeCall?.state, .done, "the matching end ends the call")
        XCTAssertNil(host.activeCallState, "a call that ended while ringing has no screen to show")
    }

    func testAnEndedCallNeverRingsAgain() async throws {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0))
        host.declineIncomingCall()
        try await waitUntil { host.activeCall == nil }
        await host.whenPublishesSettle()
        XCTAssertEqual(endReasons(h).map(\.2).last, .declined)

        host.receive(row(offer(callA), group: groupA, id: 2, atMs: t0))
        XCTAssertNil(host.activeCall)
    }

    // MARK: - Timers

    func testAnUnansweredCallTimesOutAndPublishesTimedOut() async throws {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0))
        XCTAssertEqual(h.timers.filter { !$0.cancelled }.map(\.afterMs), [45_000])

        h.fireLiveTimers()
        await host.whenPublishesSettle()
        XCTAssertEqual(endReasons(h).map(\.2), [.timedOut])
        try await waitUntil { host.activeCall == nil }
    }

    func testARefusedAcceptKeepsRingingForTheRestOfTheWindow() async throws {
        let h = Harness(nowMs: t0)
        h.camGranted = false
        let host = makeHost(h)
        host.receive(row(offer(callA, video: true), group: groupA, id: 1, atMs: t0))
        h.nowMs = t0 + 30_000
        host.acceptIncomingCall(withVideo: true)
        try await waitUntil { host.callError != nil }
        XCTAssertEqual(host.callError, CameraPermission.deniedMessage)
        XCTAssertEqual(host.activeCall?.state, .ringing)
        XCTAssertEqual(h.timers.filter { !$0.cancelled }.map(\.afterMs), [15_000])
    }

    // MARK: - What the caller chose (docs/ux/calls.md section 3)

    /// Section 3.1 — the phone button places the call. There is no preparation
    /// screen between the tap and the call at all: the old one asked the caller
    /// to check a microphone, over an empty camera box, before a voice call.
    func testThePhoneButtonDialsImmediately() async throws {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.startOutgoingCall(groupIdHex: groupA, button: .voice)
        try await waitUntil { !h.placed.isEmpty }
        XCTAssertEqual(host.outgoingButton, .voice)
        XCTAssertEqual(host.outgoingSurface, .dial(video: false))
        XCTAssertEqual(h.placed.count, 1)
        XCTAssertTrue(h.placed[0].audio)
        XCTAssertFalse(h.placed[0].video, "the phone button never turns the camera on")
        XCTAssertNil(host.outgoingNotice)
    }

    /// Section 3.2 since 2026-09-21 — the video button dials at once, carrying
    /// the camera. It opened a camera check until then, and these two tests
    /// replace the pair that watched it stop there and be released by the
    /// check's own Call button.
    func testTheVideoButtonDialsAtOnceCarryingTheCamera() async throws {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.startOutgoingCall(groupIdHex: groupA, button: .video)
        try await waitUntil { !h.placed.isEmpty }
        XCTAssertEqual(host.outgoingButton, .video)
        XCTAssertEqual(host.outgoingSurface, .dial(video: true))
        XCTAssertEqual(h.placed.count, 1)
        XCTAssertTrue(h.placed[0].audio)
        XCTAssertTrue(h.placed[0].video)
    }

    /// Neither button asks a question before the call.
    func testNoCallButtonStopsBeforeDialling() async throws {
        for button in [CallButtonKind.voice, CallButtonKind.video] {
            let h = Harness(nowMs: t0)
            let host = makeHost(h)
            host.startOutgoingCall(groupIdHex: groupA, button: button)
            try await waitUntil { !h.placed.isEmpty }
            XCTAssertEqual(h.placed.count, 1, "\(button) must place exactly one call")
            XCTAssertTrue(h.placed[0].audio, "\(button) must carry an audio section")
        }
    }

    /// Section 3.2 — a device with no camera answers the video button with a
    /// voice call and one line saying so, rather than a check of a camera that
    /// does not exist.
    func testTheVideoButtonOnACameralessDeviceCallsWithVoiceAndSaysSo() async throws {
        let h = Harness(nowMs: t0)
        h.hasCamera = false
        let host = makeHost(h)
        host.startOutgoingCall(groupIdHex: groupA, button: .video)
        try await waitUntil { !h.placed.isEmpty }
        XCTAssertEqual(host.outgoingSurface, .dial(video: false))
        XCTAssertFalse(h.placed[0].video)
        XCTAssertEqual(
            host.outgoingNotice, "This device has no camera. Calling with voice only.")
    }

    /// An incoming call has no button of ours, and section 3.3 says accepting
    /// one never shows a preparation screen.
    func testAnIncomingCallCarriesNoOutgoingButtonOrSurface() {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.receive(row(offer(callA, video: true), group: groupA, id: 1, atMs: t0))
        XCTAssertNil(host.outgoingButton)
        XCTAssertNil(host.outgoingSurface)
        XCTAssertTrue(host.incomingVideo)
        XCTAssertTrue(h.placed.isEmpty)
    }

    // MARK: - What the call leaves behind (docs/ux/calls.md section 6)

    func testADeclinedIncomingCallIsRecordedInThatConversation() async throws {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0))
        host.declineIncomingCall()
        try await waitUntil { host.activeCall == nil }
        XCTAssertEqual(h.recorded.count, 1)
        XCTAssertEqual(h.recorded.first?.groupIdHex, groupA)
        XCTAssertEqual(h.recorded.first?.row.direction, .incoming)
        XCTAssertEqual(h.recorded.first?.row.text, "Declined")
    }

    func testARingThatRanOutIsRecordedAsMissed() async throws {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0))
        h.fireLiveTimers()
        try await waitUntil { host.activeCall == nil }
        XCTAssertEqual(h.recorded.first?.row.text, "Missed call")
        XCTAssertEqual(h.recorded.first?.row.direction, .incoming)
    }

    /// A pre-join the user cancelled never rang anywhere: it is not a call and
    /// leaves no row.
    func testACancelledPreJoinLeavesNoRow() async throws {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.startOutgoingCall(groupIdHex: groupA, button: .voice)
        try await waitUntil { host.activeCall != nil }
        host.clearFinishedCall()
        try await waitUntil { host.activeCall == nil }
        XCTAssertTrue(h.recorded.isEmpty, "a call that was never placed leaves nothing behind")
    }

    /// One row per call, however many times the slot is cleared afterwards.
    func testACallIsRecordedOnce() async throws {
        let h = Harness(nowMs: t0)
        let host = makeHost(h)
        host.receive(row(offer(callA), group: groupA, id: 1, atMs: t0))
        host.declineIncomingCall()
        try await waitUntil { host.activeCall == nil }
        host.clearFinishedCall()
        host.reset()
        XCTAssertEqual(h.recorded.count, 1)
    }

    // MARK: - Helpers

    private func waitUntil(
        timeout: TimeInterval = 5,
        _ condition: @MainActor () -> Bool
    ) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() {
            if Date() > deadline {
                XCTFail("condition not met in time")
                return
            }
            try await Task.sleep(nanoseconds: 5_000_000)
        }
    }
}
