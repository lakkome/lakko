// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// UX spec §2 "Hearing nothing" — the recording bar says `No sound from the
/// microphone` when the measured level has stayed at zero, and stops saying it
/// the moment anything is heard.
///
/// Why this is an XCUITest and not a unit test. The rule itself
/// (``MicrophoneSilence``) has thirteen unit tests and needs none of this; what
/// had NO test on iOS was whether the notice reaches a screen at all. Nothing in
/// `LakkoTests` can read a SwiftUI view's text, and the one state the bar draws
/// it in is behind `private(set)` state whose only writer opens a real
/// microphone — so a unit test would have had to record for two seconds and hope
/// the host machine heard nothing, which measures the room rather than the app.
///
/// This drives the DEBUG-only `micSilence` fixture instead: it places the take's
/// STATE and then feeds real level samples through the production silence fold,
/// so every word read below was put on screen by the shipping rule. Android
/// covers the same notice with a Compose UI test; this is the iOS equivalent.
///
/// Everything queried is a PRODUCTION string. The two `fixture:` buttons belong
/// to the fixture and are named so they cannot be mistaken for app controls.
final class MicrophoneSilenceUITests: XCTestCase {

    /// `MicrophoneSilence.warningCopy` — the one wording, duplicated here
    /// because a UI-test bundle does not link the app module. Changing the copy
    /// has to fail this test; that is the point of the duplication.
    private let warningCopy = "No sound from the microphone"
    /// `RecordingBar` — what its combined spoken row always begins with, used as
    /// the proof that the bar is on screen at all. Without it, "the warning is
    /// absent" would also be true of a blank screen.
    private let recordingRowPrefix = "Recording,"
    /// `RecordingBar` — the locked arm's own control.
    private let deleteTakeLabel = "Delete recording"

    private var app: XCUIApplication!

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchArguments = ["-LakkoUITestFixture", "micSilence"]
        app.launch()
    }

    override func tearDown() {
        app = nil
        super.tearDown()
    }

    /// A take that is being held down, with a microphone reporting nothing.
    ///
    /// The bar folds that row's children into ONE accessibility element and
    /// gives it an explicit label, deliberately — so a `Text` added inside it is
    /// never spoken on its own, and the warning has to be IN the label or it is
    /// not said at all. That is what is read here, and it is the assertion the
    /// production comment on `recordingLabel` asks for.
    func testARecordingTakeWithASilentMicrophoneSaysSoInItsSpokenRow() throws {
        let row = element(containing: recordingRowPrefix)
        XCTAssertTrue(
            row.waitForExistence(timeout: 20),
            "the fixture did not present the recording bar, so nothing below is about the notice")

        XCTAssertTrue(
            element(containing: warningCopy).waitForExistence(timeout: 5),
            """
            a take whose microphone reported silence past the warning window said \
            nothing about it. The row on screen reads: \(row.label)
            """
        )
    }

    /// A locked take — §2 calls it *"the one most worth warning about"*, because
    /// the finger is off the button and it runs until it is stopped.
    ///
    /// Its arm draws the notice as its own line rather than folding it into one
    /// label, because combining would take the Delete button away from a screen
    /// reader. Both halves of that sentence are asserted: the notice is a
    /// visible line of its own, and Delete is still individually reachable.
    func testALockedTakeShowsTheNoticeAsItsOwnLine() throws {
        XCTAssertTrue(
            element(containing: recordingRowPrefix).waitForExistence(timeout: 20),
            "the fixture did not present the recording bar")

        app.buttons["fixture: lock take"].tap()

        let delete = app.buttons[deleteTakeLabel]
        XCTAssertTrue(
            delete.waitForExistence(timeout: 5),
            "the take did not lock, so the locked arm's notice is not what is being looked at")
        XCTAssertTrue(
            app.staticTexts[warningCopy].waitForExistence(timeout: 5),
            "a locked take with a silent microphone drew no notice of its own")
    }

    /// §2 — the warning *"clears the moment any level is measured"*, so a
    /// microphone unmuted mid-take, or a person who had simply not started
    /// speaking yet, does not carry an accusation through the rest of it.
    ///
    /// The second assertion is what keeps the first from being satisfied by the
    /// bar disappearing: the row has to still be there, saying what it always
    /// says, with only the warning gone.
    func testHearingAnythingClearsTheNotice() throws {
        let warning = element(containing: warningCopy)
        XCTAssertTrue(
            warning.waitForExistence(timeout: 20),
            "the notice was not there to begin with, so its going away proves nothing")

        app.buttons["fixture: hear something"].tap()

        XCTAssertTrue(
            waitForDisappearance(of: warning, timeout: 5),
            "one level reading with sound in it did not clear the notice")
        XCTAssertTrue(
            element(containing: recordingRowPrefix).exists,
            "the recording bar itself went away, so the notice clearing was the bar leaving")
    }

    // MARK: - Harness

    /// The first element on screen whose accessibility label contains `text`.
    ///
    /// Matched over every element type rather than `staticTexts`, because the
    /// recording row is a COMBINED element whose type is not a static text, and
    /// the locked row's notice is — so one query has to find either.
    private func element(containing text: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@", text))
            .firstMatch
    }

    private func waitForDisappearance(of element: XCUIElement, timeout: TimeInterval) -> Bool {
        let gone = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: element)
        return XCTWaiter.wait(for: [gone], timeout: timeout) == .completed
    }
}
