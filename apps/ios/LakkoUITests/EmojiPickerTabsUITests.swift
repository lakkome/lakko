// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// What VoiceOver hears on the two tabs of the composer's emoji and GIF
/// picker, read from the accessibility tree of the production sheet.
///
/// Seen live: both segment buttons carried "Emoji and GIFs", the label of the
/// whole control, so the two tabs were read alike. Each tab must be named by
/// what it shows, the current one must be marked selected, and the control's
/// own label must not name a tab.
final class EmojiPickerTabsUITests: XCTestCase {

    private var app: XCUIApplication!

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchArguments = ["-LakkoUITestFixture", "emojiPicker"] + UiTestLanguage.english
    }

    override func tearDown() {
        app = nil
        super.tearDown()
    }

    func testEachTabIsNamedByWhatItShows() {
        app.launch()
        let emoji = app.buttons["Emoji"]
        let gif = app.buttons["GIF"]
        XCTAssertTrue(emoji.waitForExistence(timeout: 10), "no Emoji tab:\n\(app.debugDescription)")
        XCTAssertTrue(gif.exists, "no GIF tab:\n\(app.debugDescription)")
        XCTAssertEqual(
            app.buttons.matching(identifier: "Emoji and GIFs").count, 0,
            "a tab carries the picker's own name:\n\(app.debugDescription)")
    }

    func testTheSelectedTabIsMarkedAndFollowsATap() {
        app.launch()
        let emoji = app.buttons["Emoji"]
        let gif = app.buttons["GIF"]
        XCTAssertTrue(emoji.waitForExistence(timeout: 10), "no Emoji tab:\n\(app.debugDescription)")
        XCTAssertTrue(emoji.isSelected, "Emoji is not marked selected on open")
        XCTAssertFalse(gif.isSelected, "GIF is marked selected on open")

        gif.tap()

        let selected = NSPredicate(format: "isSelected == true")
        XCTAssertEqual(
            XCTWaiter.wait(for: [expectation(for: selected, evaluatedWith: gif)], timeout: 5), .completed,
            "GIF is not marked selected after a tap")
        XCTAssertFalse(emoji.isSelected, "Emoji stayed marked selected after GIF was tapped")
    }
}
