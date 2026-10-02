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
    /// ``AuthRepository/completeFromPairing(phraseEntropy:sessionToken:sessionExpiresAtMs:)``;
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

    /// The person has NO other device, NO passkey that restores keys and NO
    /// phrase. Mint a brand-new identity via
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
                NSLog("[restore] WARN fresh identity failed: \(error)")
                self.errorText = UiText {
                    AuthErrorCopy.shared(error) ?? Localization.text("restore_identity.lost.error.generic")
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
