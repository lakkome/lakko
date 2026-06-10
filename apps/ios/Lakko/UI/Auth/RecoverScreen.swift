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

            if viewModel.mode != .anotherDevice {
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
            }

            // 1.21c2 — mode toggle: phrase | shares. Picker renders
            // as a segmented control which matches the iOS native
            // pattern for a small-mode form switch. ADR-0018 adds the
            // "Device" segment: restore via an already-linked device
            // (no password, no phrase typing).
            Picker("Recovery mode", selection: $viewModel.mode) {
                Text("Phrase").tag(RecoverViewModel.Mode.phrase)
                Text("Shares").tag(RecoverViewModel.Mode.shares)
                Text("Device").tag(RecoverViewModel.Mode.anotherDevice)
                Text("Code").tag(RecoverViewModel.Mode.totp)
                Text("Email/SMS").tag(RecoverViewModel.Mode.contact)
            }
            .pickerStyle(.segmented)
            .disabled(viewModel.busy)

            if viewModel.mode == .anotherDevice {
                anotherDeviceSection
            } else if viewModel.mode == .totp {
                totpSection
            } else if viewModel.mode == .contact {
                contactSection
            } else {
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
            }

            if let error = viewModel.error {
                Text(error)
                    .font(.body)
                    .foregroundStyle(.red)
            }

            if viewModel.mode != .anotherDevice
                && viewModel.mode != .totp
                && viewModel.mode != .contact {
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
            }

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

    /// ADR-0018 factor (c) — recover ACCESS with a 6-digit
    /// authenticator code. Identity then restores via the post-ACCESS
    /// state machine (cached seed -> straight to the session;
    /// otherwise the restore-identity prompt). Mirror of Android's
    /// `RecoverScreen` Authenticator branch.
    @ViewBuilder
    private var totpSection: some View {
        Text("6-digit code")
            .font(.headline)
        TextField("123456", text: $viewModel.totpCode)
            .textFieldStyle(.roundedBorder)
            .keyboardType(.numberPad)
            .textContentType(.oneTimeCode)
            .disabled(viewModel.busy)
        Button(action: viewModel.submitTotp) {
            HStack {
                if viewModel.busy {
                    ProgressView()
                        .controlSize(.small)
                        .progressViewStyle(.circular)
                        .tint(.white)
                    Text("Verifying…")
                } else {
                    Text("Recover with code")
                }
            }
            .frame(maxWidth: .infinity)
        }
        .buttonStyle(.borderedProminent)
        .controlSize(.large)
        .disabled(viewModel.busy)
    }

    /// ADR-0018 factor (d) — recover ACCESS with a one-time code sent
    /// to a previously-enrolled phone / email (under Devices). Identity
    /// then restores via the post-ACCESS state machine (cached seed ->
    /// straight to the session; otherwise the restore-identity prompt).
    /// Two steps: request (kind picker + contact + Send code), then
    /// code (6-digit field + submit). Mirror of the web
    /// `ContactRecoverSection`.
    @ViewBuilder
    private var contactSection: some View {
        Text(
            "Recover access with a one-time code sent to a phone or email " +
            "you enrolled earlier (under Devices). This restores access to " +
            "your account; your secure identity is then restored from your " +
            "recovery phrase or another signed-in device."
        )
        .font(.footnote)
        .foregroundStyle(.secondary)

        switch viewModel.contactStep {
        case .request:
            Picker("Contact type", selection: $viewModel.contactKind) {
                Text("Email").tag(ContactKind.email)
                Text("SMS").tag(ContactKind.phone)
            }
            .pickerStyle(.segmented)
            .disabled(viewModel.busy)

            Text(viewModel.contactKind == .email ? "Email" : "Phone number")
                .font(.headline)
            TextField(
                viewModel.contactKind == .email
                    ? "alice@example.com"
                    : "+15551234567",
                text: $viewModel.contact
            )
            .textFieldStyle(.roundedBorder)
            .keyboardType(viewModel.contactKind == .email ? .emailAddress : .phonePad)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled(true)
            .disabled(viewModel.busy)

            Button(action: viewModel.sendContactCode) {
                HStack {
                    if viewModel.busy {
                        ProgressView()
                            .controlSize(.small)
                            .progressViewStyle(.circular)
                            .tint(.white)
                        Text("Sending…")
                    } else {
                        Text("Send code")
                    }
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(viewModel.busy)

        case .code:
            Text("6-digit code")
                .font(.headline)
            TextField("123456", text: $viewModel.contactCode)
                .textFieldStyle(.roundedBorder)
                .keyboardType(.numberPad)
                .textContentType(.oneTimeCode)
                .disabled(viewModel.busy)
            Button(action: viewModel.submitContactCode) {
                HStack {
                    if viewModel.busy {
                        ProgressView()
                            .controlSize(.small)
                            .progressViewStyle(.circular)
                            .tint(.white)
                        Text("Verifying…")
                    } else {
                        Text("Recover access")
                    }
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(viewModel.busy)
        }
    }

    /// ADR-0018 factor (a), path (b) — no-password restore via an
    /// already-linked device. Resolve the username first (the pairing
    /// QR embeds the account id), then run the QR join with the
    /// recovery payload opted in. Mirror of Android's `RecoverScreen`
    /// AnotherDevice branch.
    @ViewBuilder
    private var anotherDeviceSection: some View {
        if let target = viewModel.deviceRestoreTarget {
            Text("Restoring @\(target.username)")
                .font(.body)
            RestoreViaDevicePanel(
                accountIdBytes: target.accountIdBytes,
                accountIdB64: target.accountIdB64,
                onRecovered: { outcome in
                    try await viewModel.completeFromDeviceRestore(
                        outcome: outcome
                    )
                }
            )
        } else {
            Text("Username")
                .font(.headline)
            TextField("alice", text: $viewModel.username)
                .textFieldStyle(.roundedBorder)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled(true)
                .disabled(viewModel.busy)
            Button(action: viewModel.lookupForDeviceRestore) {
                HStack {
                    if viewModel.busy {
                        ProgressView()
                            .controlSize(.small)
                            .progressViewStyle(.circular)
                            .tint(.white)
                        Text("Looking up…")
                    } else {
                        Text("Find account")
                    }
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(viewModel.busy)
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
