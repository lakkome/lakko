// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// State + dispatcher for ``SignupScreen``.
///
/// Mirror of:
///
///   * `apps/web/src/routes/Signup.tsx`'s `createSignal` collection
///     (1.17d6)
///   * `apps/android/.../ui/auth/SignupViewModel.kt` (1.15d4)
///
/// Same fields, same transitions, same two-stage shape
/// (`form` → `phrase`).
///
/// The two-stage local state machine is intentional. Splitting
/// signup from "save your recovery phrase" guarantees the user sees
/// the 24 words exactly once and explicitly acknowledges having
/// written them down before navigating into the authenticated app.
/// Re-display is impossible by construction — the `.phrase`
/// associated value holds the only in-memory reference and is
/// dropped on ``onAcknowledge()``.
@MainActor
public final class SignupViewModel: ObservableObject {

    // MARK: - Form fields

    @Published public var username: String = ""
    @Published public var password: String = ""
    @Published public var confirm: String = ""
    @Published public var acknowledged: Bool = false

    // MARK: - Internal status

    @Published public private(set) var busy: Bool = false
    @Published public private(set) var error: String?

    /// Two-stage signup state machine. ``Stage/form`` collects the
    /// username + password; ``Stage/phrase(_:)`` displays the BIP39
    /// mnemonic post-signup.
    @Published public private(set) var stage: Stage = .form

    // MARK: - Wiring

    private let repository: AuthRepository

    public init(repository: AuthRepository) {
        self.repository = repository
    }

    // MARK: - Submit

    /// Submit the signup form.
    ///
    /// Validates locally (password length, confirm match), drives
    /// ``AuthRepository/signup(username:password:)``, transitions
    /// ``stage`` to ``Stage/phrase(_:)`` on success. Errors surface
    /// in ``error`` as inline form messages. The password fields
    /// are cleared on success so the textfields don't keep the
    /// bytes alive longer than necessary — same posture as the
    /// Compose + web sides.
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

        busy = true
        error = nil

        let usernameSnapshot = username
        let passwordBytes = Data(password.utf8)

        Task {
            defer {
                // SwiftUI `@Published` writes hop to the main actor
                // automatically; the buffer reset is local-state
                // mechanics, not security-sensitive on its own — the
                // copy that crossed the FFI boundary is the one we
                // care about (see `AuthRepository.signup`).
            }
            do {
                let outcome = try await repository.signup(
                    username: usernameSnapshot,
                    password: passwordBytes
                )
                busy = false
                error = nil
                password = ""
                confirm = ""
                stage = .phrase(outcome.recoveryPhrase)
            } catch let err as AuthClientError {
                busy = false
                error = userFacing(err)
            } catch {
                busy = false
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Signup failed."
            }
        }
    }

    /// Called when the user has confirmed they wrote the recovery
    /// phrase down. Drops the in-memory phrase reference + signals
    /// the screen to navigate forward. The auth state was already
    /// flipped to ``AuthState/session(_:)`` inside
    /// ``AuthRepository/signup(username:password:)``; the
    /// app-level router will snap to the home destination
    /// automatically once we clear the local stage.
    public func onAcknowledge() {
        stage = .form
        acknowledged = false
    }

    // MARK: - Error mapping

    /// Map an ``AuthClientError`` to a user-facing message. Today's
    /// auth service distinguishes a couple of cases by status code;
    /// future cases get added here as the service surface evolves.
    /// Indistinguishable-failure cases (login 401) stay generic on
    /// purpose.
    private func userFacing(_ err: AuthClientError) -> String {
        switch err {
        case .argument(let message):
            return message
        case .http(let status, _, _):
            switch status {
            case 409:
                return "An account with that username already exists."
            case 429:
                return "Too many requests. Please wait a moment and retry."
            case 500...599:
                return "Server error. Please try again shortly."
            default:
                return "Signup failed (\(status))."
            }
        case .network:
            return "Network error. Check your connection and try again."
        }
    }

    // MARK: - Constants

    /// Minimum password length enforced client-side. Mirrors the
    /// floor in `apps/web` + `apps/android` — a real strength
    /// heuristic + Argon2id KSF cost feedback lands in a later
    /// sprint.
    public static let minPasswordLength: Int = 8
}

/// Two-stage signup state machine.
public enum Stage: Equatable {
    /// Collect the username + password.
    case form
    /// Display the freshly-generated 24-word BIP39 mnemonic.
    case phrase(String)
}
