// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import XCTest
@testable import Lakko

/// `docs/ux/calls.md` section 4.5.2 — the collapsed call window's size and
/// place, as values (``CallWindowGeometry``).
final class CallWindowPlacementTests: XCTestCase {

    /// An iPhone 15 in portrait, in points.
    private let phone = CGSize(width: 393, height: 852)
    /// An iPad in portrait.
    private let tablet = CGSize(width: 820, height: 1180)
    /// The area the window may use on the phone: the safe area.
    private let area = CGRect(x: 0, y: 0, width: 393, height: 852 - 59 - 34)

    // MARK: - Size

    func testAPhoneWindowIsUprightAtThirtySixPercentOfTheWidth() {
        let size = CallWindowGeometry.size(screen: phone, screenShare: false)
        XCTAssertEqual(size.width, (393 * 0.36).rounded())
        XCTAssertEqual(size.height, (size.width * 4 / 3).rounded(), "3:4, upright")
    }

    func testAPhoneWindowIsNeverNarrowerThan120Points() {
        let size = CallWindowGeometry.size(screen: CGSize(width: 320, height: 568), screenShare: false)
        XCTAssertEqual(size.width, 120)
    }

    func testATabletWindowLies() {
        let size = CallWindowGeometry.size(screen: tablet, screenShare: false)
        XCTAssertEqual(size.width, 320)
        XCTAssertEqual(size.height, 180, "16:9")
    }

    func testAScreenShareLiesOnAPhoneToo() {
        let size = CallWindowGeometry.size(screen: phone, screenShare: true)
        XCTAssertEqual(size.width, (393 * 0.36).rounded())
        XCTAssertEqual(size.height, (size.width * 9 / 16).rounded())
    }

    func testAPhoneInLandscapeIsStillAPhone() {
        XCTAssertTrue(CallWindowGeometry.isPhone(screen: CGSize(width: 852, height: 393)))
        XCTAssertFalse(CallWindowGeometry.isPhone(screen: tablet))
    }

    // MARK: - Corners

    func testTheTopRightCornerSitsBelowTheHeaderAndInFromTheEdge() {
        let size = CallWindowGeometry.size(screen: phone, screenShare: false)
        let frame = CallWindowGeometry.frame(corner: .topTrailing, size: size, in: area)
        XCTAssertEqual(frame.maxX, area.maxX - 16)
        XCTAssertEqual(frame.minY, area.minY + CallWindowGeometry.headerClearance)
    }

    func testABottomCornerSitsAboveTheBottomEdgeSoItRisesWithTheKeyboard() {
        let size = CallWindowGeometry.size(screen: phone, screenShare: false)
        let frame = CallWindowGeometry.frame(corner: .bottomLeading, size: size, in: area)
        XCTAssertEqual(frame.minX, 16)
        XCTAssertEqual(frame.maxY, area.maxY - 16)
        // The keyboard takes the bottom of the area away; the window follows.
        let withKeyboard = CGRect(x: 0, y: 0, width: area.width, height: area.height - 300)
        let raised = CallWindowGeometry.frame(corner: .bottomLeading, size: size, in: withKeyboard)
        XCTAssertEqual(raised.maxY, withKeyboard.maxY - 16)
    }

    func testAWindowTallerThanTheRoomLeftKeepsToTheTop() {
        let size = CallWindowGeometry.size(screen: phone, screenShare: false)
        let tiny = CGRect(x: 0, y: 0, width: 393, height: 150)
        let frame = CallWindowGeometry.frame(corner: .bottomTrailing, size: size, in: tiny)
        XCTAssertEqual(frame.minY, CallWindowGeometry.headerClearance)
    }

    func testAReleaseLandsInTheNearestCorner() {
        let size = CallWindowGeometry.size(screen: phone, screenShare: false)
        XCTAssertEqual(
            CallWindowGeometry.nearestCorner(to: CGPoint(x: 60, y: 700), size: size, in: area),
            .bottomLeading)
        XCTAssertEqual(
            CallWindowGeometry.nearestCorner(to: CGPoint(x: 330, y: 150), size: size, in: area),
            .topTrailing)
    }

    /// A flick: the point it would come to rest at decides, not the point the
    /// finger let go at.
    func testAFlickReachesTheCornerItWasThrownTowards() {
        let size = CallWindowGeometry.size(screen: phone, screenShare: false)
        let start = CallWindowGeometry.frame(corner: .topTrailing, size: size, in: area)
        // Let go a little below the start, but moving fast downwards.
        let projected = CGPoint(x: start.midX, y: start.midY + 900)
        XCTAssertEqual(
            CallWindowGeometry.nearestCorner(to: projected, size: size, in: area),
            .bottomTrailing)
    }

    // MARK: - Tuck

    func testAFlickPastTheEdgeTucksOnAPhoneOnly() {
        XCTAssertEqual(
            CallWindowGeometry.tuck(projected: CGPoint(x: -40, y: 300), in: area, phone: true),
            .leading)
        XCTAssertEqual(
            CallWindowGeometry.tuck(projected: CGPoint(x: 460, y: 300), in: area, phone: true),
            .trailing)
        XCTAssertNil(CallWindowGeometry.tuck(projected: CGPoint(x: 200, y: 300), in: area, phone: true))
        XCTAssertNil(
            CallWindowGeometry.tuck(projected: CGPoint(x: -40, y: 300), in: area, phone: false),
            "a tablet's window is not thrown")
    }

    func testTheStripSitsAtTheEdgeLevelWithItsCorner() {
        let size = CallWindowGeometry.size(screen: phone, screenShare: false)
        let window = CallWindowGeometry.frame(corner: .topTrailing, size: size, in: area)
        let strip = CallWindowGeometry.stripFrame(
            tuck: .trailing, corner: .topTrailing, size: size, in: area)
        XCTAssertEqual(strip.maxX, area.maxX)
        XCTAssertEqual(strip.midY, window.midY)
        let left = CallWindowGeometry.stripFrame(
            tuck: .leading, corner: .topTrailing, size: size, in: area)
        XCTAssertEqual(left.minX, area.minX)
    }

    func testDraggingTheStripInwardsBringsTheWindowBack() {
        XCTAssertTrue(CallWindowGeometry.untucks(CGSize(width: 60, height: 0), from: .leading))
        XCTAssertFalse(CallWindowGeometry.untucks(CGSize(width: -60, height: 0), from: .leading))
        XCTAssertTrue(CallWindowGeometry.untucks(CGSize(width: -60, height: 0), from: .trailing))
        XCTAssertFalse(CallWindowGeometry.untucks(CGSize(width: 10, height: 0), from: .leading))
    }

    // MARK: - One touch

    /// A drag that starts on the mute button only moves the window: the
    /// decision is made from the whole touch sequence, never from where the
    /// finger let go.
    func testADragThatStartsOnMuteNeverTogglesIt() {
        let window = CGRect(x: 240, y: 120, width: 145, height: 193)
        let onMute = CGPoint(x: window.maxX - 20, y: window.minY + 20)
        XCTAssertTrue(CallWindowGeometry.muteFrame(window: window).contains(onMute))
        XCTAssertEqual(
            CallWindowGeometry.touchOutcome(start: onMute, moved: true, window: window), .move)
        XCTAssertEqual(
            CallWindowGeometry.touchOutcome(start: onMute, moved: false, window: window), .toggleMute)
    }

    func testATapElsewhereBringsTheCallBack() {
        let window = CGRect(x: 240, y: 120, width: 145, height: 193)
        let middle = CGPoint(x: window.midX, y: window.midY)
        XCTAssertEqual(
            CallWindowGeometry.touchOutcome(start: middle, moved: false, window: window), .expand)
        XCTAssertEqual(
            CallWindowGeometry.touchOutcome(start: middle, moved: true, window: window), .move)
    }

    func testTheDragThresholdSeparatesATapFromADrag() {
        XCTAssertFalse(CallWindowGeometry.isDrag(CGSize(width: 3, height: 4)))
        XCTAssertTrue(CallWindowGeometry.isDrag(CGSize(width: 6, height: 6)))
    }

    // MARK: - The inset

    func testTheOwnInsetSitsAwayFromTheScreenEdge() {
        XCTAssertTrue(CallWindowGeometry.insetOnLeading(corner: .topTrailing))
        XCTAssertTrue(CallWindowGeometry.insetOnLeading(corner: .bottomTrailing))
        XCTAssertFalse(CallWindowGeometry.insetOnLeading(corner: .topLeading))
        XCTAssertFalse(CallWindowGeometry.insetOnLeading(corner: .bottomLeading))
    }
}
