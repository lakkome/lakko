// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// SwiftUI signup screen.
///
/// Mirror of:
///
///   * `apps/web/src/routes/Signup.tsx` (1.17d6)
///   * `apps/android/.../ui/auth/SignupScreen.kt` (1.15d4)
///
/// Two stages, switched on ``SignupViewModel/stage``:
///
///   1. ``Stage/form``      — username + password + confirm-password.
///   2. ``Stage/phrase(_:)`` — 24-word BIP39 recovery phrase + the
///                            "I have written it down" gate.
///
/// The screen is purely a presentation layer over
/// ``SignupViewModel``. All state lives in the VM; the view only
/// renders + forwards user events. This split:
///
///   * keeps the screen testable via XCUITest (1.14d5+ instrumented
///     suite) without spinning up a URLProtocol mock against the
///     view itself;
///   * lets the screen survive scene transitions (size-class
///     changes, dark-mode toggle) without re-running OPAQUE math —
///     SwiftUI recreates the view tree, the VM persists via the
///     `@StateObject` owner.
///
/// `onAuthenticated` is invoked exactly once after the user
/// acknowledges the recovery phrase. The app-level router uses
/// that callback to swap to the home destination.
public struct SignupScreen: View {

    @ObservedObject public var viewModel: SignupViewModel
    public var onAuthenticated: () -> Void
    public var onNavigateToLogin: () -> Void

    public init(
        viewModel: SignupViewModel,
        onAuthenticated: @escaping () -> Void,
        onNavigateToLogin: @escaping () -> Void = {}
    ) {
        self.viewModel = viewModel
        self.onAuthenticated = onAuthenticated
        self.onNavigateToLogin = onNavigateToLogin
    }

    public var body: some View {
        switch viewModel.stage {
        case .form:
            SignupForm(
                viewModel: viewModel,
                onNavigateToLogin: onNavigateToLogin
            )
        case .phrase(let phrase):
            RecoveryPhrasePanel(
                phrase: phrase,
                acknowledged: $viewModel.acknowledged,
                onAcknowledge: {
                    viewModel.onAcknowledge()
                    onAuthenticated()
                }
            )
        }
    }
}

// =============================================================================
// Stage 1: signup form
// =============================================================================

private struct SignupForm: View {
    @ObservedObject var viewModel: SignupViewModel
    let onNavigateToLogin: () -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text("Create a Lakko account")
                    .font(.largeTitle)
                    .fontWeight(.semibold)

                Text(
                    "OPAQUE-PAKE registration. The password never leaves your " +
                    "device — the server only sees an OPRF-blinded envelope. " +
                    "After signup you will be shown a 24-word recovery phrase " +
                    "once; write it down before continuing."
                )
                .font(.body)
                .foregroundStyle(.secondary)

                TextField("Username", text: $viewModel.username)
                    .textFieldStyle(.roundedBorder)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
                    .disabled(viewModel.busy)

                SecureField("Password", text: $viewModel.password)
                    .textFieldStyle(.roundedBorder)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
                    .disabled(viewModel.busy)

                SecureField("Confirm password", text: $viewModel.confirm)
                    .textFieldStyle(.roundedBorder)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
                    .disabled(viewModel.busy)

                if let error = viewModel.error {
                    Text(error)
                        .font(.body)
                        .foregroundStyle(.red)
                }

                Button(action: viewModel.onSubmit) {
                    HStack {
                        if viewModel.busy {
                            ProgressView()
                                .controlSize(.small)
                                .progressViewStyle(.circular)
                                .tint(.white)
                            Text("Creating account…")
                        } else {
                            Text("Sign up")
                        }
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(viewModel.busy)

                HStack(spacing: 4) {
                    Spacer()
                    Text("Already have an account?")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    Button(action: onNavigateToLogin) {
                        Text("Log in")
                            .font(.footnote)
                    }
                    Spacer()
                }
            }
            .padding(24)
        }
    }
}

// =============================================================================
// Stage 2: recovery phrase display
// =============================================================================

private struct RecoveryPhrasePanel: View {
    let phrase: String
    @Binding var acknowledged: Bool
    let onAcknowledge: () -> Void

    /// Split the space-joined mnemonic into the 24 words. The Rust
    /// `RecoveryMnemonic.phrase()` always returns 24 space-separated
    /// words, but defensive splitting on whitespace + filtering
    /// empties keeps the layout robust if the upstream format ever
    /// adds a trailing newline.
    private var words: [String] {
        phrase
            .split(whereSeparator: { $0.isWhitespace })
            .map(String.init)
    }

    /// Words per column. The BIP39 phrase is 24 words; rendering as
    /// two columns of 12 mirrors the Compose + web layouts.
    private static let wordsPerColumn = 12

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text("Save your recovery phrase")
                    .font(.largeTitle)
                    .fontWeight(.semibold)

                Text(
                    "These 24 words are the ONLY way to recover your account " +
                    "if you forget your password. Write them down on paper, " +
                    "in order. Do not screenshot. Do not store in plain " +
                    "text. We cannot show them to you again."
                )
                .font(.body)
                .foregroundStyle(.secondary)

                wordsCard

                Toggle(isOn: $acknowledged) {
                    Text(
                        "I have written down my 24-word recovery phrase. I " +
                        "understand that losing it means losing access to " +
                        "this account."
                    )
                    .font(.body)
                }
                .toggleStyle(.automatic)

                Button(action: onAcknowledge) {
                    Text("Done — take me to Lakko")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(!acknowledged)
            }
            .padding(24)
        }
    }

    /// Two-column grid laid out manually — SwiftUI's ``Grid`` (iOS
    /// 16+) would also work, but a pair of `VStack`s inside an
    /// `HStack` keeps the per-row index-mono layout legible.
    private var wordsCard: some View {
        let leftColumn = Array(words.prefix(Self.wordsPerColumn))
        let rightColumn = Array(words.dropFirst(Self.wordsPerColumn))
        return HStack(alignment: .top, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(leftColumn.enumerated()), id: \.offset) { i, word in
                    WordRow(index: i + 1, word: word)
                }
            }
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(rightColumn.enumerated()), id: \.offset) { i, word in
                    WordRow(index: i + 1 + Self.wordsPerColumn, word: word)
                }
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }
}

private struct WordRow: View {
    let index: Int
    let word: String

    var body: some View {
        HStack(spacing: 8) {
            Text(String(format: "%2d.", index))
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(.secondary)
                .frame(width: 28, alignment: .leading)
            Text(word)
                .font(.system(.body, design: .monospaced))
        }
    }
}
