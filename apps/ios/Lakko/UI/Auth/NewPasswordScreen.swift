// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The pure rule of the new-password form.
enum NewPasswordRule {

    /// The same floor as sign-up (``SignupViewModel/minPasswordLength``).
    static let minLength = 8

    enum Refusal: Equatable {
        case mismatch
        case tooShort
    }

    /// Why `password` / `confirm` cannot be saved, or `nil` when they can.
    static func refusal(password: String, confirm: String) -> Refusal? {
        if password != confirm { return .mismatch }
        if password.count < minLength { return .tooShort }
        return nil
    }
}

/// New password + confirm + Save, used by "Choose a new password" and by
/// Settings > Security > Password. Saving runs
/// ``AuthRepository/replacePassword(newPassword:)``: the device already
/// holds the account's keys, so only the new password is typed.
struct NewPasswordForm: View {
    @ObservedObject var authRepository: AuthRepository
    /// The primary button's title.
    let saveTitle: String
    /// "Not now", when the form may be skipped.
    var onSkip: (() -> Void)? = nil
    let onSaved: () -> Void

    @State private var password = ""
    @State private var confirm = ""
    @State private var busy = false
    @State private var error: UiText?
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        AuthSecureField(
            label: l10n.t("auth.password.new.label"),
            text: $password,
            contentType: .newPassword,
            disabled: busy
        )
        AuthHint(text: l10n.t(
            "auth.password.hint", counts: ["count": UInt64(NewPasswordRule.minLength)]))
        AuthSecureField(
            label: l10n.t("auth.password.new_confirm.label"),
            text: $confirm,
            contentType: .newPassword,
            disabled: busy,
            submitLabel: .go,
            onSubmit: save
        )
        if let error {
            AuthHint(text: error.text, isError: true)
        }
        AuthPrimaryButton(
            title: saveTitle,
            busy: busy,
            busyTitle: l10n.t("common.saving"),
            action: save
        )
        if let onSkip {
            AuthLinkRow(title: l10n.t("recovery.new_password.not_now"), disabled: busy, action: onSkip)
        }
    }

    private func save() {
        guard !busy else { return }
        switch NewPasswordRule.refusal(password: password, confirm: confirm) {
        case .mismatch:
            error = .key("auth.password.error.mismatch")
            return
        case .tooShort:
            error = .key(
                "auth.password.error.too_short", counts: ["count": UInt64(NewPasswordRule.minLength)])
            return
        case nil:
            break
        }
        busy = true
        error = nil
        let bytes = Data(password.utf8)
        Task {
            do {
                try await authRepository.replacePassword(newPassword: bytes)
                busy = false
                password = ""
                confirm = ""
                onSaved()
            } catch {
                busy = false
                NSLog("[auth] WARN replacing the password failed")
                self.error = UiText { AuthErrorCopy.newPassword(error) }
            }
        }
    }
}

/// "Choose a new password": shown once, by the app root, right after a
/// forgot-password route succeeded on a device that holds the account's
/// keys (three words, an authenticator code, a passkey, another device).
/// The person is already signed in.
///
/// Saving replaces the password and opens the app. "Not now" opens the app
/// with the old (forgotten) password still valid; Settings > Security keeps
/// the same form.
struct NewPasswordPromptScreen: View {
    @ObservedObject var authRepository: AuthRepository
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        AuthScreen {
            AuthTitle(
                title: l10n.t("recovery.new_password.title"),
                subtitle: l10n.t("recovery.new_password.lede"))
            NewPasswordForm(
                authRepository: authRepository,
                saveTitle: l10n.t("recovery.new_password.save"),
                onSkip: { authRepository.dismissNewPasswordOffer() },
                // `replacePassword` clears the offer itself.
                onSaved: {}
            )
        }
    }
}

/// Settings > Security > Password: the same form, for a person who simply
/// wants a new password. Offered to accounts with a username on a device
/// that holds the keys (``AuthRepository/canReplacePassword``).
struct PasswordSettingsScreen: View {
    @ObservedObject var authRepository: AuthRepository
    @State private var saved = false
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        AuthScreen {
            Text(l10n.t("settings.password.lede"))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
            if authRepository.canReplacePassword {
                NewPasswordForm(
                    authRepository: authRepository,
                    saveTitle: l10n.t("settings.password.change"),
                    onSaved: { saved = true }
                )
                if saved {
                    Label(l10n.t("recovery.new_password.saved"), systemImage: "checkmark.circle.fill")
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoSuccess)
                }
            } else {
                AuthHint(text: l10n.t("common.error.session_not_ready"), isError: true)
            }
        }
        .navigationTitle(l10n.t("settings.password.title"))
        .navigationBarTitleDisplayMode(.inline)
    }
}
