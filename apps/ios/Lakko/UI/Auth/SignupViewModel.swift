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
    /// ADR-0054 S4 — the last attempt was refused by the sign-up limit
    /// (`429 signup-rate-limited`): the screen offers `Try again`, the form
    /// keeps its contents.
    @Published public private(set) var rateLimited: Bool = false
    /// Whether the working state still offers `Cancel`. `false` once the
    /// account is being created (right before `/signup/finish`).
    @Published public private(set) var cancellable: Bool = false

    /// The sign-up in flight, cancelled by ``onCancel()`` and when the screen
    /// goes away.
    private var signupTask: Task<Void, Never>?
    /// Bumped per submit and per cancel, so a cancelled attempt that returns
    /// late never touches the state of the next one.
    private var attempt = 0

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
        cancellable = true
        error = nil
        rateLimited = false

        let usernameSnapshot = username
        let displayNameSnapshot = displayName
        let passwordBytes = Data(password.utf8)

        attempt += 1
        let thisAttempt = attempt
        signupTask = Task {
            do {
                let outcome = try await repository.signup(
                    username: usernameSnapshot,
                    password: passwordBytes,
                    onCommitting: {
                        if self.attempt == thisAttempt { self.cancellable = false }
                    }
                )
                cancellable = false
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
            } catch {
                guard attempt == thisAttempt else { return }
                cancellable = false
                busy = false
                // `Cancel` or leaving the screen: back to the filled-in form,
                // no message. A cancelled network call surfaces as a network
                // error, so the task's flag decides, not the error type.
                if Task.isCancelled || error is CancellationError { return }
                switch error {
                case let err as AuthClientError:
                    if let refusal = AuthRateLimit.refusal(in: err),
                       refusal.code == AuthRateLimit.signupRateLimited {
                        rateLimited = true
                    }
                    self.error = userFacing(err)
                case let err as DiscoveryClientError:
                    self.error = err.authFormMessage
                default:
                    self.error = (error as? LocalizedError)?.errorDescription
                        ?? "Could not create the account. Try again."
                }
            }
        }
    }

    /// `Cancel` in the working state (and leaving the screen): stop the
    /// proof of work and return to the filled-in form. Ignored once the
    /// account is being created.
    public func onCancel() {
        guard busy, cancellable else { return }
        attempt += 1
        signupTask?.cancel()
        signupTask = nil
        cancellable = false
        busy = false
        error = nil
    }

    /// `Try again` after the sign-up limit: the same form, submitted again.
    public func onTryAgain() {
        rateLimited = false
        onSubmit()
    }

    /// The working-state copy (`docs/ux/anti-abuse.md` section 1).
    public static let workingTitle = "Creating your account..."
    public static let workingNote = "This takes a little while, once. You will not wait like this again."

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
        if let limit = AuthRateLimit.copy(for: err) { return limit }
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
