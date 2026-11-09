// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// A remote person's sound and picture on iOS (ADR-0070, note of
/// 2026-10-12): what stands between the receivers' statistics and the shared
/// rule (`AvAlign` of `lakko-ui-rules`).
///
/// Covered here: the statistics as the rule's inputs, which receiver is
/// which, which track a tile draws, the log lines, and the per-call book
/// driven both with a recording stand-in and through the real rule over the
/// binding. The rule's own behaviour is tested in Rust and is not restated.

// MARK: - A stand-in for the rule

/// Records what it is asked and answers what it is told to.
private final class RecordingRule: AvAlignProtocol, @unchecked Sendable {
    var readings: [(nowMs: UInt64, readings: AvReadings)] = []
    var replaced = 0
    var plan: AvPlan

    init(plan: AvPlan = RecordingRule.idle) { self.plan = plan }

    static func video(
        set: UInt32? = nil, asked: UInt32 = 0, skew: Int32? = nil,
        reason: AvVideoReason = .notFlowing
    ) -> AvVideoPlan {
        AvVideoPlan(setMs: set, askedMs: asked, skewMs: skew, reason: reason)
    }

    static let idle = AvPlan(
        audio: AvAudioPlan(setMs: nil, askedMs: 0, reason: .free),
        main: video(), middle: video(), light: video())

    func read(nowMs: UInt64, readings: AvReadings) -> AvPlan {
        self.readings.append((nowMs, readings))
        return plan
    }

    func receiversReplaced() { replaced += 1 }

    func capability() -> AvCapability { .full }
}

private func sample(_ packets: Int64, delay: Double, emitted: Int64, minimum: Double = 0)
    -> AvInboundSample
{
    AvInboundSample(
        packetsReceived: packets, jitterBufferDelay: delay, jitterBufferEmittedCount: emitted,
        jitterBufferMinimumDelay: minimum)
}

// MARK: - Which receiver is which

final class AvSlotTests: XCTestCase {

    func testAudioUnderThePersonsOwnStreamIsTheirAudio() {
        XCTAssertEqual(AvSlot.of(kind: "audio", role: .main), .audio)
    }

    func testVideoIsNamedByTheStreamItArrivedUnder() {
        XCTAssertEqual(AvSlot.of(kind: "video", role: .main), .main)
        XCTAssertEqual(AvSlot.of(kind: "video", role: .middle), .middle)
        XCTAssertEqual(AvSlot.of(kind: "video", role: .light), .light)
    }

    func testAudioUnderACompanionAndAnUnknownKindNameNoSlot() {
        XCTAssertNil(AvSlot.of(kind: "audio", role: .light))
        XCTAssertNil(AvSlot.of(kind: "audio", role: .middle))
        XCTAssertNil(AvSlot.of(kind: "unknown", role: .main))
    }
}

// MARK: - Statistics as inputs

final class AvAlignStatsTests: XCTestCase {

    private func entry(_ type: String, _ values: [String: NSObject]) -> CallStatEntry {
        CallStatEntry(type: type, values: values)
    }

    func testInboundEntriesAreKeyedByTheirTrack() {
        let samples = AvAlignStats.inboundSamples([
            entry("inbound-rtp", [
                "kind": "audio" as NSString, "trackIdentifier": "a1" as NSString,
                "packetsReceived": NSNumber(value: 100),
                "jitterBufferDelay": NSNumber(value: 12.5),
                "jitterBufferEmittedCount": NSNumber(value: 48000),
                "jitterBufferMinimumDelay": NSNumber(value: 6.25),
            ]),
            entry("inbound-rtp", [
                "kind": "video" as NSString, "trackIdentifier": "v1" as NSString,
                "packetsReceived": NSNumber(value: 7),
            ]),
            // Not a receiver, and a receiver that names no track.
            entry("outbound-rtp", ["trackIdentifier": "x" as NSString]),
            entry("inbound-rtp", ["packetsReceived": NSNumber(value: 3)]),
        ])
        XCTAssertEqual(samples.count, 2)
        XCTAssertEqual(samples["a1"], sample(100, delay: 12.5, emitted: 48000, minimum: 6.25))
        XCTAssertEqual(samples["v1"], sample(7, delay: 0, emitted: 0))
    }

    func testAFigureThatIsNotANumberOrIsNegativeReadsAsZero() {
        let samples = AvAlignStats.inboundSamples([
            entry("inbound-rtp", [
                "trackIdentifier": "a1" as NSString,
                "packetsReceived": NSNumber(value: -4),
                "jitterBufferDelay": NSNumber(value: Double.nan),
                "jitterBufferEmittedCount": "many" as NSString,
                "jitterBufferMinimumDelay": NSNumber(value: -Double.infinity),
            ])
        ])
        XCTAssertEqual(samples["a1"], AvInboundSample())
    }

    func testAnIntervalIsTheDifferenceOfTwoSamples() {
        let input = AvAlignStats.trackInput(
            prev: sample(100, delay: 10, emitted: 1000, minimum: 4),
            cur: sample(150, delay: 10.42, emitted: 1001, minimum: 4.1))
        XCTAssertEqual(input, AvTrackInput(flowing: true, delayMs: 420, networkMs: 100))
    }

    func testTheFirstSampleIsMeasuredFromZero() {
        let input = AvAlignStats.trackInput(prev: nil, cur: sample(10, delay: 3, emitted: 10))
        XCTAssertEqual(input, AvTrackInput(flowing: true, delayMs: 300, networkMs: nil))
    }

    func testCountersThatWentBackwardsAreMeasuredFromZero() {
        let input = AvAlignStats.trackInput(
            prev: sample(900, delay: 90, emitted: 900),
            cur: sample(10, delay: 2, emitted: 10))
        XCTAssertEqual(input, AvTrackInput(flowing: true, delayMs: 200, networkMs: nil))
    }

    func testNothingArrivedAndNothingEmittedIsNotFlowingAndUnmeasured() {
        let same = sample(100, delay: 10, emitted: 1000, minimum: 4)
        XCTAssertEqual(
            AvAlignStats.trackInput(prev: same, cur: same),
            AvTrackInput(flowing: false, delayMs: nil, networkMs: nil))
    }

    func testPacketsWithNothingEmittedFlowButAreUnmeasured() {
        let input = AvAlignStats.trackInput(
            prev: sample(100, delay: 10, emitted: 1000),
            cur: sample(120, delay: 10, emitted: 1000))
        XCTAssertEqual(input, AvTrackInput(flowing: true, delayMs: nil, networkMs: nil))
    }

    func testAReceiverThatReportedNothingHasNoInput() {
        XCTAssertEqual(
            AvAlignStats.trackInput(prev: sample(1, delay: 1, emitted: 1), cur: nil), .none)
    }

    func testTheReadingIntervalIsTheRulesOwnWhenATimerCanRunAtIt() {
        XCTAssertEqual(AvAlignStats.readingIntervalMs(1000), 1000)
        XCTAssertEqual(AvAlignStats.readingIntervalMs(2000), 2000)
        XCTAssertEqual(AvAlignStats.readingIntervalMs(0), 1000)
        XCTAssertEqual(AvAlignStats.readingIntervalMs(60_000), 1000)
        // Through the boundary: what the shared rule says today.
        XCTAssertEqual(AvAlignStats.readingIntervalMs(), avAlignReadingMs())
    }

    // MARK: Which track a tile draws

    private func facts(
        hasTile: Bool = true, wanted: TileVideoQuality, given: TileVideoQuality? = nil,
        hasMiddle: Bool = true, hasLight: Bool = true, lightOnTop: Bool = false
    ) -> AvShownFacts {
        AvShownFacts(
            hasTile: hasTile, wanted: wanted, given: given, hasMiddle: hasMiddle,
            hasLight: hasLight, lightOnTop: lightOnTop)
    }

    func testNoTileShowsNothing() {
        XCTAssertNil(AvAlignStats.shownSlot(facts(hasTile: false, wanted: .main)))
    }

    func testATileShowsWhatItWants() {
        XCTAssertEqual(AvAlignStats.shownSlot(facts(wanted: .main)), .main)
        XCTAssertEqual(AvAlignStats.shownSlot(facts(wanted: .middle)), .middle)
        XCTAssertEqual(AvAlignStats.shownSlot(facts(wanted: .light)), .light)
    }

    func testATileShowsTheLowerOfWantedAndGiven() {
        XCTAssertEqual(AvAlignStats.shownSlot(facts(wanted: .main, given: .middle)), .middle)
        XCTAssertEqual(AvAlignStats.shownSlot(facts(wanted: .main, given: .light)), .light)
        XCTAssertEqual(AvAlignStats.shownSlot(facts(wanted: .middle, given: .main)), .middle)
    }

    func testTheMiddlePictureIsTheMainTrackWithoutAMiddleOne() {
        XCTAssertEqual(AvAlignStats.shownSlot(facts(wanted: .middle, hasMiddle: false)), .main)
    }

    func testTheLightCopyIsTheOneDrawnWhileTheTileShowsIt() {
        XCTAssertEqual(AvAlignStats.shownSlot(facts(wanted: .main, lightOnTop: true)), .light)
        // A tile cannot show a light copy the person does not send.
        XCTAssertEqual(
            AvAlignStats.shownSlot(facts(wanted: .main, hasLight: false, lightOnTop: true)),
            .main)
    }
}

// MARK: - The log lines

final class AvAlignLogTests: XCTestCase {

    func testOnePersonInOneLine() {
        let line = AvAlignLog.describeSync(
            "call media interval", peerId: "p7",
            AvPeerSync(
                audioDelayMs: 420, audioAskedMs: 0, audioReason: "no-setter",
                video: [
                    AvVideoSync(
                        slot: .main, delayMs: 431, askedMs: 0, skewMs: 11,
                        reason: "engine-synced"),
                    AvVideoSync(
                        slot: .light, delayMs: 290, askedMs: 0, skewMs: -130,
                        reason: "no-setter"),
                ]))
        XCTAssertEqual(
            line,
            "call media interval sync peer=p7 audio=420/ask=0/no-setter"
                + " main=431/ask=0/skew=11/engine-synced"
                + " light=290/ask=0/skew=-130/no-setter")
    }

    func testAFigureThatWasNotMeasuredIsADash() {
        let line = AvAlignLog.describeSync(
            "call media interval", peerId: "p7",
            AvPeerSync(
                audioDelayMs: nil, audioAskedMs: 0, audioReason: "not-flowing",
                video: [
                    AvVideoSync(
                        slot: .middle, delayMs: nil, askedMs: 0, skewMs: nil,
                        reason: "not-flowing")
                ]))
        XCTAssertEqual(
            line,
            "call media interval sync peer=p7 audio=-/ask=0/not-flowing"
                + " middle=-/ask=0/skew=-/not-flowing")
    }

    func testTheCapabilityLineUsesTheRulesOwnWord() {
        XCTAssertEqual(
            AvAlignLog.describeCapability(
                "call media", audioSettable: false, videoSettable: false, intervalMs: 1000),
            "call media sync capability=observe-only audio_setter=no video_setter=no"
                + " interval_ms=1000")
        XCTAssertEqual(
            AvAlignLog.describeCapability(
                "call media", audioSettable: true, videoSettable: true, intervalMs: 1000),
            "call media sync capability=full audio_setter=yes video_setter=yes"
                + " interval_ms=1000")
    }

    func testTheLoadedWebRTCIsNotABinaryTheSetterWasVerifiedFor() {
        // The drift guard (ADR-0070, note of 2026-10-12). The setter calls
        // the engine by vtable slot; the slot and the calling convention
        // were read from the machine code of stasel/WebRTC 125.0.0. Since
        // 2026-11-09 the app links webrtc-sdk 125.6422.07 (ADR-0088's capture
        // hook), whose slices were not read, so the shim must refuse to set
        // (observe-only). This test pins that state: it fails the day the
        // evidence in `apps/shared/sframe-bridge/receiver-delay.h` is redone
        // and the new identities are pinned in `SFrameAttach.mm`, and then
        // it is rewritten to pin those.
        XCTAssertEqual(LakkoReceiverDelay.verifiedPackageVersion, "stasel/WebRTC 125.0.0")
        let loaded = LakkoReceiverDelay.loadedEngineIdentity
        XCTAssertNotNil(loaded, "the WebRTC image has no readable identity")
        XCTAssertFalse(
            LakkoReceiverDelay.verifiedEngineIdentities.contains(loaded ?? ""),
            "WebRTC image \(loaded ?? "-") is a verified slice; rewrite this test")
        XCTAssertEqual(
            Set(LakkoReceiverDelay.verifiedEngineIdentities),
            [
                "4C4C4485-5555-3144-A186-7570B405EE6A",
                "4C4C44CF-5555-3144-A1B7-95616B307988",
                "4C4C44D7-5555-3144-A1AF-2ABA8DEDBD33",
            ])
        XCTAssertFalse(LakkoReceiverDelay.isAvailable())
    }

    func testBothKindsHaveTheShimsOneAnswer() {
        // Pinned: a change here is a change of what iOS does on a call.
        let available = LakkoReceiverDelay.isAvailable()
        XCTAssertEqual(AvReceiverDelay.audioSettable, available)
        XCTAssertEqual(AvReceiverDelay.videoSettable, available)
    }

    func testOnlyAnOkSetTook() {
        XCTAssertTrue(AvReceiverDelay.took(.OK))
        XCTAssertFalse(AvReceiverDelay.took(.notAvailable))
        XCTAssertFalse(AvReceiverDelay.took(.probeFailed))
        XCTAssertEqual(AvReceiverDelay.name(.OK), "ok")
        XCTAssertEqual(AvReceiverDelay.name(.notAvailable), "not-available")
        XCTAssertEqual(AvReceiverDelay.name(.probeFailed), "probe-failed")
    }

    func testANilReceiverIsNeverCalled() {
        XCTAssertEqual(
            LakkoReceiverDelay.setMinimumPlayoutDelayMs(120, forReceiver: nil),
            .notAvailable)
    }

    func testAWriteThatDidNotTakeSaysWhy() {
        XCTAssertEqual(
            AvAlignLog.describeWriteFailed(
                "call media", peerId: "p7", slot: .light, capability: "observe-only",
                why: "probe-failed"),
            "call media sync peer=p7 light target not taken (probe-failed);"
                + " continuing capability=observe-only")
        XCTAssertEqual(
            AvAlignLog.describeWriteFailed(
                "call media", peerId: "p7", slot: .audio, capability: "video-only"),
            "call media sync peer=p7 audio target not taken;"
                + " continuing capability=video-only")
    }
}

// MARK: - One call, with a stand-in rule

final class CallAvAlignTests: XCTestCase {

    private var rules: [RecordingRule] = []
    private var built: [(audio: Bool, video: Bool)] = []

    override func setUp() {
        super.setUp()
        rules = []
        built = []
    }

    private func book(audio: Bool = true, video: Bool = true) -> CallAvAlign {
        CallAvAlign(audioSettable: audio, videoSettable: video) { [unowned self] a, v in
            let rule = RecordingRule()
            self.rules.append(rule)
            self.built.append((a, v))
            return rule
        }
    }

    func testAPersonGetsARuleWithTheirFirstReceiver() {
        let book = self.book()
        XCTAssertEqual(book.peerIds, [])
        book.setReceiver(peerId: "p1", slot: .audio, trackId: "a1")
        book.setReceiver(peerId: "p1", slot: .main, trackId: "v1")
        book.setReceiver(peerId: "p2", slot: .audio, trackId: "a2")
        XCTAssertEqual(book.peerIds, ["p1", "p2"])
        XCTAssertEqual(rules.count, 2)
        XCTAssertTrue(book.has("p1", .main))
        XCTAssertFalse(book.has("p1", .light))
        XCTAssertFalse(book.has("p3", .audio))
    }

    func testAReceiverWithNoPeerOrNoTrackIsNobody() {
        let book = self.book()
        book.setReceiver(peerId: "", slot: .audio, trackId: "a1")
        book.setReceiver(peerId: "p1", slot: .audio, trackId: "")
        XCTAssertEqual(book.peerIds, [])
    }

    func testReadingsAreTheIntervalOfEachReceiverAndWhatIsShown() {
        let book = self.book()
        book.setReceiver(peerId: "p1", slot: .audio, trackId: "a1")
        book.setReceiver(peerId: "p1", slot: .main, trackId: "v1")
        book.setReceiver(peerId: "p1", slot: .light, trackId: "l1")
        _ = book.read(
            nowMs: 1000,
            samples: [
                "a1": sample(50, delay: 20, emitted: 50, minimum: 5),
                "v1": sample(30, delay: 6, emitted: 30),
                "l1": sample(30, delay: 3, emitted: 30),
            ],
            shownOf: { _ in .light })
        _ = book.read(
            nowMs: 2000,
            samples: [
                "a1": sample(100, delay: 40, emitted: 100, minimum: 10),
                "v1": sample(60, delay: 18.6, emitted: 60),
                // The light track reported nothing this time.
            ],
            shownOf: { _ in .main })
        let rule = rules[0]
        XCTAssertEqual(rule.readings.map(\.nowMs), [1000, 2000])
        XCTAssertEqual(
            rule.readings[0].readings,
            AvReadings(
                audio: AvAudioReading(flowing: true, delayMs: 400, networkMs: 100),
                main: AvVideoReading(
                    flowing: true, shown: false, engineSynced: true, delayMs: 200,
                    networkMs: nil),
                middle: AvVideoReading(
                    flowing: false, shown: false, engineSynced: false, delayMs: nil,
                    networkMs: nil),
                light: AvVideoReading(
                    flowing: true, shown: true, engineSynced: false, delayMs: 100,
                    networkMs: nil)))
        XCTAssertEqual(
            rule.readings[1].readings,
            AvReadings(
                audio: AvAudioReading(flowing: true, delayMs: 400, networkMs: 100),
                main: AvVideoReading(
                    flowing: true, shown: true, engineSynced: true, delayMs: 420,
                    networkMs: nil),
                middle: AvVideoReading(
                    flowing: false, shown: false, engineSynced: false, delayMs: nil,
                    networkMs: nil),
                light: AvVideoReading(
                    flowing: false, shown: false, engineSynced: false, delayMs: nil,
                    networkMs: nil)))
    }

    func testATrackThePersonDoesNotHaveIsNeverShown() {
        let book = self.book()
        book.setReceiver(peerId: "p1", slot: .audio, trackId: "a1")
        _ = book.read(nowMs: 1000, samples: [:], shownOf: { _ in .middle })
        XCTAssertFalse(rules[0].readings[0].readings.middle.shown)
    }

    func testTheRulesAnswersComeBackAsWritesToTheRightTracks() {
        let book = self.book()
        book.setReceiver(peerId: "p1", slot: .audio, trackId: "a1")
        book.setReceiver(peerId: "p1", slot: .main, trackId: "v1")
        book.setReceiver(peerId: "p1", slot: .light, trackId: "l1")
        rules[0].plan = AvPlan(
            audio: AvAudioPlan(setMs: 0, askedMs: 0, reason: .releasing),
            main: RecordingRule.video(reason: .engineSynced),
            // The person has no middle track: an answer for it goes nowhere.
            middle: RecordingRule.video(set: 300, asked: 300, reason: .started),
            light: RecordingRule.video(set: 420, asked: 420, skew: -130, reason: .raising))
        let writes = book.read(nowMs: 1000, samples: [:], shownOf: { _ in nil })
        XCTAssertEqual(
            writes,
            [
                CallAvAlign.Write(peerId: "p1", slot: .audio, trackId: "a1", setMs: 0),
                CallAvAlign.Write(peerId: "p1", slot: .light, trackId: "l1", setMs: 420),
            ])
    }

    func testTheLineNamesTheTracksThePersonHasMainFirst() {
        let book = self.book()
        book.setReceiver(peerId: "p1", slot: .light, trackId: "l1")
        book.setReceiver(peerId: "p1", slot: .audio, trackId: "a1")
        book.setReceiver(peerId: "p1", slot: .main, trackId: "v1")
        XCTAssertEqual(book.lines("call media interval"), [])
        rules[0].plan = AvPlan(
            audio: AvAudioPlan(setMs: nil, askedMs: 0, reason: .free),
            main: RecordingRule.video(skew: 20, reason: .engineSynced),
            middle: RecordingRule.video(),
            light: RecordingRule.video(asked: 400, skew: -100, reason: .raising))
        _ = book.read(
            nowMs: 1000,
            samples: [
                "a1": sample(50, delay: 20, emitted: 50),
                "v1": sample(30, delay: 12.6, emitted: 30),
                "l1": sample(30, delay: 9, emitted: 30),
            ],
            shownOf: { _ in .light })
        XCTAssertEqual(
            book.lines("call media interval"),
            [
                "call media interval sync peer=p1 audio=400/ask=0/free"
                    + " main=420/ask=0/skew=20/engine-synced"
                    + " light=300/ask=400/skew=-100/raising"
            ])
    }

    func testATrackThatReplacesOneUnderTheSameSlotIsToldToTheRule() {
        let book = self.book()
        book.setReceiver(peerId: "p1", slot: .light, trackId: "l1")
        book.setReceiver(peerId: "p1", slot: .light, trackId: "l1")
        XCTAssertEqual(rules[0].replaced, 0)
        _ = book.read(
            nowMs: 1000, samples: ["l2": sample(500, delay: 50, emitted: 500)],
            shownOf: { _ in nil })
        book.setReceiver(peerId: "p1", slot: .light, trackId: "l2")
        XCTAssertEqual(rules[0].replaced, 1)
        // The new track is measured from its own start.
        _ = book.read(
            nowMs: 2000, samples: ["l2": sample(510, delay: 51, emitted: 510)],
            shownOf: { _ in nil })
        XCTAssertEqual(rules[0].readings[1].readings.light.delayMs, 100)
    }

    func testEveryReceiverReplacedTellsEveryRuleAndStartsTheCountersAgain() {
        let book = self.book()
        book.setReceiver(peerId: "p1", slot: .audio, trackId: "a1")
        book.setReceiver(peerId: "p2", slot: .audio, trackId: "a2")
        _ = book.read(
            nowMs: 1000, samples: ["a1": sample(100, delay: 10, emitted: 100)],
            shownOf: { _ in nil })
        book.receiversReplaced()
        XCTAssertEqual(rules.map(\.replaced), [1, 1])
        _ = book.read(
            nowMs: 2000, samples: ["a1": sample(110, delay: 11, emitted: 110)],
            shownOf: { _ in nil })
        // From zero, not against the sample taken before.
        XCTAssertEqual(rules[0].readings[1].readings.audio.delayMs, 100)
    }

    func testAPersonWhoLeavesIsNoLongerReadOrLogged() {
        let book = self.book()
        book.setReceiver(peerId: "p1", slot: .audio, trackId: "a1")
        book.setReceiver(peerId: "p2", slot: .audio, trackId: "a2")
        _ = book.read(nowMs: 1000, samples: [:], shownOf: { _ in nil })
        book.removePeer("p1")
        _ = book.read(nowMs: 2000, samples: [:], shownOf: { _ in nil })
        XCTAssertEqual(book.peerIds, ["p2"])
        XCTAssertEqual(rules[0].readings.count, 1)
        XCTAssertEqual(rules[1].readings.count, 2)
        XCTAssertEqual(book.lines("x").count, 1)
        // Someone who joins again under the same id starts with a new rule.
        book.setReceiver(peerId: "p1", slot: .audio, trackId: "a9")
        XCTAssertEqual(rules.count, 3)
    }

    func testTheEndOfTheCallDropsEveryRule() {
        let book = self.book()
        book.setReceiver(peerId: "p1", slot: .audio, trackId: "a1")
        _ = book.read(nowMs: 1000, samples: [:], shownOf: { _ in nil })
        book.clear()
        XCTAssertEqual(book.peerIds, [])
        XCTAssertEqual(book.lines("x"), [])
        XCTAssertEqual(book.read(nowMs: 2000, samples: [:], shownOf: { _ in nil }), [])
        XCTAssertEqual(rules[0].readings.count, 1)
    }

    func testAWriteThatFailsIsReportedOnceAndTheRuleGoesOnWithoutThatSetter() {
        let book = self.book()
        book.setReceiver(peerId: "p1", slot: .audio, trackId: "a1")
        book.setReceiver(peerId: "p1", slot: .light, trackId: "l1")
        XCTAssertEqual(built.count, 1)
        XCTAssertEqual(book.writeFailed(peerId: "p1", slot: .light), "sound-waits-only")
        XCTAssertEqual(built.count, 2)
        XCTAssertTrue(built[1].audio)
        XCTAssertFalse(built[1].video)
        // Any video receiver of theirs: already known, said once.
        XCTAssertNil(book.writeFailed(peerId: "p1", slot: .main))
        XCTAssertEqual(built.count, 2)
        XCTAssertEqual(book.writeFailed(peerId: "p1", slot: .audio), "observe-only")
        XCTAssertFalse(built[2].audio)
        XCTAssertFalse(built[2].video)
        // The person keeps their receivers and is read by the new rule.
        _ = book.read(nowMs: 1000, samples: [:], shownOf: { _ in nil })
        XCTAssertEqual(rules[2].readings.count, 1)
        XCTAssertNil(book.writeFailed(peerId: "gone", slot: .audio))
    }

    func testAFailureOnOnePersonLeavesTheOthersAsTheyWere() {
        let book = self.book()
        book.setReceiver(peerId: "p1", slot: .light, trackId: "l1")
        book.setReceiver(peerId: "p2", slot: .light, trackId: "l2")
        _ = book.writeFailed(peerId: "p1", slot: .light)
        XCTAssertEqual(built.count, 3)
        XCTAssertEqual(book.writeFailed(peerId: "p2", slot: .light), "sound-waits-only")
    }
}

// MARK: - One call, through the shared rule

/// The real `AvAlign` over the binding. What is asserted is what follows
/// from the rule's documented contract, not its arithmetic.
final class CallAvAlignSharedRuleTests: XCTestCase {

    /// A light picture at 290 ms against a sound at 500 ms, with the main
    /// track at the sound's delay.
    private func drive(_ book: CallAvAlign, readings: Int) -> [CallAvAlign.Write] {
        book.setReceiver(peerId: "p1", slot: .audio, trackId: "a1")
        book.setReceiver(peerId: "p1", slot: .main, trackId: "v1")
        book.setReceiver(peerId: "p1", slot: .light, trackId: "l1")
        var writes: [CallAvAlign.Write] = []
        for i in 1...readings {
            let n = Int64(i) * 50
            writes += book.read(
                nowMs: UInt64(i) * 1000,
                samples: [
                    "a1": sample(n, delay: Double(n) * 0.5, emitted: n),
                    "v1": sample(n, delay: Double(n) * 0.5, emitted: n),
                    "l1": sample(n, delay: Double(n) * 0.29, emitted: n),
                ],
                shownOf: { _ in .light })
        }
        return writes
    }

    func testWithoutSettersTheRuleMeasuresTheSkewAndWritesNothing() {
        // The book of a process where the setter is not available.
        let book = CallAvAlign(audioSettable: false, videoSettable: false)
        let writes = drive(book, readings: 4)
        XCTAssertEqual(writes, [])
        XCTAssertEqual(
            book.lines("call media interval"),
            [
                "call media interval sync peer=p1 audio=500/ask=0/no-setter"
                    + " main=500/ask=0/skew=0/engine-synced"
                    + " light=290/ask=0/skew=-210/no-setter"
            ])
    }

    func testWithSettersAnEarlyPictureIsToldToWaitAndTheMainTrackIsNeverWritten() {
        let book = CallAvAlign(audioSettable: true, videoSettable: true)
        let writes = drive(book, readings: 4)
        let light = writes.filter { $0.slot == .light }
        XCTAssertFalse(light.isEmpty)
        XCTAssertTrue(light.allSatisfy { $0.trackId == "l1" && $0.setMs > 0 })
        XCTAssertFalse(writes.contains { $0.slot == .main })
    }
}

// MARK: - The tiles' side

@MainActor
final class AvAlignShownSourcesTests: XCTestCase {

    private final class FakeTile: LayerCounterSource, LayerShownSource {
        var layer: ShownLayer
        init(_ layer: ShownLayer) { self.layer = layer }
        func drain() -> LayerDisplayCounters { .zero }
        func counters() -> LayerDisplayCounters { .zero }
        func shown() -> ShownLayer { layer }
    }

    func testTheLedgerSaysWhetherATileOfAPersonShowsItsLightPicture() {
        let ledger = LayerDisplayLedger()
        XCTAssertFalse(ledger.lightShown(peerId: "p1"))
        let tile = FakeTile(.full)
        let token = ledger.register(peerId: "p1", source: tile)
        XCTAssertFalse(ledger.lightShown(peerId: "p1"))
        tile.layer = .light
        XCTAssertTrue(ledger.lightShown(peerId: "p1"))
        XCTAssertFalse(ledger.lightShown(peerId: "p2"))
        ledger.unregister(token)
        XCTAssertFalse(ledger.lightShown(peerId: "p1"))
    }

    func testTheWantsBookSaysWhetherAnyTileDrawsAPerson() {
        var wants = VideoWantsBook()
        wants.setPeers(["p1", "p2"])
        XCTAssertFalse(wants.hasTile(of: "p1"))
        let tile = NSObject()
        wants.setTile(ObjectIdentifier(tile), peerId: "p1", quality: .light)
        XCTAssertTrue(wants.hasTile(of: "p1"))
        XCTAssertFalse(wants.hasTile(of: "p2"))
        wants.setTile(ObjectIdentifier(tile), peerId: "p1", quality: nil)
        XCTAssertFalse(wants.hasTile(of: "p1"))
    }
}
