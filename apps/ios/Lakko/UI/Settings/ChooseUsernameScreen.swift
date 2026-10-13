// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// "Choose a username" (ADR-0075 pass A, part 2): one screen on which a
/// guest account gets a username and its first password. What it shows is
/// ``UsernameClaim``; what a tap does is ``UsernameClaimModel``.
///
/// If the password step fails after the name was saved, the name is kept
/// (the field is locked, showing it) and only the password is retried.
struct ChooseUsernameScreen: View {
    /// `username.claim.done` was shown and acknowledged.
    let onDone: () -> Void

    @StateObject private var model: UsernameClaimModel
    @ObservedObject private var l10n = Localization.shared

    init(authRepository: AuthRepository, onDone: @escaping () -> Void) {
        self.onDone = onDone
        let account: SessionAccount? = {
            if case .session(let account) = authRepository.state { return account }
            return nil
        }()
        self._model = StateObject(wrappedValue: UsernameClaimModel(
            ports: .live(authRepository),
            username: account?.username,
            firstPasswordOwed: authRepository.firstPasswordOwed))
    }

    var body: some View {
        AuthScreen {
            AuthTitle(title: l10n.t("username.claim.title"), subtitle: l10n.t("username.claim.body"))
            switch model.state.step {
            case .done:
                AuthHint(text: l10n.t(
                    "username.claim.done", texts: ["username": "@\(model.state.savedName ?? "")"]))
                AuthPrimaryButton(title: l10n.t("common.continue"), action: onDone)
            case .form, .passwordOwed:
                form
            }
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
    }

    @ViewBuilder private var form: some View {
        if UsernameClaim.nameEditable(model.state) {
            VStack(alignment: .leading, spacing: 6) {
                AuthTextField(
                    label: l10n.t("auth.username.label"),
                    text: $model.username,
                    contentType: .username,
                    disabled: model.busy
                )
                if let hint = model.usernameHint {
                    AuthHint(text: hint, isError: true)
                } else {
                    AuthHint(text: l10n.t("auth.signup.username.hint"))
                }
            }
        } else if let saved = model.state.savedName {
            // The name is saved; only the password is left.
            AuthHint(text: l10n.t("username.hint.shown_as", texts: ["username": "@\(saved)"]))
        }

        AuthSecureField(
            label: l10n.t("auth.password.new.label"),
            text: $model.password,
            contentType: .newPassword,
            disabled: model.busy
        )
        AuthHint(text: l10n.t("username.claim.password_help"))
        AuthSecureField(
            label: l10n.t("auth.password.new_confirm.label"),
            text: $model.confirm,
            contentType: .newPassword,
            disabled: model.busy,
            submitLabel: .go,
            onSubmit: submit
        )
        if let passwordError = model.passwordError {
            AuthHint(text: passwordError.text, isError: true)
        }
        if let failure = model.state.failure {
            AuthHint(text: text(for: failure), isError: true)
        }
        AuthPrimaryButton(
            title: l10n.t("username.claim.save"),
            busy: model.busy,
            busyTitle: l10n.t("common.saving"),
            disabled: !model.canSubmit,
            action: submit
        )
    }

    private func text(for failure: UsernameClaim.Failure) -> String {
        switch failure {
        case .taken: return l10n.t("auth.signup.error.username_taken")
        case .bindLimited(let secs): return AuthRateLimit.handleBindLimitedCopy(retryAfterSecs: secs)
        case .rule: return model.usernameHint ?? l10n.t("username.error.unknown")
        case .offline: return l10n.t("common.error.offline")
        case .failed: return l10n.t("auth.signup.error.generic")
        case .passwordFailed: return l10n.t("common.error.unavailable")
        }
    }

    private func submit() {
        guard model.canSubmit else { return }
        Task { await model.submit() }
    }
}

/// A "Choose a username" button for the places that need a username first.
/// Draws nothing for an account that has one.
struct ChooseUsernameButton: View {
    let username: String?
    let action: () -> Void
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if UsernameClaim.offered(username: username) {
            AuthSecondaryButton(
                title: l10n.t("username.claim.title"), systemImage: "at", action: action)
        }
    }
}

extension UsernameClaimModel.Ports {

    /// The production ports.
    @MainActor
    static func live(_ repository: AuthRepository) -> UsernameClaimModel.Ports {
        UsernameClaimModel.Ports(
            ruleMessage: { checkUsername($0).inlineMessage },
            claimName: { [weak repository] typed in
                guard let repository else { throw SessionNotReady() }
                return try await repository.claimUsername(typed)
            },
            setPassword: { [weak repository] newPassword in
                guard let repository else { throw SessionNotReady() }
                // The account's first password: set from the keys this
                // device holds, the same call "Choose a new password" makes.
                try await repository.replacePassword(newPassword: newPassword)
            }
        )
    }
}
