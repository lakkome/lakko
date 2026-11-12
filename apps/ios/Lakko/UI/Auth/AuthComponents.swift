// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

// MARK: - Screen scaffold

/// The frame every anonymous screen shares (UX spec sections 4 and 5.6): a
/// scrolling column on the page background, 16 pt side padding, a 480 pt
/// content cap on wide layouts, and the keyboard dismissed by dragging.
struct AuthScreen<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                content
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 24)
            .frame(maxWidth: 480)
            .frame(maxWidth: .infinity)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(Color.lakkoBackground.ignoresSafeArea())
    }
}

/// Screen title (22/600) with an optional one-line lead.
struct AuthTitle: View {
    let title: String
    var subtitle: String? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title)
                .font(.title2.weight(.semibold))
                .foregroundStyle(Color.lakkoText)
            if let subtitle {
                Text(subtitle)
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.bottom, 4)
    }
}

// MARK: - Fields

/// Label above, 44 pt control on the elevated surface with a hairline
/// border, accent ring while focused, helper or error text below.
struct AuthTextField: View {
    let label: String
    var placeholder: String = ""
    @Binding var text: String
    var keyboard: UIKeyboardType = .default
    var contentType: UITextContentType? = nil
    var capitalization: TextInputAutocapitalization = .never
    var autocorrect: Bool = false
    var disabled: Bool = false
    var submitLabel: SubmitLabel = .next
    var onSubmit: () -> Void = {}
    /// The screen's own focus of this field, when it moves the focus here
    /// (from another field's return key) or reads it; the field's own
    /// otherwise.
    var isFocused: FocusState<Bool>.Binding? = nil

    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            AuthFieldLabel(label)
            TextField(placeholder, text: $text)
                .keyboardType(keyboard)
                .textContentType(contentType)
                .textInputAutocapitalization(capitalization)
                .autocorrectionDisabled(!autocorrect)
                .submitLabel(submitLabel)
                .onSubmit(onSubmit)
                .focused(isFocused ?? $focused)
                .disabled(disabled)
                .authFieldChrome(focused: isFocused?.wrappedValue ?? focused)
        }
    }
}

/// A password field with a show / hide toggle. Switching between the
/// secure and plain control keeps the text.
struct AuthSecureField: View {
    let label: String
    var placeholder: String = ""
    @Binding var text: String
    var contentType: UITextContentType? = .password
    var disabled: Bool = false
    var submitLabel: SubmitLabel = .next
    var onSubmit: () -> Void = {}

    @State private var revealed = false
    @FocusState private var focused: Bool
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            AuthFieldLabel(label)
            HStack(spacing: 8) {
                Group {
                    if revealed {
                        TextField(placeholder, text: $text)
                    } else {
                        SecureField(placeholder, text: $text)
                    }
                }
                .textContentType(contentType)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled(true)
                .submitLabel(submitLabel)
                .onSubmit(onSubmit)
                .focused($focused)
                .disabled(disabled)

                Button {
                    revealed.toggle()
                } label: {
                    LkIconView(revealed ? .eyeSlash : .eye, size: 20)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .frame(width: 28, height: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(l10n.t(revealed ? "auth.password.hide" : "auth.password.show"))
            }
            .authFieldChrome(focused: focused)
        }
    }
}

/// 13/500 muted label above a field.
struct AuthFieldLabel: View {
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(.footnote.weight(.medium))
            .foregroundStyle(Color.lakkoTextMuted)
    }
}

/// Helper text under a field (muted) or an inline error (danger, with an
/// icon so colour is not the only signal).
struct AuthHint: View {
    let text: String
    var isError: Bool = false

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            if isError {
                LkIconView(.exclamationCircle, size: 14, relativeTo: .footnote)
            }
            Text(text)
                .font(.footnote)
        }
        .foregroundStyle(isError ? Color.lakkoDanger : Color.lakkoTextMuted)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityLabel(isError ? l10n.t("common.error.a11y", texts: ["message": text]) : text)
    }
}

private struct AuthFieldChrome: ViewModifier {
    let focused: Bool

    func body(content: Content) -> some View {
        content
            .font(.body)
            .foregroundStyle(Color.lakkoText)
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            .background(Color.lakkoSurface)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
            .overlay(
                RoundedRectangle(cornerRadius: LakkoRadius.control)
                    .stroke(
                        focused ? Color.lakkoAccent.opacity(0.6) : Color.lakkoBorder,
                        lineWidth: focused ? 2 : 1)
            )
    }
}

extension View {
    /// The input chrome from the design system (surface, hairline, focus ring).
    func authFieldChrome(focused: Bool) -> some View {
        modifier(AuthFieldChrome(focused: focused))
    }
}

// MARK: - Buttons

/// Accent fill, 44 pt, radius 8. While `busy` the label gives way to a
/// spinner and the busy title; the button stays visible, never disappears.
struct AuthPrimaryButton: View {
    let title: String
    var busy: Bool = false
    var busyTitle: String? = nil
    var disabled: Bool = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                if busy {
                    ProgressView()
                        .controlSize(.small)
                        .tint(Color.lakkoAccentForeground)
                }
                Text(busy ? (busyTitle ?? title) : title)
                    .font(.body.weight(.medium))
            }
            .foregroundStyle(Color.lakkoAccentForeground)
            .frame(maxWidth: .infinity, minHeight: 44)
            .background(Color.lakkoAccent.opacity(disabled && !busy ? 0.5 : 1))
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
        }
        .buttonStyle(.plain)
        .disabled(disabled || busy)
    }
}

/// Elevated surface with a hairline, accent text.
struct AuthSecondaryButton: View {
    let title: String
    var icon: LkIcon? = nil
    var disabled: Bool = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                if let icon {
                    LkIconView(icon, size: 20)
                }
                Text(title)
                    .font(.body.weight(.medium))
            }
            .foregroundStyle(Color.lakkoAccent)
            .frame(maxWidth: .infinity, minHeight: 44)
            .background(Color.lakkoSurface)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
            .overlay(
                RoundedRectangle(cornerRadius: LakkoRadius.control)
                    .stroke(Color.lakkoBorder, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        .opacity(disabled ? 0.5 : 1)
    }
}

/// A small text link, optionally with a muted lead ("New to Lakko?").
struct AuthLinkRow: View {
    var lead: String? = nil
    let title: String
    /// Text after the link, when the sentence continues past it.
    var trail: String? = nil
    var disabled: Bool = false
    let action: () -> Void

    /// A row built from a catalog message with a `{link}` part: the text
    /// around the link comes from the message, `title` is the link.
    init(message key: String, title: String, disabled: Bool = false, action: @escaping () -> Void) {
        let parts = Localization.shared.linkParts(key)
        let lead = parts.lead.trimmingCharacters(in: .whitespaces)
        let trail = parts.trail.trimmingCharacters(in: .whitespaces)
        self.lead = lead.isEmpty ? nil : lead
        self.title = title
        self.trail = trail.isEmpty ? nil : trail
        self.disabled = disabled
        self.action = action
    }

    init(lead: String? = nil, title: String, trail: String? = nil, disabled: Bool = false, action: @escaping () -> Void) {
        self.lead = lead
        self.title = title
        self.trail = trail
        self.disabled = disabled
        self.action = action
    }

    var body: some View {
        HStack(spacing: 4) {
            Spacer(minLength: 0)
            if let lead {
                Text(lead)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            Button(action: action) {
                Text(title)
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(Color.lakkoAccent)
            }
            .buttonStyle(.plain)
            .disabled(disabled)
            if let trail {
                Text(trail)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            Spacer(minLength: 0)
        }
        .frame(minHeight: 32)
    }
}

// MARK: - Phrase display grid

/// The phrase grid's column count for the current layout: the wide shape
/// (4 columns x 3 rows) on a regular-width layout (iPad) or a compact-height
/// one (a phone in landscape), else 3 columns x 4 rows.
struct PhraseGridColumns<Content: View>: View {
    @Environment(\.horizontalSizeClass) private var horizontal
    @Environment(\.verticalSizeClass) private var vertical
    @ViewBuilder let content: (Int) -> Content

    var body: some View {
        content(PhraseWords.gridColumns(wide: horizontal == .regular || vertical == .compact))
    }
}

/// The twelve numbered words on the sunken surface, reading across the rows:
/// 3 x 4 at phone width, 4 x 3 on a wide layout. One component for sign-up,
/// the fresh-phrase screen and "Show recovery phrase", so the phrase always
/// looks the same.
struct PhraseGrid: View {
    let words: [String]

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        PhraseGridColumns { columns in
            LazyVGrid(
                columns: Array(repeating: GridItem(.flexible(), spacing: 8, alignment: .leading), count: columns),
                alignment: .leading,
                spacing: 10
            ) {
                ForEach(PhraseWords.numbered(words), id: \.number) { cell in
                    HStack(alignment: .firstTextBaseline, spacing: 4) {
                        Text("\(cell.number)")
                            .font(.footnote.monospacedDigit())
                            .foregroundStyle(Color.lakkoTextMuted)
                            .frame(width: 20, alignment: .trailing)
                        Text(cell.word)
                            .font(.body.weight(.medium))
                            .foregroundStyle(Color.lakkoText)
                            .lineLimit(1)
                            .minimumScaleFactor(0.6)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .padding(16)
        .background(Color.lakkoSurfaceSunken)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(l10n.t("recovery_phrase.label") + ": " + words.enumerated()
            .map { "\($0.offset + 1) \($0.element)" }.joined(separator: ", "))
    }
}

// MARK: - Phrase entry grid

/// Twelve numbered fields in the same grid as ``PhraseGrid`` (3 x 4, or 4 x 3
/// on a wide layout, reading across the rows) with a Paste button. The phrase
/// is exposed as one space-joined string through `phrase`; a multi-word paste
/// into any cell spreads across the cells from that position, and Return
/// moves to the next word. Autocorrect is off: it would rewrite valid
/// wordlist entries into neighbouring dictionary words.
struct PhraseEntryGrid: View {
    @Binding var phrase: String
    var disabled: Bool = false

    @State private var cells: [String] = Array(repeating: "", count: PhraseWords.count)
    @FocusState private var focusedCell: Int?
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                AuthFieldLabel(l10n.t("recovery_phrase.label"))
                Spacer()
                Button {
                    if let pasted = UIPasteboard.general.string {
                        apply(PhraseWords.parsePasted(pasted), from: 0)
                    }
                } label: {
                    Label(l10n.t("common.paste"), icon: .paste, size: 14, relativeTo: .footnote)
                        .font(.footnote.weight(.medium))
                        .foregroundStyle(Color.lakkoAccent)
                }
                .buttonStyle(.plain)
                .disabled(disabled)
            }

            PhraseGridColumns { columns in
                LazyVGrid(
                    columns: Array(repeating: GridItem(.flexible(), spacing: 6), count: columns),
                    spacing: 8
                ) {
                    ForEach(0..<PhraseWords.count, id: \.self) { index in
                        cell(index)
                    }
                }
            }
            .padding(12)
            .background(Color.lakkoSurfaceSunken)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        }
        .onAppear {
            let words = PhraseWords.words(of: phrase)
            if !words.isEmpty { cells = PhraseWords.padded(words) }
        }
        .onChange(of: phrase) { _, newValue in
            // The owner cleared the phrase (after a submit): clear the cells.
            if newValue.isEmpty, cells.contains(where: { !$0.isEmpty }) {
                cells = Array(repeating: "", count: PhraseWords.count)
            }
        }
    }

    private func cell(_ index: Int) -> some View {
        HStack(spacing: 3) {
            Text("\(index + 1)")
                .font(.caption.monospacedDigit())
                .foregroundStyle(Color.lakkoTextMuted)
                .frame(width: 16, alignment: .trailing)
            TextField("", text: Binding(
                get: { cells[index] },
                set: { newValue in
                    let words = PhraseWords.parsePasted(newValue)
                    if words.count > 1 {
                        apply(words, from: index)
                    } else if newValue.hasSuffix(" "), let word = words.first {
                        // A space after a word moves on to the next one.
                        cells[index] = word
                        publish()
                        focusedCell = index + 1 < PhraseWords.count ? index + 1 : nil
                    } else {
                        cells[index] = newValue.lowercased()
                        publish()
                    }
                }
            ))
            .font(.subheadline)
            .foregroundStyle(Color.lakkoText)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled(true)
            .keyboardType(.asciiCapable)
            .submitLabel(index == PhraseWords.count - 1 ? .done : .next)
            .onSubmit {
                focusedCell = index + 1 < PhraseWords.count ? index + 1 : nil
            }
            .focused($focusedCell, equals: index)
            .disabled(disabled)
            .padding(.horizontal, 6)
            .frame(minHeight: 36)
            .background(Color.lakkoSurface)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
            .overlay(
                RoundedRectangle(cornerRadius: LakkoRadius.control)
                    .stroke(
                        focusedCell == index ? Color.lakkoAccent.opacity(0.6) : Color.lakkoBorder,
                        lineWidth: focusedCell == index ? 2 : 1)
            )
            .accessibilityLabel(l10n.t("recovery_phrase.word_label", texts: ["position": String(index + 1)]))
        }
    }

    /// Fill the cells from `start` with `words`, then publish the phrase.
    private func apply(_ words: [String], from start: Int) {
        var next = cells
        for (offset, word) in words.enumerated() {
            let target = start + offset
            guard target < PhraseWords.count else { break }
            next[target] = word
        }
        cells = next
        publish()
        let landing = min(start + words.count, PhraseWords.count - 1)
        focusedCell = next[landing].isEmpty ? landing : nil
    }

    private func publish() {
        let joined = cells
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
        if joined != phrase { phrase = joined }
    }
}

// MARK: - Guided phrase screen

/// The recovery-phrase step shared by sign-up and the fresh-identity
/// screen: the grid, Copy, a short explanation, an optional extra section,
/// then a confirmation step that asks for three words at random positions
/// before `onDone` fires. The words never leave the device; the check is
/// local. With `totalSteps` the two steps are numbered ("Step 1 of 3").
struct RecoveryPhraseGuide<Extra: View>: View {
    let phrase: String
    let title: String
    let intro: String
    var doneTitle: String? = nil
    /// The number of steps of the flow this guide opens (its own two come
    /// first), or `nil` for no step line.
    var totalSteps: Int? = nil
    /// The confirm button shows its spinner: `onDone` started something that
    /// is still running.
    var doneBusy = false
    /// Under the three words, above the confirm button: what the caller has
    /// to say about the step after `onDone` when it did not go through.
    var confirmFooter: AnyView? = nil
    @ViewBuilder let extra: Extra
    let onDone: () -> Void

    private enum Step { case show, confirm }

    @State private var step: Step = .show
    @State private var copied = false
    @State private var challenge = ThreeWordConfirmation.pick()
    @State private var entries: [String] = Array(repeating: "", count: ThreeWordConfirmation.wordCount)
    @State private var mismatch: String?
    @FocusState private var focusedEntry: Int?
    @ObservedObject private var l10n = Localization.shared

    private var words: [String] { PhraseWords.words(of: phrase) }

    var body: some View {
        AuthScreen {
            switch step {
            case .show: showStep
            case .confirm: confirmStep
            }
        }
    }

    @ViewBuilder private var showStep: some View {
        if let totalSteps {
            PhraseStepLabel(current: 1, total: totalSteps)
        }
        AuthTitle(title: title, subtitle: intro)

        PhraseGrid(words: words)

        AuthSecondaryButton(
            title: l10n.t(copied ? "common.copied" : "common.copy"),
            icon: copied ? .checkmark : .copy
        ) {
            UIPasteboard.general.string = phrase
            copied = true
            Task { @MainActor in
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                copied = false
            }
        }

        VStack(alignment: .leading, spacing: 8) {
            explainer(.eyeSlash, l10n.t("recovery_phrase.save.no_screenshot"))
        }
        .padding(16)
        .background(Color.lakkoSurface)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card)
                .stroke(Color.lakkoBorder, lineWidth: 1)
        )

        extra

        AuthPrimaryButton(title: l10n.t("recovery_phrase.save.written")) {
            challenge = ThreeWordConfirmation.pick()
            entries = Array(repeating: "", count: ThreeWordConfirmation.wordCount)
            mismatch = nil
            withAnimation { step = .confirm }
        }
    }

    @ViewBuilder private var confirmStep: some View {
        if let totalSteps {
            PhraseStepLabel(current: 2, total: totalSteps)
        }
        AuthTitle(
            title: l10n.t("recovery_phrase.check.title"),
            subtitle: l10n.t(
                "recovery_phrase.check.lede",
                texts: ["positions": l10n.positions(challenge.numbers)]))

        ForEach(Array(challenge.numbers.enumerated()), id: \.offset) { index, number in
            VStack(alignment: .leading, spacing: 6) {
                AuthFieldLabel(l10n.t("recovery_phrase.word_label", texts: ["position": String(number)]))
                TextField("", text: $entries[index])
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
                    .keyboardType(.asciiCapable)
                    .submitLabel(index == challenge.numbers.count - 1 ? .done : .next)
                    .onSubmit {
                        if index + 1 < challenge.numbers.count {
                            focusedEntry = index + 1
                        } else {
                            focusedEntry = nil
                            verify()
                        }
                    }
                    .focused($focusedEntry, equals: index)
                    .authFieldChrome(focused: focusedEntry == index)
                    .accessibilityLabel(l10n.t("recovery_phrase.word_label", texts: ["position": String(number)]))
            }
        }

        if let mismatch {
            AuthHint(text: mismatch, isError: true)
        }

        if let confirmFooter {
            confirmFooter
        }

        AuthPrimaryButton(
            title: doneTitle ?? l10n.t("common.continue"),
            busy: doneBusy,
            disabled: entries.contains(where: {
                $0.trimmingCharacters(in: .whitespaces).isEmpty
            })
        ) {
            verify()
        }

        AuthLinkRow(title: l10n.t("recovery_phrase.check.back")) {
            mismatch = nil
            withAnimation { step = .show }
        }
    }

    private func verify() {
        let wrong = ThreeWordConfirmation.mismatches(challenge: challenge, entries: entries, words: words)
        if wrong.isEmpty, !words.isEmpty {
            mismatch = nil
            onDone()
        } else {
            mismatch = wrong.count == 1
                ? l10n.t("recovery_phrase.check.mismatch_one", texts: ["position": String(wrong[0] + 1)])
                : l10n.t(
                    "recovery_phrase.check.mismatch_many",
                    texts: ["positions": l10n.positions(wrong.map { $0 + 1 })])
        }
    }

    private func explainer(_ icon: LkIcon, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            LkIconView(icon, size: 20)
                .foregroundStyle(Color.lakkoAccent)
                .frame(width: 22)
            Text(text)
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
        }
    }
}

/// "Step 2 of 3" above a step's title.
struct PhraseStepLabel: View {
    let current: Int
    let total: Int

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        Text(l10n.t(
            "recovery_phrase.step",
            texts: ["current": String(current), "total": String(total)]))
            .font(.footnote.weight(.medium))
            .foregroundStyle(Color.lakkoTextMuted)
    }
}

// MARK: - Password explainer

/// The one place the login screen explains the password handling, in plain
/// words (design system section 5: no protocol names).
struct PasswordExplainerSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        NavigationStack {
            AuthScreen {
                point(.smartphoneArrow, l10n.t("auth.explainer.never_leaves"))
                point(.personQuestion, l10n.t("auth.explainer.same_answer"))
                point(.lifepreserver, l10n.t("auth.explainer.ways_back"))
            }
            .navigationTitle(l10n.t("auth.explainer.title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button(l10n.t("common.done")) { dismiss() }
                }
            }
        }
    }

    private func point(_ icon: LkIcon, _ body: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            LkIconView(icon, size: 24, relativeTo: .title3)
                .foregroundStyle(Color.lakkoAccent)
                .frame(width: 28)
            Text(body)
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.vertical, 4)
    }
}
