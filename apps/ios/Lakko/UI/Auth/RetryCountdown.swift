// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import SwiftUI

/// The live wait of the rate-limited state the recovery forms share: "Too
/// many attempts. Try again in 14:59.", counting down once a second, with
/// the primary button disabled until it reaches zero. The same clock serves
/// the three-word form's own local pause.
///
/// Mirror of `apps/web/src/auth/recover/retry-countdown.ts`.
@MainActor
final class RetryCountdown: ObservableObject {

    /// Milliseconds left; 0 when no wait is running.
    @Published private(set) var remainingMs: Int64 = 0

    /// Whether a wait is running.
    var waiting: Bool { remainingMs > 0 }

    private var untilMs: Int64 = 0
    private var ticker: Task<Void, Never>?
    private let nowMs: () -> Int64

    init(nowMs: @escaping () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }) {
        self.nowMs = nowMs
    }

    /// Start (or replace) the wait: it ends at `untilMs` (unix ms).
    func start(untilMs: Int64) {
        ticker?.cancel()
        self.untilMs = untilMs
        tick()
        guard waiting else { return }
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                guard let self, !Task.isCancelled else { return }
                self.tick()
                if !self.waiting { return }
            }
        }
    }

    /// Start (or replace) the wait: it ends `secs` seconds from now.
    func start(forSecs secs: Int) {
        start(untilMs: nowMs() + Int64(secs) * 1000)
    }

    func stop() {
        ticker?.cancel()
        ticker = nil
        remainingMs = 0
    }

    private func tick() {
        remainingMs = max(0, untilMs - nowMs())
    }

    /// The wait a server limit asks for, in seconds, or `nil` when `error`
    /// is not an attempts limit with a retry time. `device-request-limited`,
    /// `signup-rate-limited` and `handle-bind-limited` have their own copy
    /// and are not counted down here.
    static func attemptsRetryAfterSecs(_ error: Error) -> Int? {
        guard let err = error as? AuthClientError,
              let refusal = AuthRateLimit.refusal(in: err),
              refusal.code == AuthRateLimit.tooManyAttempts,
              let secs = refusal.retryAfterSecs, secs > 0 else {
            return nil
        }
        return secs
    }
}

/// The line a running wait shows. `messageKey` takes a `{time}` argument
/// (`m:ss`); nothing is rendered when no wait is running.
struct WaitNotice: View {
    @ObservedObject var countdown: RetryCountdown
    var messageKey: String = "recovery.error.rate_limited"

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if countdown.waiting {
            AuthHint(
                text: l10n.t(
                    messageKey,
                    texts: ["time": ThreeWords.formatCountdown(remainingMs: countdown.remainingMs)]),
                isError: true
            )
        }
    }
}

/// The state a recovery form shows when its method is switched off on the
/// server: one sentence and the way back to the list.
struct MethodUnavailable: View {
    let onBack: () -> Void

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        AuthHint(text: l10n.t("recovery.error.unavailable"), isError: true)
        AuthSecondaryButton(title: l10n.t("recovery.methods.back_to_list"), action: onBack)
    }
}
