// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
import SwiftUI

/// The pure rule of the new-password form.
enum NewPasswordRule {

    /// The same floor as sign-up (``SignupViewModel/minPasswordLength``).
    static let minLength = 8

    enum Refusal: Equatable {
        case mismatch
        case tooShort
    }

    /// Why `password` / `confirm` cannot be saved, or `nil` when they can.
    static func refusal(password: String, confirm: String) -> Refusal? {
        if password != confirm { return .mismatch }
        if password.count < minLength { return .tooShort }
        return nil
    }
}

/// New password + confirm + Save, used by "Choose a new password" and by
/// Settings > Security > Password. Saving runs
/// ``AuthRepository/replacePassword(newPassword:)``: the device already
/// holds the account's keys, so only the new password is typed.
struct NewPasswordForm: View {
    @ObservedObject var authRepository: AuthRepository
    /// The primary button's title.
    let saveTitle: String
    /// "Not now", when the form may be skipped.
    var onSkip: (() -> Void)? = nil
    /// The repository refused the save for the proof (it ran out while the
    /// form was open): the gate in front of the form closes again.
    var onProofNeeded: () -> Void = {}
    let onSaved: () -> Void

    @State private var password = ""
    @State private var confirm = ""
    @State private var busy = false
    @State private var error: UiText?
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        AuthSecureField(
            label: l10n.t("auth.password.new.label"),
            text: $password,
            contentType: .newPassword,
            disabled: busy
        )
        AuthHint(text: l10n.t(
            "auth.password.hint", counts: ["count": UInt64(NewPasswordRule.minLength)]))
        AuthSecureField(
            label: l10n.t("auth.password.new_confirm.label"),
            text: $confirm,
            contentType: .newPassword,
            disabled: busy,
            submitLabel: .go,
            onSubmit: save
        )
        if let error {
            AuthHint(text: error.text, isError: true)
        }
        AuthPrimaryButton(
            title: saveTitle,
            busy: busy,
            busyTitle: l10n.t("common.saving"),
            action: save
        )
        if let onSkip {
            AuthLinkRow(title: l10n.t("recovery.new_password.not_now"), disabled: busy, action: onSkip)
        }
    }

    private func save() {
        guard !busy else { return }
        switch NewPasswordRule.refusal(password: password, confirm: confirm) {
        case .mismatch:
            error = .key("auth.password.error.mismatch")
            return
        case .tooShort:
            error = .key(
                "auth.password.error.too_short", counts: ["count": UInt64(NewPasswordRule.minLength)])
            return
        case nil:
            break
        }
        busy = true
        error = nil
        let bytes = Data(password.utf8)
        Task {
            do {
                try await authRepository.replacePassword(newPassword: bytes)
                busy = false
                password = ""
                confirm = ""
                onSaved()
            } catch is FreshAuthRequired {
                // Not an error: the proof ran out. The gate asks again.
                busy = false
                onProofNeeded()
            } catch {
                busy = false
                NSLog("[auth] WARN replacing the password failed")
                self.error = UiText { AuthErrorCopy.newPassword(error) }
            }
        }
    }
}

/// "Choose a new password": shown once, by the app root, right after a
/// forgot-password route succeeded on a device that holds the account's
/// keys (three words, an authenticator code, a passkey, another device).
/// The person is already signed in.
///
/// Saving replaces the password and opens the app. "Not now" opens the app
/// with the old (forgotten) password still valid; Settings > Security keeps
/// the same form.
struct NewPasswordPromptScreen: View {
    @ObservedObject var authRepository: AuthRepository
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        AuthScreen {
            AuthTitle(
                title: l10n.t("recovery.new_password.title"),
                subtitle: l10n.t("recovery.new_password.lede"))
            PasswordChangeGateView(
                authRepository: authRepository,
                onSkip: { authRepository.dismissNewPasswordOffer() }
            ) { relock in
                NewPasswordForm(
                    authRepository: authRepository,
                    saveTitle: l10n.t("recovery.new_password.save"),
                    onSkip: { authRepository.dismissNewPasswordOffer() },
                    onProofNeeded: relock,
                    // `replacePassword` clears the offer itself.
                    onSaved: {}
                )
            }
        }
    }
}

/// Settings > Security > Password: the same form, for a person who simply
/// wants a new password. Offered to accounts with a username on a device
/// that holds the keys (``AuthRepository/canReplacePassword``).
struct PasswordSettingsScreen: View {
    @ObservedObject var authRepository: AuthRepository
    @State private var saved = false
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        AuthScreen {
            Text(l10n.t("settings.password.lede"))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
            if authRepository.canReplacePassword {
                PasswordChangeGateView(authRepository: authRepository) { relock in
                    NewPasswordForm(
                        authRepository: authRepository,
                        saveTitle: l10n.t("settings.password.change"),
                        onProofNeeded: relock,
                        onSaved: { saved = true }
                    )
                }
                if saved {
                    Label(l10n.t("recovery.new_password.saved"), systemImage: "checkmark.circle.fill")
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoSuccess)
                }
            } else {
                AuthHint(text: l10n.t("common.error.session_not_ready"), isError: true)
            }
        }
        .navigationTitle(l10n.t("settings.password.title"))
        .navigationBarTitleDisplayMode(.inline)
    }
}

// MARK: - The gate in front of a password change

/// The local three-word check in front of a password change: three positions
/// of the phrase this device holds, with the same guess budget the sign-in
/// form keeps (``ThreeWords``). Nothing is sent anywhere.
@MainActor
final class PasswordThreeWordsModel: ObservableObject {

    struct Ports {
        var accountIdB64: @MainActor () -> String?
        var check: @MainActor (_ positions: Data, _ words: [String]) throws -> Bool
        var drawPositions: () -> [Int]
        var nowMs: () -> Int64

        static func live(_ repository: AuthRepository) -> Ports {
            Ports(
                accountIdB64: { [weak repository] in
                    guard let repository, case .session(let account) = repository.state else { return nil }
                    return account.accountIdB64
                },
                check: { [weak repository] positions, words in
                    guard let repository else { throw ThreeWordsError.unavailable }
                    return try repository.checkThreeWordsForPasswordChange(positions: positions, words: words)
                },
                drawPositions: { phraseCheckRandomPositions().map { Int($0) } },
                nowMs: { Int64(Date().timeIntervalSince1970 * 1000) }
            )
        }
    }

    @Published private(set) var positions: [Int] = []
    @Published var entries: [String] = Array(repeating: "", count: ThreeWords.wordCount)
    @Published private(set) var budget: ThreeWords.Budget = .fresh
    @Published private(set) var noMatch = false
    @Published private(set) var unavailable = false
    let pause: RetryCountdown

    private let ports: Ports
    private let budgets: ThreeWordsBudgetStore

    init(ports: Ports, budgets: ThreeWordsBudgetStore = ThreeWordsBudgetStore(), pause: RetryCountdown? = nil) {
        self.ports = ports
        self.budgets = budgets
        let pause = pause ?? RetryCountdown(nowMs: ports.nowMs)
        self.pause = pause
        // The form depends on whether the pause is still running.
        pauseSink = pause.objectWillChange.sink { [weak self] _ in
            self?.objectWillChange.send()
        }
    }

    private var pauseSink: AnyCancellable?

    /// Open the form: read the stored budget, resume a running pause, draw
    /// this prompt's positions.
    func enter() {
        guard let account = ports.accountIdB64() else { return }
        loadBudget(account)
        draw()
        noMatch = false
    }

    private func loadBudget(_ account: String) {
        let now = ports.nowMs()
        budget = ThreeWords.budget(budgets.load(accountIdB64: account), at: now)
        let left = ThreeWords.pauseRemainingMs(budget, at: now)
        if left > 0 { pause.start(untilMs: now + left) }
    }

    private func draw() {
        positions = ports.drawPositions()
        entries = Array(repeating: "", count: ThreeWords.wordCount)
    }

    /// Every field holds a word of the BIP39 list, so a typo is never
    /// counted as a guess.
    var complete: Bool {
        positions.count == ThreeWords.wordCount
            && entries.count == ThreeWords.wordCount
            && entries.allSatisfy { Bip39Words.contains($0) }
    }

    var triesLeft: Int? {
        guard budget.misses > 0, !pause.waiting else { return nil }
        return ThreeWords.triesLeft(budget, at: ports.nowMs())
    }

    /// Check the three words. `true` when they matched (the gate opens). A
    /// miss counts against the budget and draws new positions.
    func submit() -> Bool {
        guard let account = ports.accountIdB64() else { return false }
        loadBudget(account)
        guard !pause.waiting, complete else { return false }
        noMatch = false
        let matched: Bool
        do {
            matched = try ports.check(Data(positions.map { UInt8(clamping: $0) }), entries)
        } catch {
            unavailable = true
            return false
        }
        if matched {
            budgets.clear(accountIdB64: account)
            budget = .fresh
            entries = Array(repeating: "", count: ThreeWords.wordCount)
            return true
        }
        let now = ports.nowMs()
        let next = ThreeWords.afterMiss(budget, at: now)
        budget = next
        budgets.save(next, accountIdB64: account)
        let left = ThreeWords.pauseRemainingMs(next, at: now)
        if left > 0 { pause.start(untilMs: now + left) }
        draw()
        noMatch = true
        return false
    }
}

/// The gate's own lines (the proof block's are ``FreshAuthSurface``'s).
enum PasswordChangeGateCopy {
    /// The link to the local three-word check, for the person who forgot
    /// the password.
    static let useWordsKey = "proof.change_password.use_words"
}

/// A password change does not run on the bare session: this view stands in
/// front of the form until the person proved who they are now, with the
/// shared proof block (the current password, or a passkey) or, for the
/// person who forgot the password, the local three-word check of the
/// recovery phrase this device holds (``PasswordChangeGate``).
struct PasswordChangeGateView<Content: View>: View {
    @ObservedObject var authRepository: AuthRepository
    var onSkip: (() -> Void)?
    /// The form, handed a closure that closes the gate again.
    @ViewBuilder var content: (_ relock: @escaping () -> Void) -> Content

    @StateObject private var proof: FreshAuthProofModel
    @StateObject private var words: PasswordThreeWordsModel
    @State private var allowed = false
    @State private var usingWords = false
    @State private var checking = false
    @ObservedObject private var l10n = Localization.shared

    init(
        authRepository: AuthRepository,
        onSkip: (() -> Void)? = nil,
        @ViewBuilder content: @escaping (_ relock: @escaping () -> Void) -> Content
    ) {
        self.authRepository = authRepository
        self.onSkip = onSkip
        self.content = content
        self._proof = StateObject(
            wrappedValue: FreshAuthProofModel(surface: .changePassword, repository: authRepository))
        self._words = StateObject(wrappedValue: PasswordThreeWordsModel(ports: .live(authRepository)))
    }

    var body: some View {
        Group {
            if allowed {
                content { relock() }
            } else if usingWords {
                wordsForm
            } else {
                passwordForm
            }
        }
        .task {
            allowed = authRepository.passwordChangeAllowed
            await proof.opened()
        }
        .onDisappear { proof.closed() }
    }

    private func relock() {
        allowed = authRepository.passwordChangeAllowed
        proof.proofExpired()
    }

    private func continueWithProof() {
        guard !checking else { return }
        checking = true
        Task {
            if await proof.ensure() { allowed = authRepository.passwordChangeAllowed }
            checking = false
        }
    }

    @ViewBuilder
    private var passwordForm: some View {
        FreshAuthBlock(model: proof, onSubmit: continueWithProof)
        AuthPrimaryButton(
            title: l10n.t("common.continue"),
            busy: proof.layout.working,
            busyTitle: l10n.t("common.checking"),
            disabled: !proof.layout.ready,
            action: continueWithProof
        )
        if authRepository.threeWordsCheckAvailable {
            AuthLinkRow(title: l10n.t(PasswordChangeGateCopy.useWordsKey), disabled: proof.layout.working) {
                proof.closed()
                words.enter()
                usingWords = true
            }
        }
        if let onSkip {
            AuthLinkRow(title: l10n.t("recovery.new_password.not_now"), disabled: proof.layout.working, action: onSkip)
        }
    }

    @ViewBuilder
    private var wordsForm: some View {
        Text(l10n.t("recovery.method.words.title"))
            .font(.headline)
            .foregroundStyle(Color.lakkoText)
        if words.unavailable {
            AuthHint(text: l10n.t("recovery.words.error.unavailable"), isError: true)
        } else if words.pause.waiting {
            WaitNotice(countdown: words.pause, messageKey: "recovery.words.paused")
        } else {
            Text(l10n.t("recovery.words.prompt", texts: ["positions": l10n.positions(words.positions)]))
                .font(.body)
                .foregroundStyle(Color.lakkoText)
            ForEach(Array(words.positions.enumerated()), id: \.offset) { index, number in
                AuthTextField(
                    label: l10n.t("recovery_phrase.word_label", texts: ["position": String(number)]),
                    text: Binding(
                        get: { index < words.entries.count ? words.entries[index] : "" },
                        set: { if index < words.entries.count { words.entries[index] = $0.lowercased() } }
                    ),
                    autocorrect: false
                )
            }
            if words.noMatch {
                AuthHint(text: l10n.t("recovery.words.error.no_match"), isError: true)
            }
            if let left = words.triesLeft {
                AuthHint(text: l10n.t("recovery.words.tries_left", counts: ["count": UInt64(left)]))
            }
            AuthPrimaryButton(
                title: l10n.t("common.continue"),
                disabled: !words.complete
            ) {
                if words.submit() { allowed = authRepository.passwordChangeAllowed }
            }
        }
        AuthLinkRow(title: l10n.t("proof.know_password_link")) {
            usingWords = false
        }
        if let onSkip {
            AuthLinkRow(title: l10n.t("recovery.new_password.not_now"), action: onSkip)
        }
    }
}
