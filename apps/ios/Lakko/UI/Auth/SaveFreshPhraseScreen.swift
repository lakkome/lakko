// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The phrase screen of ``AuthState/saveFreshPhrase(_:)``: after "start over
/// with a new identity" and after a guest signup. It shows the new 12 words,
/// asks for three of them back, then lands the session.
///
/// One state comes before the words after a fresh identity: the new identity
/// is published, but replacing the account's recovery credential with one
/// made from the new phrase failed (offline, a limit). The new 12 words
/// would then not sign in anywhere, so they are NOT shown. The screen says
/// what happened and offers "Try again", which repeats only that step (it
/// never mints again), plus "Log out". The debt is also recorded on the
/// device and retried when a session next lands there.
public struct SaveFreshPhraseScreen: View {

    @ObservedObject private var authRepository: AuthRepository
    /// The account's username; empty for a guest account (ADR-0032), whose
    /// phrase is shown right after it was created without a password.
    public let username: String

    @StateObject private var wait = RetryCountdown()
    @State private var retryBusy = false
    @State private var retryError: UiText?
    @State private var confirmError: UiText?
    @ObservedObject private var l10n = Localization.shared

    public init(authRepository: AuthRepository, username: String = "") {
        self.authRepository = authRepository
        self.username = username
    }

    public var body: some View {
        if authRepository.freshRecoveryCredentialPending {
            credentialPending
        } else {
            phraseGuide
        }
    }

    private var phraseGuide: some View {
        let guest = username.isEmpty
        return RecoveryPhraseGuide(
            phrase: authRepository.freshPhrase() ?? "",
            title: l10n.t(guest ? "recovery_phrase.save.title" : "recovery_phrase.fresh.title"),
            intro: guest
                ? l10n.t("recovery_phrase.guest.lede")
                : l10n.t("recovery_phrase.fresh.lede", texts: ["username": "@\(username)"]),
            extra: {
                if !guest {
                    AuthHint(text: l10n.t("recovery_phrase.fresh.safety_note"))
                }
                if let confirmError {
                    AuthHint(text: confirmError.text, isError: true)
                }
            },
            onDone: {
                do {
                    try authRepository.confirmFreshPhraseSaved()
                } catch {
                    confirmError = .key("recovery_phrase.fresh.error.generic")
                }
            }
        )
    }

    private var credentialPending: some View {
        AuthScreen {
            AuthTitle(title: l10n.t("recovery_phrase.fresh.title"))
            AuthHint(text: l10n.t("recovery_phrase.fresh.credential_pending"), isError: true)
            if let retryError {
                AuthHint(text: retryError.text, isError: true)
            }
            WaitNotice(countdown: wait)
            AuthPrimaryButton(
                title: l10n.t("common.try_again"),
                busy: retryBusy,
                busyTitle: l10n.t("common.checking"),
                disabled: wait.waiting,
                action: retry
            )
            AuthLinkRow(
                title: l10n.t("restore_identity.log_out"),
                disabled: retryBusy,
                action: { authRepository.logout() }
            )
        }
    }

    private func retry() {
        guard !retryBusy else { return }
        retryBusy = true
        retryError = nil
        Task {
            do {
                try await authRepository.retryFreshRecoveryCredential()
            } catch {
                if let secs = RetryCountdown.attemptsRetryAfterSecs(error) {
                    wait.start(forSecs: secs)
                } else if case .network = error as? AuthClientError {
                    retryError = .key("common.error.offline")
                } else if let err = error as? AuthClientError, let limit = AuthRateLimit.copy(for: err) {
                    retryError = UiText { limit }
                } else {
                    retryError = .key("recovery_phrase.fresh.credential_error")
                }
            }
            retryBusy = false
        }
    }
}
