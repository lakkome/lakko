// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/calls.md` section 4 — the call screen names the person on the call.
///
/// The defect these pin: the 1x1 roster was built with an empty display name
/// and the tile's own fallback for an empty name was the word "Unknown", so a
/// connected call labelled the contact being spoken to "Unknown" while the
/// incoming banner, two files away, had their name.
final class CallPeerNameTests: XCTestCase {

    // MARK: - The chain

    func testTheProfileNameWins() {
        XCTAssertEqual(
            CallPeerName.resolve(profileName: "Anna", chatTitle: "Work chat"), "Anna")
    }

    /// The resolver answers "Member" for a person nothing is known about. The
    /// caller passes that rung as `nil`, and the chat title answers instead —
    /// which is the rung the defect skipped entirely.
    func testAMissingProfileNameFallsBackToTheChatTitle() {
        XCTAssertEqual(
            CallPeerName.resolve(profileName: nil, chatTitle: "Anna Petrova"), "Anna Petrova")
    }

    func testAnEmptyProfileNameFallsBackToTheChatTitle() {
        XCTAssertEqual(
            CallPeerName.resolve(profileName: "   ", chatTitle: "Anna"), "Anna")
    }

    func testNothingKnownResolvesToNothingRatherThanAWord() {
        XCTAssertNil(CallPeerName.resolve(profileName: nil, chatTitle: nil))
        XCTAssertNil(CallPeerName.resolve(profileName: "", chatTitle: "  "))
    }

    func testNamesAreTrimmed() {
        XCTAssertEqual(
            CallPeerName.resolve(profileName: "  Anna \n", chatTitle: nil), "Anna")
    }

    // MARK: - The tile

    func testTheTileShowsTheResolvedName() {
        XCTAssertEqual(
            CallPeerName.tileLabel(displayName: "Anna", isLocal: false), "Anna")
    }

    func testTheLocalTileIsAlwaysYou() {
        XCTAssertEqual(CallPeerName.tileLabel(displayName: "", isLocal: true), "You")
    }

    /// The regression itself: a nameless remote is never "Unknown".
    func testANamelessRemoteIsNeverUnknown() {
        let label = CallPeerName.tileLabel(displayName: "", isLocal: false)
        XCTAssertNotEqual(label, "Unknown")
        XCTAssertEqual(label, CallPeerName.placeholder)
    }

    /// And the placeholder is the app's one placeholder, so a call roster and
    /// a chat roster never disagree about the same stranger.
    func testThePlaceholderIsTheAppsOwn() {
        XCTAssertEqual(CallPeerName.placeholder, ResolvedDisplayName.placeholderMember)
    }

    // MARK: - The encryption sheet's sentence

    func testTheEncryptionSentenceNamesTheOtherPerson() {
        XCTAssertEqual(
            l10nPlain(CallPeerName.encryptionSentence(peerName: "Anna")),
            "Only you and Anna can hear this call.")
    }

    /// With no name the sentence must stay true for a group call too, where
    /// there is no single "other person".
    func testTheEncryptionSentenceInventsNoNameWhenThereIsNone() {
        XCTAssertEqual(
            CallPeerName.encryptionSentence(peerName: nil),
            "Only the people on this call can hear it.")
        XCTAssertEqual(
            CallPeerName.encryptionSentence(peerName: "  "),
            "Only the people on this call can hear it.")
    }
}
