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
/// ADR-0075 D2: the recovery phrase is NOT shown at sign-up. The device
/// generates and keeps it (``DevicePhrase``); this view model never sees
/// it. What follows the form is "Protect your account" (UX 19.1a), which
/// the app root shows over the landed session.
@MainActor
public final class SignupViewModel: ObservableObject {

    // MARK: - Form fields

    @Published public var username: String = ""
    /// ADR-0048 — the optional "Your name" field. Blank means others see the
    /// username.
    @Published public var displayName: String = ""
    @Published public var password: String = ""
    @Published public var confirm: String = ""

    // MARK: - Internal status

    @Published public private(set) var busy: Bool = false
    /// Kept as ``UiText`` so a message on screen follows a live language switch.
    @Published private var errorText: UiText?
    public var error: String? { errorText?.text }
    /// ADR-0054 S4 — the last attempt was refused by the sign-up limit
    /// (`429 signup-rate-limited`): the screen offers `Try again`, the form
    /// keeps its contents.
    @Published public private(set) var rateLimited: Bool = false
    /// Whether the working state still offers `Cancel`. `false` once the
    /// account is being created (right before `/signup/finish`).
    @Published public private(set) var cancellable: Bool = false

    /// `docs/ux/beta-account-cap.md` section 2 — the form, or "Lakko is full
    /// for now" in its place. The form fields are kept while it shows.
    @Published public private(set) var mode: SignupScreenMode = .form

    // MARK: - Supporter code (`docs/ux/supporters.md` section 1)

    /// The code field.
    @Published public var supporterCode: String = ""
    /// `Checking...` on `Continue`.
    @Published public private(set) var codeChecking: Bool = false
    /// The line under the code field, if any.
    @Published public private(set) var codeLine: SupporterCodeLine?
    /// A supporter place is held on this device: the form shows the held
    /// line, and the cap does not stop the sign-up here.
    @Published public private(set) var placeHeld: Bool = false
    /// The full state shows "This supporter code has already been used."
    @Published public private(set) var codeUsed: Bool = false

    /// `Continue` on the code entry.
    public var canContinueCode: Bool {
        SupporterCodeEntry.isComplete(supporterCode) && !codeChecking
    }

    /// The sign-up in flight, cancelled by ``onCancel()`` and when the screen
    /// goes away.
    private var signupTask: Task<Void, Never>?
    /// Bumped per submit and per cancel, so a cancelled attempt that returns
    /// late never touches the state of the next one.
    private var attempt = 0

    // MARK: - Wiring

    private let repository: AuthRepository
    private let l10n = Localization.shared
    private let supporterPlaces: SupporterPlaceHolder
    private let supporterRedeemer: SupporterCodeRedeeming

    /// Called once right after a successful sign-up. The repository lands
    /// the session inside `signup`, which replaces the anonymous stack (and
    /// this view model's screen) at the next render, so the step that
    /// follows cannot live here; the app root shows ``SignupProtectScreen``
    /// over the session.
    public var onSignedUp: (() -> Void)?

    public init(
        repository: AuthRepository,
        onSignedUp: (() -> Void)? = nil,
        supporterPlaces: SupporterPlaceHolder = SupporterPlaceHolder(),
        supporterRedeemer: SupporterCodeRedeeming = SupporterCodeRedeemer()
    ) {
        self.repository = repository
        self.onSignedUp = onSignedUp
        self.supporterPlaces = supporterPlaces
        self.supporterRedeemer = supporterRedeemer
    }

    // MARK: - Submit

    /// Submit the signup form.
    ///
    /// Validates locally (password length, confirm match), drives
    /// ``AuthRepository/signup(username:password:)``, and tells the app
    /// root on success (``onSignedUp``). Errors surface
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
            errorText = .key("auth.signup.error.enter_username")
            return
        case .invalid(let reason):
            errorText = UiText { reason.message }
            return
        case .valid:
            break
        }
        if displayNameError != nil {
            errorText = .key("auth.signup.error.fix_name")
            return
        }
        if password != confirm {
            errorText = .key("auth.password.error.mismatch")
            return
        }
        if password.count < Self.minPasswordLength {
            errorText = .key("auth.password.error.too_short", counts: ["count": UInt64(Self.minPasswordLength)])
            return
        }

        busy = true
        cancellable = true
        errorText = nil
        rateLimited = false

        let usernameSnapshot = username
        let displayNameSnapshot = displayName
        let passwordBytes = Data(password.utf8)

        attempt += 1
        let thisAttempt = attempt
        signupTask = Task {
            do {
                // The beta account cap: asked again before the proof of work,
                // so nobody solves it only to be refused. A failed question
                // reads as open; the bind decides.
                let open = await repository.signupOpen()
                guard attempt == thisAttempt, !Task.isCancelled else { return }
                // A held supporter place passes the cap (ADR-0097): it rides
                // the first bind, which decides.
                let place = supporterPlaces.live()
                placeHeld = place != nil
                if !open && place == nil {
                    cancellable = false
                    busy = false
                    codeUsed = false
                    mode = .closed
                    return
                }
                _ = try await repository.signup(
                    username: usernameSnapshot,
                    password: passwordBytes,
                    supporterToken: place?.headerValue,
                    onCommitting: {
                        if self.attempt == thisAttempt { self.cancellable = false }
                    }
                )
                cancellable = false
                busy = false
                errorText = nil
                password = ""
                confirm = ""
                saveDisplayName(displayNameSnapshot)
                onSignedUp?()
            } catch {
                guard attempt == thisAttempt else { return }
                cancellable = false
                busy = false
                // `Cancel` or leaving the screen: back to the filled-in form,
                // no message. A cancelled network call surfaces as a network
                // error, so the task's flag decides, not the error type.
                if Task.isCancelled || error is CancellationError { return }
                // Refused at the bind: the repository deleted the account it
                // made; the form gives way to the full state.
                if SignupCap.isRegistrationClosed(error) {
                    errorText = nil
                    codeUsed = false
                    mode = .closed
                    return
                }
                // The held place was spent by another account: the account
                // is gone as above, the place is dropped, and the full state
                // says why (`docs/ux/supporters.md` section 1.2).
                if SignupCap.isSupporterCodeUsed(error) {
                    supporterPlaces.drop()
                    placeHeld = false
                    errorText = nil
                    codeUsed = true
                    mode = .closed
                    return
                }
                if let err = error as? AuthClientError,
                   let refusal = AuthRateLimit.refusal(in: err),
                   refusal.code == AuthRateLimit.signupRateLimited {
                    rateLimited = true
                }
                self.errorText = UiText { AuthErrorCopy.signup(error) }
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
        errorText = nil
    }

    /// The screen opened: ask whether sign-up is open. Closed replaces the
    /// form; open, or a failed question, leaves it.
    public func onAppear() async {
        guard mode == .form, !busy else { return }
        placeHeld = supporterPlaces.live() != nil
        let open = await repository.signupOpen()
        guard mode == .form, !busy else { return }
        mode = SignupScreenMode.after(open: open, placeHeld: placeHeld)
    }

    /// `Try again` in the full state: ask again. Open returns to the form
    /// with everything typed kept; still closed stays, with no message.
    public func onRetryOpen() {
        guard mode == .closed else { return }
        mode = .checking
        Task {
            let open = await repository.signupOpen()
            guard mode == .checking else { return }
            placeHeld = supporterPlaces.live() != nil
            mode = SignupScreenMode.after(open: open, placeHeld: placeHeld)
        }
    }

    // MARK: - Supporter code

    /// `I have a supporter code` under the full state.
    public func onOpenSupporterCode() {
        guard mode == .closed else { return }
        codeLine = nil
        mode = .supporterCode
    }

    /// `Back` on the code entry: the full state again.
    public func onSupporterCodeBack() {
        guard mode == .supporterCode, !codeChecking else { return }
        codeLine = nil
        mode = .closed
    }

    /// `Continue` on the code entry: exchange the code for a held place.
    /// The field keeps what was typed on every failure.
    public func onSubmitSupporterCode() {
        guard mode == .supporterCode, canContinueCode else { return }
        Task { await submitSupporterCode() }
    }

    /// The body of ``onSubmitSupporterCode()``, awaitable by a test.
    func submitSupporterCode() async {
        guard mode == .supporterCode, canContinueCode else { return }
        codeChecking = true
        codeLine = nil
        let outcome = await supporterRedeemer.redeem(code: supporterCode)
        codeChecking = false
        guard mode == .supporterCode else { return }
        switch outcome {
        case .held(let place):
            supporterPlaces.hold(place)
            placeHeld = true
            codeUsed = false
            supporterCode = ""
            mode = .form
        case .line(let line):
            codeLine = line
        }
    }

    /// `Try again` after the sign-up limit: the same form, submitted again.
    public func onTryAgain() {
        rateLimited = false
        onSubmit()
    }

    /// The working-state copy (`docs/ux/anti-abuse.md` section 1).
    public static var workingTitle: String { Localization.shared.t("auth.signup.working.title") }
    public static var workingNote: String { Localization.shared.t("auth.signup.working.line") }

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
            return (error as? LocalizedError)?.errorDescription ?? l10n.t("display_name.error.unknown")
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

    // MARK: - Constants

    /// Minimum password length enforced client-side. Mirrors the
    /// floor in `apps/web` + `apps/android` — a real strength
    /// heuristic + Argon2id KSF cost feedback lands in a later
    /// sprint.
    public static let minPasswordLength: Int = 8
}

