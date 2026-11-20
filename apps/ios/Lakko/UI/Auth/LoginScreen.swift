// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The log-in screen (UX spec section 5.6): username, password, Log in,
/// Use passkey, Forgot password. Pushed from ``WelcomeScreen``; the system
/// back returns there.
///
/// The screen is a presentation layer over ``LoginViewModel``.
public struct LoginScreen: View {

    @ObservedObject public var viewModel: LoginViewModel
    public var onNavigateToSignup: () -> Void
    public var onNavigateToRecover: () -> Void

    @State private var showingExplainer = false
    @ObservedObject private var l10n = Localization.shared

    public init(
        viewModel: LoginViewModel,
        onNavigateToSignup: @escaping () -> Void,
        onNavigateToRecover: @escaping () -> Void = {}
    ) {
        self.viewModel = viewModel
        self.onNavigateToSignup = onNavigateToSignup
        self.onNavigateToRecover = onNavigateToRecover
    }

    public var body: some View {
        AuthScreen {
            AuthTitle(title: l10n.t("auth.login.title"), subtitle: l10n.t("auth.login.lede"))

            AuthTextField(
                label: l10n.t("auth.username.label"),
                text: $viewModel.username,
                contentType: .username,
                disabled: viewModel.busy
            )
            UsernameRuleHint(input: viewModel.username)

            AuthSecureField(
                label: l10n.t("auth.password.label"),
                text: $viewModel.password,
                disabled: viewModel.busy,
                submitLabel: .go,
                onSubmit: viewModel.onSubmit
            )

            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }

            AuthPrimaryButton(
                title: l10n.t("auth.login.submit"),
                busy: viewModel.busy,
                busyTitle: l10n.t("auth.login.submitting"),
                action: viewModel.onSubmit
            )

            AuthSecondaryButton(
                title: l10n.t("auth.login.passkey"),
                icon: .passkey,
                disabled: viewModel.busy,
                action: viewModel.onPasskeyLogin
            )

            AuthLinkRow(title: l10n.t("auth.login.forgot"), disabled: viewModel.busy, action: onNavigateToRecover)

            AuthLinkRow(
                message: "auth.login.no_account",
                title: l10n.t("auth.signup.submit"),
                disabled: viewModel.busy,
                action: onNavigateToSignup
            )

            AuthLinkRow(title: l10n.t("auth.explainer.title")) {
                showingExplainer = true
            }
            .padding(.top, 8)
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        .sheet(isPresented: $showingExplainer) {
            PasswordExplainerSheet()
        }
    }
}
