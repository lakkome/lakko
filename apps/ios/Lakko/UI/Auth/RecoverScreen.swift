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
    @ObservedObject private var l10n = Localization.shared

    public var body: some View {
        AuthScreen {
            AuthTitle(
                title: l10n.t("recovery.title"),
                subtitle: l10n.t("recovery.lede"))

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
                                Text(l10n.t(mode.titleKey))
                                    .font(.body.weight(.medium))
                                    .foregroundStyle(Color.lakkoText)
                                Text(l10n.t(mode.detailKey))
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

            AuthHint(text: l10n.t("recovery.access_only_note"))

            AuthLinkRow(
                message: "recovery.remembered",
                title: l10n.t("auth.login.submit"),
                action: onNavigateToLogin)
            AuthLinkRow(
                message: "auth.login.no_account",
                title: l10n.t("auth.signup.submit"),
                action: onNavigateToSignup)
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        .navigationDestination(item: $chosen) { mode in
            AuthScreen {
                AuthTitle(title: l10n.t(mode.titleKey), subtitle: l10n.t(mode.formLeadKey))
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

    /// The catalog key of the row title (ADR-0061).
    var titleKey: String {
        switch self {
        case .phrase: return "recovery.method.phrase.title"
        case .anotherDevice: return "recovery.method.device.title"
        case .shares: return "recovery.method.shares.title"
        case .totp: return "recovery.method.totp.title"
        case .contact: return "recovery.method.contact.title"
        }
    }

    /// The catalog key of the row's second line.
    var detailKey: String {
        switch self {
        case .phrase: return "recovery.method.phrase.sub"
        case .anotherDevice: return "recovery.method.device.sub"
        case .shares: return "recovery.method.shares.sub"
        case .totp: return "recovery.method.totp.sub"
        case .contact: return "recovery.method.contact.sub"
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

    /// The catalog key of the line under the form title.
    var formLeadKey: String {
        switch self {
        case .phrase: return "recovery.phrase.intro"
        // The QR flow: this client has no code-comparison link yet.
        case .anotherDevice: return "recovery.device.intro_qr"
        case .shares: return "recovery.shares.intro"
        case .totp: return "recovery.totp.intro"
        case .contact: return "recovery.contact.intro"
        }
    }
}

// MARK: - Form

private struct RecoverForm: View {
    @ObservedObject var viewModel: RecoverViewModel
    /// `nil` on a fresh device: the form then asks for the username so the
    /// account can be found.
    let cachedUsername: String?

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if viewModel.mode != .anotherDevice {
            if let cachedUsername {
                HStack(spacing: 8) {
                    Image(systemName: "person.crop.circle")
                        .foregroundStyle(Color.lakkoTextMuted)
                    Text(l10n.t("recovery.cached_account", texts: ["username": "@\(cachedUsername)"]))
                        .font(.body.weight(.medium))
                        .foregroundStyle(Color.lakkoText)
                }
            } else {
                AuthTextField(
                    label: l10n.t("auth.username.label"),
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
                label: l10n.t("auth.password.new.label"),
                text: $viewModel.password,
                contentType: .newPassword,
                disabled: viewModel.busy
            )
            AuthHint(text: l10n.t(
                "auth.password.hint", counts: ["count": UInt64(RecoverViewModel.minPasswordLength)]))

            AuthSecureField(
                label: l10n.t("auth.password.new_confirm.label"),
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
                title: l10n.t("recovery.submit"),
                busy: viewModel.busy,
                busyTitle: l10n.t("common.checking"),
                action: viewModel.onSubmit
            )
        }
    }

    /// ADR-0018 factor (c): a six-digit authenticator code grants access;
    /// the identity then restores through the usual post-access steps.
    @ViewBuilder
    private var totpSection: some View {
        AuthTextField(
            label: l10n.t("recovery.totp.code_label"),
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
            title: l10n.t("common.continue"),
            busy: viewModel.busy,
            busyTitle: l10n.t("common.checking"),
            action: viewModel.submitTotp
        )
    }

    /// ADR-0018 factor (d): a one-time code by email or text message grants
    /// access. Two steps: request, then enter the code.
    @ViewBuilder
    private var contactSection: some View {
        switch viewModel.contactStep {
        case .request:
            Picker(l10n.t("recovery.contact.send_by"), selection: $viewModel.contactKind) {
                Text(l10n.t("common.email")).tag(ContactKind.email)
                Text(l10n.t("common.phone")).tag(ContactKind.phone)
            }
            .pickerStyle(.segmented)
            .disabled(viewModel.busy)

            AuthTextField(
                label: l10n.t(viewModel.contactKind == .email ? "common.email" : "recovery.contact.phone_label"),
                placeholder: viewModel.contactKind == .email ? "you@example.com" : "+15551234567",
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
                title: l10n.t("recovery.contact.send"),
                busy: viewModel.busy,
                busyTitle: l10n.t("common.sending"),
                action: viewModel.sendContactCode
            )

        case .code:
            AuthHint(text: l10n.t(
                viewModel.contactKind == .email ? "recovery.contact.sent_email" : "recovery.contact.sent_phone"))
            AuthTextField(
                label: l10n.t("recovery.contact.code_label"),
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
                title: l10n.t("common.continue"),
                busy: viewModel.busy,
                busyTitle: l10n.t("common.checking"),
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
                label: l10n.t("auth.username.label"),
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
                title: l10n.t("common.continue"),
                busy: viewModel.busy,
                busyTitle: l10n.t("common.continue"),
                action: viewModel.prepareDeviceRestore
            )
        }
    }

    /// One share per line, on the sunken surface.
    private var sharesEditor: some View {
        VStack(alignment: .leading, spacing: 6) {
            AuthFieldLabel(l10n.t("recovery.shares.label"))
            ZStack(alignment: .topLeading) {
                if viewModel.sharesInput.isEmpty {
                    Text(verbatim: "3:abcdEFGH...\n1:zzzzABCD...\n5:mmmm0123...")
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
            AuthHint(text: l10n.t("recovery.shares.help"))
        }
    }
}
