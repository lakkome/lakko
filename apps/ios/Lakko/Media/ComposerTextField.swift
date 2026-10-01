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

    /// Record the field's current selection against the text it belongs to.
    ///
    /// SwiftUI can report a selection whose indices belong to an earlier value
    /// of `text` (the chat screen opening with a restored draft, a draft
    /// cleared after send), and `utf16Offset(in:)` traps on an index past the
    /// end, so the indices are clamped to the current text rather than
    /// trusted. A field with no selection leaves the last record in place;
    /// ``insert(_:into:at:)`` re-checks it against the text at insert time.
    @available(iOS 18.0, *)
    func record(selection: TextSelection?, in text: String) {
        guard let selection, case .selection(let range) = selection.indices else { return }
        let end = text.endIndex
        let lower = min(range.lowerBound, end)
        let upper = min(max(range.upperBound, lower), end)
        utf16Range = lower.utf16Offset(in: text)..<upper.utf16Offset(in: text)
    }

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
    /// The screen's focus for this field, so it can be given the keyboard
    /// programmatically (choosing Reply focuses the composer,
    /// `docs/ux/message-actions.md` section 3.1). `nil` leaves focus to the
    /// platform, which is what every other caller wants.
    var focus: FocusState<Bool>.Binding? = nil

    var body: some View {
        if #available(iOS 18.0, *) {
            SelectionTrackingField(text: $text, caret: caret, disabled: disabled, focus: focus)
        } else {
            TextField(Localization.text("chat.composer.placeholder"), text: $text, axis: .vertical)
                .lineLimit(1...5)
                .disabled(disabled)
                .composerFieldChrome()
                .composerFocus(focus)
        }
    }
}

extension View {
    /// `.focused(_:)` when the screen handed a focus binding in, the view
    /// unchanged otherwise.
    @ViewBuilder
    func composerFocus(_ focus: FocusState<Bool>.Binding?) -> some View {
        if let focus {
            self.focused(focus)
        } else {
            self
        }
    }
}

extension View {
    /// UX spec section 5.2 — the field grows to five lines on the sunken
    /// surface with a pill shape; the placeholder is "Message".
    func composerFieldChrome() -> some View {
        self
            .textFieldStyle(.plain)
            .font(.body)
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
            .background(Color.lakkoSurfaceSunken)
            .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
    }
}

/// The iOS 18 field: `TextField(text:selection:)`, with the selection kept
/// in this view's own state so the platform owns the caret and the parent
/// never writes it.
///
/// The caret is recorded as part of `body`, NOT through `.onChange(of:)`.
/// Injected typing (a UI test, the simulator's text action) can land more
/// than one keystroke per frame, and an `onChange` observing the text or the
/// selection then fires more than once per frame — SwiftUI reports that as
/// `onChange(of: String) action tried to update multiple times per frame`
/// (an "Invalid Configuration" fault in the unified log) and coalesces to the
/// last value. The body already runs for every change of either input, so
/// recording there gives the same freshest-value semantics with no per-frame
/// fault. The write is to a plain, unpublished cache on a class the parent
/// owns, so it invalidates nothing.
@available(iOS 18.0, *)
private struct SelectionTrackingField: View {
    @Binding var text: String
    let caret: ComposerCaret
    var disabled: Bool
    var focus: FocusState<Bool>.Binding?
    @State private var selection: TextSelection?

    var body: some View {
        let _ = caret.record(selection: selection, in: text)
        TextField(Localization.text("chat.composer.placeholder"), text: $text, selection: $selection, axis: .vertical)
            .lineLimit(1...5)
            .disabled(disabled)
            .composerFieldChrome()
            .composerFocus(focus)
    }
}
