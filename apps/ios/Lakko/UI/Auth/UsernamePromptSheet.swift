// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// "What is your username?": a sheet over the app, shown by the app root
/// while ``AuthRepository/usernamePromptAccountIdB64`` names the signed-in
/// account (a sign-in that named no username, and the backup gave none of
/// this account's). The app behind it stays usable; closing it never blocks
/// anything. Rules in ``UsernamePrompt``.
struct UsernamePromptSheet: View {
    @ObservedObject var authRepository: AuthRepository

    @State private var typed = ""
    @State private var busy = false
    @State private var error: UiText?
    /// "I don't remember" was chosen: the note is the sheet's last state.
    @State private var showingNote = false
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        AuthScreen {
            AuthTitle(
                title: l10n.t("auth.username_prompt.title"),
                subtitle: showingNote ? nil : l10n.t("auth.username_prompt.lede"))
            if showingNote {
                AuthHint(text: l10n.t("auth.username_prompt.forgot_note"))
                AuthPrimaryButton(title: l10n.t("common.ok")) {
                    authRepository.dismissUsernamePrompt()
                }
            } else {
                AuthTextField(
                    label: l10n.t("auth.username_prompt.field"),
                    text: $typed,
                    contentType: .username,
                    disabled: busy,
                    submitLabel: .go,
                    onSubmit: submit
                )
                if let error {
                    AuthHint(text: error.text, isError: true)
                }
                AuthPrimaryButton(
                    title: l10n.t("auth.username_prompt.continue"),
                    busy: busy,
                    busyTitle: l10n.t("common.checking"),
                    disabled: typed.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                    action: submit
                )
                AuthLinkRow(title: l10n.t("auth.username_prompt.forgot"), disabled: busy) {
                    error = nil
                    showingNote = true
                }
            }
        }
        .onChange(of: typed) { _, _ in
            if !busy { error = nil }
        }
    }

    private func submit() {
        guard !busy, !typed.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        busy = true
        error = nil
        let name = typed
        Task {
            let outcome = await authRepository.submitPromptedUsername(name)
            busy = false
            switch outcome {
            case .rule(let reason):
                error = UiText { reason.message }
            case .empty:
                error = nil
            default:
                error = UsernamePrompt.errorKey(outcome).map { UiText.key($0) }
            }
        }
    }
}
