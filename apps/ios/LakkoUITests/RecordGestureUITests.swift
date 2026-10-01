// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// UX spec section 3.1 — ONE composer button answering TWO gestures: a tap
/// switches between the microphone and the camera and records nothing; a press
/// held past ``RecordingMachine.holdToRecordMs`` records.
///
/// This is the one thing only an XCUITest can assert, and the reason this
/// bundle exists. The rule is a function of how long a touch lasts, so testing
/// it needs a touch that is genuinely held down — and the simulator
/// touch-injection tooling available outside XCUITest does not hold one: a
/// requested 1.8 s hold was measured arriving as a 41 ms press-to-release,
/// which reads as a tap. That measurement is why an earlier session wrongly
/// reported the record button as broken. `XCUIElement.press(forDuration:)`
/// holds the touch for real, against the production thresholds, with no test
/// hook in the state machine.
///
/// The gesture under test ends with the finger LIFTING, and lifting a plain
/// hold sends the take — so by the time `press` returns there would be nothing
/// left on screen to look at. The test therefore uses the production LOCK
/// gesture (section 2.5: slide up past `RecordingMachine.lockSlidePoints` while
/// recording) so the take is still running after the finger is gone, and the
/// running take can be observed. That is also the strongest available proof
/// that the hold really recorded: a locked take's clock keeps counting, which
/// only happens while the recorder is capturing.
///
/// Everything queried below is a PRODUCTION accessibility label or a
/// production string. Nothing in the app was given a test identifier.
final class RecordGestureUITests: XCTestCase {

    /// `RecordButton` — `.accessibilityLabel(isVideo ? … : "Record voice message")`.
    private let voiceButtonLabel = "Record voice message"
    /// The same, after a tap has switched the mode.
    private let videoButtonLabel = "Record video message"
    /// `RecordingBar` — the Delete button, present while a take is locked or
    /// waiting in preview and at no other time. Its existence IS "a take
    /// exists"; a tap must never produce one.
    private let deleteTakeLabel = "Delete recording"
    /// `ChatMediaController.toggleMode()` — what a camera-less host answers a
    /// mode-switch tap with.
    private let noCameraHint = "No camera found"

    private var app: XCUIApplication!

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchArguments = ["-LakkoUITestFixture", "voiceBubble"] + UiTestLanguage.english
        app.launch()
    }

    override func tearDown() {
        app = nil
        super.tearDown()
    }

    /// Press and hold records; tap does not.
    ///
    /// Both halves are one test because they share the launch and — on a host
    /// whose microphone permission has not been decided yet — the permission
    /// grant, which the first hold unavoidably spends (`RecordingMachine`
    /// section 6: "the gesture that raised the prompt does not record, granted
    /// or not").
    func testHoldRecordsAndTapDoesNot() throws {
        let recordButton = app.buttons[voiceButtonLabel]
        XCTAssertTrue(
            recordButton.waitForExistence(timeout: 20),
            "the fixture did not present the composer's record button"
        )

        // Spend the permission prompt, if this host still has one to spend, on
        // a hold that is expected to record nothing.
        settleMicrophonePermission(on: recordButton)

        // --- A held press records -------------------------------------------

        holdAndSlideUpToLock(recordButton)
        let deleteTake = app.buttons[deleteTakeLabel]
        XCTAssertTrue(
            deleteTake.waitForExistence(timeout: 5),
            """
            a press held past the hold threshold and slid up to the lock target \
            did not leave a take running. If the microphone is denied on this \
            host the app shows the permission card instead, which is correct \
            behaviour and not this test's subject — grant the microphone and \
            re-run.
            """
        )

        // A locked take is still CAPTURING, so its elapsed clock advances.
        // That is what separates "recording" from "a finished take sitting in
        // preview", which shows the same Delete button and a clock that does
        // not move.
        //
        // Compared as a whole set of `m:ss` labels rather than by picking one,
        // because the canned voice bubble above the composer carries a duration
        // in the same shape and a positional query could read that instead. It
        // is a constant, so the only label on this screen that CAN change is
        // the recording bar's.
        let before = clockLabels()
        XCTAssertFalse(before.isEmpty, "nothing on screen shows an elapsed time")
        var advanced = false
        for _ in 0..<12 where !advanced {
            // One second of wall clock is one tick of an `m:ss` display.
            Thread.sleep(forTimeInterval: 0.5)
            if clockLabels() != before { advanced = true }
        }
        XCTAssertTrue(
            advanced,
            "no clock moved past \(before), so the take was not capturing — a hold produced no recording"
        )

        // Put the take away through the production control, so the second half
        // starts from an idle composer.
        deleteTake.tap()
        XCTAssertTrue(
            waitForDisappearance(of: deleteTake, timeout: 5),
            "deleting the take did not return the composer to idle"
        )

        // --- A tap does not ------------------------------------------------

        let labelBeforeTap = currentRecordButtonLabel()
        // Tapped by coordinate, like the hold above, for one reason: the
        // button's touches come from a UIKit overlay (``RecordTouchSurface``)
        // that is deliberately not an accessibility element, so the SwiftUI
        // element XCUITest matched is not the view that will receive the touch.
        // A coordinate tap delivers a real touch at that point and leaves the
        // hit-testing to UIKit, exactly as a finger does.
        app.buttons[labelBeforeTap]
            .coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
            .tap()

        // The tap's own outcome is the mode switch. On a host with a front
        // camera the button's label flips; on one without (every simulator)
        // `toggleMode` refuses with a hint instead. Either proves the touch was
        // read as a TAP and not as a hold — which is the discrimination under
        // test — and neither may have recorded anything.
        //
        // Polled for BOTH outcomes at once rather than waited on in turn,
        // because the refusal is TRANSIENT: `ChatMediaController.flashHint`
        // clears itself after 1.8 s. Waiting out a `waitForExistence` on the
        // flip first spends the hint's whole lifetime, so on a camera-less
        // host neither outcome would be visible by the time it is read and the
        // test would fail with the app behaving exactly as specified. The
        // deadline below therefore sits inside that 1.8 s, and whichever
        // outcome arrives first wins.
        let flipped = labelBeforeTap == voiceButtonLabel ? videoButtonLabel : voiceButtonLabel
        var switched = false
        var refused = false
        let deadline = Date().addingTimeInterval(1.5)
        while Date() < deadline, !switched, !refused {
            switched = app.buttons[flipped].exists
            refused = app.staticTexts[noCameraHint].exists
            if !switched, !refused { Thread.sleep(forTimeInterval: 0.1) }
        }
        XCTAssertTrue(
            switched || refused,
            "a tap neither switched the record mode nor said why it could not"
        )
        XCTAssertFalse(
            app.buttons[deleteTakeLabel].exists,
            "a tap started a recording; only a held press may do that"
        )
    }

    // MARK: - Harness

    /// Hold the button past the hold threshold, then slide up onto the lock
    /// target, so the take survives the finger lifting.
    ///
    /// 0.8 s is comfortably past `RecordingMachine.holdToRecordMs` (200 ms) and
    /// leaves room for the permission check on a host that has already granted
    /// it. The travel is 140 pt up, past `lockSlidePoints` (60) with margin,
    /// and straight up so it cannot be read as the slide-left that cancels
    /// (`cancelSlidePoints`, 80 pt of horizontal travel).
    private func holdAndSlideUpToLock(_ button: XCUIElement) {
        let start = button.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let lockTarget = start.withOffset(CGVector(dx: 0, dy: -140))
        start.press(forDuration: 0.8, thenDragTo: lockTarget)
    }

    /// Leave this host's microphone permission in a decided state.
    ///
    /// The first hold on a host that has never been asked raises the system
    /// prompt, and that gesture deliberately records nothing whichever way the
    /// answer goes. So: hold once, allow if asked, and tidy up whatever the
    /// hold left behind. A host that has already answered spends this on a
    /// hold that DOES record, which the cleanup then discards — harmless
    /// either way, which is why it is unconditional rather than guessing at
    /// the current permission state.
    private func settleMicrophonePermission(on button: XCUIElement) {
        holdAndSlideUpToLock(button)
        allowSystemPermissionPromptIfShown()
        let deleteTake = app.buttons[deleteTakeLabel]
        if deleteTake.waitForExistence(timeout: 3) {
            deleteTake.tap()
            _ = waitForDisappearance(of: deleteTake, timeout: 5)
        }
        // A denial shows the inline card; dismissing it keeps the screen clean
        // for the assertions. The card's own failure surfaces in the hold
        // assertion's message rather than here.
        let dismiss = app.buttons["Dismiss"]
        if dismiss.exists { dismiss.tap() }
    }

    /// Tap the affirmative button of a system permission alert, if one is up.
    ///
    /// Driven from SpringBoard directly rather than through
    /// `addUIInterruptionMonitor`, which only fires when the test next
    /// interacts with the app and so cannot be relied on to clear an alert
    /// that appeared in the middle of a gesture.
    private func allowSystemPermissionPromptIfShown() {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        // The microphone alert's affirmative button is "OK"; other iOS
        // permission alerts use one of the others. Matching several keeps this
        // from depending on one OS version's wording.
        for label in ["OK", "Allow", "Allow While Using App"] {
            let button = springboard.buttons[label]
            if button.waitForExistence(timeout: 2) {
                button.tap()
                return
            }
        }
    }

    /// Every `m:ss` label on screen (`RecordingMachine.formatClock` and
    /// `formatTotalClock` both render that shape).
    ///
    /// Matched by shape rather than by position: the fixture shows a canned
    /// voice message whose duration reads the same way, and a positional query
    /// would be one layout change away from reading the wrong one.
    private func clockLabels() -> [String] {
        app.staticTexts
            .matching(NSPredicate(format: "label MATCHES %@", "^[0-9]+:[0-9]{2}$"))
            .allElementsBoundByIndex
            .map(\.label)
    }

    private func waitForDisappearance(of element: XCUIElement, timeout: TimeInterval) -> Bool {
        let gone = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: element)
        return XCTWaiter.wait(for: [gone], timeout: timeout) == .completed
    }

    /// Whichever of the two record-mode labels the button currently carries.
    private func currentRecordButtonLabel() -> String {
        app.buttons[voiceButtonLabel].exists ? voiceButtonLabel : videoButtonLabel
    }
}
