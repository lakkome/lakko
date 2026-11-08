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

    // MARK: - The encryption sheet's sentence and line

    private static let groupSentence = "Only the people on this call can hear it."
    private static let oneToOneHint =
        "Read the number aloud to each other. If it matches on both devices, no one else is listening."
    private static let groupHint =
        "Everyone in this call sees the same number. Read it aloud. If it matches on every device, no one else is listening."

    func testTheEncryptionSentenceNamesTheOtherPersonOnAOneToOneCall() {
        for others in [0, 1] {
            XCTAssertEqual(
                l10nPlain(CallPeerName.encryptionSentence(
                    peerName: "Anna", groupCall: false, othersInCall: others)),
                "Only you and Anna can hear this call.")
        }
    }

    /// With no name the sentence must stay true rather than inventing one.
    func testTheEncryptionSentenceInventsNoNameWhenThereIsNone() {
        XCTAssertEqual(
            CallPeerName.encryptionSentence(peerName: nil, groupCall: false, othersInCall: 1),
            Self.groupSentence)
        XCTAssertEqual(
            CallPeerName.encryptionSentence(peerName: "  ", groupCall: false, othersInCall: 1),
            Self.groupSentence)
    }

    /// The 2026-11-07 defect: a group call's sheet named one participant,
    /// claiming a two-person call. A group or link call names nobody, and a
    /// call with more than one other person reads as a group call.
    func testTheEncryptionSentenceNeverNamesOnePersonInAGroupCall() {
        for others in [0, 1, 2, 7] {
            XCTAssertEqual(
                CallPeerName.encryptionSentence(peerName: "Anna", groupCall: true, othersInCall: others),
                Self.groupSentence)
        }
        XCTAssertEqual(
            CallPeerName.encryptionSentence(peerName: "Anna", groupCall: false, othersInCall: 2),
            Self.groupSentence)
    }

    func testTheEncryptionHintFollowsTheKindOfCall() {
        XCTAssertEqual(
            CallPeerName.encryptionHint(groupCall: false, othersInCall: 1), Self.oneToOneHint)
        XCTAssertEqual(
            CallPeerName.encryptionHint(groupCall: false, othersInCall: 0), Self.oneToOneHint)
        for others in [0, 1, 4] {
            XCTAssertEqual(
                CallPeerName.encryptionHint(groupCall: true, othersInCall: others), Self.groupHint)
        }
        XCTAssertEqual(
            CallPeerName.encryptionHint(groupCall: false, othersInCall: 3), Self.groupHint)
        for hint in [Self.oneToOneHint, Self.groupHint] {
            XCTAssertFalse(hint.lowercased().contains("phone"))
        }
    }
}
