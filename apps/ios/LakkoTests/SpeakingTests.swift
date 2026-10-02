// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/group-call-grid.md` sections 5.2 and 7.3 — who is speaking. The
/// same list as the web `speaking.test.ts`, with the same four numbers.
final class SpeakingTrackerTests: XCTestCase {

    private let loud = Speaking.level * 5
    private let order = ["a", "b"]

    /// Feed `id` one level every 100 ms over `[from, to]`.
    private func feed(
        _ tracker: inout SpeakingTracker, _ id: String, _ level: Double, _ from: UInt64, _ to: UInt64
    ) {
        var now = from
        while now <= to {
            tracker.sample(id, level: level, nowMs: now)
            now += 100
        }
    }

    func testTheFourNumbersAreTheWebClients() {
        XCTAssertEqual(Speaking.level, 0.02)
        XCTAssertEqual(Speaking.ringOnMs, 200)
        XCTAssertEqual(Speaking.ringOffMs, 800)
        XCTAssertEqual(Speaking.floorOnsetMs, 1_500)
    }

    // MARK: - The ring

    func testTheRingComesOnAfterAShortSoundAndGoesOffAfterAPause() {
        var t = SpeakingTracker()
        t.sample("a", level: loud, nowMs: 0)
        XCTAssertFalse(t.snapshot(order: order, nowMs: 0).speakingIds.contains("a"))
        t.sample("a", level: loud, nowMs: Speaking.ringOnMs)
        XCTAssertTrue(
            t.snapshot(order: order, nowMs: Speaking.ringOnMs).speakingIds.contains("a"))
        t.sample("a", level: 0, nowMs: Speaking.ringOnMs + 100)
        XCTAssertTrue(
            t.snapshot(order: order, nowMs: Speaking.ringOnMs + 100).speakingIds.contains("a"))
        let off = Speaking.ringOnMs + 100 + Speaking.ringOffMs
        t.sample("a", level: 0, nowMs: off)
        XCTAssertFalse(t.snapshot(order: order, nowMs: off).speakingIds.contains("a"))
    }

    func testTheRingIsOnForEveryoneWhoIsSpeakingAtOnce() {
        var t = SpeakingTracker()
        feed(&t, "a", loud, 0, 500)
        feed(&t, "b", loud, 0, 500)
        XCTAssertEqual(t.snapshot(order: order, nowMs: 500).speakingIds, ["a", "b"])
    }

    /// A level that is not a number is silence, never sound.
    func testANonsenseLevelIsSilence() {
        var t = SpeakingTracker()
        feed(&t, "a", .nan, 0, 600)
        XCTAssertTrue(t.snapshot(order: order, nowMs: 600).speakingIds.isEmpty)
        feed(&t, "a", Speaking.level / 2, 700, 1_300)
        XCTAssertTrue(t.snapshot(order: order, nowMs: 1_300).speakingIds.isEmpty)
    }

    // MARK: - The floor

    func testTheFloorIsNotTakenByACough() {
        var t = SpeakingTracker()
        feed(&t, "a", loud, 0, 600)
        XCTAssertNil(t.snapshot(order: order, nowMs: 600).activeSpeakerId)
    }

    func testTheFloorGoesToSomeoneWhoKeepsSpeakingAndIsRememberedAfterTheyStop() {
        var t = SpeakingTracker()
        feed(&t, "a", loud, 0, Speaking.floorOnsetMs)
        let during = t.snapshot(order: order, nowMs: Speaking.floorOnsetMs)
        XCTAssertEqual(during.activeSpeakerId, "a")
        let end = Speaking.floorOnsetMs + 100 + Speaking.ringOffMs
        feed(&t, "a", 0, Speaking.floorOnsetMs + 100, end)
        let after = t.snapshot(order: order, nowMs: end)
        XCTAssertNil(after.activeSpeakerId)
        XCTAssertEqual(after.lastSpokeAtMs["a"], Speaking.floorOnsetMs)
    }

    func testTheFloorStaysWithWhoeverHoldsItWhileASecondPersonTalksOverThem() {
        var t = SpeakingTracker()
        feed(&t, "b", loud, 0, Speaking.floorOnsetMs)
        XCTAssertEqual(t.snapshot(order: order, nowMs: Speaking.floorOnsetMs).activeSpeakerId, "b")
        // `a` is earlier in the roster and now qualifies too; `b` keeps the floor.
        let end = 2 * Speaking.floorOnsetMs + 200
        feed(&t, "a", loud, Speaking.floorOnsetMs, end)
        feed(&t, "b", loud, Speaking.floorOnsetMs, end)
        XCTAssertEqual(t.snapshot(order: order, nowMs: end).activeSpeakerId, "b")
    }

    func testTheRosterOrderBreaksATie() {
        var t = SpeakingTracker()
        feed(&t, "b", loud, 0, Speaking.floorOnsetMs)
        feed(&t, "a", loud, 0, Speaking.floorOnsetMs)
        XCTAssertEqual(t.snapshot(order: order, nowMs: Speaking.floorOnsetMs).activeSpeakerId, "a")
    }

    func testSomeoneWhoLeftTheCallIsForgotten() {
        var t = SpeakingTracker()
        feed(&t, "a", loud, 0, Speaking.floorOnsetMs)
        _ = t.snapshot(order: order, nowMs: Speaking.floorOnsetMs)
        t.keepOnly(["b"])
        let snap = t.snapshot(order: ["b"], nowMs: Speaking.floorOnsetMs + 100)
        XCTAssertNil(snap.activeSpeakerId)
        XCTAssertNil(snap.lastSpokeAtMs["a"])
        XCTAssertTrue(snap.speakingIds.isEmpty)
    }
}

/// Where the levels come from: the peer connection's own statistics, read on
/// this device. The field names are pinned here because nothing else would
/// notice a renamed one: the ring would simply never come on.
final class CallAudioLevelStatsTests: XCTestCase {

    private func entry(_ type: String, _ values: [String: NSObject]) -> CallStatEntry {
        CallStatEntry(type: type, values: values)
    }

    func testAnInboundAudioStreamIsAttributedToThePersonItsTrackBelongsTo() {
        let levels = CallAudioLevelStats.levels(
            from: [
                entry("inbound-rtp", [
                    "kind": "audio" as NSString, "audioLevel": 0.31 as NSNumber,
                    "trackIdentifier": "track-a" as NSString,
                ]),
                entry("inbound-rtp", [
                    "kind": "audio" as NSString, "audioLevel": 0.0 as NSNumber,
                    "trackIdentifier": "track-b" as NSString,
                ]),
            ],
            peerIdByTrackId: ["track-a": "peer-id-a", "track-b": "peer-id-b"])
        XCTAssertEqual(levels.remoteByPeerId, ["peer-id-a": 0.31, "peer-id-b": 0.0])
        XCTAssertNil(levels.local)
    }

    func testTheLocalMicrophoneIsTheAudioMediaSource() {
        let levels = CallAudioLevelStats.levels(
            from: [
                entry("media-source", ["kind": "audio" as NSString, "audioLevel": 0.2 as NSNumber]),
                entry("media-source", ["kind": "video" as NSString, "audioLevel": 0.9 as NSNumber]),
            ],
            peerIdByTrackId: [:])
        XCTAssertEqual(levels.local, 0.2)
        XCTAssertTrue(levels.remoteByPeerId.isEmpty)
    }

    /// An unknown level is never a zero and never a guess: a video stream, a
    /// stream with no level, a stream whose track nobody is known to own, and
    /// every other kind of entry contribute nothing.
    func testAnythingWithoutAKnownLevelOrOwnerContributesNothing() {
        let levels = CallAudioLevelStats.levels(
            from: [
                entry("inbound-rtp", [
                    "kind": "video" as NSString, "audioLevel": 0.5 as NSNumber,
                    "trackIdentifier": "track-a" as NSString,
                ]),
                entry("inbound-rtp", [
                    "kind": "audio" as NSString, "trackIdentifier": "track-a" as NSString,
                ]),
                entry("inbound-rtp", [
                    "kind": "audio" as NSString, "audioLevel": 0.5 as NSNumber,
                    "trackIdentifier": "track-unknown" as NSString,
                ]),
                entry("inbound-rtp", ["kind": "audio" as NSString, "audioLevel": 0.5 as NSNumber]),
                entry("outbound-rtp", ["kind": "audio" as NSString, "audioLevel": 0.5 as NSNumber]),
                entry("candidate-pair", [:]),
            ],
            peerIdByTrackId: ["track-a": "peer-id-a"])
        XCTAssertEqual(levels, .none)
    }

    /// Some builds still spell the kind `mediaType`.
    func testTheOlderSpellingOfTheKindIsAccepted() {
        let levels = CallAudioLevelStats.levels(
            from: [
                entry("inbound-rtp", [
                    "mediaType": "audio" as NSString, "audioLevel": 0.4 as NSNumber,
                    "trackIdentifier": "track-a" as NSString,
                ]),
            ],
            peerIdByTrackId: ["track-a": "p"])
        XCTAssertEqual(levels.remoteByPeerId, ["p": 0.4])
    }
}

/// The monitor: levels in, the ring and the floor out, onto the call's state.
@MainActor
final class GroupSpeakingMonitorTests: XCTestCase {

    private func tile(_ id: String, isLocal: Bool = false) -> CallUiState.Participant {
        CallUiState.Participant(
            id: id, displayName: id, isLocal: isLocal,
            micOn: true, camOn: false, handRaised: false,
            quality: .good, speaking: false, screenSharing: false)
    }

    private func speaking(_ state: CallUiState) -> [String] {
        state.participants.filter(\.speaking).map(\.id)
    }

    func testAPersonWhoKeepsSpeakingGetsTheRingAndThenTheFloor() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a"), tile("peer-b")])
        let monitor = GroupSpeakingMonitor(ui: state)
        let loud = CallAudioLevels(local: 0, remoteByPeerId: ["a": 0.3, "b": 0])

        monitor.ingest(loud, nowMs: 0)
        XCTAssertEqual(speaking(state), [])
        monitor.ingest(loud, nowMs: Speaking.ringOnMs)
        XCTAssertEqual(speaking(state), ["peer-a"], "the ring is on the roster id")
        XCTAssertNil(state.speech.activeSpeakerId, "a short sound does not take the floor")

        var now = Speaking.ringOnMs
        while now < Speaking.floorOnsetMs {
            now += 150
            monitor.ingest(loud, nowMs: now)
        }
        XCTAssertEqual(state.speech.activeSpeakerId, "peer-a")
        XCTAssertNotNil(state.speech.lastSpokeAtMs["peer-a"])
    }

    func testTheOwnTileShowsTheOwnRing() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a")])
        let monitor = GroupSpeakingMonitor(ui: state)
        let own = CallAudioLevels(local: 0.4, remoteByPeerId: ["a": 0])
        monitor.ingest(own, nowMs: 0)
        monitor.ingest(own, nowMs: Speaking.ringOnMs)
        XCTAssertEqual(speaking(state), ["local"])
    }

    /// When a level cannot be read, the tracker is fed with nothing: no ring,
    /// and the stage falls back to the roster. Never a guessed value.
    func testAReadingWithNoLevelsDrawsNoRingAndMovesNoStage() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a")])
        let monitor = GroupSpeakingMonitor(ui: state)
        for now in stride(from: UInt64(0), through: 3_000, by: 150) {
            monitor.ingest(.none, nowMs: now)
        }
        XCTAssertEqual(speaking(state), [])
        XCTAssertEqual(state.speech, .none)
    }

    func testStoppingClearsEveryRing() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a")])
        let monitor = GroupSpeakingMonitor(ui: state)
        let loud = CallAudioLevels(local: nil, remoteByPeerId: ["a": 0.3])
        monitor.ingest(loud, nowMs: 0)
        monitor.ingest(loud, nowMs: Speaking.ringOnMs)
        XCTAssertEqual(speaking(state), ["peer-a"])
        monitor.stop()
        XCTAssertEqual(speaking(state), [])
        XCTAssertEqual(state.speech, .none)
    }

    /// The roster is written only when a ring changed: this runs several
    /// times a second, and a new array every time would redraw every tile.
    func testAReadingThatChangesNothingPublishesNothing() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a")])
        let monitor = GroupSpeakingMonitor(ui: state)
        var published = 0
        let watch = state.$participants.dropFirst().sink { _ in published += 1 }
        defer { watch.cancel() }
        let quiet = CallAudioLevels(local: 0, remoteByPeerId: ["a": 0])
        for now in stride(from: UInt64(0), through: 1_500, by: 150) {
            monitor.ingest(quiet, nowMs: now)
        }
        XCTAssertEqual(published, 0)
    }

    func testTheRosterIdMapsToTheSfuPeerId() {
        XCTAssertEqual(GroupSpeakingMonitor.peerId(ofParticipant: "peer-abc"), "abc")
        XCTAssertEqual(GroupSpeakingMonitor.peerId(ofParticipant: "local"), "local")
    }
}
