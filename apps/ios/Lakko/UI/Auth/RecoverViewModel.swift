// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// State + dispatcher for ``RecoverScreen``.
///
/// Mirror of:
///
///   * `apps/web/src/routes/Recover.tsx`'s local signals (1.17d8)
///   * `apps/android/.../ui/auth/RecoverViewModel.kt` (1.15d6)
///
/// Single-stage form (mnemonic + new password + confirm). On submit
/// drives ``AuthRepository/recover(mnemonicPhrase:newPassword:)``,
/// which rotates the password envelope on the server, re-derives
/// the device root key from the new OPAQUE export key, and flips
/// the auth state.
///
/// The ``cachedUsername`` constructor arg drives the "Account on
/// this device: @alice" line in the UI. When it's `nil` the screen
/// renders a "no cached account" stub instead (the recovery flow
/// can't run without an `accountId` on disk; that's a 1.14e /
/// discovery-service problem).
@MainActor
public final class RecoverViewModel: ObservableObject {

    // MARK: - Form fields

    @Published public var phrase: String = ""
    @Published public var password: String = ""
    @Published public var confirm: String = ""

    // MARK: - Internal status

    @Published public private(set) var busy: Bool = false
    @Published public private(set) var error: String?

    // MARK: - Read-only inputs

    /// Cached username for the on-device account, surfaced in the
    /// "Account on this device: @alice" line. When `nil` the screen
    /// renders a "no cached account" stub.
    public let cachedUsername: String?

    // MARK: - Wiring

    private let repository: AuthRepository

    public init(repository: AuthRepository, cachedUsername: String?) {
        self.repository = repository
        self.cachedUsername = cachedUsername
    }

    // MARK: - Submit

    /// Submit the recover form.
    ///
    /// Validates locally (new password ≥ 8, confirm matches, phrase
    /// non-empty), drives
    /// ``AuthRepository/recover(mnemonicPhrase:newPassword:)``,
    /// surfaces success as an ``AuthState/session(_:)`` transition
    /// (which the app-level router catches via `@Published`
    /// observation), and surfaces failures as inline form messages.
    ///
    /// The mnemonic-parse failure (bad checksum / unknown word)
    /// collapses to one user-facing message — same posture as the
    /// web + Android sides, preserves BIP39's no-probing property.
    public func onSubmit() {
        guard !busy else { return } // ignore double-tap

        if password != confirm {
            error = "Passwords do not match."
            return
        }
        if password.count < Self.minPasswordLength {
            error = "Password must be at least \(Self.minPasswordLength) characters."
            return
        }
        if phrase.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            error = "Recovery phrase must be non-empty."
            return
        }

        busy = true
        error = nil

        let phraseSnapshot = phrase
        let passwordBytes = Data(password.utf8)

        Task {
            do {
                try await repository.recover(
                    mnemonicPhrase: phraseSnapshot,
                    newPassword: passwordBytes
                )
                // `repository.recover` flipped AuthState to .session.
                // The screen will be replaced by the home destination
                // on the next render; wipe textfield buffers so they
                // don't hold the phrase / password past the
                // navigation.
                busy = false
                error = nil
                phrase = ""
                password = ""
                confirm = ""
            } catch let err as FfiError {
                // BIP39 mnemonic parse failure. UniFFI surfaces both
                // unknown-word and bad-checksum cases through the
                // single `Mnemonic` variant of `FfiError` — by
                // design (preserves the no-probing property). Swift
                // case names are PascalCase per the UniFFI codegen
                // shape (the Kotlin side uses `FfiException` with
                // PascalCase sub-classes — same vocabulary, different
                // surface).
                if case .Mnemonic = err {
                    busy = false
                    error = "Invalid recovery phrase. Check the spelling and order of all 24 words."
                } else {
                    busy = false
                    self.error = err.localizedDescription
                }
            } catch let err as AuthClientError {
                busy = false
                error = userFacing(err)
            } catch {
                busy = false
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Recovery failed."
            }
        }
    }

    // MARK: - Error mapping

    /// Map server-side HTTP errors to user-facing copy. The
    /// `/password/reset/{start,finish}` pair returns 401 on a wrong
    /// mnemonic (the OPAQUE recovery login fails at AKE
    /// finalisation). Surface generic copy — wrong-mnemonic and
    /// wrong-account collapse into the same response by design.
    private func userFacing(_ err: AuthClientError) -> String {
        switch err {
        case .argument(let message):
            return message
        case .http(let status, _, _):
            switch status {
            case 401:
                return "Invalid recovery phrase."
            case 429:
                return "Too many attempts. Please wait a moment and retry."
            case 500...599:
                return "Server error. Please try again shortly."
            default:
                return "Recovery failed (\(status))."
            }
        case .network:
            return "Network error. Check your connection and try again."
        }
    }

    // MARK: - Constants

    /// Mirror of ``SignupViewModel/minPasswordLength``. The recovery
    /// flow sets a **new** password so the same floor applies.
    public static let minPasswordLength: Int = 8
}
