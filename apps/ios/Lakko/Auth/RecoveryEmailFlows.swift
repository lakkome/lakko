// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0075 / docs/ux/identity-recovery.md sections 19.2 and 19.3 — the two
// flows of the recovery email, as pure step machines:
//
//   * `EmailEnrol`    adding or replacing the address (signed in);
//   * `EmailRecover`  getting back in with a mailed code (anonymous).
//
// Each machine takes what a route answered and gives the next state. The
// models that drive them over the network are `RecoveryEmailModel` and
// `EmailRecoverModel`. Neither machine nor model keeps the address past the
// surface it was typed on, and nothing here logs it or a code.

/// The one line a recovery-email surface shows under its field.
public enum RecoveryEmailLine: Equatable {
    /// "Type a full email address."
    case addressInvalid
    /// "That code is not right." A wrong code and a code never sent read
    /// the same.
    case codeWrong
    /// "That code has expired."
    case codeExpired
    /// "Too many attempts. Send a new code."
    case codeUsedUp
    /// `recovery.error.rate_limited` with the countdown, or the plain line
    /// when the server named no wait.
    case limited(retryAfterSecs: Int?)
    /// "We could not send the letter. Try again later."
    case sendFailed
    /// "Email codes are not available right now."
    case unavailable
    /// `protect.last_factor`: the email is the account's last second way in.
    case lastFactor
    /// "Choose a username first."
    case needsUsername
    case offline
    case failed

    /// The catalog key, or `nil` for the countdown (drawn by `WaitNotice`).
    public var key: String? {
        switch self {
        case .addressInvalid: return "recovery_email.invalid"
        case .codeWrong: return "recovery_email.code.wrong"
        case .codeExpired: return "recovery_email.code.expired"
        case .codeUsedUp: return "recovery_email.code.used_up"
        case .limited(let secs): return (secs ?? 0) > 0 ? nil : "common.error.too_many_attempts"
        case .sendFailed: return "recovery_email.send_failed"
        case .unavailable: return "recovery_email.unavailable"
        case .lastFactor: return LastFactor.messageKey
        case .needsUsername: return "recovery_email.needs_username"
        case .offline: return "common.error.offline"
        case .failed: return "common.error.unavailable"
        }
    }
}

// MARK: - Enrol

/// Adding or replacing the address (UX 19.2), pure.
public enum EmailEnrol {

    public enum Step: Equatable {
        /// The address field and `Send code`.
        case address
        /// "Enter the code".
        case code
        /// The code was accepted.
        case done
    }

    public struct State: Equatable {
        public var step: Step = .address
        public var line: RecoveryEmailLine?
        /// Wrong codes typed for the letter in hand.
        public var wrongCodes = 0
        /// The proof block must appear (`403 fresh-auth-required`).
        public var proofNeeded = false

        public init() {}
    }

    /// Tries per letter (the server's count; kept here so the surface goes
    /// back to the address step when they are used up).
    public static let triesPerCode = 5

    /// `enrol/start` answered a challenge: the code step.
    public static func started(_ state: State) -> State {
        var next = State()
        next.step = .code
        _ = state
        return next
    }

    /// `enrol/start` (or `Send again`) was refused. The step stays.
    public static func startRefused(_ state: State, _ refusal: RecoveryEmailRefusal) -> State {
        var next = state
        next.proofNeeded = false
        switch refusal {
        case .freshAuthRequired:
            // Nothing went wrong: the proof ran out. No error line.
            next.line = nil
            next.proofNeeded = true
        case .addressInvalid: next.line = .addressInvalid
        case .lettersLimited(let secs), .tooManyAttempts(let secs): next.line = .limited(retryAfterSecs: secs)
        case .mailSendFailed: next.line = .sendFailed
        case .unavailable: next.line = .unavailable
        case .usernameRequired, .handleMismatch: next.line = .needsUsername
        case .lastFactor: next.line = .lastFactor
        case .offline: next.line = .offline
        default: next.line = .failed
        }
        return next
    }

    /// `enrol/finish` accepted the code.
    public static func finished(_ state: State) -> State {
        var next = State()
        next.step = .done
        _ = state
        return next
    }

    /// `enrol/finish` was refused.
    public static func finishRefused(_ state: State, _ refusal: RecoveryEmailRefusal) -> State {
        var next = state
        switch refusal {
        case .codeRefused:
            next.wrongCodes += 1
            if next.wrongCodes >= triesPerCode {
                // The letter is used up: back to the address step.
                next = State()
                next.line = .codeUsedUp
            } else {
                next.line = .codeWrong
            }
        case .tooManyAttempts:
            next = State()
            next.line = .codeUsedUp
        case .codeExpired, .ticketRefused:
            next.line = .codeExpired
        case .freshAuthRequired:
            next.line = nil
            next.proofNeeded = true
        case .offline: next.line = .offline
        default: next.line = .failed
        }
        return next
    }

    /// `Use a different address`.
    public static func otherAddress(_ state: State) -> State {
        _ = state
        return State()
    }

    /// Whether `Send code` is enabled: the address parses, nothing is
    /// running, no wait is counting down, and the proof block (when drawn)
    /// has what it needs.
    public static func canSend(addressValid: Bool, busy: Bool, waiting: Bool, proofReady: Bool) -> Bool {
        addressValid && !busy && !waiting && proofReady
    }

    /// Six digits, or `nil`.
    public static func normalizedCode(_ typed: String) -> String? {
        let digits = typed.filter { !$0.isWhitespace }
        guard digits.count == 6, digits.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
        return digits
    }
}

// MARK: - Recover

/// Getting back in with a mailed code (UX 19.3), pure.
///
/// The order is fixed: start, the code, the authenticator code (when the
/// account has one), then the NEW PASSWORD. A mailed code gives a new
/// password and nothing else (ADR-0075, amended 2026-11-26: the email vault
/// is gone, so there is no `recover/open` and no account kind to tell
/// apart); on a device without the keys the session then lands on "Restore
/// your keys".
public enum EmailRecover {

    public enum Step: Equatable {
        /// Step 1: username and address.
        case start
        /// Step 2, always, whatever was typed: "Check your email".
        case code
        /// Step 3, only when the account has an authenticator.
        case totp
        /// Step 4: "Choose a new password".
        case newPassword
        /// The new password is being set (the password routes under the
        /// ticket).
        case restoring
    }

    public struct State: Equatable {
        public var step: Step = .start
        public var line: RecoveryEmailLine?
        public init() {}
    }

    /// `recover/start` answered. The SAME transition for a matching pair, a
    /// wrong pair, an unknown username and an account with no email: the
    /// answer carries nothing that could tell them apart, and this function
    /// takes nothing that could either.
    public static func started(_ state: State) -> State {
        var next = State()
        next.step = .code
        _ = state
        return next
    }

    /// `recover/start` (or `Send again`) was refused: for what was typed
    /// (its shape, the rate), never for whose it is. The step stays.
    public static func startRefused(_ state: State, _ refusal: RecoveryEmailRefusal) -> State {
        var next = state
        switch refusal {
        case .addressInvalid: next.line = .addressInvalid
        case .lettersLimited(let secs), .tooManyAttempts(let secs): next.line = .limited(retryAfterSecs: secs)
        case .unavailable: next.line = .unavailable
        case .offline: next.line = .offline
        default: next.line = .failed
        }
        return next
    }

    /// `recover/verify` accepted the code: the authenticator step when the
    /// account has one, else the new password.
    public static func verified(_ state: State, totpRequired: Bool) -> State {
        var next = State()
        next.step = totpRequired ? .totp : .newPassword
        _ = state
        return next
    }

    /// `recover/verify` was refused.
    public static func verifyRefused(_ state: State, _ refusal: RecoveryEmailRefusal) -> State {
        var next = state
        switch refusal {
        case .codeRefused: next.line = .codeWrong
        case .tooManyAttempts:
            // Five tries per code: a new letter is needed.
            next = State()
            next.line = .codeUsedUp
        case .codeExpired, .ticketRefused: next.line = .codeExpired
        case .lettersLimited(let secs): next.line = .limited(retryAfterSecs: secs)
        case .unavailable: next.line = .unavailable
        case .offline: next.line = .offline
        default: next.line = .failed
        }
        return next
    }

    /// `recover/totp` accepted the authenticator code: the new password.
    public static func totpAccepted(_ state: State) -> State {
        var next = State()
        next.step = .newPassword
        _ = state
        return next
    }

    /// A route that takes the ticket was refused (`recover/totp`, the
    /// password routes).
    public static func ticketRouteRefused(_ state: State, _ refusal: RecoveryEmailRefusal) -> State {
        var next = state
        switch refusal {
        case .codeRefused:
            next.step = .totp
            next.line = .codeWrong
        case .totpRequired:
            // There is no way past the authenticator.
            next.step = .totp
            next.line = nil
        case .ticketRefused, .codeExpired:
            // The ticket ran out or was spent: a new letter.
            next = State()
            next.line = .codeExpired
        case .tooManyAttempts(let secs), .lettersLimited(let secs):
            if next.step == .restoring { next.step = .newPassword }
            next.line = .limited(retryAfterSecs: secs)
        case .unavailable:
            if next.step == .restoring { next.step = .newPassword }
            next.line = .unavailable
        case .offline:
            if next.step == .restoring { next.step = .newPassword }
            next.line = .offline
        default:
            if next.step == .restoring { next.step = .newPassword }
            next.line = .failed
        }
        return next
    }

    /// The new password was submitted.
    public static func saving(_ state: State) -> State {
        var next = state
        if state.step == .newPassword { next.step = .restoring }
        next.line = nil
        return next
    }

    /// The last step failed: back to the new-password form with a line.
    public static func saveFailed(_ state: State, line: RecoveryEmailLine) -> State {
        var next = state
        if state.step == .restoring { next.step = .newPassword }
        next.line = line
        return next
    }
}

// MARK: - The enrol model

/// Adding, replacing and removing the recovery email (UX 19.2), over
/// injected ports.
@MainActor
final class RecoveryEmailModel: ObservableObject {

    struct Ports {
        /// The account's login handle; `nil` for a guest account.
        var loginHandle: @MainActor () -> Data?
        var sessionToken: @MainActor () -> String?
        /// The live fresh-auth grant, or `nil`.
        var grant: @MainActor () -> String?
        /// `normalizeRecoveryEmail`: throws for an address that is refused.
        var normalize: (String) throws -> String
        var start: (_ token: String, _ grant: String, _ handle: Data, _ address: String) async throws -> EmailChallenge
        var finish: (_ token: String, _ challengeId: String, _ code: String) async throws -> EmailEnrolled
        var remove: (_ token: String, _ grant: String) async throws -> Void
        /// The code was accepted for `address` (normalised): the account's
        /// devices keep it from here.
        var enrolled: @MainActor (_ result: EmailEnrolled, _ replaced: Bool, _ address: String) -> Void
        var removed: @MainActor () -> Void
        /// A gated route answered `403 fresh-auth-required`.
        var proofExpired: @MainActor () -> Void
    }

    @Published var address = ""
    @Published var code = ""
    @Published private(set) var state = EmailEnrol.State()
    @Published private(set) var busy = false
    /// The address shown back on the code step: only on this surface, only
    /// until it closes.
    @Published private(set) var sentTo: String?

    let resend: RetryCountdown
    let wait: RetryCountdown
    private let ports: Ports
    /// Whether an address is already on (the flow is then a replace).
    private let replacing: Bool
    private var challenge: EmailChallenge?

    /// - Parameter prefill: the address the form starts with (nothing when
    ///   an address is being replaced, else the one this device holds).
    init(
        ports: Ports, replacing: Bool, prefill: String = "",
        nowMs: @escaping () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }
    ) {
        self.ports = ports
        self.replacing = replacing
        self.address = prefill
        self.resend = RetryCountdown(nowMs: nowMs)
        self.wait = RetryCountdown(nowMs: nowMs)
    }

    /// A guest account has no username to make the stored value from.
    var needsUsername: Bool { ports.loginHandle() == nil }

    var addressValid: Bool { (try? ports.normalize(address)) != nil }

    /// The surface closed: nothing typed is kept.
    func closed() {
        challenge = nil
        address = ""
        code = ""
        sentTo = nil
        state = EmailEnrol.State()
        resend.stop()
        wait.stop()
    }

    /// `Send code` / `Send again`. The caller obtained the proof first.
    func send() async {
        guard !busy else { return }
        guard let handle = ports.loginHandle() else {
            state.line = .needsUsername
            return
        }
        guard let normalized = try? ports.normalize(address) else {
            state.line = .addressInvalid
            return
        }
        guard let token = ports.sessionToken(), let grant = ports.grant() else {
            state = EmailEnrol.startRefused(state, .freshAuthRequired)
            ports.proofExpired()
            return
        }
        busy = true
        defer { busy = false }
        do {
            let started = try await ports.start(token, grant, handle, normalized)
            challenge = started
            sentTo = normalized
            code = ""
            state = EmailEnrol.started(state)
            resend.start(forSecs: max(0, started.resendAfterSecs))
        } catch {
            let refusal = RecoveryEmailRefusal.of(error)
            state = EmailEnrol.startRefused(state, refusal)
            apply(refusal)
        }
    }

    /// Submit the six digits.
    func confirm() async {
        guard !busy, state.step == .code, let challenge, let token = ports.sessionToken(),
              let digits = EmailEnrol.normalizedCode(code) else { return }
        busy = true
        defer { busy = false }
        do {
            let result = try await ports.finish(token, challenge.challengeId, digits)
            self.challenge = nil
            code = ""
            state = EmailEnrol.finished(state)
            ports.enrolled(result, replaced, sentTo ?? "")
        } catch {
            let refusal = RecoveryEmailRefusal.of(error)
            let before = state
            state = EmailEnrol.finishRefused(state, refusal)
            code = ""
            if state.step == .address, before.step == .code {
                self.challenge = nil
                sentTo = nil
            }
            apply(refusal)
        }
    }

    /// `Use a different address`.
    func useOtherAddress() {
        challenge = nil
        sentTo = nil
        code = ""
        state = EmailEnrol.otherAddress(state)
    }

    /// `Remove`, confirmed in the app's own dialog. Returns whether it was
    /// removed. The caller obtained the proof first.
    func remove() async -> Bool {
        guard !busy else { return false }
        guard let token = ports.sessionToken(), let grant = ports.grant() else {
            state = EmailEnrol.startRefused(state, .freshAuthRequired)
            ports.proofExpired()
            return false
        }
        busy = true
        defer { busy = false }
        do {
            try await ports.remove(token, grant)
            ports.removed()
            return true
        } catch {
            let refusal = RecoveryEmailRefusal.of(error)
            state = EmailEnrol.startRefused(state, refusal)
            apply(refusal)
            return false
        }
    }

    private var replaced: Bool { replacing }

    private func apply(_ refusal: RecoveryEmailRefusal) {
        switch refusal {
        case .lettersLimited(let secs), .tooManyAttempts(let secs):
            if let secs, secs > 0 { wait.start(forSecs: secs) }
        case .freshAuthRequired:
            ports.proofExpired()
        default:
            break
        }
    }
}

// MARK: - The recover model

/// Getting back in with a mailed code (UX 19.3), over injected ports.
@MainActor
final class EmailRecoverModel: ObservableObject {

    struct Ports {
        /// `normalizeRecoveryEmail`.
        var normalize: (String) throws -> String
        /// The login handle of a typed username; throws its rule's reason.
        var handle: (String) throws -> LoginName
        var start: (_ handle: Data, _ address: String) async throws -> EmailChallenge
        var verify: (_ challengeId: String, _ code: String) async throws -> EmailTicket
        var totp: (_ ticket: String, _ code: String) async throws -> Void
        /// The new password under the ticket. Lands the session (on a device
        /// without the keys: "Restore your keys").
        var changePassword: @MainActor (_ username: String, _ ticket: String, _ newPassword: Data) async throws -> Void
        /// This device did what a notice will report.
        var noteOwnAction: @MainActor (AccountNoticeKind) -> Void
        /// The person got back in with this address (normalised): the
        /// account's devices keep it from here.
        var keepAddress: @MainActor (String) -> Void = { _ in }
    }

    @Published var username: String
    @Published var address = ""
    @Published var code = ""
    @Published var totpCode = ""
    @Published var password = ""
    @Published var confirmPassword = ""
    @Published private(set) var state = EmailRecover.State()
    @Published private(set) var busy = false
    /// The username rule's reason for what was typed, or `nil`.
    @Published private(set) var usernameError: UiText?
    @Published private(set) var passwordError: UiText?

    let resend: RetryCountdown
    let wait: RetryCountdown
    private let ports: Ports
    private var name: LoginName?
    /// The normalised address, kept only while the route runs.
    private var normalizedAddress: String?
    private var challenge: EmailChallenge?
    private var ticket: String?

    init(
        ports: Ports, cachedUsername: String?,
        nowMs: @escaping () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }
    ) {
        self.ports = ports
        // The username is pre-filled; the address never is.
        self.username = cachedUsername ?? ""
        self.resend = RetryCountdown(nowMs: nowMs)
        self.wait = RetryCountdown(nowMs: nowMs)
    }

    var addressValid: Bool { (try? ports.normalize(address)) != nil }

    var canStart: Bool {
        !busy && !wait.waiting && addressValid
            && !username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// Leave the route: nothing typed or received is kept.
    func reset() {
        name = nil
        normalizedAddress = nil
        challenge = nil
        ticket = nil
        address = ""
        code = ""
        totpCode = ""
        password = ""
        confirmPassword = ""
        usernameError = nil
        passwordError = nil
        state = EmailRecover.State()
        resend.stop()
        wait.stop()
    }

    /// `Back` on the code step: to step 1, the username kept.
    func backToStart() {
        let kept = username
        reset()
        username = kept
    }

    /// Step 1's button, and `Send again` on step 2.
    func start() async {
        guard !busy else { return }
        usernameError = nil
        let resolved: LoginName
        do {
            resolved = try ports.handle(username)
        } catch {
            usernameError = UiText { AuthErrorCopy.rule(error) ?? Localization.text("recovery.username_required") }
            return
        }
        guard let normalized = try? ports.normalize(address) else {
            state.line = .addressInvalid
            return
        }
        busy = true
        defer { busy = false }
        do {
            let answered = try await ports.start(resolved.handle, normalized)
            name = resolved
            normalizedAddress = normalized
            challenge = answered
            ticket = nil
            code = ""
            state = EmailRecover.started(state)
            resend.start(forSecs: max(0, answered.resendAfterSecs))
        } catch {
            let refusal = RecoveryEmailRefusal.of(error)
            state = EmailRecover.startRefused(state, refusal)
            startWait(refusal)
        }
    }

    /// Step 2: the six digits from the letter.
    func verify() async {
        guard !busy, state.step == .code, let challenge,
              let digits = EmailEnrol.normalizedCode(code) else { return }
        busy = true
        defer { busy = false }
        do {
            let answered = try await ports.verify(challenge.challengeId, digits)
            ticket = answered.ticket
            code = ""
            state = EmailRecover.verified(state, totpRequired: answered.totpRequired)
        } catch {
            let refusal = RecoveryEmailRefusal.of(error)
            state = EmailRecover.verifyRefused(state, refusal)
            code = ""
            if state.step == .start { self.challenge = nil }
            startWait(refusal)
        }
    }

    /// Step 3: the authenticator's six digits.
    func submitTotp() async {
        guard !busy, state.step == .totp, let ticket,
              let digits = EmailEnrol.normalizedCode(totpCode) else { return }
        busy = true
        defer { busy = false }
        do {
            try await ports.totp(ticket, digits)
            totpCode = ""
            state = EmailRecover.totpAccepted(state)
        } catch {
            totpCode = ""
            refuseTicketRoute(error)
        }
    }

    /// Step 4: the new password, set under the ticket. A mailed code gives
    /// nothing else: on a device without the keys the session lands on
    /// "Restore your keys".
    func save() async {
        guard !busy, state.step == .newPassword, let name, let normalizedAddress else { return }
        passwordError = nil
        switch NewPasswordRule.refusal(password: password, confirm: confirmPassword) {
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
        let newPassword = Data(password.utf8)
        busy = true
        defer { busy = false }
        state = EmailRecover.saving(state)
        guard let ticket else {
            state = EmailRecover.ticketRouteRefused(state, .ticketRefused)
            return
        }
        ports.noteOwnAction(.passwordChangedByEmail)
        do {
            try await ports.changePassword(name.canonical, ticket, newPassword)
            // The session landed: the router replaces this screen.
            ports.keepAddress(normalizedAddress)
            reset()
        } catch {
            refuseTicketRoute(error)
        }
    }

    private func refuseTicketRoute(_ error: Error) {
        let refusal = RecoveryEmailRefusal.of(error)
        state = EmailRecover.ticketRouteRefused(state, refusal)
        if state.step == .start {
            ticket = nil
            challenge = nil
        }
        startWait(refusal)
    }

    private func startWait(_ refusal: RecoveryEmailRefusal) {
        switch refusal {
        case .lettersLimited(let secs), .tooManyAttempts(let secs):
            if let secs, secs > 0 { wait.start(forSecs: secs) }
        default:
            break
        }
    }
}
