// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// `docs/ux/message-actions.md` sections 2 and 3 — the per-message action menu,
/// driven by a finger.
///
/// Why these are here and not in `LakkoTests`. The menu opens on a press held
/// past the long-press threshold, and that is the one thing a unit test cannot
/// do: `MessageActionMenuTests` asserts the action set, the layout arithmetic and
/// the two surfaces' sizes, but it can neither hold a touch down nor see what the
/// composed menu looks like on a real screen. Both of those are what this file
/// adds, and they are the assertions that would catch the shipped defect —
/// reactions behind a `React >` disclosure that opened a vertical submenu — from
/// the outside.
///
/// The reaction buttons are found by their accessibility identifiers
/// (`message-reaction-<emoji>`, `message-reaction-more`), because their labels
/// come from the interface catalog and change wording with the language. The
/// card's rows (`Copy`, `Edit`, `Delete`) are still read by their English
/// labels, so the app is launched in English (`UiTestLanguage.english`).
final class MessageActionMenuUITests: XCTestCase {

    /// `MessageReactionRow.palette`, duplicated deliberately: a UI-test bundle
    /// runs in its own process and cannot import the app module, and a test that
    /// read the palette out of the app could not notice the palette changing.
    /// Changing either side makes these tests fail, which is the notification a
    /// cross-process contract needs.
    private let palette = ["👍", "❤️", "😂", "😮", "😢", "🙏"]

    /// `MessageActionsFixture` — the two canned messages.
    private let theirMessage = "Their message"
    private let myMessage = "My message"

    /// Comfortably past `MessageLongPress.thresholdSeconds` (0.5 s).
    private let longPress: TimeInterval = 0.9

    private var app: XCUIApplication!

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchArguments = ["-LakkoUITestFixture", "messageActions"] + UiTestLanguage.english
        app.launch()
    }

    override func tearDown() {
        app = nil
        super.tearDown()
    }

    /// Section 2 — the reactions are a horizontal row, each one tap away.
    ///
    /// Three separate claims, and the third is the one the spec is about:
    ///
    ///   1. all six emoji are on screen immediately after the press, so there is
    ///      no `React >` row and no submenu to open first;
    ///   2. they are laid out as a ROW — one shared vertical centre, increasing
    ///      horizontal ones. A column (which is what a nested `Menu` produced)
    ///      fails this on both counts;
    ///   3. tapping one reacts. The fixture shows what the row's `onReact` was
    ///      handed, which is the closure production wires to the view model, so
    ///      "one tap" is measured by the reaction arriving and not by the button
    ///      existing.
    func testTheReactionsAreARowAndOneTapAway() throws {
        openMenu(on: theirMessage)

        var frames: [CGRect] = []
        for emoji in palette {
            let button = app.buttons["message-reaction-\(emoji)"]
            XCTAssertTrue(
                button.exists,
                "\(emoji) was not on screen after the long press, so the reactions are still behind something"
            )
            frames.append(button.frame)
        }
        XCTAssertTrue(
            app.buttons["message-reaction-more"].exists,
            "the row has no trailing control for the full picker"
        )

        let centres = frames.map(\.midY)
        let spread = (centres.max() ?? 0) - (centres.min() ?? 0)
        XCTAssertLessThan(
            spread,
            2,
            "the six emoji do not share a vertical centre (spread \(spread) pt), so they are stacked, not in a row"
        )
        for (left, right) in zip(frames, frames.dropFirst()) {
            XCTAssertGreaterThan(
                right.midX,
                left.midX,
                "the emoji do not advance across the screen: \(frames.map(\.midX))"
            )
        }

        app.buttons["message-reaction-\(palette[0])"].tap()
        XCTAssertTrue(
            app.staticTexts["fixture: reacted \(palette[0])"].waitForExistence(timeout: 3),
            "one tap on an emoji did not react"
        )
        XCTAssertTrue(
            waitForDisappearance(of: app.buttons["Copy"], timeout: 3),
            "the menu stayed open after the reaction was sent"
        )
    }

    /// Section 2 — "the action card is sized by its own content … must not
    /// inherit its width", measured on the composed menu on a real screen.
    ///
    /// `MessageActionMenuTests` proves the card ignores the width it is offered;
    /// this proves the consequence in situ, which is the claim section 2 actually
    /// makes. The reaction row's span is read off its own end buttons because the
    /// row is a layout container with no element of its own; the card's width is
    /// read off one of its rows, which fill it.
    ///
    /// Under the defect — a card that stretches to the column it sits in — the
    /// Copy row would be at least as wide as the reaction row and probably as
    /// wide as the screen, so the comparison is a real discrimination and not a
    /// restatement of the padding.
    func testTheActionCardIsNarrowerThanTheReactionRowAndSitsBelowIt() throws {
        openMenu(on: theirMessage)

        let firstEmoji = app.buttons["message-reaction-\(palette[0])"].frame
        let lastEmoji = app.buttons["message-reaction-\(palette[palette.count - 1])"].frame
        let more = app.buttons["message-reaction-more"].frame
        let rowSpan = max(lastEmoji.maxX, more.maxX) - firstEmoji.minX
        XCTAssertGreaterThan(rowSpan, 0, "the reaction row measured no width on screen")

        let copy = app.buttons["Copy"]
        XCTAssertTrue(copy.exists, "the card has no Copy row")

        // The card as DRAWN, not as any element reports it. Two measurements
        // lied here in turn: the Copy row's frame (a row is only as wide as its
        // label, and sat narrow inside a card spanning the screen), and then
        // the card's accessibility frame (reported as the union of its rows,
        // so the same narrow number). What a person sees is pixels, so the
        // pixels are what is measured: the contiguous run of card-surface
        // pixels through the Copy label, against the dimmed screen either
        // side of it.
        let cardWidth = try drawnWidthOfSurface(through: copy.frame)
        XCTAssertLessThan(
            cardWidth,
            rowSpan,
            "the card is drawn \(cardWidth) pt wide, as wide as the reaction row (\(rowSpan) pt)"
        )
        // A card sized by one short word is nowhere near a phone's width; a
        // bound rather than a value, since the word changes with the locale.
        XCTAssertLessThan(
            cardWidth,
            app.windows.firstMatch.frame.width * 0.6,
            "the card is drawn \(cardWidth) pt wide: sized by its container, not its rows"
        )
        let card = copy
        XCTAssertGreaterThan(
            card.frame.minY,
            firstEmoji.maxY,
            "the action card is not below the reaction row"
        )
    }

    /// Section 3 — Edit and Delete are offered on "own, not deleted" and on
    /// nothing else.
    ///
    /// Both directions in one test, on purpose: the assertion that matters is the
    /// DIFFERENCE between them, and each half asserted alone would pass against a
    /// menu that offered the same thing everywhere.
    func testDeleteIsOfferedOnYourOwnMessageAndNotOnTheirs() throws {
        openMenu(on: theirMessage)

        XCTAssertTrue(app.buttons["Copy"].exists, "Copy was not offered on an incoming message")
        XCTAssertFalse(
            app.buttons["Delete"].exists,
            "Delete was offered on someone else's message"
        )
        XCTAssertFalse(app.buttons["Edit"].exists, "Edit was offered on someone else's message")

        dismissMenu()
        openMenu(on: myMessage)

        XCTAssertTrue(app.buttons["Delete"].exists, "Delete was not offered on our own message")
        XCTAssertTrue(app.buttons["Edit"].exists, "Edit was not offered on our own message")
    }

    /// Section 3 — Copy "puts the text on the clipboard. Nothing leaves the
    /// device."
    ///
    /// Read back through `UIPasteboard` from inside the app rather than from this
    /// process: a test process reading another app's pasteboard is a cross-process
    /// paste, which iOS answers with a permission banner. The round trip stays in
    /// one process and the result comes back as a line on screen, so what is
    /// asserted is still the real pasteboard and not a value the fixture kept.
    func testCopyPutsTheMessageTextOnThePasteboard() throws {
        openMenu(on: myMessage)

        app.buttons["Copy"].tap()
        XCTAssertTrue(
            waitForDisappearance(of: app.buttons["Copy"], timeout: 3),
            "the menu stayed open after Copy"
        )

        app.buttons["fixture: read clipboard"].tap()
        XCTAssertTrue(
            app.staticTexts["fixture: clipboard \(myMessage)"].waitForExistence(timeout: 3),
            "the message text did not reach the pasteboard"
        )
    }

    /// Base walk 2026-11-04 — Edit opens the field WITH the keyboard and the
    /// caret at the end: what is typed right away lands after the old text,
    /// with no tap on the field first.
    func testEditTakesTheKeyboardWithTheCaretAtTheEnd() throws {
        openMenu(on: myMessage)
        app.buttons["Edit"].tap()

        let field = app.textFields["edit-message-field"].firstMatch
        let exists = field.waitForExistence(timeout: 5)
            || app.textViews["edit-message-field"].firstMatch.waitForExistence(timeout: 1)
        XCTAssertTrue(exists, "the edit sheet did not appear")
        // Typed at the application, not at the field: only a focused field gets
        // it (XCUITest refuses `typeText` when nothing has keyboard focus).
        app.typeText(" edited")
        app.buttons["Save"].tap()
        XCTAssertTrue(
            app.staticTexts["fixture: saved \(myMessage) edited"].waitForExistence(timeout: 5),
            "the text typed after Edit did not go to the end of the field"
        )
    }

    // MARK: - Harness

    /// Hold a finger on a message until the menu opens.
    ///
    /// Pressed on the bubble's own text, which is a production string and the
    /// only element the bubble offers. Note the menu draws the bubble AGAIN at its
    /// own position, so once it is open two elements carry this label — which is
    /// why the press happens before anything else queries it, and why the
    /// assertions above are written against the menu's own controls.
    private func openMenu(on message: String) {
        let bubble = app.staticTexts[message].firstMatch
        XCTAssertTrue(
            bubble.waitForExistence(timeout: 20),
            "the fixture did not present the message \"\(message)\""
        )
        bubble.press(forDuration: longPress)
        let firstEmoji = app.buttons["message-reaction-\(palette[0])"]
        XCTAssertTrue(
            firstEmoji.waitForExistence(timeout: 5),
            "a press held \(longPress) s on \"\(message)\" did not open the action menu"
        )
        waitForSettledLayout(of: firstEmoji)
    }

    /// Wait until the menu has stopped moving.
    ///
    /// The menu measures its own row and card before it can place them (the
    /// layout rule needs both heights), so the surfaces exist for one pass before
    /// they are where they belong. Every assertion here that reads a `frame` would
    /// otherwise be racing that pass — and would fail intermittently, which is
    /// worse than failing.
    private func waitForSettledLayout(of element: XCUIElement) {
        var previous = element.frame
        let deadline = Date().addingTimeInterval(2)
        while Date() < deadline {
            Thread.sleep(forTimeInterval: 0.1)
            let current = element.frame
            if current == previous, current.height > 0 { return }
            previous = current
        }
        XCTFail("the menu's layout never settled (last frame \(previous))")
    }

    /// Tap the backdrop. Section 2 — the conversation behind the menu has
    /// receded, so a tap anywhere outside the menu dismisses it rather than
    /// reaching what is under it.
    ///
    /// Low on the screen, well clear of both fixture messages and the menus they
    /// open, and by coordinate because the backdrop is deliberately not an
    /// accessibility element.
    private func dismissMenu() {
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.9)).tap()
        XCTAssertTrue(
            waitForDisappearance(of: app.buttons["message-reaction-\(palette[0])"], timeout: 3),
            "a tap on the backdrop did not dismiss the menu"
        )
    }

    private func waitForDisappearance(of element: XCUIElement, timeout: TimeInterval) -> Bool {
        let gone = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: element)
        return XCTWaiter.wait(for: [gone], timeout: timeout) == .completed
    }

    /// Width, in points, of the light surface the screenshot shows on the
    /// horizontal line through `rowFrame`'s centre, measured outward from that
    /// centre until the dimmed conversation is reached on each side.
    ///
    /// The scrim is the conversation at roughly half brightness and the card
    /// is the elevated surface, so a luminance threshold separates them in
    /// both appearances. Reads the screenshot at its own scale and converts
    /// back to points.
    private func drawnWidthOfSurface(through rowFrame: CGRect) throws -> CGFloat {
        let shot = XCUIScreen.main.screenshot().image
        let cg = try XCTUnwrap(shot.cgImage, "the screenshot has no bitmap")
        let scale = CGFloat(cg.width) / app.windows.firstMatch.frame.width
        let data = try XCTUnwrap(cg.dataProvider?.data, "the screenshot has no pixel data")
        let bytes = try XCTUnwrap(CFDataGetBytePtr(data))
        let bpr = cg.bytesPerRow
        let bpp = cg.bitsPerPixel / 8
        // The row's vertical middle: clear of the card's rounded corners, which
        // pull the surface in for the top and bottom 12 pt, and where the only
        // non-surface pixels are the icon and the word - which the two scans
        // below start OUTSIDE of, inside the row's own horizontal padding.
        let y = Int(rowFrame.midY * scale)
        func isSurface(_ x: Int) -> Bool {
            guard x >= 0, x < cg.width else { return false }
            let p = y * bpr + x * bpp
            let r = CGFloat(bytes[p]), g = CGFloat(bytes[p + 1]), b = CGFloat(bytes[p + 2])
            // Luminance on 0...255; the dimmed conversation sits well under this.
            return (0.2126 * r + 0.7152 * g + 0.0722 * b) > 200
        }
        // Left edge: from just inside the row's leading padding, outward.
        var left = Int((rowFrame.minX + 4) * scale)
        XCTAssertTrue(isSurface(left), "the leading sample point is not on the card surface")
        while isSurface(left - 1) { left -= 1 }
        // Right edge: from just inside the row's trailing padding, outward.
        var right = Int((rowFrame.maxX - 4) * scale)
        XCTAssertTrue(isSurface(right), "the trailing sample point is not on the card surface")
        while isSurface(right + 1) { right += 1 }
        let width = CGFloat(right - left + 1) / scale
        // The drawn surface must contain the row it was sampled around, or the
        // number is not a card width. Rounding at two scanned edges is under a
        // point; the rows fill the card on both sides, so no more slack is due.
        XCTAssertGreaterThanOrEqual(width, rowFrame.width - 1, "the surface run (\(width) pt) is narrower than the row it brackets (\(rowFrame.width) pt)")
        return width
    }
}
