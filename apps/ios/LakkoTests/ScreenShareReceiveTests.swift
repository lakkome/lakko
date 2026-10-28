// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
import WebRTC
@testable import Lakko

/// Receiving a shared screen on iOS (ADR-0083, `docs/ux/screen-share.md`
/// section 5). This client never sends one.
///
/// Covered without a device: the stream-id suffix, the verification of a
/// share state (group: against the peer's announced leaf key; one-to-one:
/// against the MLS sender), the fold (a newer share replaces the previous, a
/// stop and a departure end it), the routing of the one-to-one body, the byte
/// peeks that keep it from ringing or rendering as chat, the stage rule with a
/// share tile, the layout a share switches to and back, the announcements and
/// the zoom arithmetic. Not covered: a frame on screen, pinch on glass, a
/// rotation, a live call.
@MainActor
final class ScreenShareReceiveTests: XCTestCase {

    private let callId = Data(repeating: 0x5C, count: 16)
    private let otherCallId = Data(repeating: 0x5D, count: 16)

    private func key(_ byte: UInt8) -> Data { Data(repeating: byte, count: 32) }

    private func verified(
        seq: UInt64, signer: Data, peerId: String = "p1", sharing: Bool = true
    ) -> VerifiedCallScreenShare {
        VerifiedCallScreenShare(
            seq: seq, signerKey: signer, peerId: peerId, sharing: sharing,
            withSound: false, accountKey: signer)
    }

    // MARK: - Stream ids

    func testTheScreenSuffixIsTheCores() {
        XCTAssertEqual(LightStream.screenSuffix, screenStreamSuffix())
    }

    func testAScreenStreamNamesTheSharerWithoutTheSuffix() {
        XCTAssertEqual(
            LightStream.split("p7~screen"), LightStream.RemoteStreamId(peerId: "p7", role: .screen))
        XCTAssertEqual(RemoteVideoRoute.of(streamId: "p7~screen"), .screen(peerId: "p7"))
        // A bare suffix names nobody; another case is not the suffix.
        XCTAssertEqual(
            LightStream.split("~screen"), LightStream.RemoteStreamId(peerId: "~screen", role: .main))
        XCTAssertEqual(RemoteVideoRoute.of(streamId: "p7~SCREEN"), .main(streamId: "p7~SCREEN"))
        // The camera routes are unchanged.
        XCTAssertEqual(RemoteVideoRoute.of(streamId: "p7"), .main(streamId: "p7"))
        XCTAssertEqual(RemoteVideoRoute.of(streamId: "p7~light"), .light(peerId: "p7"))
    }

    func testAScreenIsNotOneOfThePersonsSoundAndPictureSlots() {
        XCTAssertNil(AvSlot.of(kind: "video", role: .screen))
        XCTAssertNil(AvSlot.of(kind: "audio", role: .screen))
    }

    // MARK: - The fold

    func testTheFoldShowsAShareAndANewerOneReplacesIt() {
        let fold = ScreenShareFold(ownKey: key(0x01))
        XCTAssertNil(fold.current)
        XCTAssertTrue(fold.observe(verified(seq: 1, signer: key(0x0A))))
        XCTAssertEqual(fold.current?.signerKey, key(0x0A))
        // The same state again changes nothing.
        XCTAssertFalse(fold.observe(verified(seq: 1, signer: key(0x0A))))
        // A later start by someone else supersedes it.
        XCTAssertTrue(fold.observe(verified(seq: 2, signer: key(0x0B), peerId: "p2")))
        XCTAssertEqual(fold.current?.signerKey, key(0x0B))
        XCTAssertEqual(fold.current?.seq, 2)
        // A late start with a lower seq never takes the place.
        XCTAssertFalse(fold.observe(verified(seq: 1, signer: key(0x0C), peerId: "p3")))
        XCTAssertEqual(fold.current?.signerKey, key(0x0B))
    }

    func testAStopEndsTheShareAndAStoppedShareNeverComesBack() {
        let fold = ScreenShareFold(ownKey: key(0x01))
        fold.observe(verified(seq: 3, signer: key(0x0A)))
        XCTAssertTrue(fold.observe(verified(seq: 3, signer: key(0x0A), sharing: false)))
        XCTAssertNil(fold.current)
        XCTAssertFalse(fold.observe(verified(seq: 3, signer: key(0x0A))))
        XCTAssertNil(fold.current)
    }

    func testASharerLeavingEndsTheirShare() {
        let fold = ScreenShareFold(ownKey: key(0x01))
        fold.observe(verified(seq: 1, signer: key(0x0A)))
        XCTAssertFalse(fold.participantLeft(signerKey: key(0x0B)))
        XCTAssertNotNil(fold.current)
        XCTAssertTrue(fold.participantLeft(signerKey: key(0x0A)))
        XCTAssertNil(fold.current)
    }

    func testAFoldWithAKeyTheCoreRefusesShowsNothing() {
        let fold = ScreenShareFold(ownKey: Data())
        XCTAssertFalse(fold.observe(verified(seq: 1, signer: key(0x0A))))
        XCTAssertNil(fold.current)
    }

    // MARK: - Verification

    private func group(of identity: Identity) throws -> Group {
        try Group.create(identity: identity, kind: .group)
    }

    func testAGroupShareStateVerifiesOnlyAgainstItsAnnouncedPeerAndKey() throws {
        let anna = try Identity.fromSeed(seed: Data(repeating: 0x41, count: 32))
        let stranger = try Identity.fromSeed(seed: Data(repeating: 0x42, count: 32))
        let g = try group(of: anna)
        let state = try composeCallScreenShare(
            identity: anna, callId: callId, peerId: "p1", seq: 4, sharing: true, withSound: true)
        let announcer = CallPeerAnnouncer.forGroup(identity: anna, callId: callId, group: { g })
        let signer = anna.signaturePublicKey()

        let accepted = announcer.verifyScreenShare("p1", signer, state)
        XCTAssertEqual(accepted?.seq, 4)
        XCTAssertEqual(accepted?.withSound, true)
        XCTAssertEqual(accepted?.sharing, true)
        // The relay's stamped sender is not the peer the state names.
        XCTAssertNil(announcer.verifyScreenShare("p2", signer, state))
        // The key the announcement named is not the signer.
        XCTAssertNil(announcer.verifyScreenShare("p1", stranger.signaturePublicKey(), state))
        // A signer who is not in the group.
        let foreign = try composeCallScreenShare(
            identity: stranger, callId: callId, peerId: "p1", seq: 1, sharing: true, withSound: false)
        XCTAssertNil(announcer.verifyScreenShare("p1", stranger.signaturePublicKey(), foreign))
        // Another call's state.
        let other = try composeCallScreenShare(
            identity: anna, callId: otherCallId, peerId: "p1", seq: 1, sharing: true, withSound: false)
        XCTAssertNil(announcer.verifyScreenShare("p1", signer, other))
        // No group: nothing verifies.
        let none = CallPeerAnnouncer.forGroup(identity: anna, callId: callId, group: { nil })
        XCTAssertNil(none.verifyScreenShare("p1", signer, state))
        XCTAssertEqual(announcer.ownSignatureKey, signer)
    }

    func testAOneToOneShareStateVerifiesAgainstTheMlsSender() throws {
        let anna = try Identity.fromSeed(seed: Data(repeating: 0x43, count: 32))
        let g = try group(of: anna)
        let peerId = callScreenShareOneToOnePeerId()
        let state = try composeCallScreenShare(
            identity: anna, callId: callId, peerId: peerId, seq: 1, sharing: true, withSound: false)
        let sender = anna.signaturePublicKey().base64EncodedString()

        XCTAssertNotNil(ChatRepository.verifiedOneToOneScreenShare(
            group: g, callId: callId, state: state, senderDeviceKey: sender))
        XCTAssertNil(ChatRepository.verifiedOneToOneScreenShare(
            group: g, callId: callId, state: state, senderDeviceKey: nil))
        XCTAssertNil(ChatRepository.verifiedOneToOneScreenShare(
            group: g, callId: callId, state: state,
            senderDeviceKey: key(0x09).base64EncodedString()))
        // A group-call peer id is not the one-to-one literal.
        let groupState = try composeCallScreenShare(
            identity: anna, callId: callId, peerId: "p1", seq: 1, sharing: true, withSound: false)
        XCTAssertNil(ChatRepository.verifiedOneToOneScreenShare(
            group: g, callId: callId, state: groupState, senderDeviceKey: sender))
    }

    func testTheRelayOpensASealedShareState() throws {
        let anna = try Identity.fromSeed(seed: Data(repeating: 0x44, count: 32))
        let state = try composeCallScreenShare(
            identity: anna, callId: callId, peerId: "p1", seq: 1, sharing: true, withSound: false)
        let relay = CallRelay()
        relay.setSeed(Data(repeating: 0x77, count: 32))
        let sealed = try XCTUnwrap(relay.seal(state))
        XCTAssertEqual(relay.open(sealed), .screenShare(state))
    }

    // MARK: - The one-to-one body

    private func screenShareBody() throws -> Data {
        try composeCallSignaling(body: .screenShare(callId: callId, state: Data([1, 2, 3])))
    }

    func testTheOneToOneBodyIsCallSignalingThatNeverRingsOrWakes() throws {
        let bytes = try screenShareBody()
        XCTAssertEqual(Array(bytes.prefix(2)), [0x8A, 0x01])
        XCTAssertTrue(ChatClient.looksLikeCallSignaling(bytes))
        XCTAssertTrue(nseLooksLikeCallSignaling(bytes))
        XCTAssertEqual(CallSignalingWake.bodyKind(bytes), .screenShare)
        XCTAssertFalse(CallSignalingWake.shouldWake(bytes))
        // The tag needs its continuation byte and its framing.
        for wrong in [Data([0x8A]), Data([0x8A, 0x02, 0x0A]), Data(bytes.dropLast())] {
            XCTAssertFalse(ChatClient.looksLikeCallSignaling(wrong), "\(Array(wrong))")
            XCTAssertFalse(nseLooksLikeCallSignaling(wrong), "\(Array(wrong))")
        }
    }

    func testTheOneToOneBodyRoutesOnlyVerifiedToTheCallInTheSlot() throws {
        let body = FfiCallSignalingBody.screenShare(callId: callId, state: Data([1]))
        XCTAssertEqual(CallSignalingRoute.of(body).forwardToOneToOne, true)
        XCTAssertNil(CallSignalingRoute.of(body).groupSignal)
        XCTAssertEqual(CallSession.callId(of: body), callId)
        let groupHex = "aa"
        let active = CallHostRouting.ActiveCall(
            callIdHex: CallHostRouting.hex(callId), groupIdHex: groupHex)
        let unverified = InboundCallRow(
            groupIdHex: groupHex, messageId: 1, createdAt: "2027-01-15T08:00:00Z", body: body)
        var verifiedRow = unverified
        verifiedRow.verifiedScreenShare = verified(seq: 1, signer: key(0x0A), peerId: "p2p")
        XCTAssertEqual(
            CallHostRouting.decide(
                row: verifiedRow, nowMs: 0, active: active, endedCallIds: [], otherCallBusy: false),
            .apply)
        XCTAssertEqual(
            CallHostRouting.decide(
                row: unverified, nowMs: 0, active: active, endedCallIds: [], otherCallBusy: false),
            .drop)
        XCTAssertEqual(
            CallHostRouting.decide(
                row: verifiedRow, nowMs: 0, active: nil, endedCallIds: [], otherCallBusy: false),
            .drop)
    }

    func testASessionFoldsOnlyItsOwnCallsShares() {
        let session = CallSession(callId: callId, onOutboundSignaling: { _, _ in })
        session.applyScreenShare(verified(seq: 1, signer: key(0x0A), peerId: "p2p"), callId: otherCallId)
        XCTAssertNil(session.screenShare)
        session.applyScreenShare(verified(seq: 1, signer: key(0x0A), peerId: "p2p"), callId: callId)
        XCTAssertEqual(session.screenShare?.seq, 1)
        session.applyScreenShare(
            verified(seq: 1, signer: key(0x0A), peerId: "p2p", sharing: false), callId: callId)
        XCTAssertNil(session.screenShare)
    }

    // MARK: - The share on screen

    func testTheShownShareNamesTheSharersTileAndNeverOwnShares() {
        let current = CallCurrentScreenShare(
            seq: 2, signerKey: key(0x0A), peerId: "p9", withSound: false, accountKey: key(0x0A),
            isOwn: false)
        let shown = ScreenShareShown.of(current, sharerParticipantId: { "peer-\($0)" })
        XCTAssertEqual(shown?.sharerParticipantId, "peer-p9")
        XCTAssertEqual(shown?.tileId, "screen:peer-p9")
        let own = CallCurrentScreenShare(
            seq: 2, signerKey: key(0x01), peerId: "p1", withSound: false, accountKey: key(0x01),
            isOwn: true)
        XCTAssertNil(ScreenShareShown.of(own, sharerParticipantId: { $0 }))
        XCTAssertNil(ScreenShareShown.of(nil, sharerParticipantId: { $0 }))
    }

    private func shown(_ sharer: String, seq: UInt64 = 1) -> ScreenShareShown {
        ScreenShareShown(sharerParticipantId: sharer, seq: seq, signerKey: key(0x0A))
    }

    private func person(_ id: String, local: Bool = false, cam: Bool = false) -> CallUiState.Participant {
        CallUiState.Participant(
            id: id, displayName: id == "peer-a" ? "Anna" : id, isLocal: local, micOn: true,
            camOn: cam, handRaised: false, quality: .good, speaking: false, screenSharing: false)
    }

    func testAShareSwitchesToFocusAndTheLayoutComesBackWhenItStops() {
        let ui = CallUiState()
        ui.setParticipants([person("local", local: true), person("peer-a"), person("peer-b")])
        ui.setLayoutChoice(.grid)
        ui.setScreenShare(shown("peer-a"))
        XCTAssertEqual(ui.layoutChoice, .focus)
        // A replacing share changes no layout.
        ui.setLayoutChoice(.grid)
        ui.setScreenShare(shown("peer-b", seq: 2))
        XCTAssertEqual(ui.layoutChoice, .grid)
        ui.setScreenShare(nil)
        XCTAssertEqual(ui.layoutChoice, .grid)

        // From the default (no choice made) and back to it.
        let fresh = CallUiState()
        fresh.setParticipants([person("local", local: true), person("peer-a")])
        fresh.setScreenShare(shown("peer-a"))
        XCTAssertEqual(fresh.layoutChoice, .focus)
        fresh.setScreenShare(nil)
        XCTAssertNil(fresh.layoutChoice)
    }

    func testTheShareTileIsItsOwnEntryNamedAfterTheSharer() {
        let ui = CallUiState()
        ui.setParticipants([person("local", local: true), person("peer-a")])
        XCTAssertEqual(ui.groupLayoutPeople.map(\.id), ["local", "peer-a"])
        ui.setScreenShare(shown("peer-a"))
        XCTAssertEqual(ui.groupLayoutPeople.map(\.id), ["local", "peer-a", "screen:peer-a"])
        XCTAssertEqual(ui.screenShareTile?.isScreen, true)
        XCTAssertEqual(ui.screenShareTile?.displayName, "Anna")
        // The roster itself never holds it.
        XCTAssertFalse(ui.participants.contains(where: \.isScreen))
    }

    func testAStagedShareWinsOverTheSpeakerAndAPinWinsOverTheShare() {
        let ui = CallUiState()
        ui.setParticipants([person("local", local: true), person("peer-a"), person("peer-b", cam: true)])
        ui.setScreenShare(shown("peer-a"))
        let people = ui.groupLayoutPeople
        let speech = StageSpeech(activeSpeakerId: "peer-b", lastSpokeAtMs: ["peer-b": 10])
        let stage = GroupCallLayout.stagePerson(
            SharedGroupCallRules(), people: people, pinnedId: nil, speech: speech)
        XCTAssertEqual(stage?.id, "screen:peer-a")
        let pinned = GroupCallLayout.stagePerson(
            SharedGroupCallRules(), people: people, pinnedId: "peer-b", speech: speech)
        XCTAssertEqual(pinned?.id, "peer-b")
        // The share's tile sits with the other people, before this device's.
        XCTAssertEqual(
            GroupCallLayout.tileOrder(people).map(\.id), ["peer-a", "peer-b", "screen:peer-a", "local"])
    }

    func testViewClearsThePinAndAReplacedSharesPinGoesWithIt() {
        let ui = CallUiState()
        ui.setParticipants([person("local", local: true), person("peer-a"), person("peer-b")])
        ui.togglePin("peer-b")
        ui.setScreenShare(shown("peer-a"))
        XCTAssertEqual(ui.pinnedId, "peer-b")
        ui.viewScreenShare()
        XCTAssertNil(ui.pinnedId)
        XCTAssertEqual(ui.layoutChoice, .focus)

        ui.pin("screen:peer-a")
        ui.setScreenShare(shown("peer-b", seq: 2))
        XCTAssertNil(ui.pinnedId)
    }

    func testAShareTrackWaitsForItsFirstFrame() {
        let ui = CallUiState()
        ui.setScreenShare(shown("peer-a"))
        XCTAssertFalse(ui.screenShareHasPicture)
        ui.setScreenShareFramed(true, forSharer: "peer-a")
        XCTAssertTrue(ui.screenShareHasPicture)
        // Another share's frame says nothing about this one.
        ui.setScreenShare(shown("peer-b", seq: 2))
        XCTAssertFalse(ui.screenShareHasPicture)
        ui.reset()
        XCTAssertNil(ui.screenShare)
    }

    func testTheOneToOneShareIsTheVideoLayoutEvenOnAVoiceCall() {
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 1, remotePicture: false, screenShare: true),
            .video)
        XCTAssertEqual(
            CallScreenPresentation.layout(remoteCount: 1, remotePicture: false, screenShare: false),
            .portrait)
        XCTAssertEqual(
            CallScreenPresentation.layout(
                groupCall: true, remoteCount: 1, remotePicture: false, screenShare: true),
            .gallery)
    }

    // MARK: - Announcements

    func testAStartAndAStopAreEachAnnouncedOnce() {
        let a = shown("peer-a")
        let b = shown("peer-b", seq: 2)
        XCTAssertEqual(ScreenShareAnnouncement.of(old: nil, new: a), .started(sharerParticipantId: "peer-a"))
        XCTAssertNil(ScreenShareAnnouncement.of(old: a, new: a))
        XCTAssertEqual(ScreenShareAnnouncement.of(old: a, new: b), .started(sharerParticipantId: "peer-b"))
        XCTAssertEqual(ScreenShareAnnouncement.of(old: b, new: nil), .stopped(sharerParticipantId: "peer-b"))
        XCTAssertNil(ScreenShareAnnouncement.of(old: nil, new: nil))
        XCTAssertEqual(ScreenShareAnnouncement.started(sharerParticipantId: "x").key, "call.screen_share.started")
        XCTAssertEqual(ScreenShareAnnouncement.stopped(sharerParticipantId: "x").key, "call.screen_share.stopped")
    }

    // MARK: - Zoom

    func testZoomStaysBetweenWholeAndItsCapAndPansOnlyWhatIsOutside() {
        XCTAssertEqual(ScreenShareZoom.clampScale(0.5), 1)
        XCTAssertEqual(ScreenShareZoom.clampScale(9), ScreenShareZoom.maxScale)
        XCTAssertEqual(ScreenShareZoom.clampScale(.nan), 1)
        let size = CGSize(width: 200, height: 100)
        XCTAssertEqual(
            ScreenShareZoom.clampOffset(CGSize(width: 50, height: 50), scale: 1, size: size), .zero)
        XCTAssertEqual(
            ScreenShareZoom.clampOffset(CGSize(width: 500, height: -500), scale: 2, size: size),
            CGSize(width: 100, height: -50))
        XCTAssertEqual(
            ScreenShareZoom.clampOffset(CGSize(width: 10, height: 5), scale: 2, size: size),
            CGSize(width: 10, height: 5))
    }

    func testTheLayoutMemoryReturnsWhatWasBeforeTheFirstShare() {
        var memory = ScreenShareLayoutMemory()
        XCTAssertEqual(memory.shareChanged(hasShare: true, choice: .grid), .focus)
        XCTAssertEqual(memory.shareChanged(hasShare: true, choice: .grid), .grid)
        XCTAssertEqual(memory.shareChanged(hasShare: false, choice: .focus), .grid)
        XCTAssertEqual(memory.shareChanged(hasShare: false, choice: .focus), .focus)
    }
}
