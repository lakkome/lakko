// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// `docs/ux/navigation.md` section 2.2.1 — reordering pinned conversations
/// with a finger.
///
/// A unit test can check what a drop writes (`PinOrderTests`), but not the
/// one thing the spec asks of the gesture: press and hold a pinned row, then
/// move, and it is dragged; press, hold and lift without moving, and the
/// row's own menu opens as before. Those need a held touch, so they are here,
/// against `PinReorderFixture` (three pins, Anna, Boris, Clara, then two
/// unpinned rows, Dmitri and Elena).
final class PinReorderUITests: XCTestCase {

    private var app: XCUIApplication!

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchArguments = ["-LakkoUITestFixture", "pinReorder"] + UiTestLanguage.english
        app.launch()
    }

    override func tearDown() {
        app = nil
        super.tearDown()
    }

    private var order: String { app.staticTexts["pin-order"].label }

    private func row(_ name: String) -> XCUIElement { app.buttons["row-\(name)"] }

    private func drag(_ name: String, to target: XCUIElement, below: Bool = false) {
        let start = row(name).coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let end = target.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: below ? 0.9 : 0.1))
        start.press(forDuration: 0.8, thenDragTo: end, withVelocity: .slow, thenHoldForDuration: 0.5)
    }

    func testHoldAndMoveDragsAPinUp() {
        XCTAssertTrue(row("Clara").waitForExistence(timeout: 10))
        XCTAssertEqual(order, "Anna,Boris,Clara")
        drag("Clara", to: row("Anna"))
        XCTAssertEqual(order, "Clara,Anna,Boris")
        XCTAssertFalse(
            app.buttons["Open menu for Clara"].waitForExistence(timeout: 2),
            "a drag opened the row's menu too")
        XCTAssertEqual(app.staticTexts["row-opened"].label, "none", "a drag opened the conversation")
    }

    /// Just below the last pin is the lowest place the list offers a pin.
    func testAPinDroppedJustBelowTheLastPinLandsLast() {
        XCTAssertTrue(row("Anna").waitForExistence(timeout: 10))
        drag("Anna", to: row("Clara"), below: true)
        XCTAssertEqual(order, "Boris,Clara,Anna")
    }

    /// NOT the spec. Navigation 2.2.1 says a pin dropped anywhere below the
    /// last pin lands last; the list's own reorder refuses a drop among the
    /// unpinned rows (`.moveDisabled` on them, which is what keeps an
    /// unpinned row from starting a drag), so the pin goes back to its place
    /// and nothing is written. Pinned here so a change in either direction is
    /// noticed.
    func testADropAmongTheUnpinnedRowsChangesNothing() {
        XCTAssertTrue(row("Anna").waitForExistence(timeout: 10))
        drag("Anna", to: row("Elena"), below: true)
        XCTAssertEqual(order, "Anna,Boris,Clara")
    }

    func testATapStillOpensAPinnedRow() {
        XCTAssertTrue(row("Boris").waitForExistence(timeout: 10))
        row("Boris").tap()
        XCTAssertEqual(app.staticTexts["row-opened"].label, "Boris")
        XCTAssertFalse(app.buttons["Open menu for Boris"].exists)
    }

    func testHoldAndLiftOpensTheMenu() {
        XCTAssertTrue(row("Boris").waitForExistence(timeout: 10))
        row("Boris").press(forDuration: 1.0)
        XCTAssertTrue(
            app.buttons["Open menu for Boris"].waitForExistence(timeout: 3),
            "holding a pinned row and lifting did not open its menu")
        XCTAssertEqual(order, "Anna,Boris,Clara")
        XCTAssertEqual(
            app.staticTexts["row-opened"].label, "none",
            "the hold that opened the menu also opened the conversation")
    }

    func testHoldAndLiftOnAnUnpinnedRowOpensTheMenu() {
        XCTAssertTrue(row("Dmitri").waitForExistence(timeout: 10))
        row("Dmitri").press(forDuration: 1.0)
        XCTAssertTrue(app.buttons["Open menu for Dmitri"].waitForExistence(timeout: 3))
    }

    func testAnUnpinnedRowIsNotDragged() {
        XCTAssertTrue(row("Dmitri").waitForExistence(timeout: 10))
        drag("Dmitri", to: row("Anna"))
        XCTAssertEqual(order, "Anna,Boris,Clara")
    }
}
