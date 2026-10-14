// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// A call that is left or ends while one of its `await`s is pending must not
/// go on as if nothing happened: no microphone, no camera, no peer connection
/// and nothing on the wire for a call that is over.
///
/// The session tests here run without WebRTC: with the guards in place a
/// session that is over never reaches the code that would build a peer
/// connection, which is the property under test.
@MainActor
final class CallAwaitRaceTests: XCTestCase {

    private final class Outbox {
        var bodies: [FfiCallSignalingBody] = []
    }

    private let callId = Data(repeating: 0x0C, count: 16)
    private let seed = Data(repeating: 0x07, count: 32)

    private func session(_ outbox: Outbox) -> CallSession {
        let session = CallSession(callId: callId) { bytes in
            if let body = try? parseCallSignaling(plaintext: bytes) { outbox.bodies.append(body) }
        }
        session.setCallKey(seed: seed, localLeaf: 0)
        return session
    }

    /// Long enough for the dial's or the accept's own task to have run.
    private func settle() async throws {
        try await Task.sleep(nanoseconds: 300_000_000)
    }

    // MARK: - One-to-one

    /// `dial()` fetches the relay credential before it builds anything. A
    /// hang-up in that window ends the call; the dial must then build no
    /// peer connection and send no offer after the End.
    func testADialHungUpBeforeItsConnectionIsBuiltSendsNothingAfterTheEnd() async throws {
        let outbox = Outbox()
        let call = session(outbox)
        call.dial(audio: true, video: true)
        XCTAssertEqual(call.state, .outgoing)
        call.hangUp()
        XCTAssertEqual(call.state, .done)

        try await settle()
        XCTAssertEqual(call.state, .done)
        XCTAssertNil(call.localVideoTrack, "no camera for a call that is over")
        XCTAssertFalse(call.cameraEnabled)
        XCTAssertEqual(outbox.bodies.count, 1, "the End, and nothing after it")
        guard case .end? = outbox.bodies.first else { return XCTFail("the hang-up sends the End") }
    }

    /// `acceptCall()` fetches the relay credential too. The caller hanging up
    /// in that window ends the call, and it must stay ended: it used to come
    /// back as `connecting`, with the microphone on.
    func testAnAcceptedCallThatEndedMeanwhileDoesNotComeBackAsConnecting() async throws {
        let outbox = Outbox()
        let call = session(outbox)
        call.applyIncomingSignaling(.offer(
            callId: callId, sdp: "v=0\r\n", createdAtMs: 1,
            media: FfiCallMediaFlags(audio: true, video: true)))
        XCTAssertEqual(call.state, .ringing)
        call.acceptCall(withVideo: true)
        call.applyIncomingSignaling(.end(callId: callId, reason: .hungUp))
        XCTAssertEqual(call.state, .done)

        try await settle()
        XCTAssertEqual(call.state, .done, "an ended call stays ended")
        XCTAssertNil(call.localVideoTrack)
        XCTAssertFalse(call.cameraEnabled)
        XCTAssertTrue(outbox.bodies.isEmpty, "no answer for a call the caller ended")
    }

    /// A declined ring is over as well; a late accept task changes nothing.
    func testAnAcceptRacingADeclineStaysDeclined() async throws {
        let outbox = Outbox()
        let call = session(outbox)
        call.applyIncomingSignaling(.offer(
            callId: callId, sdp: "v=0\r\n", createdAtMs: 1,
            media: FfiCallMediaFlags(audio: true, video: false)))
        call.acceptCall()
        call.declineCall()
        try await settle()
        XCTAssertEqual(call.state, .done)
        XCTAssertEqual(call.endReason, .declined)
        XCTAssertEqual(outbox.bodies.count, 1, "the decline's End alone")
    }

    // MARK: - SFU session

    private func sfuSession(deferPublish: Bool = true) -> SfuCallSession {
        SfuCallSession(
            config: SfuCallSession.Config(
                sfuBaseUrl: "wss://sfu.invalid", jwt: "jwt", seed: seed, localLeaf: 0,
                audio: true, video: true, deferPublish: deferPublish),
            callbacks: SfuCallSession.Callbacks())
    }

    /// A link call publishes from a task started when its Welcome arrives. If
    /// the call was hung up before that task ran, the publish must capture
    /// nothing: the hang-up has already happened and would never stop it.
    func testAPublishAfterTheHangUpOpensNothing() async throws {
        let session = sfuSession()
        session.hangUp()
        XCTAssertEqual(session.state, .closed)
        try await session.publish(seed: seed, localLeaf: 0)
        XCTAssertEqual(session.state, .closed)
        let levels = await session.sampleAudioLevels()
        XCTAssertNil(levels, "no peer connection was built")
        XCTAssertEqual(session.currentVideoPublishState(), .off)
    }

    /// A session that never connected has no call to publish for either.
    func testAPublishBeforeTheConnectOpensNothing() async throws {
        let session = sfuSession()
        try await session.publish(seed: seed, localLeaf: 0)
        XCTAssertEqual(session.state, .idle)
        let levels = await session.sampleAudioLevels()
        XCTAssertNil(levels)
    }

    /// Turning the camera on for a call that is over starts no camera.
    func testCameraOnAfterTheHangUpStartsNoCamera() async throws {
        let session = sfuSession()
        session.hangUp()
        try await session.publishVideo()
        XCTAssertEqual(session.currentVideoPublishState(), .off)
    }

    // MARK: - The camera capturer's start and stop

    func testACaptureStartsAndStopsOnce() {
        var gate = CameraCaptureGate()
        XCTAssertTrue(gate.mayStart)
        XCTAssertTrue(gate.started())
        XCTAssertTrue(gate.running)
        XCTAssertTrue(gate.release(), "a running capturer is stopped")
        XCTAssertFalse(gate.running)
        XCTAssertFalse(gate.release(), "and only once")
    }

    /// The owner's stop arrives while the capturer is still starting: there
    /// is nothing to stop yet, and the start that completes afterwards must
    /// be stopped rather than leave the camera on.
    func testAStopDuringTheStartStopsTheCameraWhenItComesUp() {
        var gate = CameraCaptureGate()
        XCTAssertFalse(gate.release(), "nothing is running yet")
        XCTAssertFalse(gate.started(), "the start that completes now is stopped again")
        XCTAssertFalse(gate.running)
        XCTAssertFalse(gate.mayStart)
    }

    /// A camera switch stops the capturer and starts it on the other camera.
    /// A call that ended between the two is not started again.
    func testACameraSwitchDoesNotRestartACaptureTheCallLetGo() {
        var gate = CameraCaptureGate()
        XCTAssertTrue(gate.started())
        XCTAssertTrue(gate.halt(), "the switch stops the running capturer")
        XCTAssertTrue(gate.mayStart, "and may start it on the other camera")
        XCTAssertFalse(gate.release(), "the call ends between the two: nothing runs")
        XCTAssertFalse(gate.mayStart, "the switch's restart is refused")
        XCTAssertFalse(gate.started())
    }

    /// The switch on a call that goes on restarts as before.
    func testACameraSwitchRestartsWhileTheCallGoesOn() {
        var gate = CameraCaptureGate()
        XCTAssertTrue(gate.started())
        XCTAssertTrue(gate.halt())
        XCTAssertFalse(gate.halt(), "a capturer that is not running is not stopped twice")
        XCTAssertTrue(gate.started())
        XCTAssertTrue(gate.running)
    }
}
