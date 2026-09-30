// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// Tests for the "this stream cannot be decrypted" state — the words
/// (``UndecryptableMedia``) and the state that decides who they are about
/// (``CallUiState/undecryptable``).
///
/// The reason this state exists is worth restating where the tests live: with
/// `RTCCryptoOptions.sframeRequireFrameEncryption` on, a receiver whose SFrame
/// decryptor failed to arm has every frame discarded before it is decoded. The
/// refusal is correct and is not what these tests are about. They are about the
/// second half of invariant #4 — that a refused call must not be allowed to
/// look like a merely bad one. A blank tile means two completely different
/// things, and only one of them is fixable by moving closer to the router.
final class UndecryptableMediaTests: XCTestCase {

    // MARK: - The words

    func testNoticeIsAbsentWhenEverythingIsOpening() {
        XCTAssertNil(UndecryptableMedia.notice(names: []))
    }

    func testOneNamedParticipantIsNamed() {
        XCTAssertEqual(
            UndecryptableMedia.notice(names: ["Alice"]).map(l10nPlain), "Can't decrypt Alice's media")
    }

    /// A 1x1 call often has no name at all (a contact that is only a
    /// fingerprint). The line still has to say the thing; it just cannot say
    /// whose, and an empty possessive would read as a bug.
    func testOneUnnamedParticipantStillGetsALine() {
        XCTAssertEqual(
            UndecryptableMedia.notice(names: [""]), "Can't decrypt the incoming media")
        XCTAssertEqual(
            UndecryptableMedia.notice(names: ["   "]), "Can't decrypt the incoming media")
    }

    func testSeveralParticipantsAreCountedNotListed() {
        XCTAssertEqual(
            UndecryptableMedia.notice(names: ["Alice", "Bob", "Carol"]),
            "Can't decrypt media from 3 participants")
    }

    func testTheSpokenTileLabelNamesWhoItIsAbout() {
        XCTAssertEqual(
            l10nPlain(UndecryptableMedia.spokenTileLabel(name: "Alice")), "Can't decrypt Alice's media")
        XCTAssertEqual(
            UndecryptableMedia.spokenTileLabel(name: ""),
            "Can't decrypt this participant's media")
    }

    /// The badge sits on a tile that is already blank, so it has to be short
    /// enough to fit one and unambiguous enough to not read as a network hint.
    func testTheTileBadgeSaysWhatItIs() {
        XCTAssertEqual(UndecryptableMedia.tileLabel, "Can't decrypt")
    }
}

/// The state layer the tile and the status line read.
@MainActor
final class UndecryptableCallStateTests: XCTestCase {

    private func participant(_ id: String, _ name: String) -> CallUiState.Participant {
        CallUiState.Participant(
            id: id, displayName: name, isLocal: false, micOn: true, camOn: false,
            handRaised: false, quality: .good, speaking: false, screenSharing: false)
    }

    func testNothingIsMarkedByDefault() {
        let state = CallUiState()
        state.setParticipants([participant("peer-a", "Alice")])
        XCTAssertTrue(state.undecryptable.isEmpty)
        XCTAssertNil(state.undecryptableNotice)
    }

    func testMarkingAParticipantNamesThemInTheNotice() {
        let state = CallUiState()
        state.setParticipants([participant("peer-a", "Alice"), participant("peer-b", "Bob")])
        state.markUndecryptable("peer-a")
        XCTAssertEqual(state.undecryptable, ["peer-a"])
        XCTAssertEqual(state.undecryptableNotice.map(l10nPlain), "Can't decrypt Alice's media")
    }

    /// One peer publishes audio and video; both receivers can fail. The
    /// participant is named once, not twice.
    func testMarkingTheSameParticipantTwiceIsOneMark() {
        let state = CallUiState()
        state.setParticipants([participant("peer-a", "Alice")])
        state.markUndecryptable("peer-a")
        state.markUndecryptable("peer-a")
        XCTAssertEqual(state.undecryptable.count, 1)
        XCTAssertEqual(state.undecryptableNotice.map(l10nPlain), "Can't decrypt Alice's media")
    }

    /// One peer failing is not the call failing — the other tiles stay clean,
    /// which is the whole reason a failed arm does not end a group call.
    func testTheOtherParticipantsAreUnaffected() {
        let state = CallUiState()
        state.setParticipants([participant("peer-a", "Alice"), participant("peer-b", "Bob")])
        state.markUndecryptable("peer-a")
        XCTAssertFalse(state.undecryptable.contains("peer-b"))
        XCTAssertFalse(state.undecryptable.contains("local"))
    }

    /// A peer that left takes the accusation with it; a rejoin under the same
    /// id must not inherit it.
    func testAParticipantLeavingClearsItsMark() {
        let state = CallUiState()
        state.setParticipants([participant("peer-a", "Alice")])
        state.markUndecryptable("peer-a")
        state.participantLeft("peer-a")
        XCTAssertTrue(state.undecryptable.isEmpty)
        XCTAssertNil(state.undecryptableNotice)
    }

    /// The mark is sticky for the life of the call: a failed receiver is never
    /// re-armed, so quietly dropping the badge would restate the same lie.
    func testARosterRebuildDoesNotClearTheMark() {
        let state = CallUiState()
        state.setParticipants([participant("remote", "Alice")])
        state.markUndecryptable("remote")
        // `driveFrom` rebuilds the whole 1x1 roster on every session emission.
        state.setParticipants([participant("remote", "Alice")])
        XCTAssertEqual(state.undecryptable, ["remote"])
    }

    func testResetClearsEverything() {
        let state = CallUiState()
        state.setParticipants([participant("peer-a", "Alice")])
        state.markUndecryptable("peer-a")
        state.reset()
        XCTAssertTrue(state.undecryptable.isEmpty)
        XCTAssertNil(state.undecryptableNotice)
    }

    /// The camera downgrade is a note about how the call started; an
    /// undecryptable stream is why there is nothing on the screen now.
    func testTheUndecryptableLineOutranksTheCameraNotice() {
        let state = CallUiState()
        state.setParticipants([participant("remote", "Alice")])
        state.setMediaNotice("No camera on this device - calling with voice")
        XCTAssertNil(state.undecryptableNotice)
        state.markUndecryptable("remote")
        XCTAssertEqual(state.undecryptableNotice.map(l10nPlain), "Can't decrypt Alice's media")
    }
}
