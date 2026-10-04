// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
import WebRTC
@testable import Lakko

/// ADR-0067 step 5 on iOS: the camera goes out as two simulcast layers, and a
/// remote person's light companion track is kept beside their main one and
/// shown while the main one stands still.
///
/// What is covered here is everything that can be decided without a device:
/// the encodings asked of the binding and the fallback, the stream-id split,
/// that a companion is never a participant and goes with its peer, the lines
/// of the media log, and the tile's controller driven through the shared rule
/// (`LayerDisplay` of `lakko-ui-rules`, over the binding). Whether an encoder
/// produces both layers and whether two Metal views keep drawing are not.

// MARK: - Sending

@MainActor
final class CameraSenderTests: XCTestCase {

    func testSimulcastEncodingsAreTheLightLayerThenTheFullOne() {
        XCTAssertEqual(
            CameraSender.simulcastEncodings(),
            [
                CameraEncodingSpec(rid: "q", scaleResolutionDownBy: 4, maxBitrateBps: 150_000),
                CameraEncodingSpec(rid: "f", scaleResolutionDownBy: nil, maxBitrateBps: nil),
            ])
    }

    func testRtcEncodingsCarryRidScaleAndBitrate() {
        let encodings = CameraSender.rtcEncodings(CameraSender.simulcastEncodings())
        XCTAssertEqual(encodings.map(\.rid), ["q", "f"])
        XCTAssertEqual(encodings[0].scaleResolutionDownBy, NSNumber(value: 4.0))
        XCTAssertEqual(encodings[0].maxBitrateBps, NSNumber(value: 150_000))
        XCTAssertNil(encodings[1].scaleResolutionDownBy)
        XCTAssertNil(encodings[1].maxBitrateBps)
        XCTAssertTrue(encodings.allSatisfy(\.isActive))
    }

    func testTheSenderIsCreatedWithTwoEncodingsWhenTheBindingTakesThem() {
        var asked: [[CameraEncodingSpec]] = []
        var plainCalls = 0
        var logged: [String] = []
        let added = CameraSender.add(
            simulcast: true,
            addWithEncodings: { specs -> String? in
                asked.append(specs)
                return "simulcast-sender"
            },
            addPlain: { () -> String? in
                plainCalls += 1
                return "plain-sender"
            },
            log: { logged.append($0) })
        XCTAssertEqual(added.sender, "simulcast-sender")
        XCTAssertEqual(added.outcome, .simulcast)
        XCTAssertEqual(asked, [CameraSender.simulcastEncodings()])
        XCTAssertEqual(plainCalls, 0)
        XCTAssertTrue(logged.isEmpty)
    }

    func testARefusalFallsBackToTheSingleEncodingAndIsLogged() {
        var plainCalls = 0
        var logged: [String] = []
        let added = CameraSender.add(
            simulcast: true,
            addWithEncodings: { _ -> String? in nil },
            addPlain: { () -> String? in
                plainCalls += 1
                return "plain-sender"
            },
            log: { logged.append($0) })
        XCTAssertEqual(added.sender, "plain-sender")
        XCTAssertEqual(added.outcome, .single)
        XCTAssertEqual(plainCalls, 1)
        XCTAssertEqual(logged, ["camera sender: simulcast refused, sending one encoding"])
    }

    func testSimulcastSwitchedOffNeverAsksForEncodings() {
        var asked = 0
        let added = CameraSender.add(
            simulcast: false,
            addWithEncodings: { _ -> String? in
                asked += 1
                return "simulcast-sender"
            },
            addPlain: { () -> String? in "plain-sender" },
            log: { _ in })
        XCTAssertEqual(added.outcome, .single)
        XCTAssertEqual(asked, 0)
    }

    func testNoSenderAtAllIsReported() {
        var logged: [String] = []
        let added = CameraSender.add(
            simulcast: true,
            addWithEncodings: { _ -> String? in nil },
            addPlain: { () -> String? in nil },
            log: { logged.append($0) })
        XCTAssertNil(added.sender)
        XCTAssertEqual(added.outcome, .none)
        XCTAssertEqual(logged.count, 2)
    }

    /// Both publish paths of the SFU session (the first publish and the
    /// mid-call camera-on) create the camera sender through
    /// `addLocalCameraTrack`. This asks the real binding, on a peer
    /// connection that never negotiates.
    func testThePeerConnectionCreatesTheCameraSenderWithBothRids() {
        let connection = WebRTCPeerConnection(iceServers: [])
        defer { connection.close() }
        XCTAssertTrue(connection.isReady)

        let outcome = connection.addLocalCameraTrack(WebRTCPeerConnection.newVideoSource())

        XCTAssertEqual(outcome, .simulcast)
        XCTAssertEqual(connection.cameraSendOutcome, .simulcast)
        XCTAssertEqual(connection.cameraSendRids(), ["q", "f"])
        XCTAssertNotNil(connection.currentLocalVideoTrack())
        XCTAssertNotNil(connection.localVideoSender)
    }

    /// The one-to-one call is unchanged: one encoding, no rid.
    func testTheOneToOnePathStillAddsOneEncoding() {
        let connection = WebRTCPeerConnection(iceServers: [])
        defer { connection.close() }

        connection.addLocalVideoTrack(WebRTCPeerConnection.newVideoSource())

        XCTAssertEqual(connection.cameraSendOutcome, CameraSender.Outcome.none)
        XCTAssertEqual(connection.cameraSendRids().count, 1)
        XCTAssertFalse(connection.cameraSendRids().contains("q"))
    }
}

// MARK: - The stream id

final class LightStreamTests: XCTestCase {

    func testAStreamIdEndingInTheSuffixIsThatPeersCompanion() {
        XCTAssertEqual(
            LightStream.split("p000000000000000518db68c125428388~light"),
            LightStream.RemoteStreamId(peerId: "p000000000000000518db68c125428388", light: true))
    }

    func testAnOrdinaryStreamIdIsItsPeer() {
        XCTAssertEqual(
            LightStream.split("p0001"), LightStream.RemoteStreamId(peerId: "p0001", light: false))
    }

    func testTheBareSuffixNamesNoPeer() {
        XCTAssertEqual(
            LightStream.split("~light"), LightStream.RemoteStreamId(peerId: "~light", light: false))
    }

    func testTheSuffixOnlyCountsAtTheEnd() {
        XCTAssertEqual(
            LightStream.split("p1~lightx"),
            LightStream.RemoteStreamId(peerId: "p1~lightx", light: false))
        XCTAssertEqual(
            LightStream.split("p1~LIGHT"), LightStream.RemoteStreamId(peerId: "p1~LIGHT", light: false))
    }

    func testSplitIsTheInverseOfLightStreamId() {
        XCTAssertEqual(LightStream.lightStreamId(of: "p7"), "p7~light")
        XCTAssertEqual(
            LightStream.split(LightStream.lightStreamId(of: "p7")),
            LightStream.RemoteStreamId(peerId: "p7", light: true))
    }

    func testARemoteVideoTrackIsRoutedByItsStreamId() {
        XCTAssertEqual(RemoteVideoRoute.of(streamId: "p7"), .main(streamId: "p7"))
        XCTAssertEqual(RemoteVideoRoute.of(streamId: "p7~light"), .light(peerId: "p7"))
        XCTAssertEqual(RemoteVideoRoute.of(streamId: nil), .main(streamId: nil))
    }

    func testAParticipantIdGivesTheSfuPeerId() {
        XCTAssertEqual(CallScreenPresentation.sfuPeerId(ofParticipant: "peer-p7"), "p7")
        XCTAssertEqual(CallScreenPresentation.sfuPeerId(ofParticipant: "local"), "local")
    }
}

// MARK: - The companion in the call state

@MainActor
final class LightCompanionStateTests: XCTestCase {

    private static let factory = RTCPeerConnectionFactory()

    private func makeTrack(_ id: String) -> RTCVideoTrack {
        Self.factory.videoTrack(with: Self.factory.videoSource(), trackId: id)
    }

    private func remote(_ id: String) -> CallUiState.Participant {
        CallUiState.Participant(
            id: id, displayName: "", isLocal: false, micOn: true, camOn: false,
            handRaised: false, quality: .good, speaking: false, screenSharing: false)
    }

    func testACompanionNeverCreatesAParticipantACameraFlagOrAMainTrack() {
        let state = CallUiState()
        state.setParticipants([remote("peer-a")])
        let light = makeTrack("light-a")

        state.setRemoteLightVideoTrack(light, forParticipant: "peer-a")
        // A companion for someone the roster has not named yet.
        state.setRemoteLightVideoTrack(makeTrack("light-b"), forParticipant: "peer-b")

        XCTAssertEqual(state.participants.map(\.id), ["peer-a"])
        XCTAssertEqual(state.participants.first?.camOn, false)
        XCTAssertTrue(state.remoteVideoTracks.isEmpty)
        XCTAssertNil(state.remoteVideoTrack)
        XCTAssertFalse(state.remoteVideoFlowing)
        XCTAssertTrue(state.remoteLightVideoTracks["peer-a"] === light)
        XCTAssertEqual(state.remoteLightVideoTracks.count, 2)
    }

    func testACompanionNeverReplacesTheMainTrack() {
        let state = CallUiState()
        let main = makeTrack("main-a")
        state.setRemoteVideoTrack(main, forParticipant: "peer-a")

        state.setRemoteLightVideoTrack(makeTrack("light-a"), forParticipant: "peer-a")

        XCTAssertTrue(state.remoteVideoTracks["peer-a"] === main)
        XCTAssertEqual(state.remoteVideoTracks.count, 1)
    }

    func testACompanionGoesWithItsPeer() {
        let state = CallUiState()
        state.setParticipants([remote("peer-a"), remote("peer-b")])
        let lightB = makeTrack("light-b")
        state.setRemoteLightVideoTrack(makeTrack("light-a"), forParticipant: "peer-a")
        state.setRemoteLightVideoTrack(lightB, forParticipant: "peer-b")

        state.participantLeft("peer-a")

        XCTAssertNil(state.remoteLightVideoTracks["peer-a"])
        XCTAssertTrue(state.remoteLightVideoTracks["peer-b"] === lightB)
    }

    func testCompanionsAreDroppedWhenTheCallEndsAndOnReset() {
        let state = CallUiState()
        state.setRemoteLightVideoTrack(makeTrack("light-a"), forParticipant: "peer-a")
        state.clearRemoteLightVideoTracks()
        XCTAssertTrue(state.remoteLightVideoTracks.isEmpty)

        state.setRemoteLightVideoTrack(makeTrack("light-a"), forParticipant: "peer-a")
        state.reset()
        XCTAssertTrue(state.remoteLightVideoTracks.isEmpty)
    }

    func testANilCompanionRemovesOnlyThatEntry() {
        let state = CallUiState()
        state.setRemoteLightVideoTrack(makeTrack("light-a"), forParticipant: "peer-a")
        state.setRemoteLightVideoTrack(makeTrack("light-b"), forParticipant: "peer-b")

        state.setRemoteLightVideoTrack(nil, forParticipant: "peer-a")

        XCTAssertEqual(Array(state.remoteLightVideoTracks.keys), ["peer-b"])
    }
}

// MARK: - The media log

@MainActor
final class CallMediaCountersTests: XCTestCase {

    func testTheShownLineHasTheWebClientsFields() {
        let counters = LayerDisplayCounters(
            lightSwitches: 3, lightMs: 4200, stallMaxMs: 310, stallTotalMs: 780)
        XCTAssertEqual(
            CallMediaCounters.describeShown("call media interval", peerId: "p7", counters),
            "call media interval shown peer=p7 light_switches=3 light_ms=4200"
                + " stall_max_ms=310 stall_total_ms=780")
        XCTAssertEqual(
            CallMediaCounters.describeShown("call media total", peerId: "p7", .zero),
            "call media total shown peer=p7 light_switches=0 light_ms=0"
                + " stall_max_ms=0 stall_total_ms=0")
    }

    private func inbound(
        _ id: String, track: String?, packets: Int, decoded: Int, kind: String = "video"
    ) -> CallMediaStatEntry {
        var values: [String: NSObject] = [
            "kind": kind as NSString,
            "packetsReceived": NSNumber(value: packets),
            "packetsLost": NSNumber(value: 2),
            "nackCount": NSNumber(value: 1),
            "pliCount": NSNumber(value: 1),
            "freezeCount": NSNumber(value: 1),
            "totalFreezesDuration": NSNumber(value: 0.25),
            "framesDecoded": NSNumber(value: decoded),
            "framesDropped": NSNumber(value: 0),
            "jitterBufferDelay": NSNumber(value: 3.0),
            "jitterBufferEmittedCount": NSNumber(value: decoded),
        ]
        if let track { values["trackIdentifier"] = track as NSString }
        return CallMediaStatEntry(id: id, type: "inbound-rtp", values: values)
    }

    func testAVideoReceiverIsNamedMainOrLightByItsTrack() {
        let reading = CallMediaCounters.read(
            [
                inbound("IT2", track: "light-track", packets: 50, decoded: 30),
                inbound("IT1", track: "main-track", packets: 500, decoded: 60),
                inbound("IT3", track: nil, packets: 5, decoded: 0),
                inbound("IT0", track: "voice", packets: 100, decoded: 0, kind: "audio"),
            ],
            lightTrackIds: ["light-track"])

        XCTAssertEqual(reading.inbound.map(\.id), ["IT1", "IT2", "IT3"])
        XCTAssertEqual(reading.inbound.map(\.counters.layer), [.main, .light, .unknown])
    }

    func testTheReceiverLineEndsWithItsLayer() {
        let reading = CallMediaCounters.read(
            [
                inbound("IT1", track: "main-track", packets: 500, decoded: 60),
                inbound("IT2", track: "light-track", packets: 50, decoded: 30),
            ],
            lightTrackIds: ["light-track"])

        let lines = CallMediaCounters.describeInbound("call media total", before: nil, now: reading)

        XCTAssertEqual(
            lines,
            [
                "call media total video#1 pkts=500 lost=2 nack=1 jb_ms=50 pli=1 freezes=1"
                    + " freeze_s=0.25 decoded=60 dropped=0 layer=main",
                "call media total video#2 pkts=50 lost=2 nack=1 jb_ms=100 pli=1 freezes=1"
                    + " freeze_s=0.25 decoded=30 dropped=0 layer=light",
            ])
    }

    func testAnIntervalLineIsTheChangeSinceTheLastReading() {
        let before = CallMediaCounters.read(
            [inbound("IT1", track: "main-track", packets: 500, decoded: 60)], lightTrackIds: [])
        let now = CallMediaCounters.read(
            [inbound("IT1", track: "main-track", packets: 740, decoded: 90)], lightTrackIds: [])

        XCTAssertEqual(
            CallMediaCounters.describeInbound("call media interval", before: before, now: now),
            [
                "call media interval video#1 pkts=240 lost=0 nack=0 jb_ms=0 pli=0 freezes=0"
                    + " freeze_s=0.00 decoded=30 dropped=0 layer=main"
            ])
    }

    func testTheSenderLineNamesEachLayerByItsRid() {
        let entries = ["f", "q"].map { rid in
            CallMediaStatEntry(
                id: "OT-\(rid)", type: "outbound-rtp",
                values: [
                    "kind": "video" as NSString,
                    "rid": rid as NSString,
                    "frameWidth": NSNumber(value: rid == "q" ? 320 : 1280),
                    "frameHeight": NSNumber(value: rid == "q" ? 180 : 720),
                    "framesPerSecond": NSNumber(value: 30),
                    "qualityLimitationReason": "none" as NSString,
                ])
        }
        let reading = CallMediaCounters.read(entries, lightTrackIds: [])

        XCTAssertEqual(
            CallMediaCounters.describeOutbound("call media interval", reading.outbound),
            [
                "call media interval send video mode=- size=1280x720 fps=30 limited_by=none rid=f",
                "call media interval send video mode=- size=320x180 fps=30 limited_by=none rid=q",
            ])
    }

    // MARK: The ledger

    /// A tile's counters as plain values.
    private final class FakeSource: LayerCounterSource {
        var interval: LayerDisplayCounters
        var total: LayerDisplayCounters
        init(interval: LayerDisplayCounters, total: LayerDisplayCounters) {
            self.interval = interval
            self.total = total
        }
        func drain() -> LayerDisplayCounters {
            defer { interval = .zero }
            return interval
        }
        func counters() -> LayerDisplayCounters { total }
    }

    private func counters(_ switches: UInt64, _ lightMs: UInt64, _ max: UInt64, _ total: UInt64)
        -> LayerDisplayCounters
    {
        LayerDisplayCounters(
            lightSwitches: switches, lightMs: lightMs, stallMaxMs: max, stallTotalMs: total)
    }

    func testAnIntervalIsTheTilesDrainAndTheTotalIsTheirCounters() {
        let ledger = LayerDisplayLedger()
        let a = FakeSource(interval: counters(1, 100, 200, 200), total: counters(4, 900, 400, 1000))
        let b = FakeSource(interval: counters(0, 0, 160, 160), total: counters(0, 0, 160, 160))
        _ = ledger.register(peerId: "pb", source: b)
        _ = ledger.register(peerId: "pa", source: a)

        let interval = ledger.takeInterval()
        XCTAssertEqual(interval.map(\.peerId), ["pa", "pb"])
        XCTAssertEqual(interval.map(\.counters), [counters(1, 100, 200, 200), counters(0, 0, 160, 160)])
        // Drained: the next interval starts from nothing.
        XCTAssertEqual(ledger.takeInterval().map(\.counters), [.zero, .zero])
        XCTAssertEqual(
            ledger.totals().map(\.counters), [counters(4, 900, 400, 1000), counters(0, 0, 160, 160)])
    }

    func testATileThatWentAwayLeavesItsCountBehind() {
        let ledger = LayerDisplayLedger()
        let first = FakeSource(interval: counters(1, 100, 200, 200), total: counters(2, 300, 250, 400))
        let token = ledger.register(peerId: "pa", source: first)
        ledger.unregister(token)
        // The same person drawn by a second tile (the stage after the grid).
        let second = FakeSource(interval: counters(1, 50, 300, 300), total: counters(1, 50, 300, 300))
        _ = ledger.register(peerId: "pa", source: second)

        XCTAssertEqual(ledger.takeInterval().map(\.counters), [counters(2, 150, 300, 500)])
        XCTAssertEqual(ledger.totals().map(\.counters), [counters(3, 350, 300, 700)])

        ledger.reset()
        // What gone tiles counted is forgotten; the tile on screen is not.
        XCTAssertEqual(ledger.totals().map(\.counters), [counters(1, 50, 300, 300)])
    }
}

// MARK: - The tile's controller, through the shared rule

final class RemoteLayerControllerTests: XCTestCase {

    /// A clock the test moves.
    private final class Clock {
        var now: UInt64 = 1_000
    }

    private func makeController(_ clock: Clock, light: Bool) -> (RemoteLayerController, () -> [ShownLayer]) {
        let controller = RemoteLayerController(
            lightAvailable: light, clock: { clock.now }, onMain: { $0() })
        var told: [ShownLayer] = []
        controller.onShown = { told.append($0) }
        return (controller, { told })
    }

    /// Both tracks present frames every 50 ms until `until`.
    private func playBoth(_ controller: RemoteLayerController, _ clock: Clock, until: UInt64) {
        while clock.now < until {
            controller.mainFrame()
            controller.lightFrame()
            controller.tick()
            clock.now += 50
        }
    }

    func testTheTileShowsTheLightTrackWhileTheMainOneStandsStillAndReturns() {
        let clock = Clock()
        let (controller, told) = makeController(clock, light: true)

        playBoth(controller, clock, until: 2_000)
        XCTAssertEqual(controller.shown, .full)
        XCTAssertTrue(told().isEmpty)

        // The main track stops; the light one keeps presenting.
        for _ in 0..<6 {
            controller.lightFrame()
            controller.tick()
            clock.now += 50
        }
        XCTAssertEqual(controller.shown, .light)
        XCTAssertEqual(told(), [.light])

        // The main track plays again: back to it after the steady second.
        playBoth(controller, clock, until: clock.now + 1_500)
        XCTAssertEqual(controller.shown, .full)
        XCTAssertEqual(told(), [.light, .full])

        let counted = controller.display.counters()
        XCTAssertEqual(counted.lightSwitches, 1)
        XCTAssertGreaterThan(counted.lightMs, 0)
    }

    func testATileWithoutACompanionStaysOnTheMainTrack() {
        let clock = Clock()
        let (controller, told) = makeController(clock, light: false)

        playBoth(controller, clock, until: 2_000)
        for _ in 0..<20 {
            controller.tick()
            clock.now += 50
        }

        XCTAssertEqual(controller.shown, .full)
        XCTAssertTrue(told().isEmpty)
        XCTAssertEqual(controller.display.counters().lightSwitches, 0)
    }

    func testTheCompanionGoingAwayReturnsTheTileToTheMainTrackAtOnce() {
        let clock = Clock()
        let (controller, told) = makeController(clock, light: true)
        playBoth(controller, clock, until: 2_000)
        for _ in 0..<6 {
            controller.lightFrame()
            controller.tick()
            clock.now += 50
        }
        XCTAssertEqual(controller.shown, .light)

        controller.setLightAvailable(false)

        XCTAssertEqual(controller.shown, .full)
        XCTAssertEqual(told(), [.light, .full])
    }

    func testTheBackgroundCountsNothingAndTheTileStartsAgainOnTheMainTrack() {
        let clock = Clock()
        let (controller, _) = makeController(clock, light: true)
        playBoth(controller, clock, until: 2_000)
        for _ in 0..<6 {
            controller.lightFrame()
            controller.tick()
            clock.now += 50
        }
        XCTAssertEqual(controller.shown, .light)
        let before = controller.display.counters()

        controller.suspend()
        // A minute in the background; reports made there are not passed on.
        for _ in 0..<1_200 {
            controller.lightFrame()
            controller.tick()
            clock.now += 50
        }
        controller.resume()

        XCTAssertEqual(controller.shown, .full)
        let after = controller.display.counters()
        XCTAssertEqual(after.lightSwitches, before.lightSwitches)
        XCTAssertEqual(after.stallTotalMs, before.stallTotalMs)
        XCTAssertLessThan(after.lightMs, before.lightMs + 1_000)
    }
}
