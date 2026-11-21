// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI
import UIKit

/// UX spec §4 — "Tapping an emoji inserts it at the caret".
///
/// The composer's text view records its selection here on every change, so
/// an insert lands at the caret; an insert with no caret known appends.
@MainActor
final class ComposerCaret: ObservableObject {
    /// The last selection as UTF-16 offsets, when the field reported one.
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

/// Base scenario 10 — a hardware keyboard works in the composer and in every
/// dialog: Return submits (send, save), Shift+Return or Option+Return breaks
/// the line. The on-screen keyboard's Return reaches no key handler and keeps
/// breaking the line.
enum HardwareReturn {
    /// Whether a hardware Return with these modifiers submits.
    static func submits(_ modifiers: EventModifiers) -> Bool {
        !modifiers.contains(.shift) && !modifiers.contains(.option)
    }

    /// The same rule for a UIKit key press (the composer's text view).
    static func submits(uiKitFlags flags: UIKeyModifierFlags) -> Bool {
        !flags.contains(.shift) && !flags.contains(.alternate)
    }
}

extension View {
    /// Return on a hardware keyboard runs `action`; with Shift or Option it
    /// is left to the field, which breaks the line.
    func submitOnHardwareReturn(_ action: @escaping () -> Void) -> some View {
        onKeyPress(keys: [.return], phases: .down) { press in
            guard HardwareReturn.submits(press.modifiers) else { return .ignored }
            action()
            return .handled
        }
    }
}

/// The chat composer's text field (UX spec section 5.2).
///
/// A UIKit text view under SwiftUI, because a SwiftUI text field pastes text
/// only and its paste cannot be intercepted below iOS 27:
/// `docs/ux/attachments.md` section 2.5 asks the composer's Paste (the edit
/// menu's, and a hardware keyboard's Command-V) to attach a copied photo or
/// video. The text view also reports its selection on every iOS version, so
/// the emoji picker inserts at the caret on iOS 17 too.
struct ComposerTextField: View {
    @Binding var text: String
    let caret: ComposerCaret
    var disabled: Bool
    /// A hardware Return sends (``HardwareReturn``).
    var onReturn: () -> Void = {}
    /// Whether the field holds the keyboard, so it can be given the keyboard
    /// programmatically (choosing Reply focuses the composer,
    /// `docs/ux/message-actions.md` section 3.1) and taken away (leaving the
    /// chat). `nil` leaves focus to the platform.
    var focus: Binding<Bool>? = nil
    /// Paste of a copied photo or video: the pasteboard's items, in order.
    /// `nil` keeps Paste a text paste only.
    var onPasteMedia: (([NSItemProvider]) -> Void)? = nil
    /// The empty field's text; `nil` is the conversation's "Message".
    var placeholder: String? = nil

    @Environment(\.chatTextScale) private var scale
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    var body: some View {
        ComposerTextView(
            text: $text, caret: caret, disabled: disabled,
            placeholder: placeholder ?? Localization.text("chat.composer.placeholder"),
            // `docs/ux/text-size.md` — the composer follows the chat text size.
            fontSize: ChatFontSize.points(style: .body, dynamicTypeSize: dynamicTypeSize, scale: scale),
            onReturn: onReturn, focus: focus, onPasteMedia: onPasteMedia)
            // The field takes every point the row leaves it. Without this the
            // UIKit view's answer to an unspecified width (its current bounds)
            // became its ideal width, and a field laid out narrow once (while
            // the row above changed) stayed narrow, centred in the bar.
            .frame(maxWidth: .infinity)
            .composerFieldChrome()
    }
}

/// The UIKit text view behind ``ComposerTextField``: grows to five lines,
/// then scrolls.
private struct ComposerTextView: UIViewRepresentable {
    @Binding var text: String
    let caret: ComposerCaret
    var disabled: Bool
    var placeholder: String
    var fontSize: CGFloat
    var onReturn: () -> Void
    var focus: Binding<Bool>?
    var onPasteMedia: (([NSItemProvider]) -> Void)?

    static let maxLines: CGFloat = 5

    func makeCoordinator() -> Coordinator { Coordinator(parent: self) }

    func makeUIView(context: Context) -> PastingComposerTextView {
        let view = PastingComposerTextView()
        view.delegate = context.coordinator
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.isScrollEnabled = false
        view.textColor = UIColor(Color.lakkoText)
        view.tintColor = UIColor(Color.lakkoAccent)
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.text = text
        apply(to: view, coordinator: context.coordinator)
        return view
    }

    func updateUIView(_ view: PastingComposerTextView, context: Context) {
        context.coordinator.parent = self
        apply(to: view, coordinator: context.coordinator)
        // The draft changed outside the field (an emoji inserted at the
        // caret, the draft cleared after send, a restored draft). Text being
        // composed (marked text) is never overwritten.
        if view.text != text, view.markedTextRange == nil {
            let wanted = caret.utf16Range
            view.text = text
            let length = (text as NSString).length
            if let wanted, wanted.lowerBound >= 0, wanted.upperBound <= length {
                view.selectedRange = NSRange(location: wanted.lowerBound, length: wanted.count)
            } else {
                view.selectedRange = NSRange(location: length, length: 0)
            }
            caret.utf16Range = view.selectedRange.location..<NSMaxRange(view.selectedRange)
            view.refreshPlaceholder()
            view.invalidateIntrinsicContentSize()
        }
        if let focus {
            if focus.wrappedValue, !view.isFirstResponder {
                DispatchQueue.main.async {
                    if view.window != nil, !view.isFirstResponder { view.becomeFirstResponder() }
                }
            } else if !focus.wrappedValue, view.isFirstResponder {
                DispatchQueue.main.async { view.resignFirstResponder() }
            }
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView view: PastingComposerTextView, context: Context) -> CGSize? {
        let width: CGFloat
        if let proposed = proposal.width, proposed.isFinite, proposed > 0 {
            width = proposed
        } else {
            width = max(view.bounds.width, 1)
        }
        let lineHeight = ceil(view.font?.lineHeight ?? 20)
        let fitting = view.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height
        let cap = ceil(lineHeight * Self.maxLines)
        let scrolls = fitting > cap + 0.5
        if view.isScrollEnabled != scrolls {
            DispatchQueue.main.async { view.isScrollEnabled = scrolls }
        }
        return CGSize(width: width, height: min(max(ceil(fitting), lineHeight), cap))
    }

    private func apply(to view: PastingComposerTextView, coordinator: Coordinator) {
        if view.font?.pointSize != fontSize {
            view.font = .systemFont(ofSize: fontSize)
            view.placeholderLabel.font = view.font
            view.invalidateIntrinsicContentSize()
        }
        view.placeholderLabel.text = placeholder
        view.accessibilityLabel = placeholder
        view.isComposerDisabled = disabled
        view.onPasteMedia = onPasteMedia
        view.onHardwareReturn = { [weak coordinator] in coordinator?.parent.onReturn() }
        view.refreshPlaceholder()
    }

    @MainActor
    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: ComposerTextView

        init(parent: ComposerTextView) { self.parent = parent }

        func textView(_ textView: UITextView, shouldChangeTextIn range: NSRange, replacementText: String) -> Bool {
            !parent.disabled
        }

        func textViewDidChange(_ textView: UITextView) {
            record(textView)
            if parent.text != textView.text { parent.text = textView.text }
            (textView as? PastingComposerTextView)?.refreshPlaceholder()
            textView.invalidateIntrinsicContentSize()
        }

        func textViewDidChangeSelection(_ textView: UITextView) {
            record(textView)
        }

        func textViewDidBeginEditing(_ textView: UITextView) {
            if let focus = parent.focus, !focus.wrappedValue { focus.wrappedValue = true }
        }

        func textViewDidEndEditing(_ textView: UITextView) {
            if let focus = parent.focus, focus.wrappedValue { focus.wrappedValue = false }
        }

        private func record(_ textView: UITextView) {
            let range = textView.selectedRange
            guard range.location != NSNotFound else { return }
            parent.caret.utf16Range = range.location..<NSMaxRange(range)
        }
    }
}

/// The composer's text view: Paste attaches a copied photo or video
/// (``PastedMedia``), a hardware Return sends (``HardwareReturn``).
final class PastingComposerTextView: UITextView {
    var onPasteMedia: (([NSItemProvider]) -> Void)?
    var onHardwareReturn: (() -> Void)?
    /// While a send is in flight nothing is typed or pasted; the keyboard
    /// stays.
    var isComposerDisabled = false
    let placeholderLabel = UILabel()
    private var handledReturns: Set<UIPress> = []

    override init(frame: CGRect, textContainer: NSTextContainer?) {
        super.init(frame: frame, textContainer: textContainer)
        placeholderLabel.textColor = .placeholderText
        placeholderLabel.numberOfLines = 1
        placeholderLabel.isAccessibilityElement = false
        addSubview(placeholderLabel)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func layoutSubviews() {
        super.layoutSubviews()
        let inset = textContainerInset
        let width = max(bounds.width - inset.left - inset.right, 0)
        let height = placeholderLabel.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height
        placeholderLabel.frame = CGRect(x: inset.left, y: inset.top, width: width, height: height)
    }

    func refreshPlaceholder() {
        placeholderLabel.isHidden = !text.isEmpty
    }

    private var pasteAttachesMedia: Bool {
        onPasteMedia != nil && PastedMedia.attachesAsMedia(itemTypes: PastedMedia.itemTypes(of: .general))
    }

    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        if action == #selector(paste(_:)) {
            if isComposerDisabled { return false }
            if pasteAttachesMedia { return true }
        }
        return super.canPerformAction(action, withSender: sender)
    }

    override func paste(_ sender: Any?) {
        guard !isComposerDisabled else { return }
        if pasteAttachesMedia, let onPasteMedia {
            onPasteMedia(UIPasteboard.general.itemProviders)
            return
        }
        super.paste(sender)
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        var rest = presses
        for press in presses {
            guard let key = press.key,
                  key.keyCode == .keyboardReturnOrEnter || key.keyCode == .keypadEnter,
                  markedTextRange == nil,
                  HardwareReturn.submits(uiKitFlags: key.modifierFlags) else { continue }
            rest.remove(press)
            handledReturns.insert(press)
            onHardwareReturn?()
        }
        if !rest.isEmpty { super.pressesBegan(rest, with: event) }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let rest = presses.subtracting(handledReturns)
        handledReturns.subtract(presses)
        if !rest.isEmpty { super.pressesEnded(rest, with: event) }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let rest = presses.subtracting(handledReturns)
        handledReturns.subtract(presses)
        if !rest.isEmpty { super.pressesCancelled(rest, with: event) }
    }
}

extension View {
    /// UX spec section 5.2 — the field grows to five lines on the sunken
    /// surface with a pill shape; the placeholder is "Message".
    func composerFieldChrome() -> some View {
        self
            .textFieldStyle(.plain)
            // `docs/ux/text-size.md` — the composer follows the chat text size.
            .chatFont(.body)
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
            .background(Color.lakkoSurfaceSunken)
            .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
    }
}
