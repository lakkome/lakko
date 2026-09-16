// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import SwiftUI
import UIKit
import XCTest

@testable import Lakko

/// The composer must keep every character of a keystroke burst that lands
/// faster than the frame rate.
///
/// Background (2026-09-16): the maintainer twice saw a burst typed into a
/// freshly opened chat lose most of its characters. The unified log showed
/// UIKit inserting every character while the field ended with a prefix (or
/// nothing): the text binding's owner, the `ChatViewModel`, was being replaced
/// under the field by a shell re-render, and SwiftUI pushed the new owner's
/// empty `draft` back into the text view. With a stable owner the field is
/// lossless; this test pins that for the composer field itself, driving the
/// real `UITextView` behind `ComposerTextField` through UIKit's own insertion
/// path (the same `insertText` the keyboard uses), several characters per
/// frame, against a `@Published` owner exactly like the view model's `draft`.
@MainActor
final class ComposerBurstTests: XCTestCase {

    /// Stand-in for the view model's `@Published var draft`.
    private final class DraftOwner: ObservableObject {
        @Published var draft: String = ""
    }

    private struct Harness: View {
        @ObservedObject var owner: DraftOwner
        let caret: ComposerCaret

        var body: some View {
            ComposerTextField(text: $owner.draft, caret: caret, disabled: false)
                .padding()
        }
    }

    private var window: UIWindow?

    override func tearDown() {
        window?.isHidden = true
        window = nil
        super.tearDown()
    }

    func testBurstFasterThanTheFrameRateLandsEveryCharacter() throws {
        let owner = DraftOwner()
        let caret = ComposerCaret()
        let textView = try mountComposer(owner: owner, caret: caret)

        let typed = "The quick brown fox jumps over"
        XCTAssertEqual(typed.count, 30)
        for (index, character) in typed.enumerated() {
            textView.insertText(String(character))
            // Three keystrokes per turn of the run loop, so the binding and
            // the selection both change more than once per frame — the
            // condition under which SwiftUI coalesces updates.
            if index % 3 == 2 { spin(seconds: 0.02) }
        }
        spin(seconds: 0.5)

        XCTAssertEqual(owner.draft, typed, "the binding lost characters")
        XCTAssertEqual(textView.text, typed, "the text view lost characters")
    }

    func testBurstAppendedToARestoredDraftLandsEveryCharacter() throws {
        let owner = DraftOwner()
        owner.draft = "Hi Anna"
        let caret = ComposerCaret()
        let textView = try mountComposer(owner: owner, caret: caret)

        let burst = " from iOS, typed fast"
        for (index, character) in burst.enumerated() {
            textView.insertText(String(character))
            if index % 4 == 3 { spin(seconds: 0.02) }
        }
        spin(seconds: 0.5)

        XCTAssertEqual(owner.draft, "Hi Anna" + burst)
        XCTAssertEqual(textView.text, "Hi Anna" + burst)
    }

    func testCaretIsRecordedAtTheEndOfTheTypedText() throws {
        let owner = DraftOwner()
        let caret = ComposerCaret()
        let textView = try mountComposer(owner: owner, caret: caret)

        for character in "Hello" {
            textView.insertText(String(character))
        }
        spin(seconds: 0.5)

        guard #available(iOS 18.0, *) else {
            // iOS 17 has no selection API; the picker appends at the end.
            XCTAssertNil(caret.utf16Range)
            return
        }
        XCTAssertEqual(caret.utf16Range, 5..<5)
        var text = owner.draft
        let end = ComposerCaret.insert("!", into: &text, at: caret.utf16Range)
        XCTAssertEqual(text, "Hello!")
        XCTAssertEqual(end, 6)
    }

    // MARK: - Harness

    private func mountComposer(owner: DraftOwner, caret: ComposerCaret) throws -> UITextView {
        let host = UIHostingController(rootView: Harness(owner: owner, caret: caret))
        // The test host is a scene-based app: a window that is not attached to
        // a scene is never laid out, so SwiftUI never creates the text view.
        let scene = try XCTUnwrap(
            UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first,
            "the app-hosted test runs inside a window scene"
        )
        let window = UIWindow(windowScene: scene)
        window.frame = scene.coordinateSpace.bounds
        window.rootViewController = host
        window.makeKeyAndVisible()
        self.window = window
        host.view.setNeedsLayout()
        host.view.layoutIfNeeded()
        spin(seconds: 0.3)
        let textView = try XCTUnwrap(
            host.view.firstDescendant(of: UITextView.self),
            "the composer renders a UITextView on this OS"
        )
        XCTAssertTrue(textView.becomeFirstResponder(), "the composer takes focus")
        spin(seconds: 0.2)
        // The text view carries whatever the binding started with.
        XCTAssertEqual(textView.text, owner.draft)
        // Type at the end, as a user who just tapped the field does.
        textView.selectedRange = NSRange(location: (textView.text as NSString).length, length: 0)
        spin(seconds: 0.05)
        return textView
    }

    private func spin(seconds: TimeInterval) {
        RunLoop.main.run(until: Date(timeIntervalSinceNow: seconds))
    }
}

private extension UIView {
    func firstDescendant<T: UIView>(of type: T.Type) -> T? {
        for subview in subviews {
            if let match = subview as? T { return match }
            if let match = subview.firstDescendant(of: type) { return match }
        }
        return nil
    }
}
