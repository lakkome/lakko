// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// The remote microphone mute mark (`docs/ux/group-call-grid.md` 7.2): each
/// participant says whether its microphone is on, and every other tile shows
/// the mute mark from it.
///
/// Group and link calls carry it in the sealed media state on the SFU relay
/// (`lakko_contacts::media_state` version 2); a one-to-one carries it in the
/// `mediaState` call signal (`CallSignalingPayload` field 18, tag `0x92 0x01`),
/// sent only once the call is connected and never a wake.
///
/// Covered without a device: the session's send and receive of the group
/// envelope (through its test seam, not a socket), the adapter's tiles, the
/// one-to-one announcement rule, the one-to-one receive, routing and the byte
/// peeks. Not covered: a live call, the mark on screen.
@MainActor
final class MicrophoneStateTests: XCTestCase {

    private let seed = Data(repeating: 0x42, count: 32)
    private let callId = Data(repeating: 0x6E, count: 16)
    private let otherCallId = Data(repeating: 0x6F, count: 16)

    // MARK: - Group: the session

    private func config(audio: Bool = true, video: Bool = true) -> SfuCallSession.Config {
        SfuCallSession.Config(
            sfuBaseUrl: "wss://sfu.invalid", jwt: "jwt", seed: seed, localLeaf: 0,
            audio: audio, video: video, deferPublish: true)
    }

    /// What another participant of the same call seals: the same seed gives
    /// the same relay key.
    private func sealedByPeer(_ plaintext: Data) throws -> String {
        let relay = CallRelay()
        relay.setSeed(seed)
        return sfuEncodePayload(payload: try XCTUnwrap(relay.seal(plaintext)))
    }

    /// Open what this session sent, as another participant would.
    private func opened(_ sealed: Data) -> CallRelay.Opened? {
        let relay = CallRelay()
        relay.setSeed(seed)
        return relay.open(sealed)
    }

    func testAMediaStateFiresCameraAndMicrophone() throws {
        var cameras: [String: Bool] = [:]
        var microphones: [String: Bool] = [:]
        var callbacks = SfuCallSession.Callbacks()
        callbacks.onRemoteMediaState = { from, on in cameras[from] = on }
        callbacks.onRemoteMicrophoneState = { from, on in microphones[from] = on }
        let session = SfuCallSession(config: config(), callbacks: callbacks)

        session.dispatchSignalRelay(
            from: "a", payloadB64: try sealedByPeer(composeMediaState(cameraOn: true, microphoneOn: false)))
        XCTAssertEqual(cameras["a"], true)
        XCTAssertEqual(microphones["a"], false)

        session.dispatchSignalRelay(
            from: "a", payloadB64: try sealedByPeer(composeMediaState(cameraOn: false, microphoneOn: true)))
        XCTAssertEqual(cameras["a"], false)
        XCTAssertEqual(microphones["a"], true)
    }

    func testAnUnsealedMediaStateFiresNothing() {
        var fired = 0
        var callbacks = SfuCallSession.Callbacks()
        callbacks.onRemoteMediaState = { _, _ in fired += 1 }
        callbacks.onRemoteMicrophoneState = { _, _ in fired += 1 }
        let session = SfuCallSession(config: config(), callbacks: callbacks)
        session.dispatchSignalRelay(
            from: "a",
            payloadB64: sfuEncodePayload(payload: composeMediaState(cameraOn: true, microphoneOn: false)))
        XCTAssertEqual(fired, 0)
    }

    func testTheVersionOneLayoutIsRefused() {
        // Domain, version 0x01, camera on: the camera-only layout.
        var v1 = Data("lakko/call-media-state/v1".utf8)
        v1.append(0x00)
        v1.append(0x01)
        v1.append(0x01)
        XCTAssertThrowsError(try decodeCallMediaState(plaintext: v1))
        let v2 = composeMediaState(cameraOn: true, microphoneOn: false)
        XCTAssertEqual(v2.count, 28)
        let decoded = try? decodeCallMediaState(plaintext: v2)
        XCTAssertEqual(decoded?.cameraOn, true)
        XCTAssertEqual(decoded?.microphoneOn, false)
    }

    func testAMuteSendsOneEnvelopeAndAnUnchangedValueSendsNothing() {
        let session = SfuCallSession(config: config(), callbacks: SfuCallSession.Callbacks())
        var sent: [Data] = []
        session.mediaStateSendSeam = { sent.append($0) }

        session.setMicrophoneEnabled(true)
        XCTAssertTrue(sent.isEmpty, "unmuting an unmuted microphone says nothing")

        session.setMicrophoneEnabled(false)
        XCTAssertEqual(sent.count, 1)
        XCTAssertEqual(opened(sent[0]), .mediaState(cameraOn: true, microphoneOn: false))

        session.setMicrophoneEnabled(false)
        XCTAssertEqual(sent.count, 1, "an unchanged value sends nothing")

        session.setMicrophoneEnabled(true)
        XCTAssertEqual(sent.count, 2)
        XCTAssertEqual(opened(sent[1]), .mediaState(cameraOn: true, microphoneOn: true))
    }

    func testAMuteChosenBeforeCaptureIsInTheReannouncement() {
        let session = SfuCallSession(config: config(video: false), callbacks: SfuCallSession.Callbacks())
        // Before any capture: no peer connection exists, the choice is kept.
        session.setMicrophoneEnabled(false)
        XCTAssertFalse(session.microphoneEnabled)

        var sent: [Data] = []
        session.mediaStateSendSeam = { sent.append($0) }
        // What a peer join and the connect re-announce.
        session.broadcastMediaState()
        XCTAssertEqual(sent.count, 1)
        XCTAssertEqual(opened(sent[0]), .mediaState(cameraOn: false, microphoneOn: false))
    }

    func testACallPlacedWithoutAudioStartsMuted() {
        let session = SfuCallSession(config: config(audio: false), callbacks: SfuCallSession.Callbacks())
        XCTAssertFalse(session.microphoneEnabled)
    }

    // MARK: - Group: the adapter's tiles

    private func micOn(_ ui: CallUiState, _ id: String) -> Bool? {
        ui.participants.first(where: { $0.id == id })?.micOn
    }

    func testTheAdapterShowsTheMuteMarkFromTheMediaState() throws {
        let ui = CallUiState()
        let call = SfuCallUi(config: config(), ui: ui)
        let callbacks = call.session.callbacks

        callbacks.onPeerJoined?("a")
        XCTAssertEqual(micOn(ui, "peer-a"), true, "unmuted until the first media state")

        call.session.dispatchSignalRelay(
            from: "a", payloadB64: try sealedByPeer(composeMediaState(cameraOn: false, microphoneOn: false)))
        XCTAssertEqual(micOn(ui, "peer-a"), false)

        call.session.dispatchSignalRelay(
            from: "a", payloadB64: try sealedByPeer(composeMediaState(cameraOn: false, microphoneOn: true)))
        XCTAssertEqual(micOn(ui, "peer-a"), true)
    }

    func testAStateThatArrivesBeforeTheTileIsAppliedWhenTheTileIsBuilt() throws {
        let ui = CallUiState()
        let call = SfuCallUi(config: config(), ui: ui)
        let callbacks = call.session.callbacks

        call.session.dispatchSignalRelay(
            from: "b", payloadB64: try sealedByPeer(composeMediaState(cameraOn: false, microphoneOn: false)))
        XCTAssertNil(micOn(ui, "peer-b"), "no tile yet")

        callbacks.onPeerJoined?("b")
        XCTAssertEqual(micOn(ui, "peer-b"), false)

        // A peer that leaves and comes back starts from a clean slate.
        callbacks.onPeerLeft?("b")
        callbacks.onPeerJoined?("b")
        XCTAssertEqual(micOn(ui, "peer-b"), true)
    }

    func testSetParticipantMicIsANoOpForAnUnknownId() {
        let ui = CallUiState()
        ui.setParticipants([
            CallUiState.Participant(
                id: "peer-a", displayName: "", isLocal: false,
                micOn: true, camOn: false, handRaised: false,
                quality: .good, speaking: false, screenSharing: false),
        ])
        ui.setParticipantMic("peer-ghost", false)
        XCTAssertEqual(ui.participants.count, 1)
        XCTAssertEqual(micOn(ui, "peer-a"), true)
        ui.setParticipantMic("peer-a", false)
        XCTAssertEqual(micOn(ui, "peer-a"), false)
    }

    // MARK: - One-to-one: when a state is sent

    func testNothingIsSentBeforeTheCallIsConnected() {
        var rule = OneToOneMediaAnnouncement()
        XCTAssertNil(rule.next(connected: false, camera: true, microphone: false))
        XCTAssertNil(rule.next(connected: false, camera: false, microphone: true))
        XCTAssertNil(rule.announced)
    }

    func testTheFirstStateGoesOutOnConnectedAndEachChangeSendsOne() {
        var rule = OneToOneMediaAnnouncement()
        XCTAssertEqual(
            rule.next(connected: true, camera: true, microphone: true),
            .init(camera: true, microphone: true))
        XCTAssertNil(rule.next(connected: true, camera: true, microphone: true), "unchanged")
        // Mute.
        XCTAssertEqual(
            rule.next(connected: true, camera: true, microphone: false),
            .init(camera: true, microphone: false))
        // Unmute.
        XCTAssertEqual(
            rule.next(connected: true, camera: true, microphone: true),
            .init(camera: true, microphone: true))
        // Camera off.
        XCTAssertEqual(
            rule.next(connected: true, camera: false, microphone: true),
            .init(camera: false, microphone: true))
        XCTAssertNil(rule.next(connected: true, camera: false, microphone: true))
        // The next call starts from nothing told.
        rule.reset()
        XCTAssertEqual(
            rule.next(connected: true, camera: false, microphone: true),
            .init(camera: false, microphone: true))
    }

    func testAPreCaptureMuteHoldsAndIsTheFirstStateSent() {
        var outbound = 0
        let session = CallSession(callId: callId) { _, _ in outbound += 1 }
        session.setMicrophoneEnabled(false)
        XCTAssertFalse(session.micEnabled)
        XCTAssertEqual(outbound, 0, "nothing is said before the call is connected")
        var rule = OneToOneMediaAnnouncement()
        XCTAssertEqual(
            rule.next(connected: true, camera: false, microphone: session.micEnabled),
            .init(camera: false, microphone: false))
    }

    // MARK: - One-to-one: receive and routing

    func testTheRemoteStateIsAppliedOnlyForThisCall() {
        let session = CallSession(callId: callId) { _, _ in }
        XCTAssertTrue(session.remoteMicOn)
        XCTAssertTrue(session.remoteCameraOn)
        session.applyIncomingSignaling(.mediaState(callId: otherCallId, cameraOn: false, microphoneOn: false))
        XCTAssertTrue(session.remoteMicOn)
        XCTAssertTrue(session.remoteCameraOn)
        session.applyIncomingSignaling(.mediaState(callId: callId, cameraOn: false, microphoneOn: false))
        XCTAssertFalse(session.remoteMicOn)
        XCTAssertFalse(session.remoteCameraOn)
        session.applyIncomingSignaling(.mediaState(callId: callId, cameraOn: true, microphoneOn: false))
        XCTAssertFalse(session.remoteMicOn)
        XCTAssertTrue(session.remoteCameraOn)
    }

    func testTheBodyRoutesToTheCallInTheSlotOnly() {
        let body = FfiCallSignalingBody.mediaState(callId: callId, cameraOn: true, microphoneOn: false)
        XCTAssertEqual(CallSignalingRoute.of(body).forwardToOneToOne, true)
        XCTAssertNil(CallSignalingRoute.of(body).groupSignal)
        XCTAssertEqual(CallSession.callId(of: body), callId)
        let groupHex = "aa"
        let row = InboundCallRow(
            groupIdHex: groupHex, messageId: 1, createdAt: "2027-01-15T08:00:00Z", body: body)
        let active = CallHostRouting.ActiveCall(
            callIdHex: CallHostRouting.hex(callId), groupIdHex: groupHex)
        let otherActive = CallHostRouting.ActiveCall(
            callIdHex: CallHostRouting.hex(otherCallId), groupIdHex: groupHex)
        XCTAssertEqual(
            CallHostRouting.decide(row: row, nowMs: 0, active: active, endedCallIds: [], otherCallBusy: false),
            .apply)
        XCTAssertEqual(
            CallHostRouting.decide(row: row, nowMs: 0, active: nil, endedCallIds: [], otherCallBusy: false),
            .drop, "never a ring")
        XCTAssertEqual(
            CallHostRouting.decide(
                row: row, nowMs: 0, active: otherActive, endedCallIds: [], otherCallBusy: false),
            .drop)
    }

    // MARK: - One-to-one: the byte peeks

    func testTheBodyIsCallSignalingThatNeverWakes() throws {
        for camera in [true, false] {
            for microphone in [true, false] {
                let bytes = try composeCallSignaling(
                    body: .mediaState(callId: callId, cameraOn: camera, microphoneOn: microphone))
                XCTAssertEqual(Array(bytes.prefix(2)), [0x92, 0x01])
                XCTAssertTrue(ChatClient.looksLikeCallSignaling(bytes))
                XCTAssertTrue(nseLooksLikeCallSignaling(bytes))
                XCTAssertEqual(CallSignalingWake.bodyKind(bytes), .mediaState)
                XCTAssertFalse(CallSignalingWake.shouldWake(bytes))
                XCTAssertNil(CallSignalingWake.wakeKind(for: bytes, sessionAsks: true))
                // The parse round-trips.
                XCTAssertEqual(
                    try parseCallSignaling(plaintext: bytes),
                    .mediaState(callId: callId, cameraOn: camera, microphoneOn: microphone))
                // The tag needs its continuation byte and its framing.
                for wrong in [Data([0x92]), Data([0x92, 0x02, 0x0A]), Data(bytes.dropLast())] {
                    XCTAssertFalse(ChatClient.looksLikeCallSignaling(wrong), "\(Array(wrong))")
                    XCTAssertFalse(nseLooksLikeCallSignaling(wrong), "\(Array(wrong))")
                    XCTAssertNil(CallSignalingWake.bodyKind(wrong))
                }
            }
        }
        XCTAssertFalse(CallSignalingWake.sessionAsks(.mediaState, everConnected: true))
        XCTAssertFalse(CallSignalingWake.sessionAsks(.mediaState, everConnected: false))
    }
}
