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
    @ObservedObject private var l10n = Localization.shared

    public init(
        viewModel: SignupViewModel,
        onNavigateToLogin: @escaping () -> Void = {}
    ) {
        self.viewModel = viewModel
        self.onNavigateToLogin = onNavigateToLogin
    }

    public var body: some View {
        AuthScreen {
            AuthTitle(title: l10n.t("auth.signup.title"), subtitle: l10n.t("auth.signup.lede"))

            VStack(alignment: .leading, spacing: 6) {
                AuthTextField(
                    label: l10n.t("auth.username.label"),
                    text: $viewModel.username,
                    contentType: .username,
                    disabled: viewModel.busy
                )
                // ADR-0048 section 4: the reason, as the user types.
                if let hint = viewModel.usernameHint {
                    AuthHint(text: hint, isError: true)
                } else {
                    AuthHint(text: l10n.t("auth.signup.username.hint"))
                }
            }

            VStack(alignment: .leading, spacing: 6) {
                AuthTextField(
                    label: l10n.t("auth.signup.name.label"),
                    placeholder: viewModel.username,
                    text: $viewModel.displayName,
                    contentType: .name,
                    capitalization: .words,
                    autocorrect: false,
                    disabled: viewModel.busy
                )
                if let nameError = viewModel.displayNameError {
                    AuthHint(text: nameError, isError: true)
                } else {
                    AuthHint(text: l10n.t("auth.signup.name.hint"))
                }
            }

            AuthSecureField(
                label: l10n.t("auth.password.label"),
                text: $viewModel.password,
                contentType: .newPassword,
                disabled: viewModel.busy
            )
            AuthHint(text: l10n.t(
                "auth.password.hint", counts: ["count": UInt64(SignupViewModel.minPasswordLength)]))

            AuthSecureField(
                label: l10n.t("auth.password.confirm.label"),
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
                title: l10n.t(viewModel.rateLimited ? "common.try_again" : "auth.signup.submit"),
                busy: viewModel.busy,
                busyTitle: SignupViewModel.workingTitle,
                action: viewModel.rateLimited ? viewModel.onTryAgain : viewModel.onSubmit
            )
            if viewModel.busy {
                AuthHint(text: SignupViewModel.workingNote)
                if viewModel.cancellable {
                    AuthSecondaryButton(title: l10n.t("common.cancel"), action: viewModel.onCancel)
                }
            }

            AuthLinkRow(
                message: "auth.signup.have_account",
                title: l10n.t("auth.login.submit"),
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
/// the 12 words in a grid, Copy, the explanation, then the three-word
/// confirmation. Shown by the
/// app root over the freshly landed session; `onDone` lets the session
/// through.
struct SignupPhraseScreen: View {
    let phrase: String
    let onDone: () -> Void

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        RecoveryPhraseGuide(
            phrase: phrase,
            title: l10n.t("recovery_phrase.save.title"),
            intro: l10n.t("recovery_phrase.save.lede"),
            extra: { EmptyView() },
            onDone: onDone
        )
    }
}
