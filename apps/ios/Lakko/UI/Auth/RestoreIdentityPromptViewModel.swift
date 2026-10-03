// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// State + dispatcher for ``RestoreIdentityPromptScreen``: "Restore your
/// keys".
///
/// Mirror of `apps/web/src/auth/RestoreIdentityPrompt.tsx`. Rendered only
/// while the auth state is ``AuthState/needsRecoveryPhrase(_:)``: the person
/// is signed in (a password, a passkey, an authenticator code) on a device
/// that does not hold the account's keys. The identity is never derived from
/// the password and never minted silently (either would break every
/// contact's pin); the keys come from one of three routes, in this order:
///
///   1. another device that is still signed in;
///   2. a passkey that carries the keys (the row is hidden when the account
///      has none);
///   3. the 12-word recovery phrase, checked fail-closed against the
///      account's published identity.
///
/// Never three words or the authenticator: those only sign in, and the
/// person is already signed in. A person with none of the three can start
/// over with a new identity.
@MainActor
public final class RestoreIdentityPromptViewModel: ObservableObject {

    /// The routes of the prompt.
    public enum Method: String, Hashable, CaseIterable {
        case device
        case passkey
        case phrase
    }

    @Published public var phrase: String = ""
    @Published public private(set) var busy: Bool = false

    /// UX 15.1 — the typed confirmation of "Reset account keys". The button
    /// is enabled only while it matches ``KeyResetConfirmation/word``.
    @Published public var resetConfirmation: String = ""
    /// `true` from the tap on "Reset account keys" until the server step
    /// answered: the full-screen "Resetting..." with no cancel.
    @Published public private(set) var resetting: Bool = false
    /// A retry wait the reset's `429` asked for, in seconds; `nil` otherwise.
    @Published public private(set) var resetRetryAfterSecs: Int?
    /// Kept as ``UiText`` so a message on screen follows a live language switch.
    @Published private var errorText: UiText?
    public var error: String? { errorText?.text }

    /// How many of the account's passkeys carry keys; `nil` while it is not
    /// known, which keeps the passkey row.
    @Published public private(set) var passkeysWithKeys: Int?

    /// The username typed for the another-device route when the signed-in
    /// account has none on this device (a passkey sign-in names no account).
    @Published public var typedUsername: String = ""
    /// The typed username once confirmed with Continue: the pairing request
    /// is made for this name, not for every keystroke.
    @Published public private(set) var confirmedTypedUsername: String?

    /// Username for the account being restored; empty when the sign-in did
    /// not name one.
    public let username: String

    /// Base64 account id for the restore-via-device panel.
    public let accountIdB64: String

    /// Raw 16-byte account id decoded from ``accountIdB64``; `nil`
    /// only on a malformed stored marker.
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

    /// The rows for this device and account, in display order. Passkeys are
    /// supported on every iOS version this app runs on, so the passkey row
    /// depends only on whether the account has a passkey that carries keys.
    public var methods: [Method] {
        Method.allCases.filter { method in
            method != .passkey
                || PasskeyVault.restoreOffersPasskey(
                    passkeysSupported: true, passkeysWithKeys: passkeysWithKeys)
        }
    }

    /// Ask which of the account's passkeys carry keys (with the parked
    /// session token).
    public func loadPasskeysWithKeys() async {
        passkeysWithKeys = await repository.passkeysWithKeysForPendingRestore()
    }

    /// The login handle the another-device route names the account by: the
    /// account's own username, else the one typed here. `nil` until a name
    /// that derives a handle is available.
    public var pairingLoginHandle: Data? {
        let name = username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? (confirmedTypedUsername ?? "") : username
        return ownLoginHandle(username: name)
    }

    /// Continue on the typed-username field of the another-device route.
    public func confirmTypedUsername() {
        guard ownLoginHandle(username: typedUsername) != nil else {
            errorText = UiText {
                checkUsername(self.typedUsername).inlineMessage
                    ?? Localization.text("recovery.username_required")
            }
            return
        }
        errorText = nil
        confirmedTypedUsername = typedUsername
    }

    /// Finish the pending restore with what ANOTHER DEVICE handed over (the
    /// phrase entropy, recovery payload v2) instead of a typed phrase. Async
    /// passthrough to
    /// ``AuthRepository/completeFromPairing(phraseEntropy:session:)``;
    /// verification failures (`PhraseMismatch` /
    /// `IdentityVerificationUnavailable`) propagate to the calling panel,
    /// which surfaces them inline. On success the repository flips the auth
    /// state to `.session`. The entropy is zeroed in the outcome either way.
    public func completeFromPairing(
        outcome: PairingOrchestrator.JoinRecoveryOutcome
    ) async throws {
        defer { outcome.wipeEntropy() }
        try await repository.completeFromPairing(
            phraseEntropy: outcome.phraseEntropy,
            session: outcome.session
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
                self.errorText = UiText { AuthErrorCopy.identityRestore(error) }
            }
        }
    }

    /// The passkey row: run the ceremony for the signed-in account and ask
    /// the passkey for the keys. `true` when the row's form should close
    /// (the passkey carried no usable keys: the list comes back with the
    /// notice on top, and the count is re-read so a passkey that carried
    /// none is no longer offered).
    public func restoreWithPasskey() async -> Bool {
        guard !busy else { return false }
        busy = true
        errorText = nil
        defer { busy = false }
        do {
            let outcome = try await repository.restoreKeysWithPasskey()
            if outcome == .restored { return false }
            await loadPasskeysWithKeys()
            return true
        } catch {
            // Dismissing the sheet is not an error: back to idle.
            if !AuthErrorCopy.isPasskeyCancelled(error) {
                self.errorText = UiText { AuthErrorCopy.passkey(error) }
            }
            return false
        }
    }

    /// Whether the typed confirmation enables "Reset account keys".
    public var resetConfirmed: Bool {
        KeyResetConfirmation.matches(resetConfirmation)
    }

    /// The person has NO other device, NO passkey that restores keys and NO
    /// phrase, and typed the confirmation word. Reset the account keys via
    /// ``AuthRepository/mintFreshIdentity()``; the repository flips the auth
    /// state to `.saveFreshPhrase` and the router shows the new phrase (or
    /// its "could not finish publishing" form). A failure BEFORE anything
    /// changed leaves an inline message under the button (UX 15.1): the
    /// `429` line with its wait, the offline line, or the generic one.
    public func startFresh() {
        guard !busy, resetConfirmed else { return }
        busy = true
        resetting = true
        errorText = nil
        resetRetryAfterSecs = nil
        Task {
            do {
                _ = try await repository.mintFreshIdentity()
                // Repository flipped to .saveFreshPhrase — clear busy.
                busy = false
                resetting = false
                errorText = nil
            } catch {
                busy = false
                resetting = false
                NSLog("[restore] WARN key reset failed: \(error)")
                if let secs = RetryCountdown.attemptsRetryAfterSecs(error) {
                    resetRetryAfterSecs = secs
                    errorText = nil
                } else if case .network = error as? AuthClientError {
                    self.errorText = .key("restore_identity.reset.error.offline")
                } else {
                    self.errorText = .key("restore_identity.reset.error.generic")
                }
            }
        }
    }

    /// Sign out of the pending session. Returns to the anon surface.
    public func onSignOut() {
        repository.logout()
    }

    /// Drop a stale inline error (the screen switches method).
    public func clearError() {
        errorText = nil
    }
}

/// UX 15.1 — the word typed to confirm a key reset. The same Latin letters
/// in every language (like the slot numbers of the grid); compared after
/// trimming, case-sensitive.
enum KeyResetConfirmation {
    static let word = "RESET"

    static func matches(_ typed: String) -> Bool {
        typed.trimmingCharacters(in: .whitespacesAndNewlines) == word
    }
}
