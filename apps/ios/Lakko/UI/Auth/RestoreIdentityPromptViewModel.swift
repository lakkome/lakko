// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// State + dispatcher for ``RestoreIdentityPromptScreen`` (ADR-0017
/// fresh-device fail-safe).
///
/// Mirror of `apps/web/src/auth/RestoreIdentityPrompt.tsx`'s local
/// signals. Rendered only while the auth state is
/// ``AuthState/needsRecoveryPhrase(_:)``: the user authenticated
/// successfully (password or passkey ACCESS) on a device that has NO
/// cached AccountIdentity (phrase) seed, so the canonical identity
/// cannot be rebuilt. Per ADR-0017 we MUST NOT derive the identity
/// from the OPAQUE export key (the wrong identity now that the
/// identity is phrase-seeded) nor silently mint a fresh one (which
/// would break every contact's pin). Instead the user supplies their
/// recovery phrase; ``AuthRepository/provideRecoveryPhrase(mnemonicPhrase:)``
/// derives the BIP39 seed, HARD-BLOCK verifies it against the
/// published bundle (fail-closed on mismatch / fetch failure), caches
/// it, and completes the session.
@MainActor
public final class RestoreIdentityPromptViewModel: ObservableObject {

    @Published public var phrase: String = ""
    @Published public private(set) var busy: Bool = false
    /// Kept as ``UiText`` so a message on screen follows a live language switch.
    @Published private var errorText: UiText?
    public var error: String? { errorText?.text }

    /// Username for the account being restored, surfaced in the
    /// "...for @alice" line.
    public let username: String

    /// ADR-0018 factor (a) — base64 account id for the
    /// restore-via-device panel (the pairing QR embeds the raw id).
    public let accountIdB64: String

    /// Raw 16-byte account id decoded from ``accountIdB64``; `nil`
    /// only on a malformed stored marker (the via-device toggle is
    /// hidden in that case).
    public var accountIdBytes: Data? {
        base64Decode(accountIdB64)
    }

    private let repository: AuthRepository
    private let l10n = Localization.shared

    public init(
        repository: AuthRepository,
        username: String,
        accountIdB64: String
    ) {
        self.repository = repository
        self.username = username
        self.accountIdB64 = accountIdB64
    }

    /// ADR-0018 factor (a), path (a) — complete the pending restore
    /// with the identity received from ANOTHER DEVICE instead of a
    /// typed phrase. Async passthrough to
    /// ``AuthRepository/completeFromPairing(phraseSeed:sessionToken:sessionExpiresAtMs:)``;
    /// verification failures (`PhraseMismatch` /
    /// `IdentityVerificationUnavailable`) propagate to the calling
    /// panel, which surfaces them inline. On success the repository
    /// flips the auth state to `.session`.
    public func completeFromPairing(
        outcome: PairingOrchestrator.JoinRecoveryOutcome
    ) async throws {
        try await repository.completeFromPairing(
            phraseSeed: outcome.phraseSeed,
            sessionToken: outcome.sessionToken,
            sessionExpiresAtMs: outcome.sessionExpiresAtMs
        )
    }

    /// Submit the recovery phrase. On success the auth state flips to
    /// ``AuthState/session(_:)`` and the surrounding router re-renders
    /// the protected content — no navigation needed here. On a typed
    /// mismatch / verification-unavailable, the error surfaces inline.
    public func onSubmit() {
        guard !busy else { return } // ignore double-tap
        if phrase.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            errorText = .key("recovery_phrase.error.incomplete")
            return
        }
        busy = true
        errorText = nil
        let phraseSnapshot = phrase
        Task {
            do {
                try await repository.provideRecoveryPhrase(
                    mnemonicPhrase: phraseSnapshot
                )
                busy = false
                errorText = nil
                phrase = ""
            } catch {
                busy = false
                // Overtaken by a sign-out or another sign-in: not a failure.
                if error is StaleBindingError { return }
                self.errorText = UiText { AuthErrorCopy.identityRestore(error) }
            }
        }
    }

    /// ADR-0017 lost-everything fallback — the user has NO recovery phrase
    /// and NO other device. Mint a brand-new identity via
    /// ``AuthRepository/mintFreshIdentity()``; the repository flips the auth
    /// state to `.saveFreshPhrase` and the router shows the new phrase. An
    /// error leaves an inline message.
    public func startFresh() {
        guard !busy else { return }
        busy = true
        errorText = nil
        Task {
            do {
                _ = try await repository.mintFreshIdentity()
                // Repository flipped to .saveFreshPhrase — clear busy.
                busy = false
                errorText = nil
            } catch {
                busy = false
                // Overtaken by a sign-out or another sign-in: not a failure.
                if error is StaleBindingError { return }
                NSLog("[restore] WARN fresh identity failed: \(error)")
                self.errorText = .key("restore_identity.lost.error.generic")
            }
        }
    }

    /// Sign out of the pending session (the user does not have their
    /// phrase). Returns to the anon surface.
    public func onSignOut() {
        repository.logout()
    }

    /// Drop a stale inline error (the screen switches method).
    public func clearError() {
        errorText = nil
    }
}
