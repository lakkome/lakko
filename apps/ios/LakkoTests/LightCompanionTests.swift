// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
import WebRTC
@testable import Lakko

/// A remote person's further pictures on iOS (ADR-0067 step 5, ADR-0070
/// steps 1 and 5): the light companion and the middle track are kept beside
/// the main one, never as participants, and the light one is shown while the
/// picture a tile watches stands still.
///
/// What is covered here is everything that can be decided without a device:
/// the stream-id split, that a companion is never a participant and goes with
/// its peer, the lines of the media log, and the tile's controller driven
/// through the shared rule (`LayerDisplay` of `lakko-ui-rules`, over the
/// binding). Whether two Metal views keep drawing is not. The sending side is
/// `GroupCallSendersTests`.

// MARK: - The stream id

final class LightStreamTests: XCTestCase {

    func testAStreamIdEndingInTheSuffixIsThatPeersCompanion() {
        XCTAssertEqual(
            LightStream.split("p000000000000000518db68c125428388~light"),
            LightStream.RemoteStreamId(
                peerId: "p000000000000000518db68c125428388", role: .light))
    }

    func testAnOrdinaryStreamIdIsItsPeer() {
        XCTAssertEqual(
            LightStream.split("p0001"), LightStream.RemoteStreamId(peerId: "p0001", role: .main))
    }

    func testTheBareSuffixNamesNoPeer() {
        XCTAssertEqual(
            LightStream.split("~light"), LightStream.RemoteStreamId(peerId: "~light", role: .main))
    }

    func testTheSuffixOnlyCountsAtTheEnd() {
        XCTAssertEqual(
            LightStream.split("p1~lightx"),
            LightStream.RemoteStreamId(peerId: "p1~lightx", role: .main))
        XCTAssertEqual(
            LightStream.split("p1~LIGHT"), LightStream.RemoteStreamId(peerId: "p1~LIGHT", role: .main))
    }

    func testSplitIsTheInverseOfLightStreamId() {
        XCTAssertEqual(LightStream.lightStreamId(of: "p7"), "p7~light")
        XCTAssertEqual(
            LightStream.split(LightStream.lightStreamId(of: "p7")),
            LightStream.RemoteStreamId(peerId: "p7", role: .light))
    }

    func testAStreamIdEndingInTheMiddleSuffixIsThatPeersMiddleTrack() {
        XCTAssertEqual(
            LightStream.split("p7~mid"), LightStream.RemoteStreamId(peerId: "p7", role: .middle))
        XCTAssertEqual(LightStream.middleStreamId(of: "p7"), "p7~mid")
        // One suffix is read, at the end.
        XCTAssertEqual(
            LightStream.split("p7~mid~light"),
            LightStream.RemoteStreamId(peerId: "p7~mid", role: .light))
        XCTAssertEqual(
            LightStream.split("~mid"), LightStream.RemoteStreamId(peerId: "~mid", role: .main))
        XCTAssertEqual(
            LightStream.split("p7~middle"),
            LightStream.RemoteStreamId(peerId: "p7~middle", role: .main))
    }

    func testARemoteVideoTrackIsRoutedByItsStreamId() {
        XCTAssertEqual(RemoteVideoRoute.of(streamId: "p7"), .main(streamId: "p7"))
        XCTAssertEqual(RemoteVideoRoute.of(streamId: "p7~light"), .light(peerId: "p7"))
        XCTAssertEqual(RemoteVideoRoute.of(streamId: "p7~mid"), .middle(peerId: "p7"))
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

    func testAMiddleTrackIsNeverAParticipantAndGoesWithItsPeer() {
        let state = CallUiState()
        state.setParticipants([remote("peer-a"), remote("peer-b")])
        let main = makeTrack("main-a")
        let middleB = makeTrack("middle-b")
        state.setRemoteVideoTrack(main, forParticipant: "peer-a")
        state.setRemoteMiddleVideoTrack(makeTrack("middle-a"), forParticipant: "peer-a")
        state.setRemoteMiddleVideoTrack(middleB, forParticipant: "peer-b")

        XCTAssertEqual(state.participants.map(\.id), ["peer-a", "peer-b"])
        XCTAssertEqual(state.participants.map(\.camOn), [false, false])
        XCTAssertTrue(state.remoteVideoTracks["peer-a"] === main)
        XCTAssertTrue(state.remoteLightVideoTracks.isEmpty)

        state.participantLeft("peer-a")
        XCTAssertNil(state.remoteMiddleVideoTracks["peer-a"])
        XCTAssertTrue(state.remoteMiddleVideoTracks["peer-b"] === middleB)

        state.clearRemoteVideoCopies()
        XCTAssertTrue(state.remoteMiddleVideoTracks.isEmpty)
    }

    func testWhatTheSfuGivesIsKeptByParticipantAndGoesWithThePeer() {
        let state = CallUiState()
        state.setParticipants([remote("peer-a"), remote("peer-b")])
        state.setRemoteVideoGiven(byPeerId: ["a": .light, "b": .middle])
        XCTAssertEqual(state.remoteVideoGiven, ["peer-a": .light, "peer-b": .middle])

        // A new set replaces the whole previous one.
        state.setRemoteVideoGiven(byPeerId: ["b": .main])
        XCTAssertEqual(state.remoteVideoGiven, ["peer-b": .main])

        state.participantLeft("peer-b")
        XCTAssertTrue(state.remoteVideoGiven.isEmpty)

        state.setRemoteVideoGiven(byPeerId: ["a": .light])
        state.clearRemoteVideoCopies()
        XCTAssertTrue(state.remoteVideoGiven.isEmpty)
    }

    func testATilesReportReachesTheCallUnderTheSfuPeerId() {
        let state = CallUiState()
        final class Tile {}
        let tile = Tile()
        var reports: [(ObjectIdentifier, String, TileVideoQuality?)] = []
        // No live call: a report is dropped.
        state.reportTileVideoQuality(
            tile: ObjectIdentifier(tile), participantId: "peer-a", quality: .main)
        state.tileVideoQualitySink = { reports.append(($0, $1, $2)) }
        state.reportTileVideoQuality(
            tile: ObjectIdentifier(tile), participantId: "peer-a", quality: .middle)
        state.reportTileVideoQuality(
            tile: ObjectIdentifier(tile), participantId: "peer-a", quality: nil)
        XCTAssertEqual(reports.map(\.1), ["a", "a"])
        XCTAssertEqual(reports.map(\.2), [.middle, nil])
        XCTAssertEqual(reports.map(\.0), [ObjectIdentifier(tile), ObjectIdentifier(tile)])
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
            CallMediaCounters.describeShown(
                "call media interval", peerId: "p7", counters, want: .middle, given: .light),
            "call media interval shown peer=p7 want=middle given=light light_switches=3"
                + " light_ms=4200 stall_max_ms=310 stall_total_ms=780")
        XCTAssertEqual(
            CallMediaCounters.describeShown("call media total", peerId: "p7", .zero, want: .main),
            "call media total shown peer=p7 want=main given=- light_switches=0 light_ms=0"
                + " stall_max_ms=0 stall_total_ms=0")
    }

    func testTheShownLinesAskWhatIsWantedAndGivenOfEachPeer() {
        XCTAssertEqual(
            CallMediaCounters.describeShownPeers(
                "call media interval",
                [(peerId: "a", counters: .zero), (peerId: "b", counters: .zero)],
                wantOf: { $0 == "a" ? .main : .light },
                givenOf: { $0 == "a" ? .middle : nil }),
            [
                "call media interval shown peer=a want=main given=middle light_switches=0"
                    + " light_ms=0 stall_max_ms=0 stall_total_ms=0",
                "call media interval shown peer=b want=light given=- light_switches=0"
                    + " light_ms=0 stall_max_ms=0 stall_total_ms=0",
            ])
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

    func testAVideoReceiverIsNamedMainMiddleOrLightByItsTrack() {
        let reading = CallMediaCounters.read(
            [
                inbound("IT2", track: "light-track", packets: 50, decoded: 30),
                inbound("IT1", track: "main-track", packets: 500, decoded: 60),
                inbound("IT3", track: nil, packets: 5, decoded: 0),
                inbound("IT4", track: "middle-track", packets: 90, decoded: 40),
                inbound("IT0", track: "voice", packets: 100, decoded: 0, kind: "audio"),
            ],
            lightTrackIds: ["light-track"], middleTrackIds: ["middle-track"])

        XCTAssertEqual(reading.inbound.map(\.id), ["IT1", "IT2", "IT3", "IT4"])
        XCTAssertEqual(
            reading.inbound.map(\.counters.layer), [.main, .light, .unknown, .middle])
    }

    private func inboundAudio(
        _ id: String, packets: Int, samples: Int, concealed: Int
    ) -> CallMediaStatEntry {
        CallMediaStatEntry(
            id: id, type: "inbound-rtp",
            values: [
                "kind": "audio" as NSString,
                "packetsReceived": NSNumber(value: packets),
                "totalSamplesReceived": NSNumber(value: samples),
                "concealedSamples": NSNumber(value: concealed),
            ])
    }

    func testAnAudioReceiverHasItsOwnLineWithTheConcealedShare() {
        let reading = CallMediaCounters.read(
            [
                inbound("IT1", track: "main-track", packets: 500, decoded: 60),
                inboundAudio("IA1", packets: 50, samples: 96_000, concealed: 960),
            ],
            lightTrackIds: [])

        XCTAssertEqual(
            CallMediaCounters.describeInbound("call media total", before: nil, now: reading),
            [
                "call media total audio#1 pkts=50 lost=0 nack=0 jb_ms=- concealed=1.0%",
                "call media total video#1 pkts=500 lost=2 nack=1 jb_ms=50 pli=1 freezes=1"
                    + " freeze_s=0.25 decoded=60 dropped=0 layer=main",
            ])
    }

    func testTheOneToOneLogWritesEachIntervalAndTheWholeCallOnce() {
        var log = InboundCounterLog()
        XCTAssertEqual(
            log.interval([inboundAudio("IA1", packets: 50, samples: 96_000, concealed: 960)]),
            ["call media interval audio#1 pkts=50 lost=0 nack=0 jb_ms=- concealed=1.0%"])
        XCTAssertEqual(
            log.interval([inboundAudio("IA1", packets: 150, samples: 192_000, concealed: 5_760)]),
            ["call media interval audio#1 pkts=100 lost=0 nack=0 jb_ms=- concealed=5.0%"])
        XCTAssertEqual(
            log.total(),
            ["call media total audio#1 pkts=150 lost=0 nack=0 jb_ms=- concealed=3.0%"])
        XCTAssertEqual(log.total(), [])
    }

    func testTheOneToOneLogSaysNothingForACallThatWasNeverRead() {
        var log = InboundCounterLog()
        XCTAssertEqual(log.total(), [])
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

    private func outbound(
        _ id: String, mid: String?, source: String?, width: Int, height: Int, bytes: Int
    ) -> CallMediaStatEntry {
        var values: [String: NSObject] = [
            "kind": "video" as NSString,
            "frameWidth": NSNumber(value: width),
            "frameHeight": NSNumber(value: height),
            "framesPerSecond": NSNumber(value: 30),
            "qualityLimitationReason": "none" as NSString,
            "bytesSent": NSNumber(value: bytes),
        ]
        if let mid { values["mid"] = mid as NSString }
        if let source { values["mediaSourceId"] = source as NSString }
        return CallMediaStatEntry(id: id, type: "outbound-rtp", values: values)
    }

    private func source(_ id: String, track: String) -> CallMediaStatEntry {
        CallMediaStatEntry(
            id: id, type: "media-source",
            values: ["kind": "video" as NSString, "trackIdentifier": track as NSString])
    }

    /// Each camera sender is named by the media line it is sent on.
    func testASenderIsNamedByItsMid() {
        let reading = CallMediaCounters.read(
            [
                outbound("OT3", mid: "3", source: nil, width: 320, height: 180, bytes: 10),
                outbound("OT1", mid: "1", source: nil, width: 1280, height: 720, bytes: 30),
                outbound("OT4", mid: "4", source: nil, width: 640, height: 360, bytes: 20),
            ],
            lightTrackIds: [],
            senderOfMid: ["1": .main, "3": .light, "4": .middle])
        XCTAssertEqual(reading.outbound.map(\.sender), [.main, .middle, .light])
        XCTAssertEqual(reading.outbound.map(\.frameWidth), [1280, 640, 320])
    }

    /// Where the report carries no `mid` the sender is read from the track
    /// behind the stream's media source.
    func testASenderWithoutAMidIsNamedByItsSourceTrack() {
        let reading = CallMediaCounters.read(
            [
                source("SV1", track: CameraSender.trackId(of: .light)),
                source("SV2", track: CameraSender.trackId(of: .main)),
                source("SV3", track: "something-else"),
                outbound("OT1", mid: nil, source: "SV1", width: 320, height: 180, bytes: 1),
                outbound("OT2", mid: nil, source: "SV2", width: 1280, height: 720, bytes: 1),
                outbound("OT3", mid: nil, source: "SV3", width: 8, height: 8, bytes: 1),
            ],
            lightTrackIds: [])
        XCTAssertEqual(reading.outbound.map(\.sender), [.main, .light, nil])
        // A media source is not a stream of its own.
        XCTAssertTrue(reading.inbound.isEmpty)
        XCTAssertEqual(reading.outbound.count, 3)
    }

    func testTheSenderLinesNameEachSenderItsBitrateAndItsActivity() {
        let mids: [String: CameraSenderKind] = ["1": .main, "3": .light, "4": .middle]
        let before = CallMediaCounters.read(
            [
                outbound("OT1", mid: "1", source: nil, width: 1280, height: 720, bytes: 100_000),
                outbound("OT3", mid: "3", source: nil, width: 320, height: 180, bytes: 10_000),
            ],
            lightTrackIds: [], senderOfMid: mids)
        let now = CallMediaCounters.read(
            [
                outbound("OT1", mid: "1", source: nil, width: 1280, height: 720, bytes: 350_000),
                outbound("OT3", mid: "3", source: nil, width: 320, height: 180, bytes: 45_000),
            ],
            lightTrackIds: [], senderOfMid: mids)

        let lines = CallMediaCounters.describeOutbound(
            "call media interval", before: before, now: now, elapsedMs: 2_000,
            activityOf: { kind in
                switch kind {
                case .main: return OutboundActivity(needed: true, active: true)
                case .middle: return OutboundActivity(needed: false, active: false)
                case .light: return OutboundActivity(needed: nil, active: true)
                }
            },
            existing: [.main, .middle, .light])

        XCTAssertEqual(
            lines,
            [
                "call media interval send video mode=- size=1280x720 fps=30 limited_by=none"
                    + " kbps=1000 sender=main needed=yes active=yes",
                "call media interval send video mode=- size=320x180 fps=30 limited_by=none"
                    + " kbps=140 sender=light needed=- active=yes",
                // A sender the report carries no stream for still has its line.
                "call media interval send video mode=- size=- fps=- limited_by=-"
                    + " kbps=- sender=middle needed=no active=no",
            ])
    }

    func testASenderLineHasNoBitrateWithoutAnEarlierReading() {
        let now = CallMediaCounters.read(
            [outbound("OT1", mid: "1", source: nil, width: 1280, height: 720, bytes: 350_000)],
            lightTrackIds: [], senderOfMid: ["1": .main])
        XCTAssertEqual(
            CallMediaCounters.describeOutbound("call media interval", now: now),
            [
                "call media interval send video mode=- size=1280x720 fps=30 limited_by=none"
                    + " kbps=- sender=main"
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

// MARK: - A heavy track that was just bound

/// After each bind of a heavy track the heavy view stays out of sight until
/// that binding's first frame (the web client's `BindingFrameGate`): the view
/// cannot be cleared and holds whatever it drew before the bind.
final class HeavyBindingGateTests: XCTestCase {

    func testABoundTrackIsHiddenUntilItsFirstFrameIsRevealed() {
        var gate = HeavyBindingGate()
        let generation = gate.bind(hasTrack: true)
        XCTAssertFalse(gate.visible)

        XCTAssertTrue(gate.firstFrame(generation: generation))
        XCTAssertFalse(gate.visible, "shown only after the reveal delay")
        XCTAssertTrue(gate.reveal(generation: generation))
        XCTAssertTrue(gate.visible)

        // Later frames and a second reveal change nothing.
        XCTAssertFalse(gate.firstFrame(generation: generation))
        XCTAssertFalse(gate.reveal(generation: generation))
        XCTAssertTrue(gate.visible)
    }

    func testARebindHidesTheViewAgainAndIgnoresTheEarlierBinding() {
        var gate = HeavyBindingGate()
        let first = gate.bind(hasTrack: true)
        XCTAssertTrue(gate.firstFrame(generation: first))
        XCTAssertTrue(gate.reveal(generation: first))
        XCTAssertTrue(gate.visible)

        // The tile shows the light picture alone, then asks for the heavy
        // one again.
        _ = gate.bind(hasTrack: false)
        XCTAssertFalse(gate.visible)
        let again = gate.bind(hasTrack: true)
        XCTAssertFalse(gate.visible)

        // A frame or a reveal of the earlier binding, arriving late, is not
        // this binding's.
        XCTAssertFalse(gate.firstFrame(generation: first))
        XCTAssertFalse(gate.reveal(generation: first))
        XCTAssertFalse(gate.visible)

        XCTAssertTrue(gate.firstFrame(generation: again))
        XCTAssertTrue(gate.reveal(generation: again))
        XCTAssertTrue(gate.visible)
    }

    func testARevealWithoutAFirstFrameShowsNothing() {
        var gate = HeavyBindingGate()
        let generation = gate.bind(hasTrack: true)
        XCTAssertFalse(gate.reveal(generation: generation))
        XCTAssertFalse(gate.visible)
    }

    func testNoTrackIsNeverVisible() {
        var gate = HeavyBindingGate()
        let generation = gate.bind(hasTrack: false)
        XCTAssertFalse(gate.firstFrame(generation: generation))
        XCTAssertFalse(gate.reveal(generation: generation))
        XCTAssertFalse(gate.visible)
    }

    func testTheLatchLetsOneCallThrough() {
        let latch = FirstFrameLatch()
        XCTAssertTrue(latch.take())
        XCTAssertFalse(latch.take())
        XCTAssertFalse(latch.take())
    }
}

final class LayeredPictureArrangementTests: XCTestCase {

    func testAHeavyPictureNotYetPlayingIsBehindTheLightOneWhateverTheRuleSays() {
        for shown in [ShownLayer.full, .light] {
            XCTAssertEqual(
                LayeredPictureArrangement(shown: shown, heavyVisible: false, hasLight: true).front,
                .light)
        }
    }

    func testAPlayingHeavyPictureFollowsTheRule() {
        XCTAssertEqual(
            LayeredPictureArrangement(shown: .full, heavyVisible: true, hasLight: true).front,
            .heavy)
        XCTAssertEqual(
            LayeredPictureArrangement(shown: .light, heavyVisible: true, hasLight: true).front,
            .light)
    }

    func testWithoutALightPictureTheHeavyOneIsInFrontAndHiddenUntilItPlays() {
        let waiting = LayeredPictureArrangement(shown: .full, heavyVisible: false, hasLight: false)
        XCTAssertEqual(waiting.front, .heavy)
        XCTAssertFalse(waiting.heavyVisible)
        let playing = LayeredPictureArrangement(shown: .full, heavyVisible: true, hasLight: false)
        XCTAssertEqual(playing.front, .heavy)
        XCTAssertTrue(playing.heavyVisible)
    }
}

final class RemoteLayerControllerResyncTests: XCTestCase {

    func testAReplacedHeavyTrackStartsTheRuleAgainOnTheFullPicture() {
        var now: UInt64 = 1_000
        let controller = RemoteLayerController(
            lightAvailable: true, clock: { now }, onMain: { $0() })
        var told: [ShownLayer] = []
        controller.onShown = { told.append($0) }
        while now < 2_000 {
            controller.mainFrame()
            controller.lightFrame()
            controller.tick()
            now += 50
        }
        for _ in 0..<6 {
            controller.lightFrame()
            controller.tick()
            now += 50
        }
        XCTAssertEqual(controller.shown, .light)

        controller.resync()

        XCTAssertEqual(controller.shown, .full)
        XCTAssertEqual(told, [.light, .full])
    }

    func testInTheBackgroundAResyncTellsNothing() {
        var now: UInt64 = 1_000
        let controller = RemoteLayerController(
            lightAvailable: true, clock: { now }, onMain: { $0() })
        var told: [ShownLayer] = []
        controller.onShown = { told.append($0) }
        while now < 2_000 {
            controller.mainFrame()
            controller.lightFrame()
            controller.tick()
            now += 50
        }
        for _ in 0..<6 {
            controller.lightFrame()
            controller.tick()
            now += 50
        }
        controller.suspend()

        controller.resync()

        XCTAssertEqual(controller.shown, .light)
        XCTAssertEqual(told, [.light])
    }
}

/// The tile itself, over real libwebrtc tracks fed by hand: a frame pushed into
/// a track's source reaches the tile's sinks as a decoded frame would.
final class LayeredRemoteVideoUIViewBindingTests: XCTestCase {

    private static let factory = RTCPeerConnectionFactory()

    private struct Feed {
        let source: RTCVideoSource
        let track: RTCVideoTrack
    }

    private func feed(_ id: String) -> Feed {
        let source = Self.factory.videoSource()
        return Feed(source: source, track: Self.factory.videoTrack(with: source, trackId: id))
    }

    private var frameCount: Int64 = 0

    /// Push one 64x64 frame into `feed`'s track.
    private func push(_ feed: Feed) {
        var pixelBuffer: CVPixelBuffer?
        CVPixelBufferCreate(
            kCFAllocatorDefault, 64, 64, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            nil, &pixelBuffer)
        guard let pixelBuffer else { return XCTFail("no pixel buffer") }
        frameCount += 1
        let frame = RTCVideoFrame(
            buffer: RTCCVPixelBuffer(pixelBuffer: pixelBuffer),
            rotation: ._0,
            timeStampNs: Int64(DispatchTime.now().uptimeNanoseconds) + frameCount)
        feed.source.capturer(RTCVideoCapturer(delegate: feed.source), didCapture: frame)
    }

    /// Let the main queue run past the reveal delay.
    private func settle() {
        let done = expectation(description: "settle")
        DispatchQueue.main.asyncAfter(
            deadline: .now() + .milliseconds(Int(HeavyBindingGate.revealDelayMs) + 100)
        ) { done.fulfill() }
        wait(for: [done], timeout: 2)
    }

    @MainActor
    func testAHeavyTrackBoundAgainStaysBehindTheLightOneUntilItPlaysAgain() {
        let main = feed("main")
        let light = feed("light")
        let view = LayeredRemoteVideoUIView(peerId: "p1", contentMode: .scaleAspectFill)
        defer { view.teardown() }

        view.setTracks(heavy: main.track, light: light.track)
        XCTAssertEqual(view.arrangement.front, .light, "nothing played yet")
        XCTAssertFalse(view.arrangement.heavyVisible)

        push(main)
        push(light)
        settle()
        XCTAssertTrue(view.arrangement.heavyVisible)
        XCTAssertEqual(view.arrangement.front, .heavy)

        // The tile shows the light picture alone for a while.
        view.setTracks(heavy: nil, light: light.track)
        push(light)
        settle()
        XCTAssertEqual(view.arrangement.front, .light)
        XCTAssertFalse(view.arrangement.heavyVisible)

        // It asks for the heavy picture again: the view still holds the frame
        // it drew before, so it stays behind until the track plays.
        view.setTracks(heavy: main.track, light: light.track)
        settle()
        XCTAssertEqual(view.arrangement.front, .light)
        XCTAssertFalse(view.arrangement.heavyVisible)

        push(main)
        push(light)
        settle()
        XCTAssertTrue(view.arrangement.heavyVisible)
        XCTAssertEqual(view.arrangement.front, .heavy)
    }

    @MainActor
    func testWithoutACompanionAReplacedHeavyTrackIsTransparentUntilItPlays() {
        let main = feed("main")
        let middle = feed("middle")
        let view = LayeredRemoteVideoUIView(peerId: "p2", contentMode: .scaleAspectFill)
        defer { view.teardown() }

        view.setTracks(heavy: main.track, light: nil)
        push(main)
        settle()
        XCTAssertTrue(view.arrangement.heavyVisible)

        view.setTracks(heavy: middle.track, light: nil)
        XCTAssertFalse(view.arrangement.heavyVisible)
        XCTAssertEqual(view.arrangement.front, .heavy)

        // A frame of the track it left is not this binding's.
        push(main)
        settle()
        XCTAssertFalse(view.arrangement.heavyVisible)

        push(middle)
        settle()
        XCTAssertTrue(view.arrangement.heavyVisible)
    }
}
