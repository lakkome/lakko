// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// UX spec §4 — "Tapping an emoji inserts it at the caret".
///
/// SwiftUI exposes a text field's selection only from iOS 18
/// (`TextField(text:selection:)`); the deployment floor is iOS 17. So the
/// caret is tracked where it can be, and an insert on iOS 17 appends at the
/// end of the draft — the one honest fallback without replacing the composer
/// with a UIKit text view.
@MainActor
final class ComposerCaret: ObservableObject {
    /// The last selection as UTF-16 offsets, when the platform reports one.
    var utf16Range: Range<Int>?

    /// Insert `insertion` into `text` at the tracked caret (replacing a
    /// selection), or append when no caret is known or it no longer fits the
    /// text. Returns the new caret offset.
    @discardableResult
    static func insert(_ insertion: String, into text: inout String, at range: Range<Int>?) -> Int {
        let utf16 = text.utf16
        guard let range, range.lowerBound >= 0, range.upperBound <= utf16.count,
              let lower = utf16.index(utf16.startIndex, offsetBy: range.lowerBound, limitedBy: utf16.endIndex)?
                .samePosition(in: text),
              let upper = utf16.index(utf16.startIndex, offsetBy: range.upperBound, limitedBy: utf16.endIndex)?
                .samePosition(in: text) else {
            text += insertion
            return text.utf16.count
        }
        text.replaceSubrange(lower..<upper, with: insertion)
        return range.lowerBound + insertion.utf16.count
    }
}

struct ComposerTextField: View {
    @Binding var text: String
    let caret: ComposerCaret
    var disabled: Bool

    var body: some View {
        if #available(iOS 18.0, *) {
            SelectionTrackingField(text: $text, caret: caret, disabled: disabled)
        } else {
            TextField("Message", text: $text, axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(1...4)
                .disabled(disabled)
        }
    }
}

@available(iOS 18.0, *)
private struct SelectionTrackingField: View {
    @Binding var text: String
    let caret: ComposerCaret
    var disabled: Bool
    @State private var selection: TextSelection?

    var body: some View {
        TextField("Message", text: $text, selection: $selection, axis: .vertical)
            .textFieldStyle(.roundedBorder)
            .lineLimit(1...4)
            .disabled(disabled)
            .onChange(of: selection) { _, new in
                guard let new, case .selection(let range) = new.indices else { return }
                caret.utf16Range = range.lowerBound.utf16Offset(in: text)..<range.upperBound.utf16Offset(in: text)
            }
            .onChange(of: text) { _, new in
                // An edit the selection did not report (an insert from the
                // picker) leaves the caret at the recorded end of the insert,
                // clamped to the text.
                if let r = caret.utf16Range, r.upperBound > new.utf16.count {
                    caret.utf16Range = new.utf16.count..<new.utf16.count
                }
            }
    }
}
