// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// State + dispatcher for ``RecoverScreen``: the "Get back in" list and the
/// form of each method.
///
/// Mirror of the web `routes/Recover.tsx` + `auth/recover/*`.
///
/// Two kinds of route, and the screen's order says which is which:
///
///   - routes that bring back the keys (and so the chats): another device
///     first ("if you have one"), then a passkey, then the 12-word phrase
///     (which also sets a new password);
///   - routes that only sign the person in: three words from the phrase and
///     an authenticator code. Afterwards the app offers "Choose a new
///     password" from the keys the device holds.
///
/// Nothing in the list depends on server state: before a session the server
/// must not say whether an account has other devices, a passkey or an
/// authenticator. One row depends on a LOCAL fact: "Three words from your
/// phrase" is checked on the device against the recovery phrase the device
/// holds, so it is listed only where ``AuthRepository/threeWordsAccount()``
/// answers.
@MainActor
public final class RecoverViewModel: ObservableObject {

    // MARK: - Methods

    /// One way back in.
    public enum Mode: Hashable {
        /// ADR-0075 / UX 19.3 — a code sent to the recovery email. What it
        /// brings depends on the account, so the row stands above both
        /// groups; it is always listed (no row depends on server state).
        case email
        /// Restore through a device that is still signed in.
        case anotherDevice
        /// A passkey: signs in, and restores the keys when it carries them.
        case passkey
        /// The 12-word phrase and a new password.
        case phrase
        /// Three words of the phrase, checked on this device. Signs in only.
        case words
        /// A six-digit authenticator code. Signs in only.
        case totp
    }

    /// The account the three-word route can sign in on this device, or
    /// `nil` when the route is not offered here.
    @Published public private(set) var threeWordsAccount: SessionAccount?

    /// The row above both groups.
    public var topMethods: [Mode] { [.email] }

    /// The list's two groups in display order.
    public var keysMethods: [Mode] { [.anotherDevice, .passkey, .phrase] }
    public var signInMethods: [Mode] { threeWordsAccount == nil ? [.totp] : [.words, .totp] }

    /// The catalog key of the note under the list: it names three words
    /// only where the row is offered, so it never promises an absent route.
    public var accessOnlyNoteKey: String {
        threeWordsAccount == nil ? "recovery.access_only_note" : "recovery.access_only_note_words"
    }

    @Published public var mode: Mode = .phrase {
        didSet {
            // Clear the resolved restore target + any stale error when
            // leaving the another-device form.
            if mode != .anotherDevice {
                deviceRestoreTarget = nil
            }
            errorText = nil
            methodUnavailable = false
            wordsRetired = false
            if mode == .words { enterWords() }
            if mode != .phrase { phraseRecall = nil }
            if mode != .email { email.reset() }
        }
    }

    /// The email-code route (UX 19.3).
    let email: EmailRecoverModel

    /// UX 19.7 — the words typed are not the account's phrase now, but were
    /// before its keys were changed within the last month: the prompt that
    /// offers to take the account back. `nil` otherwise.
    @Published public private(set) var phraseRecall: PhraseRecallOffer?
    /// The take-back from the 19.7 prompt is running.
    @Published public private(set) var phraseRecallBusy: Bool = false

    /// Non-nil once the another-device form turned the typed username into
    /// its login handle (ADR-0056, no lookup).
    @Published public private(set) var deviceRestoreTarget: DeviceRestoreTarget?
    @Published public var phrase: String = ""
    @Published public var password: String = ""
    @Published public var confirm: String = ""
    /// ADR-0017 — username for the fresh-device recover path. Every
    /// recovery factor names the account by the login handle of this name
    /// (ADR-0054 S5; no discovery lookup before a session), so on a device
    /// with no cached account the user must type it. Pre-filled with
    /// ``cachedUsername`` when present.
    @Published public var username: String = ""

    // MARK: - Internal status

    @Published public private(set) var busy: Bool = false
    /// Kept as ``UiText`` so a message on screen follows a live language switch.
    @Published private var errorText: UiText?
    public var error: String? { errorText?.text }
    /// The 6-digit authenticator code field.
    @Published public var totpCode: String = ""
    /// The chosen method is switched off on the server (`503`): the form
    /// gives way to one sentence and the way back to the list.
    @Published public private(set) var methodUnavailable: Bool = false

    // MARK: - Three words

    /// The sign-in made from the stored phrase was refused and the route was
    /// suspended on this device: the form gives way to that one sentence and
    /// the way back to the list, which no longer has the three-word row.
    @Published public private(set) var wordsRetired: Bool = false

    /// The three word numbers of this prompt, in the order drawn (not
    /// sorted). Empty until the form is entered.
    @Published public private(set) var wordsPositions: [Int] = []
    /// What the person typed; `wordsEntries[i]` answers `wordsPositions[i]`.
    @Published public var wordsEntries: [String] = Array(repeating: "", count: ThreeWords.wordCount)
    /// The local guess budget for the cached account.
    @Published private(set) var wordsBudget: ThreeWords.Budget = .fresh
    /// The local pause after five misses.
    let wordsPause: RetryCountdown
    /// A server limit on the sign-in that follows a match. A different wait
    /// with a different sentence.
    let serverWait: RetryCountdown

    // MARK: - Read-only inputs

    /// Cached username for the on-device account, surfaced in the
    /// "Account on this device: @alice" line. When `nil` the screen
    /// renders a "no cached account" stub.
    public let cachedUsername: String?

    // MARK: - Wiring

    private let repository: AuthRepository
    private let l10n = Localization.shared
    private let budgets: ThreeWordsBudgetStore
    private let nowMs: () -> Int64
    private let drawPositions: () -> [Int]

    public convenience init(repository: AuthRepository, cachedUsername: String?) {
        self.init(repository: repository, cachedUsername: cachedUsername, noteOwnAction: { _ in })
    }

    /// - Parameter noteOwnAction: this device did what an account notice
    ///   will report (it opened the account, or changed the password, by
    ///   email), so the notice is not shown here.
    convenience init(
        repository: AuthRepository, cachedUsername: String?,
        noteOwnAction: @escaping @MainActor (AccountNoticeKind) -> Void,
        keepAddress: @escaping @MainActor (String) -> Void = { _ in }
    ) {
        self.init(
            repository: repository,
            cachedUsername: cachedUsername,
            email: EmailRecoverModel(
                ports: .live(repository: repository, noteOwnAction: noteOwnAction, keepAddress: keepAddress),
                cachedUsername: cachedUsername),
            budgets: ThreeWordsBudgetStore(),
            nowMs: { Int64(Date().timeIntervalSince1970 * 1000) },
            drawPositions: { phraseCheckRandomPositions().map { Int($0) } }
        )
    }

    init(
        repository: AuthRepository,
        cachedUsername: String?,
        email: EmailRecoverModel? = nil,
        budgets: ThreeWordsBudgetStore,
        nowMs: @escaping () -> Int64,
        drawPositions: @escaping () -> [Int]
    ) {
        self.repository = repository
        self.cachedUsername = cachedUsername
        // A caller that passes no model gets one that reaches no server.
        self.email = email ?? EmailRecoverModel(ports: .unwired, cachedUsername: cachedUsername)
        self.budgets = budgets
        self.nowMs = nowMs
        self.drawPositions = drawPositions
        self.wordsPause = RetryCountdown(nowMs: nowMs)
        self.serverWait = RetryCountdown(nowMs: nowMs)
        // Pre-fill the username field with the cached handle when one
        // exists; the fresh-device path leaves it empty for the user.
        self.username = cachedUsername ?? ""
        self.threeWordsAccount = repository.threeWordsAccount()
    }

    /// Re-read the local fact behind the three-word row (the list is shown
    /// again after a sign-out on the same device).
    public func refreshMethods() {
        threeWordsAccount = repository.threeWordsAccount()
    }

    /// Back to the list from a form.
    public func leaveForm() {
        errorText = nil
        methodUnavailable = false
        wordsRetired = false
        phraseRecall = nil
        email.reset()
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
            errorText = .key("auth.password.error.mismatch")
            return
        }
        if password.count < Self.minPasswordLength {
            errorText = .key("auth.password.error.too_short", counts: ["count": UInt64(Self.minPasswordLength)])
            return
        }
        // ADR-0017 / ADR-0054 S5 — on a fresh device (no cached account)
        // the username is required: the account is named by its handle.
        let trimmedUsername = username.trimmingCharacters(in: .whitespacesAndNewlines)
        if cachedUsername == nil && trimmedUsername.isEmpty {
            errorText = .key("recovery.username_required")
            return
        }

        // Only the phrase mode reaches the phrase + new password flow; the
        // other modes submit through their own entry points.
        let phraseSnapshot: String
        switch mode {
        case .anotherDevice, .passkey, .words, .email:
            // Defensive no-op: these forms never show this button.
            errorText = .key("recovery.error.generic")
            return
        case .totp:
            // The authenticator form submits via `submitTotp` (no new
            // password on that path). Defensive no-op.
            errorText = .key("recovery.totp.error.enter_code")
            return
        case .phrase:
            if phrase.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                errorText = .key("recovery_phrase.error.incomplete")
                return
            }
            phraseSnapshot = phrase
        }

        busy = true
        errorText = nil

        let passwordBytes = Data(password.utf8)

        let usernameSnapshot = trimmedUsername.isEmpty ? nil : trimmedUsername

        Task {
            do {
                // Typed by the person: they hold it (ADR-0077 decision 3).
                try await repository.recover(
                    mnemonicPhrase: phraseSnapshot,
                    newPassword: passwordBytes,
                    username: usernameSnapshot,
                    phraseHeldByPerson: true
                )
                // `repository.recover` flipped AuthState to .session.
                // The screen will be replaced by the home destination
                // on the next render; wipe textfield buffers so they
                // don't hold the phrase / password past the
                // navigation.
                busy = false
                errorText = nil
                phrase = ""
                password = ""
                confirm = ""
            } catch {
                // UX 19.7 — the words are not this account's phrase NOW. If
                // they were its phrase before its keys were changed in the
                // last month, the form continues with the question; for
                // every other answer it says what it always says.
                if PhraseRecallRule.asksServer(afterRecoverFailure: error),
                   let offer = await repository.phraseRecallOffer(
                       username: usernameSnapshot ?? cachedUsername ?? "",
                       mnemonicPhrase: phraseSnapshot) {
                    busy = false
                    errorText = nil
                    phraseRecall = offer
                    return
                }
                // A bad phrase (unknown word or checksum) reads as one
                // sentence; a wrong phrase and an unknown account too.
                busy = false
                // Overtaken by a sign-out or another sign-in: not a failure.
                if error is StaleBindingError { return }
                self.errorText = UiText { AuthErrorCopy.recover(error) }
            }
        }
    }

    // MARK: - UX 19.7: taking the account back with the phrase

    /// `It was me`: the keys were changed by the person; these words are no
    /// longer the account's.
    public func phraseRecallWasMe() {
        phraseRecall = nil
        errorText = .key("recovery.error.no_match")
    }

    /// `Take the account back`: the recall with the phrase as the proof and
    /// the new password typed on the form.
    public func phraseRecallTakeBack() {
        guard !phraseRecallBusy, let offer = phraseRecall else { return }
        if password != confirm {
            errorText = .key("auth.password.error.mismatch")
            return
        }
        if password.count < Self.minPasswordLength {
            errorText = .key("auth.password.error.too_short", counts: ["count": UInt64(Self.minPasswordLength)])
            return
        }
        phraseRecallBusy = true
        errorText = nil
        let phraseSnapshot = phrase
        let passwordBytes = Data(password.utf8)
        Task {
            do {
                try await repository.takeBackWithPhrase(
                    offer: offer, mnemonicPhrase: phraseSnapshot, newPassword: passwordBytes)
                // The account is back: the router shows the new words.
                phraseRecallBusy = false
                phraseRecall = nil
                phrase = ""
                password = ""
                confirm = ""
            } catch {
                phraseRecallBusy = false
                if error is FreshPhraseKeep.NotStored {
                    // Refused before anything was sent (ADR-0077 decision 3).
                    errorText = .key("restore_identity.lost.error.phrase_not_stored")
                    return
                }
                switch RecallAnswers.refusal(of: error) {
                case .refused:
                    // The window passed or the attempts ran out.
                    phraseRecall = nil
                    errorText = .key("recovery.error.no_match")
                case .wrongPassword:
                    errorText = .key(RecallCopy.wrongPhrase)
                case .rateLimited:
                    errorText = .key("common.error.too_many_attempts")
                case .offline:
                    errorText = .key("common.error.offline")
                case .failed:
                    errorText = .key("common.error.unavailable")
                }
            }
        }
    }

    // MARK: - ADR-0018 factor (a), path (b) — another-device restore

    /// Turn the typed username into the login handle the pairing code names
    /// the account by (ADR-0056), so the AnotherDevice restore can show its
    /// QR. No network: nothing before a session looks the account up, and
    /// the account id is learned from the grant redeem after the other
    /// device confirms. A name that breaks the username rule gets its
    /// reason copy.
    public func prepareDeviceRestore() {
        guard !busy else { return }
        do {
            let name = try LoginName.make(
                username,
                emptyError: AuthClientError.argument("empty username")
            )
            deviceRestoreTarget = DeviceRestoreTarget(
                username: name.canonical,
                loginHandle: name.handle
            )
            errorText = nil
        } catch {
            self.errorText = UiText { AuthErrorCopy.rule(error) ?? Localization.text("recovery.username_required") }
        }
    }

    /// Complete the no-password restore with what the other device handed
    /// over (the phrase entropy, recovery payload v2) and the redeemed
    /// session token. The account id is the one the grant redeem returned
    /// (ADR-0056). Async passthrough to
    /// ``AuthRepository/completeFromDeviceRestore(accountIdBytes:accountIdB64:username:phraseEntropy:session:forgotPassword:persistJoin:)``;
    /// verification failures propagate to the calling panel. Requires
    /// the redeemed token and account id (without them this device has no
    /// access) — refused otherwise with a clear message.
    public func completeFromDeviceRestore(
        outcome: PairingOrchestrator.JoinRecoveryOutcome
    ) async throws {
        guard let target = deviceRestoreTarget else {
            outcome.wipeEntropy()
            throw AuthClientError.argument(
                "no username — enter your username first"
            )
        }
        guard let session = outcome.session,
              let accountIdBytes = outcome.accountId else {
            // Keys-only outcome: the grant redeem failed, so this
            // device has the keys but no access. Refuse rather than
            // leave a half-restored profile; the user retries (a
            // fresh pairing mints a fresh grant). The entropy is zeroed
            // where it lives, in the outcome.
            outcome.wipeEntropy()
            throw DeviceRestoreNotLetIn()
        }
        // The repository rebuilds the phrase and its seed, verifies the
        // seed, checks the typed name at discovery, and only then runs the
        // pairing's held writes (`persistJoin`) and its own. On any refusal
        // nothing was written. The entropy is zeroed here, in the outcome,
        // once the repository is done with it. This list is reached through
        // "Forgot password?", so the new-password step follows.
        defer { outcome.wipeEntropy() }
        try await repository.completeFromDeviceRestore(
            accountIdBytes: accountIdBytes,
            accountIdB64: base64Encode(accountIdBytes),
            username: target.username,
            phraseEntropy: outcome.phraseEntropy,
            session: session,
            forgotPassword: true,
            predecessors: outcome.predecessors,
            persistJoin: outcome.persistJoin ?? {}
        )
    }

    /// Submit the authenticator form: username (cached account wins, else
    /// the typed field) + the current 6-digit code.
    /// `AuthRepository.recoverWithTotp` drives the state flip — `.session`
    /// on a device that holds the keys (the new-password step follows),
    /// `.needsRecoveryPhrase` on one that does not — and the app router
    /// follows automatically.
    public func submitTotp() {
        guard !busy else { return }
        let user = (cachedUsername ?? username)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !user.isEmpty else {
            errorText = .key("recovery.username_required_to_recover")
            return
        }
        guard let normalized = normalizeTotpCode(totpCode) else {
            errorText = .key("recovery.totp.error.enter_code")
            return
        }
        busy = true
        errorText = nil
        Task {
            do {
                try await repository.recoverWithTotp(
                    username: user,
                    code: normalized
                )
                // The repository flipped the auth state; wipe the
                // form buffer before the router replaces this screen.
                busy = false
                errorText = nil
                totpCode = ""
            } catch {
                busy = false
                // Overtaken by a sign-out or another sign-in: not a failure.
                if error is StaleBindingError { return }
                if case .http(503, _, _) = error as? AuthClientError,
                   let err = error as? AuthClientError,
                   !AuthRateLimit.isLimiterUnavailable(err),
                   !AuthRateLimit.isLoginByNameDisabled(err) {
                    // Authenticator recovery is switched off on this server.
                    methodUnavailable = true
                    return
                }
                let typedUsername = cachedUsername == nil
                self.errorText = UiText { AuthErrorCopy.totp(error, typedUsername: typedUsername) }
            }
        }
    }

    // MARK: - Passkey

    /// "Use a passkey": no username field, the system sheet lists the
    /// passkeys saved for Lakko, and the sign-in works on a device that has
    /// never held the account. One button, three endings, all decided in
    /// ``AuthRepository/loginWithPasskey(usernameHint:forgotPassword:)``: a
    /// device that holds the keys lands signed in; a passkey that carries
    /// the keys restores them; otherwise the restore prompt opens, whose
    /// first line says the passkey signed in and did not bring keys.
    public func submitPasskey() {
        guard !busy else { return }
        busy = true
        errorText = nil
        let hint = username
        Task {
            do {
                try await repository.loginWithPasskey(usernameHint: hint, forgotPassword: true)
                busy = false
            } catch {
                busy = false
                if AuthErrorCopy.isPasskeyServerDisabled(error) {
                    // Passkeys are switched off on this server.
                    methodUnavailable = true
                } else if !AuthErrorCopy.isPasskeyCancelled(error) {
                    // Dismissing the sheet is not an error: back to idle.
                    self.errorText = UiText { AuthErrorCopy.passkey(error) }
                }
            }
        }
    }

    // MARK: - Three words

    /// Open the three-word form: read the stored budget, resume a running
    /// pause, draw the positions of this prompt.
    func enterWords() {
        guard let account = threeWordsAccount else { return }
        let now = nowMs()
        wordsBudget = ThreeWords.budget(budgets.load(accountIdB64: account.accountIdB64), at: now)
        let left = ThreeWords.pauseRemainingMs(wordsBudget, at: now)
        if left > 0 { wordsPause.start(untilMs: now + left) }
        drawWordPositions()
    }

    /// Draw three fresh positions, in the order drawn, and clear the fields.
    private func drawWordPositions() {
        wordsPositions = drawPositions()
        wordsEntries = Array(repeating: "", count: ThreeWords.wordCount)
    }

    /// Whether every field holds a word of the BIP39 list: the submit
    /// button is enabled only then, so a typo is never counted as a guess.
    public var wordsComplete: Bool {
        wordsPositions.count == ThreeWords.wordCount
            && wordsEntries.count == ThreeWords.wordCount
            && wordsEntries.allSatisfy { Bip39Words.contains($0) }
    }

    /// Tries left before the pause, shown once a miss was counted.
    public var wordsTriesLeft: Int? {
        guard wordsBudget.misses > 0, !wordsPause.waiting else { return nil }
        return ThreeWords.triesLeft(wordsBudget, at: nowMs())
    }

    /// Submit the three words. A miss counts against the local budget and
    /// draws NEW positions; nothing is sent anywhere. A match signs in with
    /// the seed this device already holds; an error after a match is never
    /// charged to the budget.
    public func submitWords() {
        guard !busy, let account = threeWordsAccount else { return }
        errorText = nil
        // Re-read the stored budget before checking.
        let now = nowMs()
        wordsBudget = ThreeWords.budget(budgets.load(accountIdB64: account.accountIdB64), at: now)
        let left = ThreeWords.pauseRemainingMs(wordsBudget, at: now)
        if left > 0 {
            wordsPause.start(untilMs: now + left)
            return
        }
        guard wordsComplete, !serverWait.waiting else { return }
        busy = true
        let positions = Data(wordsPositions.map { UInt8(clamping: $0) })
        let words = wordsEntries
        Task {
            do {
                let matched = try await repository.recoverWithThreeWords(
                    positions: positions, words: words)
                busy = false
                if matched {
                    // Signed in: the app root shows "Choose a new password".
                    budgets.clear(accountIdB64: account.accountIdB64)
                    wordsEntries = Array(repeating: "", count: ThreeWords.wordCount)
                } else {
                    recordWordsMiss(accountIdB64: account.accountIdB64)
                    errorText = .key("recovery.words.error.no_match")
                }
            } catch {
                busy = false
                // Overtaken by a sign-out or another sign-in: not a failure.
                if error is StaleBindingError { return }
                // The words matched (or the check could not run); nothing
                // here is a wrong guess, so the budget is not charged.
                if case .suspended = error as? ThreeWordsError {
                    // The repository suspended the route. Drop the budget
                    // and the row with it; the phrase and the keys stay.
                    budgets.clear(accountIdB64: account.accountIdB64)
                    wordsBudget = .fresh
                    wordsEntries = Array(repeating: "", count: ThreeWords.wordCount)
                    wordsRetired = true
                    threeWordsAccount = repository.threeWordsAccount()
                    self.errorText = UiText { AuthErrorCopy.threeWords(error) }
                } else if let secs = RetryCountdown.attemptsRetryAfterSecs(error) {
                    serverWait.start(forSecs: secs)
                } else if case .http(503, _, _) = error as? AuthClientError,
                          let err = error as? AuthClientError,
                          !AuthRateLimit.isLimiterUnavailable(err) {
                    methodUnavailable = true
                } else {
                    NSLog("[three-words] sign-in after a match failed")
                    self.errorText = UiText { AuthErrorCopy.threeWords(error) }
                }
            }
        }
    }

    private func recordWordsMiss(accountIdB64: String) {
        let now = nowMs()
        let next = ThreeWords.afterMiss(wordsBudget, at: now)
        wordsBudget = next
        budgets.save(next, accountIdB64: accountIdB64)
        let left = ThreeWords.pauseRemainingMs(next, at: now)
        if left > 0 { wordsPause.start(untilMs: now + left) }
        // New positions for every prompt: a miss never asks the same three
        // again on purpose.
        drawWordPositions()
    }

    // MARK: - Constants

    /// Mirror of ``SignupViewModel/minPasswordLength``. The recovery
    /// flow sets a **new** password so the same floor applies.
    public static let minPasswordLength: Int = 8
}

/// ADR-0018 factor (a), path (b) — who the AnotherDevice mode pairs as:
/// the canonical username typed and its login handle (ADR-0056). No account
/// id: the fresh device learns it from the grant redeem.
public struct DeviceRestoreTarget: Equatable {
    public let username: String
    public let loginHandle: Data

    public init(username: String, loginHandle: Data) {
        self.username = username
        self.loginHandle = loginHandle
    }
}

/// UX 19.7 — when a failed phrase recovery asks the server whether the words
/// were the account's phrase before a reset, pure.
enum PhraseRecallRule {

    /// Only a refusal of the words themselves (`401`: not this account's
    /// recovery credential, by design the same answer as an unknown name).
    /// A limit, a server fault, no connection and words that are no phrase
    /// at all ask nothing.
    static func asksServer(afterRecoverFailure error: Error) -> Bool {
        guard case .http(let status, _, _) = error as? AuthClientError else { return false }
        return status == 401
    }
}

extension EmailRecoverModel.Ports {

    /// Ports that reach no server: every route is refused as unavailable.
    static var unwired: EmailRecoverModel.Ports {
        struct Unwired: Error {}
        return EmailRecoverModel.Ports(
            normalize: { try normalizeRecoveryEmail(address: $0) },
            handle: { try LoginName.make($0, emptyError: Unwired()) },
            start: { _, _ in throw Unwired() },
            verify: { _, _ in throw Unwired() },
            totp: { _, _ in throw Unwired() },
            open: { _ in throw Unwired() },
            unseal: { _, _, _, _ in throw Unwired() },
            restoreWithPhrase: { _, _, _ in throw Unwired() },
            changePassword: { _, _, _ in throw Unwired() },
            noteOwnAction: { _ in }
        )
    }

    /// The production ports.
    @MainActor
    static func live(
        repository: AuthRepository,
        noteOwnAction: @escaping @MainActor (AccountNoticeKind) -> Void,
        keepAddress: @escaping @MainActor (String) -> Void = { _ in }
    ) -> EmailRecoverModel.Ports {
        // The letter is written in the language of this device.
        let lang: @Sendable () async -> String = { await MainActor.run { Localization.shared.language } }
        return EmailRecoverModel.Ports(
            normalize: { try normalizeRecoveryEmail(address: $0) },
            handle: { try LoginName.make($0, emptyError: AuthClientError.argument("empty username")) },
            start: { handle, address in
                try await AuthClient().recoveryEmailRecoverStart(handle: handle, address: address, lang: await lang())
            },
            verify: { challengeId, code in
                try await AuthClient().recoveryEmailRecoverVerify(challengeId: challengeId, code: code)
            },
            totp: { ticket, code in
                try await AuthClient().recoveryEmailRecoverTotp(ticket: ticket, code: code)
            },
            open: { ticket in try await AuthClient().recoveryEmailRecoverOpen(ticket: ticket) },
            unseal: { handle, address, salt, sealed in
                try await EmailVaultWork.open(handle: handle, address: address, vaultSalt: salt, sealed: sealed)
            },
            restoreWithPhrase: { [weak repository] phrase, newPassword, username in
                guard let repository else { throw SessionNotReady() }
                // Opened from the email vault: never typed, never shown.
                try await repository.recover(
                    mnemonicPhrase: phrase, newPassword: newPassword, username: username,
                    phraseHeldByPerson: false)
            },
            changePassword: { [weak repository] username, ticket, newPassword in
                guard let repository else { throw SessionNotReady() }
                try await repository.recoverPasswordByEmail(
                    username: username, ticket: ticket, newPassword: newPassword)
            },
            noteOwnAction: noteOwnAction,
            keepAddress: keepAddress
        )
    }
}
