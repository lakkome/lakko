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
    @Published public private(set) var error: String?

    /// Username for the account being restored, surfaced in the
    /// "...for @alice" line.
    public let username: String

    private let repository: AuthRepository

    public init(repository: AuthRepository, username: String) {
        self.repository = repository
        self.username = username
    }

    /// Submit the recovery phrase. On success the auth state flips to
    /// ``AuthState/session(_:)`` and the surrounding router re-renders
    /// the protected content — no navigation needed here. On a typed
    /// mismatch / verification-unavailable, the error surfaces inline.
    public func onSubmit() {
        guard !busy else { return } // ignore double-tap
        if phrase.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            error = "Recovery phrase must be non-empty."
            return
        }
        busy = true
        error = nil
        let phraseSnapshot = phrase
        Task {
            do {
                try await repository.provideRecoveryPhrase(
                    mnemonicPhrase: phraseSnapshot
                )
                busy = false
                error = nil
                phrase = ""
            } catch let err as FfiError {
                // BIP39 mnemonic parse failure — unknown-word and
                // bad-checksum collapse into one message by design.
                busy = false
                if case .Mnemonic = err {
                    error = "Invalid recovery phrase. Check the spelling " +
                        "and order of all 24 words."
                } else {
                    error = err.localizedDescription
                }
            } catch let err as PhraseMismatch {
                busy = false
                error = err.errorDescription
            } catch let err as IdentityVerificationUnavailable {
                busy = false
                error = err.errorDescription
            } catch let err as AuthClientError {
                busy = false
                error = (err as LocalizedError).errorDescription ?? "Restore failed."
            } catch {
                busy = false
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Restore failed."
            }
        }
    }

    /// Sign out of the pending session (the user does not have their
    /// phrase). Returns to the anon surface.
    public func onSignOut() {
        repository.logout()
    }
}
