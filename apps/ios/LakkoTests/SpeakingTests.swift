// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/group-call-grid.md` sections 5.2 and 7.3: who is speaking. The
/// same list as the web `speaking.test.ts`, with the same numbers.
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

    func testTheNumbersAreTheWebClients() {
        XCTAssertEqual(Speaking.level, 0.02)
        XCTAssertEqual(Speaking.speechOverNoise, 3)
        XCTAssertEqual(Speaking.levelMin, 0.005)
        XCTAssertEqual(Speaking.levelMax, 0.05)
        XCTAssertEqual(Speaking.noiseRisePerSecond, 1.1)
        XCTAssertEqual(Speaking.noiseFallPerSample, 0.5)
        XCTAssertEqual(Speaking.noiseEvidenceMin, 1e-4)
        XCTAssertEqual(Speaking.ringOnMs, 200)
        XCTAssertEqual(Speaking.ringOffMs, 800)
        XCTAssertEqual(Speaking.floorOnsetMs, 1_500)
    }

    // MARK: - The ring

    func testTheRingComesOnAfterAShortSoundAndGoesOffAfterAPause() {
        var t = SpeakingTracker()
        t.sample("a", level: loud, nowMs: 0)
        XCTAssertFalse(t.snapshot(candidates: order, nowMs: 0).speakingIds.contains("a"))
        t.sample("a", level: loud, nowMs: Speaking.ringOnMs)
        XCTAssertTrue(
            t.snapshot(candidates: order, nowMs: Speaking.ringOnMs).speakingIds.contains("a"))
        t.sample("a", level: 0, nowMs: Speaking.ringOnMs + 100)
        XCTAssertTrue(
            t.snapshot(candidates: order, nowMs: Speaking.ringOnMs + 100).speakingIds.contains("a"))
        let off = Speaking.ringOnMs + 100 + Speaking.ringOffMs
        t.sample("a", level: 0, nowMs: off)
        XCTAssertFalse(t.snapshot(candidates: order, nowMs: off).speakingIds.contains("a"))
    }

    func testTheRingIsOnForEveryoneWhoIsSpeakingAtOnce() {
        var t = SpeakingTracker()
        feed(&t, "a", loud, 0, 500)
        feed(&t, "b", loud, 0, 500)
        XCTAssertEqual(t.snapshot(candidates: order, nowMs: 500).speakingIds, ["a", "b"])
    }

    /// A level that is not a number is silence, never sound.
    func testANonsenseLevelIsSilence() {
        var t = SpeakingTracker()
        feed(&t, "a", .nan, 0, 600)
        XCTAssertTrue(t.snapshot(candidates: order, nowMs: 600).speakingIds.isEmpty)
        feed(&t, "a", Speaking.level / 2, 700, 1_300)
        XCTAssertTrue(t.snapshot(candidates: order, nowMs: 1_300).speakingIds.isEmpty)
    }

    // MARK: - The floor

    func testTheFloorIsNotTakenByACough() {
        var t = SpeakingTracker()
        feed(&t, "a", loud, 0, 600)
        XCTAssertNil(t.snapshot(candidates: order, nowMs: 600).activeSpeakerId)
    }

    func testTheFloorGoesToSomeoneWhoKeepsSpeakingAndIsRememberedAfterTheyStop() {
        var t = SpeakingTracker()
        feed(&t, "a", loud, 0, Speaking.floorOnsetMs)
        let during = t.snapshot(candidates: order, nowMs: Speaking.floorOnsetMs)
        XCTAssertEqual(during.activeSpeakerId, "a")
        let end = Speaking.floorOnsetMs + 100 + Speaking.ringOffMs
        feed(&t, "a", 0, Speaking.floorOnsetMs + 100, end)
        let after = t.snapshot(candidates: order, nowMs: end)
        XCTAssertNil(after.activeSpeakerId)
        XCTAssertEqual(after.lastSpokeAtMs["a"], Speaking.floorOnsetMs)
    }

    func testTheFloorStaysWithWhoeverHoldsItWhileASecondPersonTalksOverThem() {
        var t = SpeakingTracker()
        feed(&t, "b", loud, 0, Speaking.floorOnsetMs)
        XCTAssertEqual(t.snapshot(candidates: order, nowMs: Speaking.floorOnsetMs).activeSpeakerId, "b")
        // `a` is earlier in the roster and now qualifies too; `b` keeps the floor.
        let end = 2 * Speaking.floorOnsetMs + 200
        feed(&t, "a", loud, Speaking.floorOnsetMs, end)
        feed(&t, "b", loud, Speaking.floorOnsetMs, end)
        XCTAssertEqual(t.snapshot(candidates: order, nowMs: end).activeSpeakerId, "b")
    }

    func testTheRosterOrderBreaksATie() {
        var t = SpeakingTracker()
        feed(&t, "b", loud, 0, Speaking.floorOnsetMs)
        feed(&t, "a", loud, 0, Speaking.floorOnsetMs)
        XCTAssertEqual(t.snapshot(candidates: order, nowMs: Speaking.floorOnsetMs).activeSpeakerId, "a")
    }

    /// The onset is measured on the speech itself. The time the ring stays
    /// on after the last sound does not count, or a 0.8-second sound would
    /// grow into a 1.5-second one by standing still.
    func testTheRingsTailDoesNotCountTowardsTheOnset() {
        var t = SpeakingTracker()
        feed(&t, "a", loud, 0, 800)
        // Silence, sampled while the ring is still on (it goes off 800 ms
        // after the last sound).
        feed(&t, "a", 0, 900, 1_600)
        let snap = t.snapshot(candidates: order, nowMs: 1_600)
        XCTAssertTrue(snap.speakingIds.contains("a"), "the ring is still on")
        XCTAssertNil(snap.activeSpeakerId, "0.8 s of sound is not 1.5 s of speech")
    }

    /// The floor exists for the large tile, which never shows the own tile
    /// while anyone else is present: this device's own person is sampled for
    /// its ring and is not a candidate.
    func testSomeoneWhoIsNotACandidateNeverTakesTheFloor() {
        var t = SpeakingTracker()
        feed(&t, "local", loud, 0, 2 * Speaking.floorOnsetMs)
        let snap = t.snapshot(candidates: ["a", "b"], nowMs: 2 * Speaking.floorOnsetMs)
        XCTAssertEqual(snap.speakingIds, ["local"], "the ring is for everyone sampled")
        XCTAssertNil(snap.activeSpeakerId)
        XCTAssertNil(snap.lastSpokeAtMs["local"])
    }

    /// A floor held by the own microphone kept everyone else from taking it.
    func testACandidateTakesTheFloorWhileTheOwnMicrophoneIsLoud() {
        var t = SpeakingTracker()
        let end = 2 * Speaking.floorOnsetMs
        feed(&t, "local", loud, 0, end)
        feed(&t, "b", loud, Speaking.floorOnsetMs - 200, end)
        XCTAssertEqual(t.snapshot(candidates: ["a", "b"], nowMs: end).activeSpeakerId, "b")
    }

    /// A holder who stops being a candidate gives the floor up at once.
    func testAHolderWhoIsNoLongerACandidateLosesTheFloor() {
        var t = SpeakingTracker()
        feed(&t, "a", loud, 0, Speaking.floorOnsetMs)
        XCTAssertEqual(
            t.snapshot(candidates: order, nowMs: Speaking.floorOnsetMs).activeSpeakerId, "a")
        XCTAssertNil(
            t.snapshot(candidates: ["b"], nowMs: Speaking.floorOnsetMs + 100).activeSpeakerId)
    }

    /// A muted person is never speaking, whatever a level reading says.
    func testSilencingTurnsTheRingOffAtOnceAndGivesUpTheFloor() {
        var t = SpeakingTracker()
        feed(&t, "a", loud, 0, Speaking.floorOnsetMs)
        XCTAssertEqual(
            t.snapshot(candidates: order, nowMs: Speaking.floorOnsetMs).activeSpeakerId, "a")
        t.silence("a")
        let snap = t.snapshot(candidates: order, nowMs: Speaking.floorOnsetMs + 100)
        XCTAssertTrue(snap.speakingIds.isEmpty)
        XCTAssertNil(snap.activeSpeakerId)
        // Sound has to last all over again.
        t.sample("a", level: loud, nowMs: Speaking.floorOnsetMs + 200)
        XCTAssertTrue(
            t.snapshot(candidates: order, nowMs: Speaking.floorOnsetMs + 200).speakingIds.isEmpty)
    }

    func testSomeoneWhoLeftTheCallIsForgotten() {
        var t = SpeakingTracker()
        feed(&t, "a", loud, 0, Speaking.floorOnsetMs)
        _ = t.snapshot(candidates: order, nowMs: Speaking.floorOnsetMs)
        t.keepOnly(["b"])
        let snap = t.snapshot(candidates: ["b"], nowMs: Speaking.floorOnsetMs + 100)
        XCTAssertNil(snap.activeSpeakerId)
        XCTAssertNil(snap.lastSpokeAtMs["a"])
        XCTAssertTrue(snap.speakingIds.isEmpty)
    }
}

/// What counts as sound: a level well above the person's own noise floor
/// (``NoiseGate``). The same list as the web `speaking.test.ts`.
final class NoiseGateTests: XCTestCase {

    /// Feed one level every `step` ms for `ms`, starting at `from`.
    @discardableResult
    private func hold(
        _ gate: inout NoiseGate, _ level: Double, from: UInt64, ms: UInt64, step: UInt64 = 100
    ) -> UInt64 {
        var now = from
        while now < from + ms {
            gate.update(level, nowMs: now)
            now += step
        }
        return now
    }

    func testItStartsAtTheFixedLevelBeforeAnythingIsKnown() {
        XCTAssertEqual(NoiseGate().threshold, Speaking.level, accuracy: 1e-9)
        XCTAssertEqual(SpeakingTracker().threshold(of: "nobody"), Speaking.level)
    }

    func testItFollowsAQuietRoomDownToABound() {
        var gate = NoiseGate()
        hold(&gate, 0.002, from: 0, ms: 1_000)
        XCTAssertEqual(gate.threshold, 0.002 * Speaking.speechOverNoise, accuracy: 1e-9)
        hold(&gate, 0.0002, from: 1_000, ms: 2_000)
        XCTAssertEqual(gate.threshold, Speaking.levelMin)
    }

    func testOneTornReadingDoesNotEmptyTheFloor() {
        var gate = NoiseGate()
        hold(&gate, 0.006, from: 0, ms: 1_000)
        let before = gate.threshold
        gate.update(0.0002, nowMs: 1_000)
        XCTAssertEqual(gate.threshold, before / 2, accuracy: 1e-9)
    }

    func testItLearnsNothingFromDigitalSilenceOrFromAReadingThatIsNotANumber() {
        var gate = NoiseGate()
        hold(&gate, 0.01, from: 0, ms: 20_000)
        let before = gate.threshold
        hold(&gate, 0, from: 20_000, ms: 5_000)
        hold(&gate, Speaking.noiseEvidenceMin / 2, from: 25_000, ms: 5_000)
        gate.update(.nan, nowMs: 30_000)
        gate.update(.infinity, nowMs: 30_100)
        XCTAssertEqual(gate.threshold, before)
    }

    func testItClimbsSlowlyTowardsSteadySoundAndNeverPastTheBoundOrTheSoundItself() {
        var gate = NoiseGate()
        // Three seconds of speech barely move it.
        hold(&gate, 0.2, from: 0, ms: 3_000)
        XCTAssertLessThan(gate.threshold, Speaking.level * 1.4)
        // A minute of the same steady sound stops at the bound.
        hold(&gate, 0.2, from: 3_000, ms: 60_000)
        XCTAssertEqual(gate.threshold, Speaking.levelMax)
        // A steady hum settles the floor at the hum, not above it.
        var hum = NoiseGate()
        hold(&hum, 0.01, from: 0, ms: 60_000)
        XCTAssertEqual(hum.threshold, 0.01 * Speaking.speechOverNoise, accuracy: 1e-9)
    }

    func testALongGapBetweenReadingsIsNotALongRise() {
        var gate = NoiseGate()
        gate.update(0.2, nowMs: 0)
        gate.update(0.2, nowMs: 3_600_000)
        XCTAssertLessThan(gate.threshold, Speaking.level * 1.2)
    }

    /// A clock that steps back counts as no time passed, never as a fall.
    func testAClockThatGoesBackChangesNothing() {
        var gate = NoiseGate()
        gate.update(0.2, nowMs: 5_000)
        let before = gate.threshold
        gate.update(0.2, nowMs: 1_000)
        XCTAssertEqual(gate.threshold, before)
    }

    /// The rise is a rule per second, so it is the same whether readings
    /// come every 100 ms (the web client) or every 150 ms (this one).
    func testTheRiseDoesNotDependOnHowOftenALevelIsRead() {
        var web = NoiseGate()
        var ios = NoiseGate()
        hold(&web, 0.2, from: 0, ms: 3_001, step: 100)
        hold(&ios, 0.2, from: 0, ms: 3_001, step: GroupSpeakingMonitor.sampleIntervalMs)
        XCTAssertEqual(web.threshold, ios.threshold, accuracy: 1e-9)
    }

    /// The fall is a rule per reading: a floor well above the room halves at
    /// every reading, whatever the time between two of them.
    /// The fall is per unit of time, so this client's 150 ms readings and
    /// the web client's 100 ms readings let the floor fall alike.
    func testTheFallIsPerUnitOfTime() {
        var gate = NoiseGate()
        // Long enough for the floor to reach the level itself.
        hold(&gate, 0.016, from: 0, ms: 20_000)
        let learned = gate.threshold
        XCTAssertEqual(learned, 0.048, accuracy: 1e-9)
        // 100 ms after the last reading: one web reading, a half.
        gate.update(0.001, nowMs: 20_000)
        XCTAssertEqual(gate.threshold, learned / 2, accuracy: 1e-9)
        // 150 ms later: one and a half web readings.
        gate.update(0.001, nowMs: 20_000 + GroupSpeakingMonitor.sampleIntervalMs)
        XCTAssertEqual(gate.threshold, learned / 2 * pow(0.5, 1.5), accuracy: 1e-9)
    }

    /// Two readings at one instant: no time passed, nothing falls.
    func testNoTimeNoFall() {
        var gate = NoiseGate()
        hold(&gate, 0.016, from: 0, ms: 20_000)
        gate.update(0.001, nowMs: 20_000)
        let after = gate.threshold
        gate.update(0.001, nowMs: 20_000)
        XCTAssertEqual(gate.threshold, after, accuracy: 1e-9)
    }
}

/// One scale for a loud room and a quiet microphone: the tracker over the
/// noise gate. The same list as the web `speaking.test.ts`.
final class SpeakingOverNoiseTests: XCTestCase {

    private let order = ["a", "b"]

    /// Speech as a call delivers it: syllables around `level`, never steady.
    private func talk(
        _ t: inout SpeakingTracker, _ id: String, _ level: Double, from: UInt64, ms: UInt64
    ) -> UInt64 {
        var now = from
        var i = 0
        while now < from + ms {
            t.sample(id, level: level * (i % 3 == 0 ? 0.6 : 1), nowMs: now)
            now += 100
            i += 1
        }
        return now
    }

    private func speaking(_ t: inout SpeakingTracker, _ now: UInt64) -> Bool {
        t.snapshot(candidates: order, nowMs: now).speakingIds.contains("a")
    }

    /// The 2026-10-11 call: a phone whose speech peaked at 0.03 to 0.05.
    func testSpeechAtThreeHundredthsOverAQuietRoomDrawsTheRing() {
        var t = SpeakingTracker()
        var now: UInt64 = 0
        while now < 3_000 {
            t.sample("a", level: 0.002, nowMs: now)
            now += 100
        }
        XCTAssertFalse(speaking(&t, now))
        // 0.6 * 0.03 = 0.018 is under the old fixed 0.02 every third reading.
        now = talk(&t, "a", 0.03, from: now, ms: Speaking.floorOnsetMs + 500)
        let snap = t.snapshot(candidates: order, nowMs: now)
        XCTAssertTrue(snap.speakingIds.contains("a"))
        XCTAssertEqual(snap.activeSpeakerId, "a")
    }

    func testARoomThatIdlesBelowTheStartLevelDrawsNoRingAndSpeechOverItDoes() {
        var t = SpeakingTracker()
        var now: UInt64 = 0
        while now < 10_000 {
            t.sample("a", level: 0.015, nowMs: now)
            XCTAssertFalse(speaking(&t, now))
            now += 100
        }
        now = talk(&t, "a", 0.15, from: now, ms: 1_000)
        XCTAssertTrue(speaking(&t, now))
    }

    func testASteadyNoiseThatCameOnStopsCountingWithinSeconds() {
        var t = SpeakingTracker()
        var now: UInt64 = 0
        while now < 1_000 {
            t.sample("a", level: 0.03, nowMs: now)
            now += 100
        }
        XCTAssertTrue(speaking(&t, now))
        while now < 7_000 {
            t.sample("a", level: 0.03, nowMs: now)
            now += 100
        }
        XCTAssertFalse(speaking(&t, now))
    }

    func testWhatWasLearnedAboutTheRoomIsKeptAcrossAMute() {
        var t = SpeakingTracker()
        var now: UInt64 = 0
        while now < 3_000 {
            t.sample("a", level: 0.002, nowMs: now)
            now += 100
        }
        let learned = t.threshold(of: "a")
        XCTAssertLessThan(learned, Speaking.level)
        t.silence("a")
        XCTAssertEqual(t.threshold(of: "a"), learned)
    }

    /// A level that could not be read is silence for the ring and teaches
    /// the noise floor nothing.
    func testAMissingReadingIsSilenceAndTeachesNothing() {
        var t = SpeakingTracker()
        let loud = Speaking.level * 5
        var now: UInt64 = 0
        while now <= 500 {
            t.sample("a", level: loud, nowMs: now)
            now += 100
        }
        XCTAssertTrue(speaking(&t, 500))
        let before = t.threshold(of: "a")
        now = 600
        while now <= 600 + Speaking.ringOffMs {
            t.sample("a", level: nil, nowMs: now)
            now += 100
        }
        XCTAssertFalse(speaking(&t, 600 + Speaking.ringOffMs))
        XCTAssertEqual(t.threshold(of: "a"), before)
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
        for now in stride(from: UInt64(0), through: 2 * Speaking.floorOnsetMs, by: 150) {
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
        for now in stride(from: UInt64(0), through: 2 * Speaking.floorOnsetMs, by: 150) {
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
        monitor.ingest(own, nowMs: Speaking.ringOnMs)
        XCTAssertEqual(speaking(state), ["local"])
        state.setMic(false)
        monitor.ingest(own, nowMs: Speaking.ringOnMs + 150)
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
        monitor.ingest(loud, nowMs: Speaking.ringOnMs)
        XCTAssertEqual(speaking(state), ["peer-a"])
        let gone = CallAudioLevels(local: 0, remoteByPeerId: [:])
        monitor.ingest(gone, nowMs: Speaking.ringOnMs + 150)
        monitor.ingest(gone, nowMs: Speaking.ringOnMs + 150 + Speaking.ringOffMs)
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
        XCTAssertEqual(monitor.threshold(ofParticipant: "peer-a"), Speaking.level)
        XCTAssertEqual(monitor.threshold(ofParticipant: "local"), Speaking.level)
        // A quiet room, read: the threshold follows it down.
        let quiet = CallAudioLevels(local: 0.002, remoteByPeerId: ["a": 0.002])
        for now in stride(from: UInt64(3_150), through: 6_000, by: 150) {
            monitor.ingest(quiet, nowMs: now)
        }
        XCTAssertEqual(
            monitor.threshold(ofParticipant: "peer-a"), 0.002 * Speaking.speechOverNoise,
            accuracy: 1e-9)
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
