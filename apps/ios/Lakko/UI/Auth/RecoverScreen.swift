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

    /// UX spec section 5.6: the methods are a list of rows, each pushing its
    /// form. `nil` shows the list; a method shows that method's form.
    @State private var chosen: RecoverViewModel.Mode?

    public var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text("Recover account")
                        .font(.largeTitle)
                        .fontWeight(.semibold)
                    Text("Choose how to prove this account is yours.")
                        .font(.body)
                        .foregroundStyle(.secondary)

                    VStack(spacing: 0) {
                        ForEach(RecoverViewModel.Mode.allCases, id: \.self) { mode in
                            Button {
                                viewModel.mode = mode
                                chosen = mode
                            } label: {
                                HStack(spacing: 12) {
                                    Image(systemName: mode.systemImage)
                                        .frame(width: 28)
                                        .foregroundStyle(Color.lakkoAccent)
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(mode.title)
                                            .foregroundStyle(Color.lakkoText)
                                        Text(mode.detail)
                                            .font(.footnote)
                                            .foregroundStyle(Color.lakkoTextMuted)
                                    }
                                    Spacer()
                                    Image(systemName: "chevron.right")
                                        .font(.footnote.weight(.semibold))
                                        .foregroundStyle(Color.lakkoTextMuted)
                                }
                                .padding(.vertical, 12)
                                .padding(.horizontal, 16)
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .disabled(viewModel.busy)
                            if mode != RecoverViewModel.Mode.allCases.last {
                                Divider().padding(.leading, 56)
                            }
                        }
                    }
                    .background(Color.lakkoSurface)
                    .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))

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
                .padding(24)
            }
            .navigationDestination(item: $chosen) { mode in
                ScrollView {
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
                    .padding(24)
                }
                .navigationTitle(mode.title)
                .navigationBarTitleDisplayMode(.inline)
            }
        }
    }
}

extension RecoverViewModel.Mode {
    /// Row order on the method list (UX spec section 5.6).
    static let allCases: [RecoverViewModel.Mode] = [.phrase, .anotherDevice, .shares, .totp, .contact]

    var title: String {
        switch self {
        case .phrase: return "Recovery phrase"
        case .anotherDevice: return "Another device"
        case .shares: return "Recovery shares"
        case .totp: return "Authenticator app"
        case .contact: return "Email or phone"
        }
    }

    var detail: String {
        switch self {
        case .phrase: return "Your 24 words. Restores everything."
        case .anotherDevice: return "Approve from a device that is still signed in."
        case .shares: return "Three of the five shares you gave to trusted people."
        case .totp: return "A code from your authenticator app. Gets you back in."
        case .contact: return "A one-time code sent to you. Gets you back in."
        }
    }

    var systemImage: String {
        switch self {
        case .phrase: return "key"
        case .anotherDevice: return "iphone.gen3.radiowaves.left.and.right"
        case .shares: return "person.2"
        case .totp: return "lock.rotation"
        case .contact: return "envelope"
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
                    UsernameRuleHint(input: viewModel.username)
                }
            }

            // The method was chosen on the list one level up; `viewModel.mode`
            // already carries it.
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
            "A one-time code goes to the email or phone number you set up "
                + "under Settings > Recovery. It gets you back in; your messages "
                + "and contacts then come back from your recovery phrase or another device."
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
