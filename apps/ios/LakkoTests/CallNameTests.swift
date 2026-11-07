// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The name a participant gives themself for one group or link call
/// (`docs/ux/calls.md` section 4.1): the per-call channel, the naming order,
/// the Settings field's rule, and the guest name on the link page
/// (`docs/ux/call-links.md` section 9.1).
@MainActor
final class CallNameTests: XCTestCase {

    // MARK: - A channel over fakes

    /// A channel whose compose stamps `seq|name` and whose verify reads it
    /// back when the signer key is `goodKey`.
    @MainActor
    private final class Harness {
        var selfPeerId: String?
        var sent: [String] = []
        var names: [(String, String?)] = []
        let goodKey = Data(repeating: 7, count: 32)
        lazy var channel = CallNameChannel(deps: CallNameChannel.Deps(
            compose: { selfPeerId, seq, name in Data("\(selfPeerId)|\(seq)|\(name)".utf8) },
            verify: { [goodKey] from, key, bytes in
                guard key == goodKey, let text = String(data: bytes, encoding: .utf8) else { return nil }
                let parts = text.split(separator: "|", omittingEmptySubsequences: false).map(String.init)
                guard parts.count == 3, parts[0] == from, let seq = UInt64(parts[1]) else { return nil }
                return CallNameChannel.Verified(seq: seq, name: parts[2].isEmpty ? nil : parts[2])
            },
            sendSealed: { [weak self] plaintext in
                self?.sent.append(String(decoding: plaintext, as: UTF8.self))
                return true
            },
            selfPeerId: { [weak self] in self?.selfPeerId },
            onName: { [weak self] peer, name in self?.names.append((peer, name)) }
        ))

        func wire(_ from: String, _ seq: UInt64, _ name: String) -> Data {
            Data("\(from)|\(seq)|\(name)".utf8)
        }
    }

    func testANameSetBeforeTheWelcomeGoesOutWithTheFirstAnnouncement() {
        let h = Harness()
        h.channel.setOwnName("Blue Fox")
        XCTAssertEqual(h.sent, [], "nothing goes out before the SFU named this device")
        h.selfPeerId = "me"
        h.channel.resend()
        XCTAssertEqual(h.sent, ["me|1|Blue Fox"])
        // Every later announcement re-sends the same name under the same seq.
        h.channel.resend()
        XCTAssertEqual(h.sent, ["me|1|Blue Fox", "me|1|Blue Fox"])
    }

    func testTheSameNameAgainSendsNothingAndAChangeBumpsTheSequence() {
        let h = Harness()
        h.selfPeerId = "me"
        XCTAssertFalse(h.channel.setOwnName(""), "an empty name before any name is no change")
        h.channel.resend()
        XCTAssertEqual(h.sent, [], "nothing to say before a name was ever set")
        XCTAssertTrue(h.channel.setOwnName("Anna"))
        XCTAssertFalse(h.channel.setOwnName("Anna"))
        XCTAssertTrue(h.channel.setOwnName(""))
        XCTAssertEqual(h.sent, ["me|1|Anna", "me|2|"])
        XCTAssertEqual(h.channel.ownCallName, "")
    }

    func testANameBeforeThePeersAnnouncementIsReadOnceItVerifies() {
        let h = Harness()
        h.channel.received(from: "p1", bytes: h.wire("p1", 1, "Old"))
        h.channel.received(from: "p1", bytes: h.wire("p1", 2, "Mia"))
        XCTAssertTrue(h.names.isEmpty, "held until the announcement verifies")
        h.channel.peerIdentified("p1", signerKey: h.goodKey)
        XCTAssertEqual(h.names.map(\.0), ["p1"])
        XCTAssertEqual(h.names.map(\.1), ["Mia"], "only the latest held payload is kept")
    }

    func testAnOlderOrRepeatedSequenceIsNotAnUpdateAndANilNameWithdraws() {
        let h = Harness()
        h.channel.peerIdentified("p1", signerKey: h.goodKey)
        h.channel.received(from: "p1", bytes: h.wire("p1", 3, "Mia"))
        h.channel.received(from: "p1", bytes: h.wire("p1", 3, "Mia"))
        h.channel.received(from: "p1", bytes: h.wire("p1", 2, "Older"))
        h.channel.received(from: "p1", bytes: h.wire("p1", 4, ""))
        XCTAssertEqual(h.names.map(\.1), ["Mia", nil])
    }

    func testANameSignedByAnotherKeyOrForAnotherPeerIsDropped() {
        let h = Harness()
        h.channel.peerIdentified("p1", signerKey: Data(repeating: 1, count: 32))
        h.channel.received(from: "p1", bytes: h.wire("p1", 1, "Mia"))
        h.channel.peerIdentified("p2", signerKey: h.goodKey)
        h.channel.received(from: "p2", bytes: h.wire("p1", 1, "Mia"))
        XCTAssertTrue(h.names.isEmpty)
    }

    func testAPeerThatLeftStartsAgainFromNothing() {
        let h = Harness()
        h.channel.peerIdentified("p1", signerKey: h.goodKey)
        h.channel.received(from: "p1", bytes: h.wire("p1", 5, "Mia"))
        h.channel.peerLeft("p1")
        h.channel.received(from: "p1", bytes: h.wire("p1", 1, "Again"))
        XCTAssertEqual(h.names.count, 1, "held: the returning peer has not announced itself yet")
        h.channel.peerIdentified("p1", signerKey: h.goodKey)
        XCTAssertEqual(h.names.map(\.1), ["Mia", "Again"])
    }

    // MARK: - The naming order

    func testTheNameTypedForTheCallComesFirst() {
        XCTAssertEqual(
            CallTileName.name(callName: "Blue Fox", resolved: "Anna", announcedName: "A", claimedUsername: "a"),
            "Blue Fox")
        XCTAssertEqual(
            CallTileName.name(callName: "  ", resolved: "Anna", announcedName: nil, claimedUsername: nil),
            "Anna")
        XCTAssertEqual(
            CallTileName.name(callName: nil, resolved: nil, announcedName: nil, claimedUsername: "anna"),
            "anna")
    }

    func testAGroupCallTileTakesTheCallNameAndGoesBackToThePerson() {
        let names = GroupCallPeerNames(name: { $0 == "anna-key" ? "Anna" : nil })
        names.setRoster(["p1"])
        XCTAssertEqual(names.setCallName("p1", "Blue Fox"), "Blue Fox", "named before the announcement")
        XCTAssertEqual(names.identify("p1", personKeyB64: "anna-key"), "Blue Fox")
        XCTAssertEqual(names.setCallName("p1", nil), "Anna")
        XCTAssertNil(names.setCallName("nobody", "X"), "a peer not on the roster")
        names.peerLeft("p1")
        names.peerJoined("p1")
        XCTAssertEqual(names.name(participantId: SfuCallUi.participantId(forPeer: "p1")), "")
    }

    func testTheOwnTileShowsTheCallNameElseTheProfileName() {
        XCTAssertEqual(SfuCallUi.ownTileName(callName: "Blue Fox", profileName: "Anna"), "Blue Fox")
        XCTAssertEqual(SfuCallUi.ownTileName(callName: "", profileName: "Anna"), "Anna")
        XCTAssertEqual(SfuCallUi.ownTileName(callName: "", profileName: ""), "")
    }

    // MARK: - The Settings field

    func testTheFieldSavesOnlyAChangedNameThatPassesTheRule() {
        XCTAssertEqual(CallNameField.verdict(text: "  Anna  ", current: "Anna"), .unchanged)
        XCTAssertEqual(CallNameField.verdict(text: " Blue Fox ", current: "Anna"), .save("Blue Fox"))
        XCTAssertEqual(CallNameField.verdict(text: "   ", current: "Anna"), .save(""))
        XCTAssertEqual(CallNameField.verdict(text: "", current: ""), .unchanged)
        guard case .invalid(let reason) = CallNameField.verdict(text: "Two\nLines", current: "") else {
            return XCTFail("a line break fails the display-name rule")
        }
        XCTAssertEqual(reason, Localization.text("display_name.error.line_break"))
    }

    func testTheCallScreenStateCarriesTheNameOnlyWhenTheCallCan() {
        let ui = CallUiState()
        var saved: [String] = []
        ui.saveCallName("Ignored")
        XCTAssertFalse(ui.callNameAvailable)
        XCTAssertEqual(ui.ownCallName, "")
        ui.setCallNameControl(ownName: "Anna", defaultName: "Profile", save: { saved.append($0) })
        XCTAssertTrue(ui.callNameAvailable)
        XCTAssertEqual(ui.callNameDefault, "Profile")
        ui.saveCallName("Blue Fox")
        XCTAssertEqual(saved, ["Blue Fox"])
        XCTAssertEqual(ui.ownCallName, "Blue Fox")
        ui.reset()
        XCTAssertFalse(ui.callNameAvailable)
        XCTAssertEqual(ui.ownCallName, "")
    }

    func testANameTypedForACallIsRememberedForThatCallOnly() {
        CallNameMemory.clearAll()
        CallNameMemory.remember(callIdHex: "aa", name: "Blue Fox")
        XCTAssertEqual(CallNameMemory.remembered(callIdHex: "aa"), "Blue Fox")
        XCTAssertEqual(CallNameMemory.remembered(callIdHex: "bb"), "")
        CallNameMemory.remember(callIdHex: "aa", name: "")
        XCTAssertEqual(CallNameMemory.remembered(callIdHex: "aa"), "")
    }

    // MARK: - The link page

    func testTheLinkPageAsksAGuestForANameAndNobodyElse() {
        typealias VM = JoinCallLandingViewModel
        XCTAssertTrue(VM.showsGuestNameField(hasSession: false, isGuest: false, isOwner: false))
        XCTAssertTrue(VM.showsGuestNameField(hasSession: true, isGuest: true, isOwner: false),
                      "a guest account still on the device is asked again")
        XCTAssertFalse(VM.showsGuestNameField(hasSession: true, isGuest: false, isOwner: false))
        XCTAssertFalse(VM.showsGuestNameField(hasSession: true, isGuest: true, isOwner: true))
        XCTAssertEqual(VM.callName(fromTyped: "  Blue Fox "), "Blue Fox")
        XCTAssertEqual(VM.callName(fromTyped: "   "), "")
        XCTAssertNil(VM.callName(fromTyped: "Two\nLines"))
    }

    func testEveryOpeningOfTheLinkPageDrawsANewName() {
        var drawn = 0
        let draw: () -> String = {
            drawn += 1
            return "Name \(drawn)"
        }
        let first = landing(guest: true, draw: draw).model
        let second = landing(guest: true, draw: draw).model
        XCTAssertEqual(first.guestName, "Name 1")
        XCTAssertEqual(second.guestName, "Name 2")
    }

    func testOpeningTheSameLinkAgainWhileItsPageShowsDrawsANewName() {
        let intake = InviteIntake()
        var drawn = 0
        let model = landing(guest: true, intake: intake, draw: {
            drawn += 1
            return "Name \(drawn)"
        }).model
        XCTAssertEqual(model.guestName, "Name 1", "the open the page was built for draws nothing more")
        intake.handle(URL(string: "https://lakko.me/c/abcdefgh#secret")!)
        XCTAssertEqual(model.guestName, "Name 2")
        intake.handle(URL(string: "https://lakko.me/c/abcdefgh#secret")!)
        XCTAssertEqual(model.guestName, "Name 3", "the same link again is a new open")
        model.onLinkOpened(intake.callJoinOpens)
        XCTAssertEqual(model.guestName, "Name 3", "the same open seen twice draws nothing")
        intake.parkCallJoin(slug: "abcdefgh#secret")
        XCTAssertEqual(model.guestName, "Name 4")
    }

    func testNoNameIsDrawnWhileAGuestSignUpsJoinIsParked() {
        let intake = InviteIntake()
        var drawn = 0
        let model = landing(guest: true, intake: intake, draw: {
            drawn += 1
            return "Name \(drawn)"
        }).model
        model.guestName = "Typed Name"
        intake.armCallJoinAutoResume(guestName: "Typed Name")
        intake.parkCallJoin(slug: "abcdefgh#secret")
        XCTAssertEqual(model.guestName, "Typed Name")
    }

    func testAReturningGuestJoinsUnderTheTypedNameWithoutTouchingTheProfile() async {
        let (model, requests) = landing(guest: true)
        model.guestName = "  Quiet Otter "
        await model.onStart(decoded())
        XCTAssertEqual(requests.value.map(\.initialCallName), ["Quiet Otter"])
    }

    func testARealAccountJoinsWithNoNameFromThePage() async {
        let (model, requests) = landing(guest: false)
        model.guestName = "Quiet Otter"
        await model.onStart(decoded())
        XCTAssertEqual(requests.value.count, 1)
        XCTAssertNil(requests.value.first?.initialCallName ?? nil)
    }

    func testANewGuestsNameIsParkedForTheCallTheNextLandingStarts() {
        let intake = InviteIntake()
        intake.armCallJoinAutoResume(guestName: "Blue Fox")
        XCTAssertEqual(intake.takeCallJoinGuestName(), "Blue Fox")
        XCTAssertNil(intake.takeCallJoinGuestName(), "one-shot")
        intake.armCallJoinAutoResume(guestName: "Blue Fox")
        intake.clearCallJoin()
        XCTAssertNil(intake.takeCallJoinGuestName())
    }

    // MARK: - Helpers

    private final class Box<T> {
        var value: T
        init(_ value: T) { self.value = value }
    }

    private func decoded() -> JoinCallLandingViewModel.Decoded {
        JoinCallLandingViewModel.Decoded(
            callId: Data(repeating: 9, count: 16),
            ownerAccountId: Data(repeating: 2, count: 16),
            ownerSignatureKey: Data(repeating: 3, count: 32),
            ownerFingerprint: "",
            mediaVideo: false,
            valid: true,
            isOwner: false)
    }

    private func landing(
        guest: Bool,
        intake: InviteIntake? = nil,
        draw: @escaping () -> String = { "Drawn Name" }
    ) -> (model: JoinCallLandingViewModel, requests: Box<[LinkCallHost.Request]>) {
        let requests = Box<[LinkCallHost.Request]>([])
        let host = LinkCallHost(ports: LinkCallHost.Ports(
            session: { (Data(repeating: 1, count: 64), "token") },
            sfuBaseUrl: { "wss://sfu.test" },
            microphoneGranted: { true },
            cameraGranted: { _ in true },
            startCall: { request, _, _, _, _, _ in
                requests.value.append(request)
                return FakeLinkCallHandle()
            },
            ownIdentity: { (Data(repeating: 2, count: 16), Data(repeating: 3, count: 32)) },
            isGuest: { guest }
        ))
        let model = JoinCallLandingViewModel(
            slug: "slug#secret",
            authRepository: AuthRepository.offline(),
            intake: intake ?? InviteIntake(),
            linkCallHost: host,
            hasSession: { true },
            isGuestSession: { guest },
            drawGuestName: draw)
        return (model, requests)
    }
}
