// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// A `/c/` link opened after its call ended (`docs/ux/call-links.md` section
/// 5). Seen live on 2026-11-24: the SFU answered `CallEnded` in place of the
/// Welcome, the session failed, the host folded that as an ordinary `ended`
/// and the landing, which sends a signed-in person back into the app when a
/// call they were in ends, closed itself: Join looked like it did nothing.
@MainActor
final class LinkCallAlreadyEndedTests: XCTestCase {

    private let callId = Data(repeating: 0xC1, count: 16)

    private var decoded: JoinCallLandingViewModel.Decoded {
        JoinCallLandingViewModel.Decoded(
            callId: callId,
            ownerAccountId: Data(repeating: 0x0A, count: 16),
            ownerSignatureKey: Data(repeating: 0x0B, count: 32),
            ownerFingerprint: "",
            mediaVideo: false,
            valid: true,
            isOwner: false)
    }

    private func landing(
        callbacks: EndedBox<[MultiJoinerCall.Callbacks]>,
        probe: @escaping JoinCallLandingViewModel.RoomStateProbe = { _ in nil }
    ) -> JoinCallLandingViewModel {
        let host = LinkCallHost(ports: LinkCallHost.Ports(
            session: { (Data(repeating: 1, count: 64), "token") },
            sfuBaseUrl: { "wss://sfu.test" },
            microphoneGranted: { true },
            cameraGranted: { _ in true },
            startCall: { _, _, _, _, _, cbs in
                callbacks.value.append(cbs)
                return FakeLinkCallHandle()
            },
            ownIdentity: { (Data(repeating: 2, count: 16), Data(repeating: 3, count: 32)) },
            isGuest: { false }
        ))
        let model = JoinCallLandingViewModel(
            slug: "slug#secret",
            authRepository: AuthRepository.offline(),
            intake: InviteIntake(),
            linkCallHost: host,
            hasSession: { true },
            isGuestSession: { false },
            drawGuestName: { "Drawn Name" },
            probeRoom: probe)
        model.decoded = decoded
        return model
    }

    // MARK: - The fold

    func testTheSfusCallEndedRefusalIsSpentNotEnded() {
        typealias R = LinkCallHostRules
        XCTAssertEqual(R.reduce(.waiting, .callAlreadyEnded), .spent)
        XCTAssertEqual(R.reduce(.starting, .callAlreadyEnded), .spent)
        // An `ended` folded a moment earlier for the same teardown is replaced.
        XCTAssertEqual(R.reduce(.ended, .callAlreadyEnded), .spent)
        // Final: the admission timer that fires afterwards changes nothing.
        XCTAssertEqual(R.reduce(.spent, .noCommitter), .spent)
        XCTAssertEqual(R.reduce(.spent, .ended), .spent)
        XCTAssertFalse(R.occupiesSlot(.spent))
        XCTAssertEqual(
            R.notice(phase: .spent, landingShowing: false, errorMessage: nil)?.text,
            Localization.text("call.link.ended"))
    }

    // MARK: - The landing

    /// The SFU refuses the join with `CallEnded`: the landing shows
    /// `The call has ended.` and stays; it does not send the person back to
    /// the chat list.
    func testACallEndedAnswerOnJoinLeavesTheLandingShowingTheEndedState() async throws {
        let callbacks = EndedBox<[MultiJoinerCall.Callbacks]>([])
        let model = landing(callbacks: callbacks)

        await model.onStart(decoded)
        XCTAssertEqual(callbacks.value.count, 1, "the join reached the host")
        XCTAssertEqual(model.phase, .waiting)

        callbacks.value[0].onCallAlreadyEnded?()
        XCTAssertEqual(model.phase, .spent)
        XCTAssertFalse(
            JoinCallLandingScreen.returnsToApp(phase: model.phase, isGuest: false),
            "the page stays, saying the call has ended")
    }

    /// The control: a call this device was in that ends still sends a
    /// signed-in person back into the app, and a guest stays.
    func testACallThatEndsAfterJoiningStillReturnsToTheApp() async throws {
        let callbacks = EndedBox<[MultiJoinerCall.Callbacks]>([])
        let model = landing(callbacks: callbacks)

        await model.onStart(decoded)
        callbacks.value[0].onJoined?()
        XCTAssertEqual(model.phase, .live)
        callbacks.value[0].onEnded?()
        XCTAssertEqual(model.phase, .ended)
        XCTAssertTrue(JoinCallLandingScreen.returnsToApp(phase: .ended, isGuest: false))
        XCTAssertFalse(JoinCallLandingScreen.returnsToApp(phase: .ended, isGuest: true))
    }

    /// The room probe says `ended` when the link opens: the ended state shows
    /// before anyone presses Join.
    func testARoomProbeAnsweringEndedShowsTheEndedStateUpFront() async {
        var probed: [Data] = []
        let callbacks = EndedBox<[MultiJoinerCall.Callbacks]>([])
        let model = landing(callbacks: callbacks, probe: { id in
            probed.append(id)
            return .ended
        })
        XCTAssertEqual(model.phase, .idle)
        await model.checkRoom(callId: callId)
        XCTAssertEqual(probed, [callId])
        XCTAssertEqual(model.phase, .spent)
        XCTAssertTrue(callbacks.value.isEmpty, "nothing was joined")
    }

    /// Any other answer, or none, leaves Join on the page.
    func testARoomProbeThatIsNotEndedChangesNothing() async {
        for answer: SfuCallState? in [nil, .unknown, .live, .closing] {
            let model = landing(callbacks: EndedBox([]), probe: { _ in answer })
            await model.checkRoom(callId: callId)
            XCTAssertEqual(model.phase, .idle, "answer \(String(describing: answer))")
        }
    }
}

private final class EndedBox<T> {
    var value: T
    init(_ value: T) { self.value = value }
}
