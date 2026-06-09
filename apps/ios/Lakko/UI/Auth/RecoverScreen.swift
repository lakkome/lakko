// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// SwiftUI recover-from-mnemonic screen.
///
/// Mirror of:
///
///   * `apps/web/src/routes/Recover.tsx` (1.17d8)
///   * `apps/android/.../ui/auth/RecoverScreen.kt` (1.15d6)
///
/// Two render paths:
///
///   * **No cached account** (``RecoverViewModel/cachedUsername``
///     is `nil`) — the recovery flow can't run without an
///     `accountId` on disk. We show a "no cached account" stub
///     with deep links to Signup / Login instead of a form the
///     user would fill and fail at submit. Cross-device recovery
///     (mnemonic-only) is a 1.14e / discovery-service problem.
///   * **Cached account present** — single-stage form: 24-word
///     phrase + new password + confirm. On submit drives
///     ``AuthRepository/recover(mnemonicPhrase:newPassword:)``.
///
/// The mnemonic textfield is a multi-line ``TextEditor`` with a
/// monospaced font + autocorrect / autocapitalisation disabled.
/// Autocorrect on a wordlist phrase would helpfully rewrite valid
/// BIP39 words into adjacent dictionary entries; we have to turn
/// it off. SwiftUI's ``TextEditor`` does not ship a placeholder
/// API — we overlay one manually with a `ZStack` when the buffer
/// is empty.
public struct RecoverScreen: View {

    @ObservedObject public var viewModel: RecoverViewModel
    public var onNavigateToLogin: () -> Void
    public var onNavigateToSignup: () -> Void

    public init(
        viewModel: RecoverViewModel,
        onNavigateToLogin: @escaping () -> Void,
        onNavigateToSignup: @escaping () -> Void
    ) {
        self.viewModel = viewModel
        self.onNavigateToLogin = onNavigateToLogin
        self.onNavigateToSignup = onNavigateToSignup
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text("Recover account")
                    .font(.largeTitle)
                    .fontWeight(.semibold)

                // ADR-0017: recover now works on a FRESH device — when
                // no account is cached we resolve the account_id via
                // `DiscoveryClient.lookup(username)`, so the form
                // renders a username field instead of the old
                // "no cached account" dead-end stub.
                RecoverForm(
                    viewModel: viewModel,
                    cachedUsername: viewModel.cachedUsername,
                    onNavigateToLogin: onNavigateToLogin,
                    onNavigateToSignup: onNavigateToSignup
                )
            }
            .padding(24)
        }
    }
}

// =============================================================================
// Form
// =============================================================================

private struct RecoverForm: View {
    @ObservedObject var viewModel: RecoverViewModel
    /// `nil` on a fresh device — the form then renders a username
    /// field so the recover flow can resolve the account via
    /// discovery (ADR-0017).
    let cachedUsername: String?
    let onNavigateToLogin: () -> Void
    let onNavigateToSignup: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(
                "Paste your 24-word recovery phrase and pick a new password. " +
                "We'll prove ownership to the server and rotate the password " +
                "envelope."
            )
            .font(.body)
            .foregroundStyle(.secondary)

            if let cachedUsername {
                Text("Account on this device: @\(cachedUsername)")
                    .font(.body)
            } else {
                // ADR-0017 fresh-device recover — no cached account, so
                // ask for the username to look it up via discovery.
                Text("Username")
                    .font(.headline)
                TextField("alice", text: $viewModel.username)
                    .textFieldStyle(.roundedBorder)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
                    .disabled(viewModel.busy)
            }

            // 1.21c2 — mode toggle: phrase | shares. Picker
            // renders as a segmented control which matches the
            // iOS native pattern for a two-mode form switch.
            Picker("Recovery mode", selection: $viewModel.mode) {
                Text("24-word phrase").tag(RecoverViewModel.Mode.phrase)
                Text("Recovery shares").tag(RecoverViewModel.Mode.shares)
            }
            .pickerStyle(.segmented)
            .disabled(viewModel.busy)

            if viewModel.mode == .phrase {
                phraseEditor
            } else {
                sharesEditor
            }

            SecureField("New password", text: $viewModel.password)
                .textFieldStyle(.roundedBorder)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled(true)
                .disabled(viewModel.busy)

            SecureField("Confirm new password", text: $viewModel.confirm)
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
                        Text("Verifying…")
                    } else {
                        Text("Recover account")
                    }
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(viewModel.busy)

            HStack(spacing: 4) {
                Spacer()
                Text("Remembered your password?")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                Button(action: onNavigateToLogin) {
                    Text("Log in")
                        .font(.footnote)
                }
                Spacer()
            }

            HStack(spacing: 4) {
                Spacer()
                Text("No account yet?")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                Button(action: onNavigateToSignup) {
                    Text("Create one")
                        .font(.footnote)
                }
                Spacer()
            }
        }
    }

    /// 1.21c2 — multi-line shares editor. Same `ZStack`-
    /// placeholder shape as `phraseEditor`; placeholder explains
    /// the one-share-per-line convention.
    private var sharesEditor: some View {
        ZStack(alignment: .topLeading) {
            if viewModel.sharesInput.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    Text("3:abcdEFGH...")
                    Text("1:zzzzABCD...")
                    Text("5:mmmm0123...")
                }
                .font(.system(.body, design: .monospaced))
                .foregroundStyle(.tertiary)
                .padding(.horizontal, 8)
                .padding(.vertical, 8)
                .allowsHitTesting(false)
            }
            TextEditor(text: $viewModel.sharesInput)
                .font(.system(.body, design: .monospaced))
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled(true)
                .scrollContentBackground(.hidden)
                .disabled(viewModel.busy)
                .frame(minHeight: 140)
        }
        .background(Color(.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Color(.separator), lineWidth: 1)
        )
        .overlay(alignment: .bottom) {
            Text(
                "Paste at least 3 shares (Lakko default is 3-of-5). " +
                "Each line is one share of the form <index>:<base64>."
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 8)
            .padding(.bottom, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(.secondarySystemBackground).opacity(0.0))
        }
    }

    /// Multi-line phrase editor with a manual placeholder overlay.
    ///
    /// SwiftUI's ``TextEditor`` does not surface a placeholder API.
    /// The standard pattern is to overlay a faded ``Text`` aligned
    /// to the top-leading corner that fades out the moment the
    /// buffer becomes non-empty. The monospaced font matches the
    /// Compose `FontFamily.Monospace` posture — visual cue that
    /// these are wordlist tokens, not free-form prose.
    private var phraseEditor: some View {
        ZStack(alignment: .topLeading) {
            if viewModel.phrase.isEmpty {
                Text("abandon abandon abandon … art")
                    .font(.system(.body, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 8)
                    .allowsHitTesting(false)
            }
            TextEditor(text: $viewModel.phrase)
                .font(.system(.body, design: .monospaced))
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled(true)
                .scrollContentBackground(.hidden)
                .disabled(viewModel.busy)
                .frame(minHeight: 120)
        }
        .background(Color(.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Color(.separator), lineWidth: 1)
        )
    }
}

// ADR-0017: the old `NoCachedAccountPanel` dead-end stub was removed.
// Recover now works on a fresh device — `RecoverForm` renders a
// username field and the flow resolves the account_id via
// `DiscoveryClient.lookup(username)`.
