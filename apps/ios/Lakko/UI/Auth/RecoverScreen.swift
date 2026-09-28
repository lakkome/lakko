// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The recover screen (UX spec section 5.6): a list of methods as rows,
/// each pushing its form on the anonymous stack. Pushed from the log-in
/// screen ("Forgot password?"); the system back returns there.
///
/// Every method's logic is the existing ``RecoverViewModel``; the screen
/// only presents it. Three-word recovery is not on the list because the
/// iOS client has no such flow yet (the vault exists on web and the
/// server only).
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

    /// `nil` shows the list; a method pushes that method's form.
    @State private var chosen: RecoverViewModel.Mode?

    public var body: some View {
        AuthScreen {
            AuthTitle(
                title: "Recover your account",
                subtitle: "Choose how to prove the account is yours.")

            VStack(spacing: 0) {
                ForEach(RecoverViewModel.Mode.allCases, id: \.self) { mode in
                    Button {
                        viewModel.mode = mode
                        chosen = mode
                    } label: {
                        HStack(spacing: 12) {
                            Image(systemName: mode.systemImage)
                                .font(.body)
                                .frame(width: 28)
                                .foregroundStyle(Color.lakkoAccent)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(mode.title)
                                    .font(.body.weight(.medium))
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
            .overlay(
                RoundedRectangle(cornerRadius: LakkoRadius.card)
                    .stroke(Color.lakkoBorder, lineWidth: 1)
            )

            AuthLinkRow(lead: "Remembered your password?", title: "Log in", action: onNavigateToLogin)
            AuthLinkRow(lead: "No account yet?", title: "Create account", action: onNavigateToSignup)
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        .navigationDestination(item: $chosen) { mode in
            AuthScreen {
                AuthTitle(title: mode.title, subtitle: mode.formLead)
                RecoverForm(
                    viewModel: viewModel,
                    cachedUsername: viewModel.cachedUsername
                )
            }
            .navigationTitle("")
            .navigationBarTitleDisplayMode(.inline)
        }
    }
}

extension RecoverViewModel.Mode {
    /// Row order on the method list (UX spec section 5.6).
    static let allCases: [RecoverViewModel.Mode] = [.phrase, .anotherDevice, .totp, .contact, .shares]

    var title: String {
        switch self {
        case .phrase: return "Recovery phrase"
        case .anotherDevice: return "Another device"
        case .shares: return "Social recovery shares"
        case .totp: return "Authenticator"
        case .contact: return "Email or phone"
        }
    }

    var detail: String {
        switch self {
        case .phrase: return "Your 24 words. Restores everything."
        case .anotherDevice: return "Confirm from a device that is still signed in."
        case .shares: return "Three of the shares you gave to people you trust."
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

    /// One line under the form title.
    var formLead: String? {
        switch self {
        case .phrase: return "Enter your 24 words and choose a new password."
        case .anotherDevice: return "Your other device links this one and carries your account across."
        case .shares: return "Enter any three shares and choose a new password."
        case .totp: return "Your authenticator app gets you back in. Your messages come back from your recovery phrase or another device."
        case .contact: return "A code sent to the email or phone number you set up under Settings > Recovery gets you back in. Your messages come back from your recovery phrase or another device."
        }
    }
}

// MARK: - Form

private struct RecoverForm: View {
    @ObservedObject var viewModel: RecoverViewModel
    /// `nil` on a fresh device: the form then asks for the username so the
    /// account can be found.
    let cachedUsername: String?

    var body: some View {
        if viewModel.mode != .anotherDevice {
            if let cachedUsername {
                HStack(spacing: 8) {
                    Image(systemName: "person.crop.circle")
                        .foregroundStyle(Color.lakkoTextMuted)
                    Text("@\(cachedUsername)")
                        .font(.body.weight(.medium))
                        .foregroundStyle(Color.lakkoText)
                    Text("on this device")
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            } else {
                AuthTextField(
                    label: "Username",
                    placeholder: "username",
                    text: $viewModel.username,
                    contentType: .username,
                    disabled: viewModel.busy
                )
                UsernameRuleHint(input: viewModel.username)
            }
        }

        switch viewModel.mode {
        case .anotherDevice:
            anotherDeviceSection
        case .totp:
            totpSection
        case .contact:
            contactSection
        case .phrase, .shares:
            if viewModel.mode == .phrase {
                PhraseEntryGrid(phrase: $viewModel.phrase, disabled: viewModel.busy)
            } else {
                sharesEditor
            }

            AuthSecureField(
                label: "New password",
                text: $viewModel.password,
                contentType: .newPassword,
                disabled: viewModel.busy
            )
            AuthHint(text: "At least \(RecoverViewModel.minPasswordLength) characters.")

            AuthSecureField(
                label: "Confirm new password",
                text: $viewModel.confirm,
                contentType: .newPassword,
                disabled: viewModel.busy,
                submitLabel: .go,
                onSubmit: viewModel.onSubmit
            )

            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }

            AuthPrimaryButton(
                title: "Recover account",
                busy: viewModel.busy,
                busyTitle: "Checking...",
                action: viewModel.onSubmit
            )
        }
    }

    /// ADR-0018 factor (c): a six-digit authenticator code grants access;
    /// the identity then restores through the usual post-access steps.
    @ViewBuilder
    private var totpSection: some View {
        AuthTextField(
            label: "Six-digit code",
            placeholder: "123456",
            text: $viewModel.totpCode,
            keyboard: .numberPad,
            contentType: .oneTimeCode,
            disabled: viewModel.busy,
            submitLabel: .go,
            onSubmit: viewModel.submitTotp
        )
        if let error = viewModel.error {
            AuthHint(text: error, isError: true)
        }
        AuthPrimaryButton(
            title: "Continue",
            busy: viewModel.busy,
            busyTitle: "Checking...",
            action: viewModel.submitTotp
        )
    }

    /// ADR-0018 factor (d): a one-time code by email or text message grants
    /// access. Two steps: request, then enter the code.
    @ViewBuilder
    private var contactSection: some View {
        switch viewModel.contactStep {
        case .request:
            Picker("Send the code by", selection: $viewModel.contactKind) {
                Text("Email").tag(ContactKind.email)
                Text("Text message").tag(ContactKind.phone)
            }
            .pickerStyle(.segmented)
            .disabled(viewModel.busy)

            AuthTextField(
                label: viewModel.contactKind == .email ? "Email" : "Phone number",
                placeholder: viewModel.contactKind == .email ? "name@example.com" : "+1 555 123 4567",
                text: $viewModel.contact,
                keyboard: viewModel.contactKind == .email ? .emailAddress : .phonePad,
                contentType: viewModel.contactKind == .email ? .emailAddress : .telephoneNumber,
                disabled: viewModel.busy,
                submitLabel: .go,
                onSubmit: viewModel.sendContactCode
            )
            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }
            AuthPrimaryButton(
                title: "Send code",
                busy: viewModel.busy,
                busyTitle: "Sending...",
                action: viewModel.sendContactCode
            )

        case .code:
            AuthHint(text: "If that email or number is set up for this account, a code is on its way.")
            AuthTextField(
                label: "Six-digit code",
                placeholder: "123456",
                text: $viewModel.contactCode,
                keyboard: .numberPad,
                contentType: .oneTimeCode,
                disabled: viewModel.busy,
                submitLabel: .go,
                onSubmit: viewModel.submitContactCode
            )
            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }
            AuthPrimaryButton(
                title: "Continue",
                busy: viewModel.busy,
                busyTitle: "Checking...",
                action: viewModel.submitContactCode
            )
        }
    }

    /// ADR-0018 factor (a), path (b): restore through an already linked
    /// device. The pairing code names the account by the login handle of
    /// the typed username (ADR-0056, no lookup), then the QR flow runs with
    /// the recovery payload.
    @ViewBuilder
    private var anotherDeviceSection: some View {
        if let target = viewModel.deviceRestoreTarget {
            HStack(spacing: 8) {
                Image(systemName: "person.crop.circle")
                    .foregroundStyle(Color.lakkoTextMuted)
                Text("@\(target.username)")
                    .font(.body.weight(.medium))
                    .foregroundStyle(Color.lakkoText)
            }
            RestoreViaDevicePanel(
                target: .byName(loginHandle: target.loginHandle),
                onRecovered: { outcome in
                    try await viewModel.completeFromDeviceRestore(outcome: outcome)
                }
            )
        } else {
            AuthTextField(
                label: "Username",
                placeholder: "username",
                text: $viewModel.username,
                contentType: .username,
                disabled: viewModel.busy,
                submitLabel: .go,
                onSubmit: viewModel.prepareDeviceRestore
            )
            UsernameRuleHint(input: viewModel.username)
            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }
            AuthPrimaryButton(
                title: "Continue",
                busy: viewModel.busy,
                busyTitle: "Continue",
                action: viewModel.prepareDeviceRestore
            )
        }
    }

    /// One share per line, on the sunken surface.
    private var sharesEditor: some View {
        VStack(alignment: .leading, spacing: 6) {
            AuthFieldLabel("Shares, one per line")
            ZStack(alignment: .topLeading) {
                if viewModel.sharesInput.isEmpty {
                    Text("Paste at least three shares here")
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 12)
                        .allowsHitTesting(false)
                }
                TextEditor(text: $viewModel.sharesInput)
                    .font(.system(.subheadline, design: .monospaced))
                    .foregroundStyle(Color.lakkoText)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
                    .scrollContentBackground(.hidden)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .disabled(viewModel.busy)
                    .frame(minHeight: 140)
            }
            .background(Color.lakkoSurface)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
            .overlay(
                RoundedRectangle(cornerRadius: LakkoRadius.control)
                    .stroke(Color.lakkoBorder, lineWidth: 1)
            )
        }
    }
}
