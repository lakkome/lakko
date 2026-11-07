// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
import WebRTC
@testable import Lakko

/// The group call's protocol with the SFU on iOS (ADR-0070, ADR-0071,
/// `docs/ux/group-call-grid.md` sections 6.2 and 7.6): the three camera
/// senders, which of them send, what this device asks for of each person and
/// what it shows of what it is given, and the pin control.
///
/// Everything here is decided without a device. Whether three encoders run
/// from one capturer, and what the SFU does with a frame, is not.

// MARK: - The camera's senders

@MainActor
final class GroupCallSendersTests: XCTestCase {

    func testThePlanIsMainLightMiddleEachUnderItsOwnStreamIndex() {
        let plan = CameraSender.plan(middleActive: true)
        XCTAssertEqual(plan.map(\.kind), [.main, .light, .middle])
        XCTAssertEqual(plan.map(\.streamIndex), [1, 3, 4])
        XCTAssertEqual(Set(plan.map(\.streamIndex)).count, 3)
        XCTAssertEqual(Set(plan.map(\.trackId)).count, 3)
        // The microphone keeps index 0, and 2 stays free for a shared screen.
        XCTAssertEqual(CameraSender.streamIndexAudio, 0)
        XCTAssertEqual(CameraSender.streamIndexScreenShare, 2)
    }

    func testTheSizesAndCeilingsAreTheWebClients() {
        let plan = CameraSender.plan(middleActive: true)
        XCTAssertEqual(
            plan[0].encoding,
            CameraEncodingSpec(
                scaleResolutionDownBy: nil, maxBitrateBps: nil, active: true,
                numTemporalLayers: 3))
        XCTAssertEqual(
            plan[1].encoding,
            CameraEncodingSpec(scaleResolutionDownBy: 4, maxBitrateBps: 150_000, active: true))
        XCTAssertEqual(
            plan[2].encoding,
            CameraEncodingSpec(scaleResolutionDownBy: 2, maxBitrateBps: 500_000, active: true))
    }

    /// The middle sender exists from the first offer and only its encoding
    /// follows the people count.
    func testTheMiddleSenderIsAlwaysPlannedAndOnlyItsEncodingIsSwitched() {
        let plan = CameraSender.plan(middleActive: false)
        XCTAssertEqual(plan.map(\.kind), [.main, .light, .middle])
        XCTAssertEqual(plan.map(\.encoding.active), [true, true, false])
    }

    /// The people-count rule is the shared one: from three people on.
    func testTheMiddleSenderIsWantedFromThreePeopleOn() {
        let rules = SharedTileQualityRules()
        XCTAssertFalse(rules.middleSenderWanted(people: 1))
        XCTAssertFalse(rules.middleSenderWanted(people: 2))
        XCTAssertTrue(rules.middleSenderWanted(people: 3))
        XCTAssertTrue(rules.middleSenderWanted(people: 10))
    }

    func testTheCopiesAreAddedLightFirst() {
        XCTAssertEqual(CameraCopyKind.addOrder, [.light, .middle])
        XCTAssertEqual(CameraCopyKind.light.senderKind, .light)
        XCTAssertEqual(CameraCopyKind.middle.senderKind, .middle)
    }

    func testATrackIdNamesItsSender() {
        for kind in CameraSenderKind.allCases {
            XCTAssertEqual(CameraSender.senderKind(ofTrackId: CameraSender.trackId(of: kind)), kind)
        }
        XCTAssertNil(CameraSender.senderKind(ofTrackId: "lakko-audio0"))
    }

    func testAnOfferNamesACopysMidOnlyOnceItHasOne() {
        XCTAssertEqual(CameraSender.mids(ofCopyMid: "3"), ["3"])
        XCTAssertEqual(CameraSender.mids(ofCopyMid: nil), [])
        XCTAssertEqual(CameraSender.mids(ofCopyMid: ""), [])
    }

    func testTheBindingsEncodingCarriesScaleCeilingAndActivityAndNoRid() {
        let light = CameraSender.rtcEncoding(CameraSender.copyEncoding(.light))
        XCTAssertNil(light.rid)
        XCTAssertEqual(light.scaleResolutionDownBy, NSNumber(value: 4.0))
        XCTAssertEqual(light.maxBitrateBps, NSNumber(value: 150_000))
        XCTAssertTrue(light.isActive)

        let middle = CameraSender.rtcEncoding(CameraSender.copyEncoding(.middle, active: false))
        XCTAssertNil(middle.rid)
        XCTAssertEqual(middle.scaleResolutionDownBy, NSNumber(value: 2.0))
        XCTAssertEqual(middle.maxBitrateBps, NSNumber(value: 500_000))
        XCTAssertFalse(middle.isActive)
    }

    /// The main sender is asked for three temporal layers, as the web
    /// client's `L1T3`; the copies are asked for none, as on web.
    func testOnlyTheMainSenderIsAskedForThreeTemporalLayers() {
        XCTAssertEqual(CameraSender.mainTemporalLayers, 3)
        for middleActive in [true, false] {
            let plan = CameraSender.plan(middleActive: middleActive)
            XCTAssertEqual(plan[0].kind, .main)
            XCTAssertEqual(plan[0].encoding.numTemporalLayers, 3)
            XCTAssertNil(plan[1].encoding.numTemporalLayers)
            XCTAssertNil(plan[2].encoding.numTemporalLayers)
        }
        let main = CameraSender.rtcEncoding(CameraSender.mainEncoding)
        XCTAssertEqual(main.numTemporalLayers, NSNumber(value: 3))
        XCTAssertNil(main.scaleResolutionDownBy)
        XCTAssertNil(main.maxBitrateBps)
        XCTAssertNil(
            CameraSender.rtcEncoding(CameraSender.copyEncoding(.light)).numTemporalLayers)
    }

    func testTheLineSaysWhatTheStackKeptOfTheLayers() {
        XCTAssertEqual(
            CameraSender.temporalLayersLine(asked: 3, kept: 3),
            "camera sender: asked for 3 temporal layers, the stack kept 3")
        XCTAssertEqual(
            CameraSender.temporalLayersLine(asked: 3, kept: 1),
            "camera sender: asked for 3 temporal layers, the stack kept 1 instead")
        XCTAssertEqual(
            CameraSender.temporalLayersLine(asked: 3, kept: nil),
            "camera sender: asked for 3 temporal layers, the stack reports none")
    }

    /// A copy refused with its encoding is asked for again without one and
    /// then sized through its sender; the second attempt is made only after
    /// the first was refused.
    func testACopyRefusedWithItsEncodingIsTriedPlainly() {
        var plainAsked = 0
        func plain(_ taken: Bool) -> Bool {
            plainAsked += 1
            return taken
        }
        XCTAssertEqual(
            CameraSender.copyAddStep(withEncodingTaken: true, plainTaken: plain(true)), .keep)
        XCTAssertEqual(plainAsked, 0, "a taken transceiver is not asked for twice")
        XCTAssertEqual(
            CameraSender.copyAddStep(withEncodingTaken: false, plainTaken: plain(true)),
            .configureWithParameters)
        XCTAssertEqual(
            CameraSender.copyAddStep(withEncodingTaken: false, plainTaken: plain(false)), .absent)
        XCTAssertEqual(plainAsked, 2)
    }

    /// A copy sized through its sender stays only when the sender holds the
    /// size, the ceiling and the activity asked for: never a full-size copy.
    func testACopyThatCouldNotBeSizedDoesNotStay() {
        let wanted = CameraSender.copyEncoding(.middle, active: false)
        XCTAssertTrue(CameraSender.copyIsSized(wanted: wanted, readBack: wanted))
        XCTAssertFalse(CameraSender.copyIsSized(wanted: wanted, readBack: nil))
        // The binding kept its own encoding: full size, no ceiling.
        XCTAssertFalse(
            CameraSender.copyIsSized(
                wanted: wanted,
                readBack: CameraEncodingSpec(
                    scaleResolutionDownBy: nil, maxBitrateBps: nil, active: true)))
        XCTAssertFalse(
            CameraSender.copyIsSized(
                wanted: wanted,
                readBack: CameraEncodingSpec(
                    scaleResolutionDownBy: 2, maxBitrateBps: nil, active: false)))
        XCTAssertFalse(
            CameraSender.copyIsSized(
                wanted: wanted,
                readBack: CameraEncodingSpec(
                    scaleResolutionDownBy: 4, maxBitrateBps: 500_000, active: false)))
        XCTAssertFalse(
            CameraSender.copyIsSized(
                wanted: wanted,
                readBack: CameraEncodingSpec(
                    scaleResolutionDownBy: 2, maxBitrateBps: 500_000, active: true)))
    }

    /// Writing a copy's encoding onto the binding's own and reading it back
    /// gives the same values: what the fallback compares.
    func testACopysEncodingSurvivesTheBindingsOwnObject() {
        let wanted = CameraSender.copyEncoding(.light)
        let encoding = RTCRtpEncodingParameters()
        CameraSender.apply(wanted, to: encoding)
        XCTAssertTrue(
            CameraSender.copyIsSized(wanted: wanted, readBack: CameraSender.spec(of: encoding)))
        XCTAssertNil(encoding.rid)
    }

    /// Both publish paths of the SFU session create the camera's senders
    /// through these two calls. This asks the real binding, on a peer
    /// connection that never negotiates: three senders, three tracks, each
    /// on a source of its own, and no media line named before a description
    /// is set.
    func testThePeerConnectionCreatesThreeOrdinarySenders() {
        let connection = WebRTCPeerConnection(iceServers: [])
        defer { connection.close() }
        XCTAssertTrue(connection.isReady)
        let source = WebRTCPeerConnection.newVideoSource()
        let copySources: [CameraCopyKind: RTCVideoSource] = [
            .light: WebRTCPeerConnection.newVideoSource(),
            .middle: WebRTCPeerConnection.newVideoSource(),
        ]

        connection.addLocalCameraTrack(source)
        connection.addCameraCopies(sources: { copySources[$0] }, middleActive: false)

        let main = connection.cameraSender(.main)
        let light = connection.cameraSender(.light)
        let middle = connection.cameraSender(.middle)
        XCTAssertNotNil(main)
        XCTAssertNotNil(light)
        XCTAssertNotNil(middle)
        XCTAssertFalse(main === light)
        XCTAssertFalse(light === middle)
        XCTAssertEqual(main?.track?.trackId, CameraSender.trackId(of: .main))
        XCTAssertEqual(light?.track?.trackId, CameraSender.trackId(of: .light))
        XCTAssertEqual(middle?.track?.trackId, CameraSender.trackId(of: .middle))
        // Each sender is on its own source: on one shared source the smallest
        // picture any encoder asked for went to all three. The binding hands
        // out a fresh wrapper for `track.source`, so this asks which track a
        // frame pushed into each source reaches.
        let tracks: [CameraSenderKind: RTCVideoTrack] = [
            .main: main?.track as? RTCVideoTrack,
            .light: light?.track as? RTCVideoTrack,
            .middle: middle?.track as? RTCVideoTrack,
        ].compactMapValues { $0 }
        XCTAssertEqual(tracks.count, 3)
        let pushInto: [(CameraSenderKind, RTCVideoSource?)] = [
            (.main, source), (.light, copySources[.light]), (.middle, copySources[.middle]),
        ]
        for (kind, pushed) in pushInto {
            guard let pushed else { return XCTFail("no source for \(kind)") }
            XCTAssertEqual(
                Self.tracksReached(by: pushed, among: tracks), [kind],
                "a frame pushed into the \(kind) source")
        }
        // No transceiver has a mid before a local description is set.
        XCTAssertEqual(connection.cameraCopyMids(.light), [])
        XCTAssertEqual(connection.cameraCopyMids(.middle), [])
        // Adding again adds nothing.
        connection.addCameraCopies(sources: { copySources[$0] }, middleActive: true)
        XCTAssertTrue(connection.cameraSender(.light) === light)
        XCTAssertTrue(connection.cameraSender(.middle) === middle)
    }

    /// Which of `tracks` render a frame pushed into `source`.
    private static func tracksReached(
        by source: RTCVideoSource, among tracks: [CameraSenderKind: RTCVideoTrack]
    ) -> Set<CameraSenderKind> {
        final class Seen: NSObject, RTCVideoRenderer {
            private let lock = NSLock()
            private var frames = 0
            var count: Int {
                lock.lock()
                defer { lock.unlock() }
                return frames
            }
            func setSize(_ size: CGSize) {}
            func renderFrame(_ frame: RTCVideoFrame?) {
                lock.lock()
                frames += 1
                lock.unlock()
            }
        }
        let seen = tracks.mapValues { _ in Seen() }
        for (kind, track) in tracks { track.add(seen[kind]!) }
        defer { for (kind, track) in tracks { track.remove(seen[kind]!) } }
        let frame = RTCVideoFrame(
            buffer: RTCI420Buffer(width: 640, height: 360), rotation: ._0,
            timeStampNs: 1_000_000_000)
        source.capturer(RTCVideoCapturer(delegate: source), didCapture: frame)
        return Set(seen.filter { $0.value.count > 0 }.keys)
    }

    /// The camera toggle switches every copy with the camera, and removing
    /// the camera removes its copies.
    func testTheCopiesFollowTheCamera() {
        let connection = WebRTCPeerConnection(iceServers: [])
        defer { connection.close() }
        connection.addLocalCameraTrack(WebRTCPeerConnection.newVideoSource())
        connection.addCameraCopies(
            sources: { _ in WebRTCPeerConnection.newVideoSource() }, middleActive: true)

        connection.setCameraEnabled(false)
        XCTAssertEqual(connection.cameraSender(.main)?.track?.isEnabled, false)
        XCTAssertEqual(connection.cameraSender(.light)?.track?.isEnabled, false)
        XCTAssertEqual(connection.cameraSender(.middle)?.track?.isEnabled, false)
        connection.setCameraEnabled(true)
        XCTAssertEqual(connection.cameraSender(.light)?.track?.isEnabled, true)

        connection.removeLocalVideoTrack()
        XCTAssertNil(connection.cameraSender(.main))
        XCTAssertNil(connection.cameraSender(.light))
        XCTAssertNil(connection.cameraSender(.middle))
    }

    /// A copy no source can be made for is absent; the others are added.
    func testACopyWithNoSourceIsAbsent() {
        let connection = WebRTCPeerConnection(iceServers: [])
        defer { connection.close() }
        connection.addLocalCameraTrack(WebRTCPeerConnection.newVideoSource())
        connection.addCameraCopies(
            sources: { $0 == .light ? WebRTCPeerConnection.newVideoSource() : nil },
            middleActive: true)

        XCTAssertNotNil(connection.cameraSender(.main))
        XCTAssertNotNil(connection.cameraSender(.light))
        XCTAssertNil(connection.cameraSender(.middle))
    }

    /// The one-to-one call is unchanged: one sender, no copies.
    func testTheOneToOnePathStillAddsOneSender() {
        let connection = WebRTCPeerConnection(iceServers: [])
        defer { connection.close() }

        connection.addLocalVideoTrack(WebRTCPeerConnection.newVideoSource())

        XCTAssertNotNil(connection.cameraSender(.main))
        XCTAssertNil(connection.cameraSender(.light))
        XCTAssertNil(connection.cameraSender(.middle))
    }
}

// MARK: - Which senders send

final class SenderActivityRuleTests: XCTestCase {

    private func input(
        cameraOn: Bool = true, middleWanted: Bool = true, needed: VideoNeeded? = nil
    ) -> SenderActivityRule.Input {
        SenderActivityRule.Input(cameraOn: cameraOn, middleWanted: middleWanted, needed: needed)
    }

    /// Before the SFU's first word every sender is needed.
    func testNoWordYetMeansNeeded() {
        let active = SenderActivityRule.allActive(input()) { _ in true }
        XCTAssertEqual(active, [.main: true, .middle: true, .light: true])
    }

    func testTheSfusWordSwitchesTheMainAndTheMiddleSender() {
        let none = SenderActivityRule.allActive(
            input(needed: VideoNeeded(main: false, middle: false))) { _ in true }
        XCTAssertEqual(none, [.main: false, .middle: false, .light: true])

        let mainOnly = SenderActivityRule.allActive(
            input(needed: VideoNeeded(main: true, middle: false))) { _ in true }
        XCTAssertEqual(mainOnly, [.main: true, .middle: false, .light: true])

        let middleOnly = SenderActivityRule.allActive(
            input(needed: VideoNeeded(main: false, middle: true))) { _ in true }
        XCTAssertEqual(middleOnly, [.main: false, .middle: true, .light: true])
    }

    /// The light sender is always needed: the frame does not name it.
    func testTheLightSenderIgnoresTheSfusWord() {
        let needed = VideoNeeded(main: false, middle: false)
        XCTAssertNil(SenderActivityRule.neededWord(.light, needed))
        XCTAssertTrue(SenderActivityRule.active(.light, exists: true, input(needed: needed)))
        XCTAssertEqual(SenderActivityRule.neededWord(.main, needed), false)
        XCTAssertEqual(SenderActivityRule.neededWord(.middle, needed), false)
        XCTAssertNil(SenderActivityRule.neededWord(.main, nil))
    }

    /// The middle sender also needs the people count: under three people it
    /// is off even when the SFU says it is needed.
    func testTheMiddleSenderNeedsThePeopleCountToo() {
        let needed = VideoNeeded(main: true, middle: true)
        XCTAssertFalse(
            SenderActivityRule.active(
                .middle, exists: true, input(middleWanted: false, needed: needed)))
        XCTAssertTrue(
            SenderActivityRule.active(
                .middle, exists: true, input(middleWanted: true, needed: needed)))
        // The people count does not touch the other two.
        XCTAssertTrue(
            SenderActivityRule.active(.main, exists: true, input(middleWanted: false)))
        XCTAssertTrue(
            SenderActivityRule.active(.light, exists: true, input(middleWanted: false)))
    }

    func testNothingIsActiveWithoutACameraOrWithoutTheSender() {
        let off = SenderActivityRule.allActive(input(cameraOn: false)) { _ in true }
        XCTAssertEqual(off, [.main: false, .middle: false, .light: false])
        let missing = SenderActivityRule.allActive(input()) { $0 != .middle }
        XCTAssertEqual(missing, [.main: true, .middle: false, .light: true])
    }

    func testTheLogFields() {
        XCTAssertEqual(SenderActivityRule.neededField(nil), "-")
        XCTAssertEqual(SenderActivityRule.neededField(true), "yes")
        XCTAssertEqual(SenderActivityRule.neededField(false), "no")
        XCTAssertEqual(
            SenderActivityRule.describe(needed: false, active: false), " needed=no active=no")
        XCTAssertEqual(
            SenderActivityRule.describe(needed: nil, active: true), " needed=- active=yes")
    }

    func testAVideoNeededFrameIsLoggedOnlyWhenItChangesWhatIsActive() {
        let all: [CameraSenderKind: Bool] = [.main: true, .middle: true, .light: true]
        let needed = VideoNeeded(main: false, middle: true)
        XCTAssertNil(SenderActivityRule.describeChange(needed, before: all, after: all))
        XCTAssertEqual(
            SenderActivityRule.describeChange(
                needed, before: all, after: [.main: false, .middle: true, .light: true]),
            "call video needed main=no middle=yes active main=no middle=yes light=yes")
    }
}

/// A sender as the activity keeper sees it: what it holds, every call made,
/// and what the next calls answer.
@MainActor
private final class FakeActivitySender: ActivitySender {
    var active = true
    var calls: [Bool] = []
    var refuse = false
    var hasEncoding = true
    /// Suspends each call until released, to ask faster than it answers.
    var gate: CheckedContinuation<Void, Never>?
    var holdNext = false

    func applyEncodingActive(_ wanted: Bool) async -> SenderActivityOutcome {
        calls.append(wanted)
        if holdNext {
            holdNext = false
            await withCheckedContinuation { gate = $0 }
        }
        if !hasEncoding { return .noEncoding }
        if refuse { return .refused }
        active = wanted
        return .applied
    }
}

@MainActor
final class SenderActivityTests: XCTestCase {

    func testNothingIsAskedOfNoSender() async {
        let activity = SenderActivity(name: "main", warn: { _ in })
        XCTAssertFalse(activity.bound)
        activity.want(false)
        await activity.settled()
        XCTAssertEqual(activity.lastWanted, false)
    }

    func testTheEncodingFollowsWhatIsWanted() async {
        let sender = FakeActivitySender()
        let activity = SenderActivity(name: "main", warn: { _ in })
        activity.bind(sender)
        XCTAssertTrue(activity.bound)
        activity.want(false)
        await activity.settled()
        XCTAssertFalse(sender.active)
        activity.want(true)
        await activity.settled()
        XCTAssertTrue(sender.active)
    }

    /// Asked faster than the binding answers, the last word wins and the
    /// calls never overlap.
    func testTheLastWordWins() async {
        let sender = FakeActivitySender()
        sender.holdNext = true
        let activity = SenderActivity(name: "middle", warn: { _ in })
        activity.bind(sender)
        activity.want(false)
        // Let the first step reach the sender and park there.
        while sender.gate == nil { await Task.yield() }
        activity.want(true)
        activity.want(false)
        activity.want(true)
        sender.gate?.resume()
        sender.gate = nil
        await activity.settled()
        XCTAssertTrue(sender.active)
        XCTAssertEqual(sender.calls.last, true)
        XCTAssertLessThanOrEqual(sender.calls.count, 2)
    }

    /// A refused switch-off leaves the sender sending and is not asked again
    /// until something else is wanted; it is said once.
    func testARefusedSwitchOffIsNotRepeated() async {
        let sender = FakeActivitySender()
        sender.refuse = true
        var warnings: [String] = []
        let activity = SenderActivity(name: "main", warn: { warnings.append($0) })
        activity.bind(sender)
        activity.want(false)
        await activity.settled()
        activity.want(false)
        await activity.settled()
        XCTAssertEqual(sender.calls, [false])
        XCTAssertTrue(sender.active)
        XCTAssertEqual(
            warnings,
            ["camera sender activity: the main camera sender could not be stopped, it goes on sending"])
    }

    /// A refused switch-on is asked again at every later ask: a sender
    /// somebody watches must not stay off.
    func testARefusedSwitchOnIsAskedAgain() async {
        let sender = FakeActivitySender()
        sender.active = false
        sender.refuse = true
        var warnings: [String] = []
        let activity = SenderActivity(name: "middle", warn: { warnings.append($0) })
        activity.bind(sender)
        activity.want(true)
        await activity.settled()
        activity.want(true)
        await activity.settled()
        XCTAssertEqual(sender.calls, [true, true])
        XCTAssertEqual(warnings.count, 1)
        sender.refuse = false
        activity.want(true)
        await activity.settled()
        XCTAssertTrue(sender.active)
    }

    /// A new sender starts from nothing: what is wanted is applied to it and
    /// an earlier refusal is forgotten.
    func testANewSenderIsAskedAfresh() async {
        let first = FakeActivitySender()
        first.refuse = true
        let activity = SenderActivity(name: "main", warn: { _ in })
        activity.bind(first)
        activity.want(false)
        await activity.settled()

        let second = FakeActivitySender()
        activity.bind(second)
        activity.want(false)
        await activity.settled()
        XCTAssertEqual(second.calls, [false])
        XCTAssertFalse(second.active)

        activity.reset()
        XCTAssertFalse(activity.bound)
        XCTAssertNil(activity.lastWanted)
    }
}

// MARK: - What this device asks for

final class VideoWantsTests: XCTestCase {

    /// The rule a tile asks, recorded.
    private final class RecordingRules: TileQualityRules {
        var asked: [(people: UInt32, widthPx: UInt32, pinnedOrStage: Bool)] = []
        var answer: TileVideoQuality = .middle
        func tileVideoQuality(
            people: UInt32, tileWidthPx: UInt32, pinnedOrStage: Bool
        ) -> TileVideoQuality {
            asked.append((people, tileWidthPx, pinnedOrStage))
            return answer
        }
        func middleSenderWanted(people: UInt32) -> Bool { people >= 3 }
    }

    /// Pixels are the device's own, not points: the picture is stretched
    /// over points times the screen's scale.
    func testATilesWidthIsItsPointsTimesTheScreensScale() {
        XCTAssertEqual(VideoWants.physicalTileWidth(points: 200, scale: 3), 600)
        XCTAssertEqual(VideoWants.physicalTileWidth(points: 104, scale: 2), 208)
        XCTAssertEqual(VideoWants.physicalTileWidth(points: 100.4, scale: 1), 100)
        // A scale that is nonsense is read as 1; no width is no pixels.
        XCTAssertEqual(VideoWants.physicalTileWidth(points: 200, scale: 0), 200)
        XCTAssertEqual(VideoWants.physicalTileWidth(points: 200, scale: .nan), 200)
        XCTAssertEqual(VideoWants.physicalTileWidth(points: 0, scale: 3), 0)
        XCTAssertEqual(VideoWants.physicalTileWidth(points: -5, scale: 3), 0)
        XCTAssertEqual(VideoWants.physicalTileWidth(points: .infinity, scale: 3), 0)
    }

    func testTheRuleIsAskedWithThePeopleTheWidthAndTheLargeTile() {
        let rules = RecordingRules()
        let answer = VideoWants.tileQuality(rules, people: 5, tileWidthPx: 600, pinnedOrStage: true)
        XCTAssertEqual(answer, .middle)
        XCTAssertEqual(rules.asked.count, 1)
        XCTAssertEqual(rules.asked[0].people, 5)
        XCTAssertEqual(rules.asked[0].widthPx, 600)
        XCTAssertTrue(rules.asked[0].pinnedOrStage)
    }

    /// The shared rule itself, through the binding: the table of section 7.6.
    func testTheSharedRuleIsTheTableOfTheSpec() {
        // Two people: the full picture at any size.
        XCTAssertEqual(VideoWants.tileQuality(people: 2, tileWidthPx: 100, pinnedOrStage: false), .main)
        // Three or more: the large tile at least 960 px wide is full.
        XCTAssertEqual(VideoWants.tileQuality(people: 3, tileWidthPx: 960, pinnedOrStage: true), .main)
        XCTAssertEqual(VideoWants.tileQuality(people: 3, tileWidthPx: 959, pinnedOrStage: true), .middle)
        // A tile narrower than 240 px is light.
        XCTAssertEqual(VideoWants.tileQuality(people: 3, tileWidthPx: 239, pinnedOrStage: false), .light)
        // Any other tile is middle, however wide.
        XCTAssertEqual(VideoWants.tileQuality(people: 3, tileWidthPx: 240, pinnedOrStage: false), .middle)
        XCTAssertEqual(VideoWants.tileQuality(people: 9, tileWidthPx: 2000, pinnedOrStage: false), .middle)
    }

    /// A strip tile on a phone: 104 points wide. At 2x it is 208 px and
    /// light; at 3x it is 312 px and middle.
    func testTheSameTileAsksForMoreOnADenserScreen() {
        let at2x = VideoWants.physicalTileWidth(points: 104, scale: 2)
        let at3x = VideoWants.physicalTileWidth(points: 104, scale: 3)
        XCTAssertEqual(VideoWants.tileQuality(people: 4, tileWidthPx: at2x, pinnedOrStage: false), .light)
        XCTAssertEqual(VideoWants.tileQuality(people: 4, tileWidthPx: at3x, pinnedOrStage: false), .middle)
    }

    /// `light` alone is nothing at all for a person who sends no light copy.
    func testAPersonWithoutALightCopyIsAskedForTheMiddlePicture() {
        XCTAssertEqual(VideoWants.askedQuality(.light, hasLight: false), .middle)
        XCTAssertEqual(VideoWants.askedQuality(.light, hasLight: true), .light)
        XCTAssertEqual(VideoWants.askedQuality(.main, hasLight: false), .main)
        XCTAssertEqual(VideoWants.askedQuality(.middle, hasLight: false), .middle)
    }

    func testTheHeavyTrackOfAQuality() {
        XCTAssertEqual(VideoWants.heavyTrack(for: .main, main: "M", middle: "m"), "M")
        XCTAssertEqual(VideoWants.heavyTrack(for: .middle, main: "M", middle: "m"), "m")
        // No middle track: the SFU forwards the main one.
        XCTAssertEqual(VideoWants.heavyTrack(for: .middle, main: "M", middle: nil), "M")
        XCTAssertNil(VideoWants.heavyTrack(for: .light, main: "M", middle: "m"))
        XCTAssertNil(VideoWants.heavyTrack(for: .main, main: nil as String?, middle: "m"))
    }

    func testQualitiesAreOrderedMainOverMiddleOverLight() {
        XCTAssertEqual(TileVideoQuality.higher(.light, .middle), .middle)
        XCTAssertEqual(TileVideoQuality.higher(.main, .middle), .main)
        XCTAssertEqual(TileVideoQuality.lower(.main, .middle), .middle)
        XCTAssertEqual(TileVideoQuality.lower(.light, .main), .light)
        XCTAssertEqual(
            [TileVideoQuality.main, .middle, .light].map(\.wireName), ["main", "middle", "light"])
    }

    // MARK: The book

    private final class Tile {}

    func testAPersonNoTileShowsWantsTheLightPictureAlone() {
        var book = VideoWantsBook()
        book.setPeers(["a", "b"])
        XCTAssertEqual(book.people, 3)
        XCTAssertEqual(book.quality(of: "a"), .light)
        XCTAssertEqual(book.wants().map(\.peer), ["a", "b"])
        XCTAssertEqual(book.wants().map(\.quality), [.light, .light])
    }

    func testAPersonsWantIsTheHighestOfTheirTiles() {
        var book = VideoWantsBook()
        book.setPeers(["a", "b"])
        let grid = Tile()
        let stage = Tile()
        XCTAssertTrue(book.setTile(ObjectIdentifier(grid), peerId: "a", quality: .middle))
        XCTAssertTrue(book.setTile(ObjectIdentifier(stage), peerId: "a", quality: .main))
        XCTAssertEqual(book.quality(of: "a"), .main)
        // The same report again changes nothing.
        XCTAssertFalse(book.setTile(ObjectIdentifier(stage), peerId: "a", quality: .main))
        // The stage tile goes away: the grid tile's want is what is left.
        XCTAssertTrue(book.setTile(ObjectIdentifier(stage), peerId: "a", quality: nil))
        XCTAssertEqual(book.quality(of: "a"), .middle)
        XCTAssertFalse(book.setTile(ObjectIdentifier(stage), peerId: "a", quality: nil))
        XCTAssertEqual(book.quality(of: "b"), .light)
    }

    /// A tile that now shows someone else replaces its own entry.
    func testATileShowsOnePersonAtATime() {
        var book = VideoWantsBook()
        book.setPeers(["a", "b"])
        let stage = Tile()
        book.setTile(ObjectIdentifier(stage), peerId: "a", quality: .main)
        book.setTile(ObjectIdentifier(stage), peerId: "b", quality: .main)
        XCTAssertEqual(book.quality(of: "a"), .light)
        XCTAssertEqual(book.quality(of: "b"), .main)
    }

    func testTheSetIsThePeopleOfTheCallInTheOrderTheyJoined() {
        var book = VideoWantsBook()
        book.setPeers(["b", "a"])
        book.addPeer("c")
        book.addPeer("a")
        XCTAssertEqual(book.wants().map(\.peer), ["b", "a", "c"])
        book.removePeer("a")
        XCTAssertEqual(book.wants().map(\.peer), ["b", "c"])
        XCTAssertEqual(book.people, 3)
        book.clear()
        XCTAssertTrue(book.wants().isEmpty)
        XCTAssertEqual(book.people, 1)
    }

    // MARK: The frame

    func testAFrameWithinTheBoundCarriesEveryone() {
        let frame = VideoWants.frame([("a", .main), ("b", .light), ("c", .middle)])
        XCTAssertEqual(frame.map(\.peer), ["a", "b", "c"])
        XCTAssertEqual(frame.map(\.quality), [.main, .light, .middle])
        XCTAssertEqual(
            VideoWants.describe(frame), "call video wants a=main b=light c=middle")
    }

    /// Past the bound the `main` entries are left out (a person a frame
    /// leaves out is at `main`); if the rest still does not fit, the last to
    /// join are left out.
    func testPastTheBoundTheMainEntriesAreLeftOutFirst() {
        let wants: [(peer: String, quality: TileVideoQuality)] = [
            ("a", .main), ("b", .light), ("c", .middle), ("d", .light),
        ]
        XCTAssertEqual(VideoWants.frame(wants, max: 3).map(\.peer), ["b", "c", "d"])
        XCTAssertEqual(VideoWants.frame(wants, max: 2).map(\.peer), ["b", "c"])
        XCTAssertEqual(VideoWants.maxEntries, 64)
    }

    // MARK: The sender

    /// A timer and a clock a test drives.
    private final class FakeTimers {
        var now: Int64 = 0
        private var pending: [(at: Int64, id: Int, work: () -> Void)] = []
        private var nextId = 0

        var deps: CallTimerDeps {
            CallTimerDeps(
                now: { [unowned self] in self.now },
                schedule: { [unowned self] ms, work in
                    self.nextId += 1
                    let id = self.nextId
                    self.pending.append((self.now + ms, id, work))
                    return { [unowned self] in self.pending.removeAll { $0.id == id } }
                })
        }

        var scheduled: Int { pending.count }

        /// Move the clock forward, running what comes due in order.
        func advance(_ ms: Int64) {
            let end = now + ms
            while let next = pending.filter({ $0.at <= end }).min(by: { $0.at < $1.at }) {
                pending.removeAll { $0.id == next.id }
                now = max(now, next.at)
                next.work()
            }
            now = end
        }
    }

    func testTheReportsOfOneTurnBecomeOneFrame() {
        let timers = FakeTimers()
        var book = VideoWantsBook()
        book.setPeers(["a", "b"])
        var sent: [[SfuVideoWant]] = []
        let sender = VideoWantsSender(
            wants: { book.wants() }, send: { sent.append($0); return true }, deps: timers.deps)
        let tileA = Tile()
        let tileB = Tile()

        book.setTile(ObjectIdentifier(tileA), peerId: "a", quality: .middle)
        sender.changed()
        book.setTile(ObjectIdentifier(tileB), peerId: "b", quality: .main)
        sender.changed()
        XCTAssertTrue(sent.isEmpty, "nothing is sent in the turn that asked")
        timers.advance(0)

        XCTAssertEqual(sent.count, 1)
        XCTAssertEqual(sent[0].map(\.peer), ["a", "b"])
        XCTAssertEqual(sent[0].map(\.quality), [.middle, .main])
    }

    func testAnUnchangedSetIsNotSentAgainUnlessAskedTo() {
        let timers = FakeTimers()
        var book = VideoWantsBook()
        book.setPeers(["a"])
        var sent = 0
        let sender = VideoWantsSender(
            wants: { book.wants() }, send: { _ in sent += 1; return true }, deps: timers.deps)

        sender.changed()
        timers.advance(1_000)
        sender.changed()
        timers.advance(1_000)
        XCTAssertEqual(sent, 1)

        // After a person joins the set is sent whatever it is.
        sender.resend()
        timers.advance(1_000)
        XCTAssertEqual(sent, 2)
    }

    func testTwoFramesAreNeverCloserThanTheInterval() {
        let timers = FakeTimers()
        var book = VideoWantsBook()
        book.setPeers(["a"])
        let tile = Tile()
        var sentAt: [Int64] = []
        let sender = VideoWantsSender(
            wants: { book.wants() }, send: { _ in sentAt.append(timers.now); return true },
            deps: timers.deps)

        sender.changed()
        timers.advance(0)
        book.setTile(ObjectIdentifier(tile), peerId: "a", quality: .main)
        sender.changed()
        timers.advance(50)
        XCTAssertEqual(sentAt, [0])
        timers.advance(VideoWants.minIntervalMs)
        XCTAssertEqual(sentAt, [0, VideoWants.minIntervalMs])
    }

    /// A frame that could not go (no connection yet) is tried again at the
    /// next call, and an empty set is never sent.
    func testAFrameThatCouldNotGoIsTriedAgain() {
        let timers = FakeTimers()
        var book = VideoWantsBook()
        var open = false
        var sent = 0
        let sender = VideoWantsSender(
            wants: { book.wants() },
            send: { _ in
                guard open else { return false }
                sent += 1
                return true
            },
            deps: timers.deps)

        // Nobody else in the call: nothing to ask for.
        sender.resend()
        timers.advance(1_000)
        XCTAssertEqual(sent, 0)

        book.setPeers(["a"])
        sender.changed()
        timers.advance(1_000)
        XCTAssertEqual(sent, 0, "the socket is not open")
        open = true
        sender.changed()
        timers.advance(1_000)
        XCTAssertEqual(sent, 1)

        sender.reset()
        sender.changed()
        timers.advance(1_000)
        XCTAssertEqual(sent, 2, "after a reset the same set is new again")
    }

    // MARK: A tile's width

    func testTheFirstWidthIsUsedAtOnceAndLaterOnesOnceTheyStand() {
        let timers = FakeTimers()
        var applied: [UInt32] = []
        let width = SettledWidth(deps: timers.deps) { applied.append($0) }

        width.observe(600)
        XCTAssertEqual(applied, [600])
        // A rotation in steps: only the width it ends at is used.
        width.observe(700)
        timers.advance(100)
        width.observe(800)
        timers.advance(100)
        width.observe(900)
        XCTAssertEqual(applied, [600])
        timers.advance(VideoWants.tileWidthSettleMs)
        XCTAssertEqual(applied, [600, 900])
        // Back to the applied width before it settled: nothing to apply.
        width.observe(1_000)
        width.observe(900)
        timers.advance(1_000)
        XCTAssertEqual(applied, [600, 900])
        width.observe(300)
        width.dispose()
        timers.advance(1_000)
        XCTAssertEqual(applied, [600, 900])
    }
}

// MARK: - What the SFU gives

final class VideoGivenTests: XCTestCase {

    /// A tile shows the lower of what it wants and what it is given.
    func testATileShowsTheLowerOfWantAndGiven() {
        XCTAssertEqual(VideoGiven.effectiveQuality(wanted: .main, given: .middle), .middle)
        XCTAssertEqual(VideoGiven.effectiveQuality(wanted: .main, given: .light), .light)
        XCTAssertEqual(VideoGiven.effectiveQuality(wanted: .middle, given: .main), .middle)
        XCTAssertEqual(VideoGiven.effectiveQuality(wanted: .light, given: .main), .light)
        XCTAssertEqual(VideoGiven.effectiveQuality(wanted: .middle, given: .middle), .middle)
    }

    /// The SFU has said nothing: the tile shows what it wants.
    func testNothingSaidMeansWhatTheTileWants() {
        XCTAssertEqual(VideoGiven.effectiveQuality(wanted: .main, given: nil), .main)
        XCTAssertEqual(VideoGiven.effectiveQuality(wanted: .light, given: nil), .light)
    }

    /// Given the light picture alone of a person who sends none, the tile
    /// keeps the heavy picture it wants: there is nothing else to show.
    func testGivenLightOfAPersonWithoutALightCopyKeepsTheHeavyPicture() {
        XCTAssertEqual(
            VideoGiven.shownQuality(wanted: .main, given: .light, hasLight: false), .main)
        XCTAssertEqual(
            VideoGiven.shownQuality(wanted: .middle, given: .light, hasLight: false), .middle)
        XCTAssertEqual(
            VideoGiven.shownQuality(wanted: .main, given: .light, hasLight: true), .light)
        XCTAssertEqual(
            VideoGiven.shownQuality(wanted: .main, given: .middle, hasLight: false), .middle)
    }

    func testTheHeavyTrackATileWatches() {
        func heavy(
            _ wanted: TileVideoQuality, _ given: TileVideoQuality?, middle: String? = "m",
            hasLight: Bool = true
        ) -> String? {
            VideoGiven.heavyTrack(
                wanted: wanted, given: given, main: "M", middle: middle, hasLight: hasLight)
        }
        XCTAssertEqual(heavy(.main, nil), "M")
        XCTAssertEqual(heavy(.middle, nil), "m")
        XCTAssertEqual(heavy(.main, .middle), "m")
        // Given middle of a person with no middle track: the main one.
        XCTAssertEqual(heavy(.main, .middle, middle: nil), "M")
        // Given light: the light picture alone, no heavy renderer waits.
        XCTAssertNil(heavy(.main, .light))
        XCTAssertNil(heavy(.light, nil))
        XCTAssertEqual(heavy(.main, .light, hasLight: false), "M")
    }

    func testAFrameReplacesTheWholeSet() {
        var book = VideoGivenBook()
        XCTAssertNil(book.given(of: "a"))
        book.replace([
            SfuVideoGiven(peer: "a", quality: .light),
            SfuVideoGiven(peer: "b", quality: .middle),
            SfuVideoGiven(peer: "", quality: .main),
        ])
        XCTAssertEqual(book.snapshot, ["a": .light, "b": .middle])
        // A person the latest frame does not name is back to nothing said.
        book.replace([
            SfuVideoGiven(peer: "b", quality: .light),
            SfuVideoGiven(peer: "b", quality: .main),
        ])
        XCTAssertNil(book.given(of: "a"))
        XCTAssertEqual(book.given(of: "b"), .main, "a person named twice has the later entry")
        XCTAssertEqual(book.count, 1)
        XCTAssertTrue(book.removePeer("b"))
        XCTAssertFalse(book.removePeer("b"))
        book.replace([SfuVideoGiven(peer: "a", quality: .light)])
        book.clear()
        XCTAssertEqual(book.count, 0)
    }

    func testOnlyAChangeOfTheShownPictureIsLogged() {
        let wants: [(peer: String, quality: TileVideoQuality)] = [
            ("a", .main), ("b", .middle), ("c", .light),
        ]
        let after: [String: TileVideoQuality] = ["a": .middle, "b": .main, "c": .main]
        let changes = VideoGiven.changes(wants: wants, before: { _ in nil }, after: { after[$0] })
        // b wants middle and is given main: still middle. c wants light.
        XCTAssertEqual(changes, [VideoGiven.Change(peer: "a", wanted: .main, given: .middle)])
        XCTAssertEqual(
            VideoGiven.describe("call video given", changes),
            "call video given peer=a want=main given=middle")
        XCTAssertNil(VideoGiven.describe("call video given", []))

        let back = VideoGiven.changes(wants: wants, before: { after[$0] }, after: { _ in nil })
        XCTAssertEqual(
            VideoGiven.describe("call video given", back),
            "call video given peer=a want=main given=-")
    }
}

// MARK: - The pin control

@MainActor
final class TilePinTests: XCTestCase {

    /// A timer a test fires by hand.
    private final class OneTimer {
        var pending: [(ms: Int64, work: () -> Void)] = []
        var deps: CallTimerDeps {
            CallTimerDeps(
                now: { 0 },
                schedule: { [unowned self] ms, work in
                    self.pending.append((ms, work))
                    let index = self.pending.count - 1
                    return { [unowned self] in self.pending[index].work = {} }
                })
        }
        func fireLast() { pending.last?.work() }
    }

    func testATapBringsOneTilesControlsUpForThreeSeconds() {
        let timer = OneTimer()
        var changes: [String?] = []
        let reveal = TileControlsReveal(deps: timer.deps) { changes.append($0) }

        reveal.tap("peer-a")
        XCTAssertEqual(reveal.tileId, "peer-a")
        XCTAssertEqual(timer.pending.last?.ms, 3_000)
        XCTAssertEqual(TileControlsReveal.revealMs, 3_000)
        timer.fireLast()
        XCTAssertNil(reveal.tileId)
        XCTAssertEqual(changes, ["peer-a", nil])
    }

    func testATapOnAnotherTileMovesTheControlsAndStartsTheTimeAgain() {
        let timer = OneTimer()
        var changes: [String?] = []
        let reveal = TileControlsReveal(deps: timer.deps) { changes.append($0) }

        reveal.tap("peer-a")
        reveal.tap("peer-b")
        XCTAssertEqual(reveal.tileId, "peer-b")
        // The first tap's timer was cancelled: firing it hides nothing.
        timer.pending[0].work()
        XCTAssertEqual(reveal.tileId, "peer-b")
        // A second tap on the same tile keeps it up and reports nothing.
        reveal.tap("peer-b")
        XCTAssertEqual(changes, ["peer-a", "peer-b"])
    }

    func testAPressAnywhereElseHidesTheControls() {
        let timer = OneTimer()
        let reveal = TileControlsReveal(deps: timer.deps) { _ in }
        reveal.tap("peer-a")
        reveal.pressBegan("peer-a")
        XCTAssertEqual(reveal.tileId, "peer-a", "a press on the same tile keeps them")
        reveal.pressBegan(nil)
        XCTAssertNil(reveal.tileId)
        reveal.tap("peer-a")
        reveal.pressBegan("peer-b")
        XCTAssertNil(reveal.tileId)
        reveal.tap("peer-a")
        reveal.hide()
        XCTAssertNil(reveal.tileId)
    }

    /// A press is on the tile whose frame holds the point where it began,
    /// and on no tile anywhere else: the control bar, the status line, the
    /// gap between two tiles.
    func testWhichTileAPressBeganOn() {
        let frames: [String: CGRect] = [
            "peer-a": CGRect(x: 0, y: 100, width: 180, height: 240),
            "peer-b": CGRect(x: 188, y: 100, width: 180, height: 240),
        ]
        XCTAssertEqual(TilePressTarget.tileId(at: CGPoint(x: 90, y: 200), frames: frames), "peer-a")
        // The pin control sits in its tile's top right corner.
        XCTAssertEqual(TilePressTarget.tileId(at: CGPoint(x: 170, y: 110), frames: frames), "peer-a")
        XCTAssertEqual(TilePressTarget.tileId(at: CGPoint(x: 200, y: 300), frames: frames), "peer-b")
        XCTAssertNil(TilePressTarget.tileId(at: CGPoint(x: 184, y: 200), frames: frames))
        XCTAssertNil(TilePressTarget.tileId(at: CGPoint(x: 90, y: 40), frames: frames))
        XCTAssertNil(TilePressTarget.tileId(at: CGPoint(x: 90, y: 700), frames: frames))
        XCTAssertNil(TilePressTarget.tileId(at: CGPoint(x: 90, y: 200), frames: [:]))
    }

    /// The whole rule as the call screen drives it: a press anywhere but the
    /// tile that owns the controls hides them at once, and the later
    /// movements of one press change nothing.
    func testAPressBesideTheOwningTileHidesTheControlsAtOnce() {
        let frames: [String: CGRect] = [
            "peer-a": CGRect(x: 0, y: 100, width: 180, height: 240),
            "peer-b": CGRect(x: 188, y: 100, width: 180, height: 240),
        ]
        let timer = OneTimer()
        var changes: [String?] = []
        let reveal = TileControlsReveal(deps: timer.deps) { changes.append($0) }
        func press(_ x: CGFloat, _ y: CGFloat) {
            reveal.pressBegan(TilePressTarget.tileId(at: CGPoint(x: x, y: y), frames: frames))
        }

        reveal.tap("peer-a")
        press(170, 110)
        press(170, 110)
        XCTAssertEqual(reveal.tileId, "peer-a", "on the control or its tile: kept")
        press(90, 700)
        XCTAssertNil(reveal.tileId, "on the control bar: gone")
        press(90, 700)
        reveal.tap("peer-a")
        press(184, 200)
        XCTAssertNil(reveal.tileId, "on empty space in the grid: gone")
        reveal.tap("peer-a")
        press(200, 300)
        XCTAssertNil(reveal.tileId, "on another tile: gone, until its own tap")
        reveal.tap("peer-b")
        press(200, 300)
        XCTAssertEqual(reveal.tileId, "peer-b")
        XCTAssertEqual(changes, ["peer-a", nil, "peer-a", nil, "peer-a", nil, "peer-b"])
    }

    /// A tile is never itself the pin control: there is a control only on
    /// another person's tile, and only where the spec puts one.
    func testWhichControlATileCarries() {
        // This device's own tile has none, whatever else is true.
        XCTAssertEqual(TilePinControl.of(isLocal: true, isPinned: false, revealed: true), .none)
        XCTAssertEqual(TilePinControl.of(isLocal: true, isPinned: true, revealed: true), .none)
        // Another person's tile: nothing until a tap brought the controls up.
        XCTAssertEqual(TilePinControl.of(isLocal: false, isPinned: false, revealed: false), .none)
        XCTAssertEqual(
            TilePinControl.of(isLocal: false, isPinned: false, revealed: true), .pinButton)
        // The pinned tile always carries its chip.
        XCTAssertEqual(
            TilePinControl.of(isLocal: false, isPinned: true, revealed: false), .pinnedChip)
        XCTAssertEqual(
            TilePinControl.of(isLocal: false, isPinned: true, revealed: true), .pinnedChip)
        XCTAssertEqual(TilePinControl.touchTarget, 44)
    }

    private func person(_ id: String, isLocal: Bool = false) -> CallUiState.Participant {
        CallUiState.Participant(
            id: id, displayName: id, isLocal: isLocal, micOn: true, camOn: false,
            handRaised: false, quality: .good, speaking: false, screenSharing: false)
    }

    /// Pinning from the grid switches to Focus; unpinning leaves the layout
    /// as it is, and the stage goes back to following the speaker.
    func testPinningSwitchesToFocusAndUnpinningLeavesTheLayout() {
        let state = CallUiState()
        let people = [person("local", isLocal: true), person("peer-a"), person("peer-b")]
        state.setParticipants(people)
        XCTAssertEqual(
            GroupCallLayout.kind(
                choice: state.layoutChoice, pinnedId: state.pinnedId, people: people), .grid)

        state.togglePin("peer-a")
        XCTAssertEqual(state.pinnedId, "peer-a")
        XCTAssertEqual(state.layoutChoice, .focus)

        // One pin at a time: pinning someone else moves it.
        state.togglePin("peer-b")
        XCTAssertEqual(state.pinnedId, "peer-b")

        state.togglePin("peer-b")
        XCTAssertNil(state.pinnedId)
        XCTAssertEqual(
            GroupCallLayout.kind(
                choice: state.layoutChoice, pinnedId: state.pinnedId, people: people), .focus)
    }

    /// The pinned person leaves: the pin is cleared and the layout stays.
    func testThePinnedPersonLeavingClearsThePinAndKeepsTheLayout() {
        let state = CallUiState()
        state.setParticipants([person("local", isLocal: true), person("peer-a"), person("peer-b")])
        state.togglePin("peer-a")
        state.participantLeft("peer-a")
        XCTAssertNil(state.pinnedId)
        XCTAssertEqual(state.layoutChoice, .focus)
    }

    /// With nobody pinned the stage follows the shared rule: the floor, then
    /// the most recent speaker; a pin holds it whoever speaks.
    func testTheStageFollowsTheSpeakerUnlessSomeoneIsPinned() {
        let rules = SharedGroupCallRules()
        let people = [person("local", isLocal: true), person("peer-a"), person("peer-b")]
        let speech = StageSpeech(activeSpeakerId: "peer-b", lastSpokeAtMs: ["peer-b": 10])
        XCTAssertEqual(
            GroupCallLayout.stagePerson(rules, people: people, pinnedId: nil, speech: speech)?.id,
            "peer-b")
        XCTAssertEqual(
            GroupCallLayout.stagePerson(
                rules, people: people, pinnedId: "peer-a", speech: speech)?.id,
            "peer-a")
        let quiet = StageSpeech(activeSpeakerId: nil, lastSpokeAtMs: ["peer-b": 10])
        XCTAssertEqual(
            GroupCallLayout.stagePerson(rules, people: people, pinnedId: nil, speech: quiet)?.id,
            "peer-b")
    }
}

// MARK: - The voices line

final class VoiceDiagnosticsTests: XCTestCase {

    private struct Person: GroupLayoutPerson {
        let id: String
        let isLocal: Bool
        let camOn: Bool
    }

    func testTheLineNamesEveryPersonTheFloorTheStageAndTheLayout() {
        let voices = [
            VoiceReport(
                id: "local", isLocal: true, path: .statsLevel, peak: 0.4, speaking: true,
                threshold: 0.05),
            VoiceReport(
                id: "peer-a", isLocal: false, path: .statsLevel, peak: 0.012, speaking: false,
                threshold: 0.0125),
            VoiceReport(
                id: "peer-b", isLocal: false, path: .none, peak: nil, speaking: false,
                threshold: speakingLevel()),
        ]
        XCTAssertEqual(
            VoiceDiagnostics.describe(
                "call media interval", voices: voices, floorId: "peer-a",
                stage: (id: "peer-a", reason: .speaking), layout: .focus),
            "call media interval voices me=stats-level:0.400:yes[thr=0.050]"
                + " a=stats-level:0.012:no[thr=0.013] b=none:-:no[thr=0.020]"
                + " floor=a stage=a(speaking) layout=focus")
        XCTAssertEqual(
            VoiceDiagnostics.describe(
                "call media interval", voices: [], floorId: nil, stage: nil, layout: .grid),
            "call media interval voices floor=- stage=- layout=grid")
    }

    func testWhyAPersonIsOnTheLargeTile() {
        let people = [
            Person(id: "peer-a", isLocal: false, camOn: true),
            Person(id: "peer-b", isLocal: false, camOn: false),
        ]
        let speech = StageSpeech(activeSpeakerId: "peer-a", lastSpokeAtMs: ["peer-b": 5])
        XCTAssertEqual(
            VoiceDiagnostics.stageReason(
                stageId: "peer-a", people: people, pinnedId: "peer-a", speech: speech), .pinned)
        XCTAssertEqual(
            VoiceDiagnostics.stageReason(
                stageId: "peer-a", people: people, pinnedId: nil, speech: speech), .speaking)
        XCTAssertEqual(
            VoiceDiagnostics.stageReason(
                stageId: "peer-b", people: people, pinnedId: nil, speech: speech), .recent)
        XCTAssertEqual(
            VoiceDiagnostics.stageReason(
                stageId: "peer-a", people: people, pinnedId: nil, speech: .none), .picture)
        XCTAssertEqual(
            VoiceDiagnostics.stageReason(
                stageId: "peer-b", people: people, pinnedId: nil, speech: .none), .first)
    }
}
