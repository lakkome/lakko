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
                .focused($focused)
                .disabled(disabled)
                .authFieldChrome(focused: focused)
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
                    Image(systemName: revealed ? "eye.slash" : "eye")
                        .foregroundStyle(Color.lakkoTextMuted)
                        .frame(width: 28, height: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(revealed ? "Hide password" : "Show password")
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

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            if isError {
                Image(systemName: "exclamationmark.circle")
                    .font(.footnote)
            }
            Text(text)
                .font(.footnote)
        }
        .foregroundStyle(isError ? Color.lakkoDanger : Color.lakkoTextMuted)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityLabel(isError ? "Error: \(text)" : text)
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
    var systemImage: String? = nil
    var disabled: Bool = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                if let systemImage {
                    Image(systemName: systemImage)
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
    var disabled: Bool = false
    let action: () -> Void

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
            Spacer(minLength: 0)
        }
        .frame(minHeight: 32)
    }
}

// MARK: - Phrase display grid

/// The 24 numbered words in two columns on the sunken surface. One
/// component for sign-up, the fresh-phrase screen and any later "show my
/// phrase" surface, so the phrase always looks the same.
struct PhraseGrid: View {
    let words: [String]

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            ForEach(Array(PhraseWords.columns(words).enumerated()), id: \.offset) { _, column in
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(column, id: \.number) { cell in
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Text("\(cell.number)")
                                .font(.footnote.monospacedDigit())
                                .foregroundStyle(Color.lakkoTextMuted)
                                .frame(width: 22, alignment: .trailing)
                            Text(cell.word)
                                .font(.body.weight(.medium))
                                .foregroundStyle(Color.lakkoText)
                                .lineLimit(1)
                                .minimumScaleFactor(0.8)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding(16)
        .background(Color.lakkoSurfaceSunken)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Recovery phrase: " + words.enumerated()
            .map { "\($0.offset + 1) \($0.element)" }.joined(separator: ", "))
    }
}

// MARK: - Phrase entry grid

/// 24 numbered fields in two columns with a Paste button. The phrase is
/// exposed as one space-joined string through `phrase`; a multi-word paste
/// into any cell spreads across the cells from that position, and Return
/// moves to the next word. Autocorrect is off: it would rewrite valid
/// wordlist entries into neighbouring dictionary words.
struct PhraseEntryGrid: View {
    @Binding var phrase: String
    var disabled: Bool = false

    @State private var cells: [String] = Array(repeating: "", count: PhraseWords.count)
    @FocusState private var focusedCell: Int?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                AuthFieldLabel("Recovery phrase")
                Spacer()
                Button {
                    if let pasted = UIPasteboard.general.string {
                        apply(PhraseWords.parsePasted(pasted), from: 0)
                    }
                } label: {
                    Label("Paste", systemImage: "doc.on.clipboard")
                        .font(.footnote.weight(.medium))
                        .foregroundStyle(Color.lakkoAccent)
                }
                .buttonStyle(.plain)
                .disabled(disabled)
            }

            HStack(alignment: .top, spacing: 12) {
                ForEach(Array(columnRanges.enumerated()), id: \.offset) { _, range in
                    VStack(spacing: 8) {
                        ForEach(range, id: \.self) { index in
                            cell(index)
                        }
                    }
                    .frame(maxWidth: .infinity)
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

    private var columnRanges: [Range<Int>] {
        let half = PhraseWords.count / 2
        return [0..<half, half..<PhraseWords.count]
    }

    private func cell(_ index: Int) -> some View {
        HStack(spacing: 6) {
            Text("\(index + 1)")
                .font(.footnote.monospacedDigit())
                .foregroundStyle(Color.lakkoTextMuted)
                .frame(width: 22, alignment: .trailing)
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
            .padding(.horizontal, 8)
            .frame(minHeight: 36)
            .background(Color.lakkoSurface)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
            .overlay(
                RoundedRectangle(cornerRadius: LakkoRadius.control)
                    .stroke(
                        focusedCell == index ? Color.lakkoAccent.opacity(0.6) : Color.lakkoBorder,
                        lineWidth: focusedCell == index ? 2 : 1)
            )
            .accessibilityLabel("Word \(index + 1)")
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
/// screen (UX spec section 5.6): the grid, Copy, a short explanation, an
/// optional extra section (social recovery), then a confirmation step that
/// asks for three words at random positions before `onDone` fires. The
/// words never leave the device; the check is local.
struct RecoveryPhraseGuide<Extra: View>: View {
    let phrase: String
    let title: String
    let intro: String
    var doneTitle: String = "Continue"
    @ViewBuilder let extra: Extra
    let onDone: () -> Void

    private enum Step { case show, confirm }

    @State private var step: Step = .show
    @State private var copied = false
    @State private var challenge = ThreeWordConfirmation.pick()
    @State private var entries: [String] = Array(repeating: "", count: ThreeWordConfirmation.wordCount)
    @State private var mismatch: String?
    @FocusState private var focusedEntry: Int?

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
        AuthTitle(title: title, subtitle: intro)

        PhraseGrid(words: words)

        AuthSecondaryButton(
            title: copied ? "Copied" : "Copy",
            systemImage: copied ? "checkmark" : "doc.on.doc"
        ) {
            UIPasteboard.general.string = phrase
            copied = true
            Task { @MainActor in
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                copied = false
            }
        }

        VStack(alignment: .leading, spacing: 8) {
            explainer("lock.shield", "These 24 words are the only way back into your account if you lose this device or forget your password.")
            explainer("pencil.and.list.clipboard", "Write them down in order and keep them somewhere safe. A screenshot is easy to lose or steal.")
            explainer("eye.slash", "Lakko cannot show them again and never has a copy.")
        }
        .padding(16)
        .background(Color.lakkoSurface)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card)
                .stroke(Color.lakkoBorder, lineWidth: 1)
        )

        extra

        AuthPrimaryButton(title: "I have written it down") {
            challenge = ThreeWordConfirmation.pick()
            entries = Array(repeating: "", count: ThreeWordConfirmation.wordCount)
            mismatch = nil
            withAnimation { step = .confirm }
        }
    }

    @ViewBuilder private var confirmStep: some View {
        AuthTitle(
            title: "Check three words",
            subtitle: "Type the words at these positions to make sure you saved the phrase correctly.")

        ForEach(Array(challenge.numbers.enumerated()), id: \.offset) { index, number in
            VStack(alignment: .leading, spacing: 6) {
                AuthFieldLabel("Word \(number)")
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
                    .accessibilityLabel("Word \(number)")
            }
        }

        if let mismatch {
            AuthHint(text: mismatch, isError: true)
        }

        AuthPrimaryButton(title: doneTitle, disabled: entries.contains(where: {
            $0.trimmingCharacters(in: .whitespaces).isEmpty
        })) {
            verify()
        }

        AuthLinkRow(title: "Show the phrase again") {
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
            let positions = wrong.map { "\($0 + 1)" }.joined(separator: ", ")
            mismatch = wrong.count == 1
                ? "Word \(positions) does not match. Check your notes and try again."
                : "Words \(positions) do not match. Check your notes and try again."
        }
    }

    private func explainer(_ symbol: String, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: symbol)
                .font(.body)
                .foregroundStyle(Color.lakkoAccent)
                .frame(width: 22)
            Text(text)
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
        }
    }
}

// MARK: - Password explainer

/// The one place the login screen explains the password handling, in plain
/// words (design system section 5: no protocol names).
struct PasswordExplainerSheet: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            AuthScreen {
                point("iphone.and.arrow.forward", "Your password never leaves your device.",
                      "Lakko checks it with a method that lets the server confirm you know the password without ever receiving it.")
                point("server.rack", "Nothing on the server can be used to guess it.",
                      "A stolen copy of the server's data does not contain your password or anything that reveals it.")
                point("key.horizontal", "Your messages are protected separately.",
                      "Changing your password does not change the keys your contacts trust, and losing it does not expose your messages.")
                point("lifepreserver", "Your recovery phrase is the way back.",
                      "If you forget your password, the 24-word phrase you saved at sign-up restores your account.")
            }
            .navigationTitle("How Lakko protects your password")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    private func point(_ symbol: String, _ title: String, _ body: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: symbol)
                .font(.title3)
                .foregroundStyle(Color.lakkoAccent)
                .frame(width: 28)
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.body.weight(.medium))
                    .foregroundStyle(Color.lakkoText)
                Text(body)
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .padding(.vertical, 4)
    }
}
