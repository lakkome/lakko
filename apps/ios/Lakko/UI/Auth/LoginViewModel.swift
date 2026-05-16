// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// State + dispatcher for ``LoginScreen``.
///
/// Mirror of:
///
///   * `apps/web/src/routes/Login.tsx`'s local signals (1.17d7)
///   * `apps/android/.../ui/auth/LoginViewModel.kt` (1.15d5)
///
/// Simpler than ``SignupViewModel`` — single-stage form, no
/// recovery-phrase post-step.
///
/// The ``init(repository:cachedUsername:)`` `cachedUsername` arg
/// pre-fills the username field from
/// ``AuthState/anon(cachedUsername:)``. On a fresh device (no
/// cache) it's `nil` and the user types the handle from scratch.
@MainActor
public final class LoginViewModel: ObservableObject {

    // MARK: - Form fields

    @Published public var username: String
    @Published public var password: String = ""

    // MARK: - Internal status

    @Published public private(set) var busy: Bool = false
    @Published public private(set) var error: String?

    // MARK: - Wiring

    private let repository: AuthRepository

    public init(repository: AuthRepository, cachedUsername: String? = nil) {
        self.repository = repository
        self.username = cachedUsername ?? ""
    }

    // MARK: - Submit

    /// Submit the login form. Drives
    /// ``AuthRepository/login(username:password:)``, surfaces
    /// success as an ``AuthState/session(_:)`` transition (which
    /// the app-level router catches via `@Published` observation),
    /// and surfaces failures as inline form messages.
    ///
    /// Wrong-password / unknown-username collapse to the same
    /// generic copy — by design (OPAQUE indistinguishable failure
    /// + the same posture as the web side).
    public func onSubmit() {
        guard !busy else { return } // ignore double-tap

        busy = true
        error = nil

        let usernameSnapshot = username
        let passwordBytes = Data(password.utf8)

        Task {
            do {
                try await repository.login(
                    username: usernameSnapshot,
                    password: passwordBytes
                )
                // `repository.login` flipped the AuthState; the screen
                // will be replaced by the home destination on the
                // next SwiftUI render. Clear the password field so
                // the TextField buffer doesn't hold it past the
                // navigation.
                busy = false
                error = nil
                password = ""
            } catch let err as AuthClientError {
                busy = false
                error = userFacing(err)
            } catch {
                busy = false
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Login failed."
            }
        }
    }

    /// Phase 2a2c2-ios — drive a passkey-only login. Mirrors
    /// ``onSubmit`` but skips the password path: tap →
    /// ASAuthorizationController sheet → server /webauthn/auth
    /// round-trip → state flip. Cache miss / cross-account
    /// failures surface as inline form messages.
    @MainActor
    public func onPasskeyLogin() {
        guard !busy else { return }
        busy = true
        error = nil
        Task {
            do {
                try await repository.loginWithPasskey()
                busy = false
                error = nil
            } catch let err as AuthClientError {
                busy = false
                error = userFacing(err)
            } catch let err as WebauthnAssertion.AssertionError {
                busy = false
                error = err.errorDescription
            } catch {
                busy = false
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Passkey sign-in failed."
            }
        }
    }

    // MARK: - Error mapping

    /// Map server-side HTTP errors to user-facing copy. 401 stays
    /// generic on purpose — wrong password and unknown account are
    /// intentionally indistinguishable (OPAQUE's no-probing
    /// property + the username-resolution layer can't leak account
    /// existence).
    private func userFacing(_ err: AuthClientError) -> String {
        switch err {
        case .argument(let message):
            return message
        case .http(let status, _, _):
            switch status {
            case 401:
                return "Invalid credentials."
            case 429:
                return "Too many attempts. Please wait a moment and retry."
            case 500...599:
                return "Server error. Please try again shortly."
            default:
                return "Login failed (\(status))."
            }
        case .network:
            return "Network error. Check your connection and try again."
        }
    }
}
