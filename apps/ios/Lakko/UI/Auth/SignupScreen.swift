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
                // ADR-0048 §4 — the reason, as the user types.
                if let hint = viewModel.usernameHint {
                    Text(hint)
                        .font(.footnote)
                        .foregroundStyle(.red)
                } else {
                    Text("Latin letters, digits and _, 3 to 32 characters.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }

                // ADR-0048 — optional; others see the username without it.
                TextField(
                    "Your name",
                    text: $viewModel.displayName,
                    prompt: Text(viewModel.username.isEmpty ? "Your name (optional)" : viewModel.username)
                )
                .textFieldStyle(.roundedBorder)
                .textInputAutocapitalization(.words)
                .disabled(viewModel.busy)
                if let nameError = viewModel.displayNameError {
                    Text(nameError)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }

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

                // 1.21c2 — optional Shamir 3-of-5 social recovery
                // panel. Collapsed via a `DisclosureGroup` by
                // default so the canonical "write 24 words on
                // paper" flow stays the prominent path.
                SocialRecoveryPanel(phrase: phrase)

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

// ============================================================================
// 1.21c2 — optional Shamir 3-of-5 social recovery panel
// ============================================================================

/// Collapsed-by-default `DisclosureGroup` that lets the user
/// split the just-generated 24-word mnemonic into 5 shares (any
/// 3 reconstruct) for distribution to guardians. Sits between
/// the wordsCard + the "Done" gate; complement to the paper-
/// phrase backup, not a replacement.
private struct SocialRecoveryPanel: View {
    let phrase: String

    @State private var expanded: Bool = false
    @State private var shares: [String] = []
    @State private var busy: Bool = false
    @State private var error: String?
    @State private var copiedIndex: Int?

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            content
                .padding(.top, 8)
        } label: {
            HStack(spacing: 8) {
                Image(systemName: "person.2.fill")
                    .foregroundStyle(.secondary)
                Text("Optional: split into 3-of-5 social-recovery shares")
                    .font(.body)
            }
        }
        .padding(12)
        .background(Color(.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    @ViewBuilder
    private var content: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(
                "Generate 5 share strings; any 3 reconstruct " +
                "your recovery phrase. Distribute one to each " +
                "trusted guardian (family, friends, a safe " +
                "deposit box). No single guardian alone — or any " +
                "two colluding — can recover your account; you " +
                "can lose contact with two guardians and still " +
                "recover."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)

            if shares.isEmpty {
                Button {
                    onGenerate()
                } label: {
                    HStack {
                        if busy {
                            ProgressView().controlSize(.small)
                            Text("Generating…")
                        } else {
                            Text("Generate 5 shares")
                        }
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.regular)
                .disabled(busy)
            } else {
                ForEach(Array(shares.enumerated()), id: \.offset) { i, share in
                    ShareRow(
                        index: i + 1,
                        share: share,
                        copied: copiedIndex == i,
                        onCopy: { copyShare(at: i, value: share) }
                    )
                }
                Text(
                    "Distribute one share to each of 5 trusted " +
                    "guardians. Recover via Recover → Recovery " +
                    "shares with any 3."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }

            if let error = error {
                Text(error)
                    .font(.footnote)
                    .foregroundStyle(.red)
            }
        }
    }

    private func onGenerate() {
        busy = true
        error = nil
        // Synchronous (the underlying UniFFI call is in-process
        // arithmetic, sub-millisecond). Wrap in a `Task` anyway
        // so the UI stays responsive on slow devices + future
        // proofs against an async-ification of the underlying
        // surface.
        Task { @MainActor in
            defer { busy = false }
            do {
                shares = try ShamirBridge.splitMnemonic(
                    phrase: phrase,
                    threshold: ShamirBridge.defaultThreshold,
                    shares: ShamirBridge.defaultShareCount
                )
            } catch {
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Couldn't generate shares."
            }
        }
    }

    private func copyShare(at index: Int, value: String) {
        UIPasteboard.general.string = value
        copiedIndex = index
        // Reset the "copied" flag after a short delay so a
        // subsequent copy on the same chip surfaces a fresh
        // confirmation.
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            if copiedIndex == index {
                copiedIndex = nil
            }
        }
    }
}

private struct ShareRow: View {
    let index: Int
    let share: String
    let copied: Bool
    let onCopy: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Text("#\(index)")
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(.secondary)
                .frame(width: 32, alignment: .leading)
            Text(share)
                .font(.system(.caption2, design: .monospaced))
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button(action: onCopy) {
                Text(copied ? "Copied" : "Copy")
                    .font(.caption)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(Color(.tertiarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 6))
    }
}
