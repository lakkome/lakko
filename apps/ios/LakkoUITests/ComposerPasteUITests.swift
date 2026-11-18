// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import UIKit
import XCTest

/// `docs/ux/attachments.md` section 2.5 — the composer's Paste attaches a
/// copied photo and keeps pasting text as text. Drives the production
/// composer field through the edit menu, as a person does.
final class ComposerPasteUITests: XCTestCase {

    private var app: XCUIApplication!

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchArguments = ["-LakkoUITestFixture", "composerPaste"] + UiTestLanguage.english
    }

    override func tearDown() {
        app = nil
        super.tearDown()
    }

    func testPastingACopiedPhotoAttachesIt() throws {
        let image = UIGraphicsImageRenderer(size: CGSize(width: 8, height: 8)).image { context in
            UIColor.red.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 8, height: 8))
        }
        UIPasteboard.general.image = image
        app.launch()
        try pasteIntoComposer()
        let result = app.staticTexts["composer-paste-result"]
        let photo = NSPredicate(format: "label BEGINSWITH 'pasted: photo'")
        XCTAssertEqual(
            XCTWaiter.wait(for: [expectation(for: photo, evaluatedWith: result)], timeout: 5), .completed,
            "a pasted photo did not reach the media path")
        XCTAssertFalse(composerText().contains("pasted"), "the photo paste also put text in the field")
    }

    func testPastingTextPastesText() throws {
        UIPasteboard.general.string = "hello from the pasteboard"
        app.launch()
        try pasteIntoComposer()
        let field = composer()
        let pasted = NSPredicate(format: "value CONTAINS 'hello from the pasteboard'")
        XCTAssertEqual(
            XCTWaiter.wait(for: [expectation(for: pasted, evaluatedWith: field)], timeout: 5), .completed,
            "pasted text did not land in the field")
        XCTAssertEqual(app.staticTexts["composer-paste-result"].label, "pasted: none")
    }

    private func composer() -> XCUIElement {
        app.textViews["composer-field"].firstMatch
    }

    private func composerText() -> String {
        (composer().value as? String) ?? ""
    }

    /// Focus the field, open its edit menu, choose Paste.
    private func pasteIntoComposer() throws {
        let field = composer()
        XCTAssertTrue(field.waitForExistence(timeout: 10), "the fixture did not present the composer")
        field.tap()
        let paste = app.menuItems["Paste"].firstMatch
        for _ in 0..<3 where !paste.exists {
            field.tap()
            _ = paste.waitForExistence(timeout: 2)
        }
        XCTAssertTrue(paste.exists, "the edit menu offered no Paste")
        paste.tap()
    }
}
