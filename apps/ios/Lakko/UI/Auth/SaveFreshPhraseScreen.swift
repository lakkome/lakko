// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The phrase screen of ``AuthState/saveFreshPhrase(_:)``: after "Reset
/// account keys" and after taking the account back (not after a sign-up:
/// ADR-0075 D2, a guest's included). It shows the new 12 words, asks
/// for three of them back, then lands the session.
///
/// One state comes before the words after a reset (UX 15.4): the account
/// keys were reset in one server step, but telling the directory, this
/// device's KeyPackage row and the account's device list about the new
/// keys did not finish (offline, a limit). Until that is done the contacts
/// and the other devices cannot find the new keys, so they are NOT shown.
/// The screen says what happened and offers "Try again", which repeats only
/// the publish (it never resets again), plus "Log out". While the
/// directory's cool-down runs (a second reset within its hour) it says how
/// many minutes are left instead, shows no Try again, and publishes by
/// itself when the time is up. The debt is also
/// recorded on the device and retried when a session next lands there. If
/// the reset is older than 30 days when the retry runs, the screen asks for
/// a sign-in first.
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
        if authRepository.freshPublishPending {
            publishPending
        } else if authRepository.freshShowsWords {
            phraseGuide
        } else {
            // Not phrase-protected: no words; the session is landing.
            Color.lakkoBackground.ignoresSafeArea()
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
                    AuthHint(text: l10n.t("recovery_phrase.fresh.devices_signed_out"))
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

    private var publishPending: some View {
        // Rendered once a second so the minutes of the directory's
        // cool-down count down (UX 15.4); the task retries when it is over,
        // while this screen is open.
        TimelineView(.periodic(from: .now, by: 1)) { context in
            publishPendingBody(display(at: context.date))
        }
        .task(id: authRepository.keyResetPublishWait) { await retryWhenTheWaitIsOver() }
    }

    private func display(at date: Date) -> PublishOwedDisplay {
        let wait = authRepository.keyResetPublishWait
        let accountIdB64 = authRepository.signedInAccountIdB64
        return PublishOwedDisplay.decide(
            copyKey: KeysChangedCopy.publishOwedKey(
                showsWords: authRepository.freshShowsWords,
                needsSignIn: authRepository.freshPublishNeedsSignIn),
            showsWords: authRepository.freshShowsWords,
            waitUntilMs: wait?.accountIdB64 == accountIdB64 ? wait?.untilMs : nil,
            needsSignIn: authRepository.freshPublishNeedsSignIn,
            nowMs: Int64(date.timeIntervalSince1970 * 1000))
    }

    @ViewBuilder
    private func publishPendingBody(_ display: PublishOwedDisplay) -> some View {
        AuthScreen {
            if authRepository.freshShowsWords {
                AuthTitle(title: l10n.t("recovery_phrase.fresh.title"))
            }
            switch display.message {
            case .copy(let key):
                AuthHint(text: l10n.t(key), isError: true)
                if let retryError {
                    AuthHint(text: retryError.text, isError: true)
                }
                WaitNotice(countdown: wait)
            case .waiting(let key, let minutes):
                AuthHint(text: l10n.t(key, counts: ["minutes": UInt64(minutes)]))
            }
            if display.showsTryAgain {
                AuthPrimaryButton(
                    title: l10n.t("common.try_again"),
                    busy: retryBusy,
                    busyTitle: l10n.t("common.checking"),
                    disabled: wait.waiting || authRepository.freshPublishNeedsSignIn,
                    action: retry
                )
            }
            AuthLinkRow(
                title: l10n.t("restore_identity.log_out"),
                disabled: retryBusy,
                action: { authRepository.logout() }
            )
        }
    }

    /// Sleep until the directory's cool-down is over, then publish once. A
    /// new refusal names a new time, which restarts this task.
    private func retryWhenTheWaitIsOver() async {
        guard let pending = authRepository.keyResetPublishWait else { return }
        let dueMs = pending.untilMs + PublishOwedDisplay.retryMarginMs
        let delayMs = dueMs - Int64(Date().timeIntervalSince1970 * 1000)
        if delayMs > 0 {
            try? await Task.sleep(nanoseconds: UInt64(delayMs) * 1_000_000)
        }
        guard !Task.isCancelled, authRepository.keyResetPublishWait == pending else { return }
        retry()
    }

    private func retry() {
        guard !retryBusy else { return }
        retryBusy = true
        retryError = nil
        Task {
            do {
                try await authRepository.retryFreshPublish()
            } catch {
                let cause = (error as? KeyResetPublishFailed)?.underlying ?? error
                if authRepository.keyResetPublishWait != nil {
                    // The directory's cool-down: the waiting copy says it.
                    retryError = nil
                } else if let secs = RetryCountdown.attemptsRetryAfterSecs(cause) {
                    wait.start(forSecs: secs)
                } else if case .network = cause as? AuthClientError {
                    retryError = .key("common.error.offline")
                } else if case .network = cause as? DiscoveryClientError {
                    retryError = .key("common.error.offline")
                } else if let err = cause as? AuthClientError, let limit = AuthRateLimit.copy(for: err) {
                    retryError = UiText { limit }
                } else if cause is KeyResetSignInRequired {
                    retryError = nil
                } else {
                    retryError = .key("recovery_phrase.fresh.publish_error")
                }
            }
            retryBusy = false
        }
    }
}
