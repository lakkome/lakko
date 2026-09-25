// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// `docs/ux/calls.md` section 4.5 — the collapsed call bar, on a screen.
///
/// Why this exists next to ``CollapsedCallBarTests``. That file pins the bar as
/// VALUES — which phase it sees, how long a collapse lasts, the four words, the
/// swipe — and says so at the top: nothing in it asserts that the bar honours
/// any of it, because a SwiftUI body is unreachable from XCTest. (Its comment
/// that "this client has no XCUITest target" is now out of date; this bundle is
/// the target.) A test that drives the rule directly can only show what the rule
/// was asked; it cannot show that the view asked, or that it drew the answer.
///
/// Four things are asserted here that no value test can reach: the bar renders
/// the rule's verdict and only one of the four things it can say, a tap on it
/// brings the call back, the mute button is there with both faces, and there is
/// **no End control anywhere on it**.
///
/// It reads the bar's ACCESSIBILITY LABEL rather than its visible glyphs, and
/// that is forced rather than chosen: the production view folds the strip into
/// one element (`accessibilityElement(children: .ignore)`) and gives it the whole
/// section-4.5 sentence, so the individual `Text`s are deliberately not in the
/// tree. The visible line and the spoken one are printed from the same
/// ``CallBarStatus`` by ``CallBarPresentation/line(_:)`` and `spokenLine(_:)`,
/// whose words `CollapsedCallBarTests` already pins against the call screen's.
///
/// Everything queried is a PRODUCTION string. The two `fixture:` labels belong
/// to the fixture and are named so they cannot be mistaken for app controls.
final class CollapsedCallBarUITests: XCTestCase {

    /// `CallBarPresentation.spokenLabel` — how the whole bar's one sentence
    /// ends, so "the bar is on screen" is a question that can be asked without
    /// knowing what state it is in.
    private let barSentenceTail = "return to call"
    /// `CallStatusText.spokenControlLabel(.microphone, on:)`, both faces.
    private let muteLabel = "Mute microphone"
    private let unmuteLabel = "Unmute microphone"
    /// `CallStatusText.spokenControlLabel(.end, on:)` — the label the bar must
    /// NOT carry. Duplicated here because a UI-test bundle does not link the app
    /// module; if the call screen's End is ever renamed this test goes stale
    /// rather than wrong, and the absence assertion below is paired with a
    /// positive control so it can never pass on an empty screen.
    private let endLabel = "End call"
    /// The three things the status line can say that are not a running time.
    private let stateWords = ["Calling...", "Connecting...", "Reconnecting..."]

    private var app: XCUIApplication!

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchArguments = ["-LakkoUITestFixture", "callBar"]
        app.launch()
    }

    override func tearDown() {
        app = nil
        super.tearDown()
    }

    /// Section 4.5 — *"the state or the running time, in the words of the status
    /// line and never two at once"*.
    ///
    /// A connecting call says `Connecting...` and says nothing else: not the
    /// other two states, and not a duration. The sum type the Rust rule returns
    /// is what makes that impossible to get wrong in the view, and this is the
    /// check that the view is in fact printing that verdict rather than
    /// composing its own line.
    func testAConnectingBarSaysThatOneStateAndNothingElse() throws {
        let bar = element(containing: barSentenceTail)
        XCTAssertTrue(
            bar.waitForExistence(timeout: 20),
            "the fixture did not present the collapsed call bar")

        XCTAssertTrue(
            bar.label.contains("Connecting..."),
            "a connecting call's bar does not say so. It reads: \(bar.label)")
        for word in ["Calling...", "Reconnecting..."] {
            XCTAssertFalse(
                bar.label.contains(word),
                "the bar said \(word) as well as Connecting..., which is two states at once")
        }
        XCTAssertFalse(
            bar.label.contains(" second"),
            "a call that has not connected is showing a running time: \(bar.label)")
    }

    /// The other half of the same rule: once media is flowing the bar reads the
    /// running time INSTEAD of a state, not beside it.
    ///
    /// The duration is asserted by its spoken shape rather than its digits,
    /// because section 8 says a duration is read as a duration and because the
    /// clock is live — a fresh call is some number of seconds old by the time
    /// the label is read, and any number of them says `second`.
    func testAConnectedBarSaysTheRunningTimeInsteadOfAState() throws {
        XCTAssertTrue(
            element(containing: barSentenceTail).waitForExistence(timeout: 20),
            "the fixture did not present the collapsed call bar")

        app.buttons["fixture: connect"].tap()

        // Re-queried after the tap: the label is part of the match, so the
        // element found before it changed is not the one to read.
        let connected = element(containing: barSentenceTail)
        XCTAssertTrue(
            connected.waitForExistence(timeout: 5),
            "the bar went away when the call connected")
        XCTAssertTrue(
            connected.label.contains(" second"),
            "a connected call's bar is not saying how long it has been running: \(connected.label)")
        for word in stateWords {
            XCTAssertFalse(
                connected.label.contains(word),
                "the bar said \(word) as well as the running time, which is two things at once")
        }
    }

    /// Section 4.5 — *"**There is no End on the bar.**"*
    ///
    /// Ending is the one irreversible control in a call, and a strip a thumb
    /// crosses on the way to a chat is the wrong place for it. The absence is
    /// paired with two positive controls — the bar itself and the mute button —
    /// so it cannot be satisfied by a screen with nothing on it.
    func testTheBarCarriesNoEndControl() throws {
        XCTAssertTrue(
            element(containing: barSentenceTail).waitForExistence(timeout: 20),
            "the fixture did not present the collapsed call bar")
        XCTAssertTrue(
            app.buttons[muteLabel].exists,
            "the bar's one control is missing, so 'no End' would be true of an empty bar too")

        XCTAssertEqual(
            app.descendants(matching: .any)
                .matching(NSPredicate(format: "label CONTAINS %@", endLabel))
                .count,
            0,
            "the collapsed call bar carries a control that can end the call")
    }

    /// Section 4.5 — *"**Tapping the bar anywhere else brings the call back.**"*
    ///
    /// Tapped by coordinate for the reason `RecordGestureUITests` gives: a
    /// coordinate tap delivers a real touch and leaves the hit-testing to UIKit,
    /// exactly as a finger does, rather than depending on which of the nested
    /// elements XCUITest decided to match.
    func testTappingTheBarBringsTheCallBack() throws {
        let bar = element(containing: barSentenceTail)
        XCTAssertTrue(
            bar.waitForExistence(timeout: 20),
            "the fixture did not present the collapsed call bar")
        XCTAssertFalse(app.staticTexts["fixture: expanded"].exists)

        bar.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()

        XCTAssertTrue(
            app.staticTexts["fixture: expanded"].waitForExistence(timeout: 5),
            "a tap on the bar did not bring the call back")
    }

    /// Section 4.5 — *"one round mute button, drawn on and off exactly as the
    /// call screen draws it"*, and it is the exception to "tapping anywhere
    /// else": pressing mute must not also put the person back on the call
    /// screen, or the one control on the bar would be unusable without leaving
    /// it.
    func testTheMuteButtonTogglesWithoutBringingTheCallBack() throws {
        let mute = app.buttons[muteLabel]
        XCTAssertTrue(
            mute.waitForExistence(timeout: 20),
            "the bar did not offer a mute button")

        mute.tap()

        XCTAssertTrue(
            app.buttons[unmuteLabel].waitForExistence(timeout: 5),
            "muting did not change the button to its unmute face")
        XCTAssertFalse(
            app.staticTexts["fixture: expanded"].exists,
            "pressing mute on the bar also brought the call screen back")
    }

    // MARK: - Harness

    /// The first element on screen whose accessibility label contains `text`.
    ///
    /// Matched over every element type rather than `buttons` or `staticTexts`:
    /// the strip is one combined element inside a `Button`, and which of the two
    /// XCUITest surfaces is SwiftUI's business, not this test's.
    private func element(containing text: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@", text))
            .firstMatch
    }
}
