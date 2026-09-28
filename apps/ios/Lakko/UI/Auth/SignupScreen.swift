// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The sign-up form (UX spec section 5.6): username with the live
/// availability hint, name, password, confirm, Create account. Pushed
/// from ``WelcomeScreen``.
///
/// The recovery-phrase step that follows a successful sign-up is NOT
/// rendered here: the repository lands the session as part of sign-up,
/// which replaces the anonymous stack, so the phrase is handed up through
/// ``SignupViewModel/onPhraseReady`` and the app root shows
/// ``SignupPhraseScreen`` over the session until the user has confirmed
/// it.
public struct SignupScreen: View {

    @ObservedObject public var viewModel: SignupViewModel
    public var onNavigateToLogin: () -> Void

    public init(
        viewModel: SignupViewModel,
        onNavigateToLogin: @escaping () -> Void = {}
    ) {
        self.viewModel = viewModel
        self.onNavigateToLogin = onNavigateToLogin
    }

    public var body: some View {
        AuthScreen {
            AuthTitle(title: "Create account")

            VStack(alignment: .leading, spacing: 6) {
                AuthTextField(
                    label: "Username",
                    placeholder: "username",
                    text: $viewModel.username,
                    contentType: .username,
                    disabled: viewModel.busy
                )
                // ADR-0048 section 4: the reason, as the user types.
                if let hint = viewModel.usernameHint {
                    AuthHint(text: hint, isError: true)
                } else {
                    AuthHint(text: "Letters, digits and underscores, 3 to 32 characters. Others find you by it.")
                }
            }

            VStack(alignment: .leading, spacing: 6) {
                AuthTextField(
                    label: "Name",
                    placeholder: viewModel.username.isEmpty ? "Optional" : viewModel.username,
                    text: $viewModel.displayName,
                    contentType: .name,
                    capitalization: .words,
                    autocorrect: false,
                    disabled: viewModel.busy
                )
                if let nameError = viewModel.displayNameError {
                    AuthHint(text: nameError, isError: true)
                } else {
                    AuthHint(text: "Shown to your contacts. Leave it empty to show your username.")
                }
            }

            AuthSecureField(
                label: "Password",
                text: $viewModel.password,
                contentType: .newPassword,
                disabled: viewModel.busy
            )
            AuthHint(text: "At least \(SignupViewModel.minPasswordLength) characters.")

            AuthSecureField(
                label: "Confirm password",
                text: $viewModel.confirm,
                contentType: .newPassword,
                disabled: viewModel.busy,
                submitLabel: .go,
                onSubmit: viewModel.onSubmit
            )

            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }

            // ADR-0054 S4 / `docs/ux/anti-abuse.md` section 1: the one heavy
            // wait in the app. The button becomes a disabled working row, one
            // line below it, and `Cancel` back to the filled-in form.
            AuthPrimaryButton(
                title: viewModel.rateLimited ? "Try again" : "Create account",
                busy: viewModel.busy,
                busyTitle: SignupViewModel.workingTitle,
                action: viewModel.rateLimited ? viewModel.onTryAgain : viewModel.onSubmit
            )
            if viewModel.busy {
                AuthHint(text: SignupViewModel.workingNote)
                if viewModel.cancellable {
                    AuthSecondaryButton(title: "Cancel", action: viewModel.onCancel)
                }
            }

            AuthLinkRow(
                lead: "Already have an account?",
                title: "Log in",
                disabled: viewModel.busy,
                action: onNavigateToLogin
            )
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        // Leaving the screen while working cancels the work.
        .onDisappear { viewModel.onCancel() }
    }
}

/// The guided recovery-phrase step after sign-up (UX spec section 5.6):
/// the 24 words in a grid, Copy, the explanation, social-recovery shares
/// behind "More options", then the three-word confirmation. Shown by the
/// app root over the freshly landed session; `onDone` lets the session
/// through.
struct SignupPhraseScreen: View {
    let phrase: String
    let onDone: () -> Void

    var body: some View {
        RecoveryPhraseGuide(
            phrase: phrase,
            title: "Save your recovery phrase",
            intro: "Your account is ready. These 24 words are the only way to get it back if you lose this device or forget your password.",
            extra: {
                SocialRecoverySharesPanel(phrase: phrase)
            },
            onDone: onDone
        )
    }
}
