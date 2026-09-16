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
    /// ADR-0048 — the optional "Your name" field. Blank means others see the
    /// username.
    @Published public var displayName: String = ""
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

    /// Called once with the 24-word phrase right after a successful sign-up.
    /// The repository lands the session inside `signup`, which replaces the
    /// anonymous stack (and this view model's screen) at the next render, so
    /// the phrase step cannot live here; the app root receives it through
    /// this hook and shows ``SignupPhraseScreen`` over the session.
    public var onPhraseReady: ((String) -> Void)?

    public init(repository: AuthRepository, onPhraseReady: ((String) -> Void)? = nil) {
        self.repository = repository
        self.onPhraseReady = onPhraseReady
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

        // ADR-0048 §4 — the username rule, checked as the user typed and again
        // here, before anything reaches the auth service.
        switch checkUsername(username) {
        case .empty:
            error = "Choose a username first."
            return
        case .invalid(let reason):
            error = reason.message
            return
        case .valid:
            break
        }
        if let nameError = displayNameError {
            error = nameError
            return
        }
        if password != confirm {
            error = "The passwords do not match. Type the same password twice."
            return
        }
        if password.count < Self.minPasswordLength {
            error = "Use at least \(Self.minPasswordLength) characters for the password."
            return
        }

        busy = true
        error = nil

        let usernameSnapshot = username
        let displayNameSnapshot = displayName
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
                saveDisplayName(displayNameSnapshot)
                if let onPhraseReady {
                    // The owner shows the phrase step; keep no second copy here.
                    onPhraseReady(outcome.recoveryPhrase)
                } else {
                    stage = .phrase(outcome.recoveryPhrase)
                }
            } catch let err as AuthClientError {
                busy = false
                error = userFacing(err)
            } catch let err as DiscoveryClientError {
                busy = false
                error = err.authFormMessage
            } catch {
                busy = false
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Could not create the account. Try again."
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

    // MARK: - ADR-0048 profile name

    /// The inline reason under the username field, or `nil`.
    public var usernameHint: String? {
        checkUsername(username).inlineMessage
    }

    /// The inline reason under the name field, or `nil` when the name is fine
    /// (blank included — the field is optional).
    public var displayNameError: String? {
        do {
            _ = try checkDisplayName(displayName)
            return nil
        } catch {
            return (error as? LocalizedError)?.errorDescription ?? "That name cannot be used."
        }
    }

    /// Store the name typed at signup as the new account's own profile. The
    /// session has just landed, so the account id is known. Best-effort: a
    /// name is optional, and Settings -> Profile can set it later.
    private func saveDisplayName(_ input: String) {
        guard let accountId = repository.identitySecrets?.accountId else { return }
        let normalized: String?
        do { normalized = try checkDisplayName(input) } catch { return }
        guard normalized != nil else { return }
        do {
            try ProfileDirectory.shared.setOwnDisplayName(input, accountIdB64: base64Encode(accountId))
        } catch {
            NSLog("ADR-0048 signup name not saved: \(error)")
        }
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
                return "That username is taken. Choose another one."
            case 429:
                return "Too many attempts. Wait a moment, then try again."
            case 500...599:
                return "Lakko is having trouble right now. Try again in a moment."
            default:
                return "Could not create the account. Try again."
            }
        case .network:
            return "No connection. Check your network and try again."
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
