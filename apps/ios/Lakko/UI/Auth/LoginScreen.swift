// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The log-in screen (UX spec section 5.6): username, password, Log in,
/// Use passkey, Forgot password. Pushed from ``WelcomeScreen``; the system
/// back returns there.
///
/// The screen is a presentation layer over ``LoginViewModel``; the
/// password sign-in and the passkey sign-in are the repository's existing
/// paths, unchanged.
public struct LoginScreen: View {

    @ObservedObject public var viewModel: LoginViewModel
    public var onNavigateToSignup: () -> Void
    public var onNavigateToRecover: () -> Void

    @State private var showingExplainer = false

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
            AuthTitle(title: "Log in")

            AuthTextField(
                label: "Username",
                placeholder: "username",
                text: $viewModel.username,
                contentType: .username,
                disabled: viewModel.busy
            )
            UsernameRuleHint(input: viewModel.username)

            AuthSecureField(
                label: "Password",
                text: $viewModel.password,
                disabled: viewModel.busy,
                submitLabel: .go,
                onSubmit: viewModel.onSubmit
            )

            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }

            AuthPrimaryButton(
                title: "Log in",
                busy: viewModel.busy,
                busyTitle: "Logging in...",
                action: viewModel.onSubmit
            )

            AuthSecondaryButton(
                title: "Use passkey",
                systemImage: "person.badge.key",
                disabled: viewModel.busy,
                action: viewModel.onPasskeyLogin
            )

            AuthLinkRow(title: "Forgot password?", disabled: viewModel.busy, action: onNavigateToRecover)

            AuthLinkRow(
                lead: "New to Lakko?",
                title: "Create account",
                disabled: viewModel.busy,
                action: onNavigateToSignup
            )

            AuthLinkRow(title: "How Lakko protects your password") {
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
