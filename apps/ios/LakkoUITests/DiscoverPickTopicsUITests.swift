// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// What VoiceOver hears inside Discover's first-run "Pick a few topics." card,
/// read from the accessibility tree of the production card.
///
/// Seen live: the card's own name, "Pick topics", reached every element in it,
/// so the heading, the explanation, the language picker, Not now and Save were
/// all read as "Pick topics". Each element must keep its own name, and the
/// card's name must stay on the card.
final class DiscoverPickTopicsUITests: XCTestCase {

    private var app: XCUIApplication!

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchArguments = ["-LakkoUITestFixture", "discoverPickTopics"] + UiTestLanguage.english
    }

    override func tearDown() {
        app = nil
        super.tearDown()
    }

    func testEachElementKeepsItsOwnName() {
        app.launch()
        let save = app.buttons["Save"]
        XCTAssertTrue(save.waitForExistence(timeout: 10), "no Save button:\n\(app.debugDescription)")
        XCTAssertTrue(app.buttons["Not now"].exists, "no Not now button:\n\(app.debugDescription)")

        let picker = app.buttons["Language"]
        XCTAssertTrue(picker.exists, "no Language picker:\n\(app.debugDescription)")
        XCTAssertEqual(picker.value as? String, "English", "the Language picker does not read its value")

        XCTAssertTrue(
            app.staticTexts["Pick a few topics."].exists, "no heading text:\n\(app.debugDescription)")
        XCTAssertTrue(
            app.staticTexts["Lakko downloads suggestions for them, mixed with a few random topics."].exists,
            "the explanation does not read its own text:\n\(app.debugDescription)")

        XCTAssertEqual(
            app.buttons.matching(identifier: "Pick topics").count, 0,
            "a button carries the card's name:\n\(app.debugDescription)")
        XCTAssertEqual(
            app.staticTexts.matching(identifier: "Pick topics").count, 0,
            "a text carries the card's name:\n\(app.debugDescription)")
    }

    /// The name stays on the card itself, which holds the elements above:
    /// VoiceOver announces "Pick topics" on entering the card, not on each
    /// control. (The heading trait has no public XCUITest reader; it is set
    /// in `PickTopicsCard` beside the card's `.contain`.)
    func testTheCardKeepsItsNameAndHoldsItsControls() {
        app.launch()
        let card = app.otherElements["Pick topics"]
        XCTAssertTrue(card.waitForExistence(timeout: 10), "the card is not named:\n\(app.debugDescription)")
        XCTAssertTrue(card.buttons["Save"].exists, "Save is not inside the card:\n\(app.debugDescription)")
        XCTAssertTrue(card.buttons["Not now"].exists, "Not now is not inside the card:\n\(app.debugDescription)")
        XCTAssertTrue(
            card.staticTexts["Pick a few topics."].exists, "the heading is not inside the card:\n\(app.debugDescription)")
    }
}
