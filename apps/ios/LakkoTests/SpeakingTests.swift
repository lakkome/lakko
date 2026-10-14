// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

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
///
/// The rule itself (what counts as sound, the ring's and the floor's timing)
/// is the shared one of `crates/lakko-ui-rules/src/speaking.rs` and is tested
/// there. These cases drive the real tracker through the binding, so they
/// assert what the monitor tells it and what it does with the answers.
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
        monitor.ingest(loud, nowMs: speakingRingOnMs())
        XCTAssertEqual(speaking(state), ["peer-a"], "the ring is on the roster id")
        XCTAssertNil(state.speech.activeSpeakerId, "a short sound does not take the floor")

        var now = speakingRingOnMs()
        while now < speakingFloorOnsetMs() {
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
        monitor.ingest(own, nowMs: speakingRingOnMs())
        XCTAssertEqual(speaking(state), ["local"])
    }

    /// When a level cannot be read, the person is sampled as silence, as on
    /// the web client: no ring, and the stage falls back to the roster.
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

    private func tile(_ id: String, isLocal: Bool, micOn: Bool) -> CallUiState.Participant {
        CallUiState.Participant(
            id: id, displayName: id, isLocal: isLocal,
            micOn: micOn, camOn: false, handRaised: false,
            quality: .good, speaking: false, screenSharing: false)
    }

    /// The own microphone never takes the floor, however long it is loud:
    /// the roster's first person used to be this device's own, and a floor
    /// held by it kept the large tile from moving to the person speaking.
    func testTheOwnMicrophoneNeverTakesTheFloor() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a"), tile("peer-b")])
        let monitor = GroupSpeakingMonitor(ui: state)
        let own = CallAudioLevels(local: 0.5, remoteByPeerId: ["a": 0, "b": 0])
        for now in stride(from: UInt64(0), through: 2 * speakingFloorOnsetMs(), by: 150) {
            monitor.ingest(own, nowMs: now)
        }
        XCTAssertEqual(speaking(state), ["local"], "the own ring is drawn")
        XCTAssertNil(state.speech.activeSpeakerId)
        XCTAssertNil(state.speech.lastSpokeAtMs["local"])
    }

    /// Someone else takes the floor while the own microphone is loud too.
    func testAnotherPersonTakesTheFloorOverTheOwnMicrophone() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a"), tile("peer-b")])
        let monitor = GroupSpeakingMonitor(ui: state)
        let both = CallAudioLevels(local: 0.5, remoteByPeerId: ["a": 0, "b": 0.3])
        for now in stride(from: UInt64(0), through: 2 * speakingFloorOnsetMs(), by: 150) {
            monitor.ingest(both, nowMs: now)
        }
        XCTAssertEqual(state.speech.activeSpeakerId, "peer-b")
    }

    /// A muted microphone still measures the room. Its level is not read
    /// while the microphone is off: no ring on the own tile.
    func testTheOwnLevelIsIgnoredWhileTheMicrophoneIsOff() {
        let state = CallUiState()
        state.setParticipants([
            tile("local", isLocal: true, micOn: false), tile("peer-a"),
        ])
        let monitor = GroupSpeakingMonitor(ui: state)
        let own = CallAudioLevels(local: 0.5, remoteByPeerId: ["a": 0])
        for now in stride(from: UInt64(0), through: 1_500, by: 150) {
            monitor.ingest(own, nowMs: now)
        }
        XCTAssertEqual(speaking(state), [])
    }

    /// Turning the microphone off turns the ring off at once, not 800 ms
    /// later.
    func testMutingTurnsTheOwnRingOffAtOnce() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a")])
        let monitor = GroupSpeakingMonitor(ui: state)
        let own = CallAudioLevels(local: 0.5, remoteByPeerId: ["a": 0])
        monitor.ingest(own, nowMs: 0)
        monitor.ingest(own, nowMs: speakingRingOnMs())
        XCTAssertEqual(speaking(state), ["local"])
        state.setMic(false)
        monitor.ingest(own, nowMs: speakingRingOnMs() + 150)
        XCTAssertEqual(speaking(state), [])
    }

    /// A person whose level stops being reported is sampled as silence, so
    /// their ring goes off instead of standing.
    func testARingGoesOffWhenItsLevelStopsBeingReported() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a")])
        let monitor = GroupSpeakingMonitor(ui: state)
        let loud = CallAudioLevels(local: 0, remoteByPeerId: ["a": 0.3])
        monitor.ingest(loud, nowMs: 0)
        monitor.ingest(loud, nowMs: speakingRingOnMs())
        XCTAssertEqual(speaking(state), ["peer-a"])
        let gone = CallAudioLevels(local: 0, remoteByPeerId: [:])
        monitor.ingest(gone, nowMs: speakingRingOnMs() + 150)
        monitor.ingest(gone, nowMs: speakingRingOnMs() + 150 + speakingRingOffMs())
        XCTAssertEqual(speaking(state), [])
    }

    /// A person the report carries no level for teaches their noise floor
    /// nothing, and a muted one keeps what was learned.
    func testAMissingLevelIsNotAQuietRoom() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a")])
        let monitor = GroupSpeakingMonitor(ui: state)
        for now in stride(from: UInt64(0), through: 3_000, by: 150) {
            monitor.ingest(.none, nowMs: now)
        }
        XCTAssertEqual(monitor.threshold(ofParticipant: "peer-a"), speakingLevel())
        XCTAssertEqual(monitor.threshold(ofParticipant: "local"), speakingLevel())
        // A quiet room, read: the threshold follows it down.
        let quiet = CallAudioLevels(local: 0.002, remoteByPeerId: ["a": 0.002])
        for now in stride(from: UInt64(3_150), through: 6_000, by: 150) {
            monitor.ingest(quiet, nowMs: now)
        }
        XCTAssertEqual(
            monitor.threshold(ofParticipant: "peer-a"), 0.002 * 3, accuracy: 1e-9,
            "three times the floor of the room")
    }

    /// A person listening with their own microphone off still sees who is
    /// speaking: the ring and the floor of the others do not depend on it.
    func testAMutedListenerSeesTheRemoteSpeaker() {
        let state = CallUiState()
        state.setParticipants([
            tile("local", isLocal: true, micOn: false), tile("peer-a"), tile("peer-b"),
        ])
        let monitor = GroupSpeakingMonitor(ui: state)
        let levels = CallAudioLevels(local: 0.5, remoteByPeerId: ["a": 0, "b": 0.3])
        for now in stride(from: UInt64(0), through: 2 * speakingFloorOnsetMs(), by: 150) {
            monitor.ingest(levels, nowMs: now)
        }
        XCTAssertEqual(speaking(state), ["peer-b"])
        XCTAssertEqual(state.speech.activeSpeakerId, "peer-b")
    }

    /// The own person never holds the floor wherever they stand in the
    /// roster, not only when they are first.
    func testTheOwnMicrophoneNeverTakesTheFloorWhereverItStandsInTheRoster() {
        for roster in [
            [tile("peer-a"), tile("local", isLocal: true), tile("peer-b")],
            [tile("peer-a"), tile("peer-b"), tile("local", isLocal: true)],
        ] {
            let state = CallUiState()
            state.setParticipants(roster)
            let monitor = GroupSpeakingMonitor(ui: state)
            let own = CallAudioLevels(local: 0.5, remoteByPeerId: ["a": 0, "b": 0])
            for now in stride(from: UInt64(0), through: 2 * speakingFloorOnsetMs(), by: 150) {
                monitor.ingest(own, nowMs: now)
            }
            XCTAssertEqual(speaking(state), ["local"], "the own ring is drawn")
            XCTAssertNil(state.speech.activeSpeakerId)
            XCTAssertNil(state.speech.lastSpokeAtMs["local"])
        }
    }

    /// The person holding the floor is muted: they give it up at the same
    /// reading, and are still remembered as the last to have held it.
    func testAMutedHolderGivesUpTheFloorAtOnce() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a"), tile("peer-b")])
        let monitor = GroupSpeakingMonitor(ui: state)
        let loud = CallAudioLevels(local: 0, remoteByPeerId: ["a": 0.3, "b": 0])
        var now: UInt64 = 0
        while now <= speakingFloorOnsetMs() + 150 {
            monitor.ingest(loud, nowMs: now)
            now += 150
        }
        XCTAssertEqual(state.speech.activeSpeakerId, "peer-a")
        state.setParticipants([
            tile("local", isLocal: true), tile("peer-a", isLocal: false, micOn: false),
            tile("peer-b"),
        ])
        monitor.ingest(loud, nowMs: now)
        XCTAssertEqual(speaking(state), [])
        XCTAssertNil(state.speech.activeSpeakerId)
        XCTAssertNotNil(state.speech.lastSpokeAtMs["peer-a"])
    }

    /// The person holding the floor leaves: the floor is empty at the next
    /// reading and nothing remembers them.
    func testAHolderWhoLeavesGivesUpTheFloorAndIsForgotten() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a"), tile("peer-b")])
        let monitor = GroupSpeakingMonitor(ui: state)
        let loud = CallAudioLevels(local: 0, remoteByPeerId: ["a": 0.3, "b": 0])
        var now: UInt64 = 0
        while now <= speakingFloorOnsetMs() + 150 {
            monitor.ingest(loud, nowMs: now)
            now += 150
        }
        XCTAssertEqual(state.speech.activeSpeakerId, "peer-a")
        state.setParticipants([tile("local", isLocal: true), tile("peer-b")])
        monitor.ingest(loud, nowMs: now)
        XCTAssertNil(state.speech.activeSpeakerId)
        XCTAssertNil(state.speech.lastSpokeAtMs["peer-a"])
    }

    /// A level above full scale is read as full scale: it draws the ring
    /// like any loud sound and leaves the threshold inside its bounds.
    func testALevelAboveFullScaleIsReadAsFullScale() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a")])
        let monitor = GroupSpeakingMonitor(ui: state)
        let off = CallAudioLevels(local: 0, remoteByPeerId: ["a": 7])
        for now in stride(from: UInt64(0), through: 20_000, by: 150) {
            monitor.ingest(off, nowMs: now)
            if now == 300 { XCTAssertEqual(speaking(state), ["peer-a"]) }
        }
        XCTAssertEqual(monitor.threshold(ofParticipant: "peer-a"), speakingLevelMax())
    }

    /// One tracker per call: stopping drops what was learned about a room.
    func testStoppingForgetsWhatWasLearned() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a")])
        let monitor = GroupSpeakingMonitor(ui: state)
        let quiet = CallAudioLevels(local: 0.002, remoteByPeerId: ["a": 0.002])
        for now in stride(from: UInt64(0), through: 3_000, by: 150) {
            monitor.ingest(quiet, nowMs: now)
        }
        XCTAssertLessThan(monitor.threshold(ofParticipant: "peer-a"), speakingLevel())
        monitor.stop()
        XCTAssertEqual(monitor.threshold(ofParticipant: "peer-a"), speakingLevel())
    }

    /// The numbers the monitor's tests are written against are the shared
    /// rule's.
    func testTheTimingsComeFromTheSharedRule() {
        XCTAssertEqual(speakingRingOnMs(), 200)
        XCTAssertEqual(speakingRingOffMs(), 800)
        XCTAssertEqual(speakingFloorOnsetMs(), 1_500)
        XCTAssertEqual(speakingLevel(), 0.02)
        XCTAssertEqual(speakingLevelMin(), 0.005)
        XCTAssertEqual(speakingLevelMax(), 0.05)
    }

    func testStoppingClearsEveryRing() {
        let state = CallUiState()
        state.setParticipants([tile("local", isLocal: true), tile("peer-a")])
        let monitor = GroupSpeakingMonitor(ui: state)
        let loud = CallAudioLevels(local: nil, remoteByPeerId: ["a": 0.3])
        monitor.ingest(loud, nowMs: 0)
        monitor.ingest(loud, nowMs: speakingRingOnMs())
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
