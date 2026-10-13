// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// A guest chooses a username and a password (ADR-0075, pass A, part 2).
//
// A guest account (created by the call-link join) has no username and a
// random password nobody knows. "Choose a username" is the sign-up claim run
// later: the login handle at auth, the name in discovery, the name in the
// local account marker and the live session, then the account's FIRST
// password (set from the keys this device holds, with no proof block: there
// is no password anyone could prove), then the profile re-published with
// the username and its certificate.
//
// Pure: the step machine (name saved / password owed) and the entry-point
// rule. `UsernameClaimModel` drives them over injected ports.

public enum UsernameClaim {

    public enum Step: Equatable {
        /// Username, new password, confirm.
        case form
        /// The name is saved; the password is not set yet. Only the
        /// password is retried: the name is kept.
        case passwordOwed
        /// Both done: `username.claim.done`, once.
        case done
    }

    public enum Failure: Equatable {
        /// `409` at auth or discovery: `auth.signup.error.username_taken`.
        case taken
        /// `429 handle-bind-limited`.
        case bindLimited(retryAfterSecs: Int?)
        /// The username rule's own reason.
        case rule
        case offline
        case failed
        /// The name was saved; the password step failed.
        case passwordFailed
    }

    public struct State: Equatable {
        public var step: Step = .form
        public var failure: Failure?
        /// The canonical name once saved.
        public var savedName: String?

        public init() {}
    }

    /// Whether the claim is offered: only to an account with no username.
    public static func offered(username: String?) -> Bool {
        (username ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// Where the screen starts: an account whose name was saved in an
    /// earlier attempt but whose first password is still owed starts on the
    /// password step.
    public static func initial(username: String?, firstPasswordOwed: Bool) -> State {
        var state = State()
        if !offered(username: username), firstPasswordOwed {
            state.step = .passwordOwed
            state.savedName = username
        }
        return state
    }

    /// The name was claimed and saved.
    public static func nameSaved(_ state: State, canonical: String) -> State {
        var next = state
        next.step = .passwordOwed
        next.savedName = canonical
        next.failure = nil
        return next
    }

    /// The name was not claimed: nothing was saved, the form stays.
    public static func nameFailed(_ state: State, _ failure: Failure) -> State {
        var next = state
        next.step = .form
        next.failure = failure
        return next
    }

    /// The first password was set.
    public static func passwordSet(_ state: State) -> State {
        var next = state
        next.step = .done
        next.failure = nil
        return next
    }

    /// The password step failed after the name was saved: the name is kept
    /// and only the password is retried.
    public static func passwordFailed(_ state: State) -> State {
        var next = state
        next.step = .passwordOwed
        next.failure = .passwordFailed
        return next
    }

    /// Whether the username field is still editable.
    public static func nameEditable(_ state: State) -> Bool { state.step == .form }

    /// The line a failure shows.
    public static func failure(of error: Error) -> Failure {
        if error is UsernameTaken { return .taken }
        if let limited = error as? UsernameBindLimited {
            return .bindLimited(retryAfterSecs: limited.retryAfterSecs)
        }
        if error is UsernameRuleViolation { return .rule }
        if case .network = error as? AuthClientError { return .offline }
        if case .network = error as? DiscoveryClientError { return .offline }
        if case .http(409, _, _) = error as? AuthClientError { return .taken }
        if case .http(409, _, _) = error as? DiscoveryClientError { return .taken }
        return .failed
    }
}

/// "Choose a username", over injected ports.
@MainActor
final class UsernameClaimModel: ObservableObject {

    struct Ports {
        /// The local username rule's reason for a typed name, or `nil`.
        var ruleMessage: (String) -> String?
        /// Bind the handle, register the name, save it. Returns the
        /// canonical name.
        var claimName: @MainActor (_ typed: String) async throws -> String
        /// Set the account's first password.
        var setPassword: @MainActor (_ newPassword: Data) async throws -> Void
    }

    @Published var username = ""
    @Published var password = ""
    @Published var confirm = ""
    @Published private(set) var state: UsernameClaim.State
    @Published private(set) var busy = false
    /// The password rule's refusal, before anything is sent.
    @Published private(set) var passwordError: UiText?

    private let ports: Ports

    init(ports: Ports, username: String?, firstPasswordOwed: Bool) {
        self.ports = ports
        self.state = UsernameClaim.initial(username: username, firstPasswordOwed: firstPasswordOwed)
    }

    /// The username rule's reason while typing, or `nil`.
    var usernameHint: String? {
        UsernameClaim.nameEditable(state) ? ports.ruleMessage(username) : nil
    }

    var canSubmit: Bool {
        guard !busy, !password.isEmpty, !confirm.isEmpty else { return false }
        if UsernameClaim.nameEditable(state) {
            return !username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && usernameHint == nil
        }
        return true
    }

    /// The main button: the name (unless already saved), then the password.
    func submit() async {
        guard !busy, state.step != .done else { return }
        passwordError = nil
        // Validate everything locally before the first request.
        switch NewPasswordRule.refusal(password: password, confirm: confirm) {
        case .mismatch?:
            passwordError = .key("auth.password.error.mismatch")
            return
        case .tooShort?:
            passwordError = .key(
                "auth.password.error.too_short", counts: ["count": UInt64(NewPasswordRule.minLength)])
            return
        case nil:
            break
        }
        if UsernameClaim.nameEditable(state), ports.ruleMessage(username) != nil {
            state = UsernameClaim.nameFailed(state, .rule)
            return
        }
        busy = true
        defer { busy = false }
        if UsernameClaim.nameEditable(state) {
            do {
                let canonical = try await ports.claimName(username)
                state = UsernameClaim.nameSaved(state, canonical: canonical)
            } catch {
                state = UsernameClaim.nameFailed(state, UsernameClaim.failure(of: error))
                return
            }
        }
        do {
            try await ports.setPassword(Data(password.utf8))
            password = ""
            confirm = ""
            state = UsernameClaim.passwordSet(state)
        } catch {
            NSLog("[auth] WARN the first password was not set; the username is kept")
            state = UsernameClaim.passwordFailed(state)
        }
    }
}
