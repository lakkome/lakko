// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore
import SwiftUI

// UX `docs/ux/identity-recovery.md` 15.1b and 15.1c — the "Confirm it is
// you" section of the reset panel: the password (absent while a password
// proof is live), then one of the second factors the account has, then the
// line that says whether this reset will bring things back.
//
//   * ``ResetFactorLine``     the lines the section can show;
//   * ``ResetFactorModel``    the state machine over injected ports;
//   * ``ResetFactorSectionView`` the view.
//
// The rules (which rows, the "cannot reset" panel, when the button is
// enabled) are ``ResetFactorSection`` in `Auth/ResetFactor.swift`.

/// One line of the section. Shown under the field it is about.
enum ResetFactorLine: Equatable {
    case wrongPassword
    case usernameNeeded
    case wrongTotp
    /// The countdown is drawn by ``WaitNotice``; this is the line when the
    /// server named no wait.
    case limited
    case emailAddressInvalid
    case emailWrongCode
    /// Five wrong codes: a new letter is needed.
    case emailUsedUp
    case emailExpired
    case emailSendFailed
    case emailUnavailable
    case passkeyCancelled
    case passkeyOtherAccount
    case passkeyFailed
    case offline
    case failed

    var key: String {
        switch self {
        case .wrongPassword: return "restore_identity.reset.error.wrong_password"
        case .usernameNeeded: return "recovery.username_required"
        case .wrongTotp: return "settings.recovery.totp.error.wrong"
        case .limited: return "common.error.too_many_attempts"
        case .emailAddressInvalid: return "recovery_email.invalid"
        case .emailWrongCode: return "recovery_email.code.wrong"
        case .emailUsedUp: return "recovery_email.code.used_up"
        case .emailExpired: return "recovery_email.code.expired"
        case .emailSendFailed: return "recovery_email.send_failed"
        case .emailUnavailable: return "recovery_email.unavailable"
        case .passkeyCancelled: return "proof.reset.cancelled"
        case .passkeyOtherAccount: return "proof.passkey.other_account"
        case .passkeyFailed: return "proof.passkey.failed"
        case .offline: return "common.error.offline"
        case .failed: return "restore_identity.reset.error.generic"
        }
    }

    /// A cancelled passkey sheet is not an error (nothing went wrong).
    var isError: Bool { self != .passkeyCancelled }
}

@MainActor
final class ResetFactorModel: ObservableObject {

    struct Ports {
        /// The account's factors, asked of the server; `nil` when they
        /// could not be read.
        var factors: @MainActor () async -> AccountFactors?
        /// A password sign-in in the last ten minutes.
        var passwordProofLive: @MainActor () -> Bool
        /// This device knows the account's username.
        var usernameKnown: @MainActor () -> Bool
        /// The password sign-in. Throws ``FreshAuthWrongPassword`` and the
        /// repository's other proof errors.
        var provePassword: @MainActor (_ password: Data, _ username: String?) async throws -> Void
        var confirmTotp: @MainActor (_ code: String) async throws -> ResetFactorConfirmation
        var startEmail: @MainActor (_ address: String, _ username: String?) async throws -> EmailChallenge
        var confirmEmail: @MainActor (_ challengeId: String, _ code: String) async throws -> ResetFactorConfirmation
        var confirmPasskey: @MainActor () async throws -> AuthRepository.ResetPasskeyFactor
        /// What the device found about the seal for a confirmed factor.
        var proof: @MainActor (_ factor: ResetFactorConfirmation, _ passkey: AuthRepository.ResetPasskeyFactor?)
            async -> KeyResetProof?
        /// `normalizeRecoveryEmail`.
        var normalize: (String) throws -> String
        /// The recovery address this device kept, or `nil`.
        var heldAddress: @MainActor () -> String? = { nil }
        var nowMs: () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }

        static func live(repository: AuthRepository) -> Ports {
            let lang: @Sendable () async -> String = { await MainActor.run { Localization.shared.language } }
            return Ports(
                factors: { [weak repository] in await repository?.resetFactorsForPendingRestore() },
                passwordProofLive: { [weak repository] in
                    repository?.holdsLivePasswordProofForPendingRestore() ?? false
                },
                usernameKnown: { [weak repository] in repository?.proofUsernameKnown ?? true },
                provePassword: { [weak repository] password, username in
                    guard let repository else { throw FreshAuthNoAccount() }
                    try await repository.proveWithPassword(password: password, username: username)
                },
                confirmTotp: { [weak repository] code in
                    guard let repository else { throw FreshAuthNoAccount() }
                    return try await repository.confirmResetFactor(totpCode: code)
                },
                startEmail: { [weak repository] address, username in
                    guard let repository else { throw FreshAuthNoAccount() }
                    return try await repository.startResetEmailFactor(
                        address: address, username: username, lang: await lang())
                },
                confirmEmail: { [weak repository] challengeId, code in
                    guard let repository else { throw FreshAuthNoAccount() }
                    return try await repository.confirmResetFactor(emailChallengeId: challengeId, code: code)
                },
                confirmPasskey: { [weak repository] in
                    guard let repository else { throw FreshAuthNoAccount() }
                    return try await repository.confirmResetFactorWithPasskey()
                },
                proof: { [weak repository] factor, passkey in
                    await repository?.resetProof(factor: factor, passkey: passkey)
                },
                normalize: { try normalizeRecoveryEmail(address: $0) })
        }
    }

    @Published var password = ""
    /// Typed only when this device does not know the account's username.
    @Published var username = ""
    @Published var totpCode = ""
    @Published var address = ""
    @Published var emailCode = ""

    @Published private(set) var factors: AccountFactors?
    /// The row that is open; the others stay visible.
    @Published private(set) var expanded: ResetFactorKind?
    @Published private(set) var choseNone = false
    @Published private(set) var busy = false
    /// The system's passkey sheet is up.
    @Published private(set) var passkeySheet = false
    /// The letter step of the email row: "a code is on its way".
    @Published private(set) var emailSent = false
    @Published private(set) var passwordLine: ResetFactorLine?
    @Published private(set) var line: ResetFactorLine?
    /// The confirmed factor with what the device found about the seal.
    @Published private(set) var proof: KeyResetProof?

    /// Too many authenticator codes: the code field is disabled until zero.
    let totpWait: RetryCountdown
    /// "Send again (in 0:45)".
    let emailResend: RetryCountdown
    /// Too many letters or codes.
    let emailWait: RetryCountdown

    private let ports: Ports
    private var challenge: EmailChallenge?
    private var passwordProofLive = false
    private var loaded = false
    private var sinks: [AnyCancellable] = []

    init(ports: Ports) {
        self.ports = ports
        self.totpWait = RetryCountdown(nowMs: ports.nowMs)
        self.emailResend = RetryCountdown(nowMs: ports.nowMs)
        self.emailWait = RetryCountdown(nowMs: ports.nowMs)
        for countdown in [totpWait, emailResend, emailWait] {
            countdown.objectWillChange.sink { [weak self] _ in self?.objectWillChange.send() }.store(in: &sinks)
        }
    }

    // MARK: What the view reads

    var usernameKnown: Bool { ports.usernameKnown() }

    var form: ResetFactorSection.Form {
        ResetFactorSection.form(
            // Every iOS version this app runs on can use passkeys.
            factors: factors, passkeysSupported: true,
            passwordProofLive: passwordProofLive, choseNone: choseNone)
    }

    /// The confirmed factor, while its ten minutes last.
    var confirmed: ResetFactorConfirmation? {
        guard let factor = proof?.factor, factor.live(nowMs: ports.nowMs()) else { return nil }
        return factor
    }

    /// UX 15.1c — the line under the confirmed factor.
    var sealLine: ResetSealLine? {
        guard confirmed != nil else { return nil }
        return proof?.line(passkeysSupported: true)
    }

    func canReset(confirmation: String) -> Bool {
        ResetFactorSection.canReset(form: form, factorConfirmed: confirmed != nil, confirmation: confirmation)
    }

    // MARK: Lifecycle

    /// The panel opened: ask the server which factors the account has.
    func opened() async {
        passwordProofLive = ports.passwordProofLive()
        if !loaded || factors == nil {
            factors = await ports.factors()
            loaded = true
        }
        if address.isEmpty, let held = ports.heldAddress() { address = held }
        objectWillChange.send()
    }

    /// The panel closed: nothing typed or confirmed is kept.
    func closed() {
        password = ""
        username = ""
        totpCode = ""
        address = ""
        emailCode = ""
        expanded = nil
        choseNone = false
        emailSent = false
        passwordLine = nil
        line = nil
        proof = nil
        challenge = nil
        totpWait.stop()
        emailResend.stop()
        emailWait.stop()
    }

    func choose(_ kind: ResetFactorKind) {
        guard !busy, confirmed == nil else { return }
        expanded = expanded == kind ? nil : kind
        line = nil
    }

    /// `I cannot use any of these`.
    func chooseNone() {
        guard !busy else { return }
        choseNone = true
    }

    /// `Back` on the panel that came from `I cannot use any of these`.
    func backToSection() { choseNone = false }

    /// The confirmation ran out (or the server said so): the section opens
    /// again as it was, with no error styling.
    func confirmationExpired() {
        proof = nil
        passwordProofLive = ports.passwordProofLive()
        line = nil
        objectWillChange.send()
    }

    // MARK: The password, checked first

    /// With no live password proof, sign in with the typed password. A wrong
    /// password stops here: the factor is not tried and no code is used up.
    private func ensurePassword() async -> Bool {
        passwordLine = nil
        passwordProofLive = ports.passwordProofLive()
        if passwordProofLive { return true }
        guard !password.isEmpty else {
            passwordLine = .wrongPassword
            return false
        }
        if !usernameKnown, username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            passwordLine = .usernameNeeded
            return false
        }
        do {
            try await ports.provePassword(Data(password.utf8), usernameKnown ? nil : username)
            passwordProofLive = true
            // Never kept after it was used.
            password = ""
            return true
        } catch {
            if error is FreshAuthWrongPassword {
                passwordLine = .wrongPassword
            } else if error is FreshAuthUsernameRequired {
                passwordLine = .usernameNeeded
            } else if FreshAuthRefusal.isOffline(error) {
                passwordLine = .offline
            } else if RetryCountdown.attemptsRetryAfterSecs(error) != nil {
                passwordLine = .limited
            } else {
                passwordLine = .failed
            }
            return false
        }
    }

    private func confirmedWith(
        _ factor: ResetFactorConfirmation, passkey: AuthRepository.ResetPasskeyFactor? = nil
    ) async {
        proof = await ports.proof(factor, passkey)
        expanded = nil
        line = nil
        totpCode = ""
        emailCode = ""
    }

    /// What a refused factor route means on `kind`'s row.
    private func refuse(_ error: Error, on kind: ResetFactorKind) {
        switch ResetFactorRefusal.of(error) {
        case .refused:
            line = kind == .authenticator ? .wrongTotp : (kind == .email ? .emailWrongCode : .passkeyFailed)
        case .passwordRequired:
            // The password proof ran out on the way: the field comes back.
            passwordProofLive = false
            line = nil
        case .factorRequired:
            line = .failed
        case .tooManyAttempts(let secs):
            if kind == .email {
                // Five tries per code: a new letter.
                emailSent = false
                challenge = nil
                line = .emailUsedUp
                if let secs, secs > 0 { emailWait.start(forSecs: secs) }
            } else {
                line = (secs ?? 0) > 0 ? nil : .limited
                if let secs, secs > 0 { totpWait.start(forSecs: secs) }
            }
        case .lettersLimited(let secs):
            line = (secs ?? 0) > 0 ? nil : .limited
            if let secs, secs > 0 { emailWait.start(forSecs: secs) }
        case .codeExpired:
            emailSent = false
            challenge = nil
            line = .emailExpired
        case .mailSendFailed: line = .emailSendFailed
        case .unavailable: line = kind == .email ? .emailUnavailable : .failed
        case .offline: line = .offline
        case .failed: line = .failed
        }
    }

    // MARK: Authenticator app

    /// The code is checked on the sixth digit, with no separate button.
    func totpCodeChanged() {
        guard totpCode.filter(\.isNumber).count >= 6, !busy, !totpWait.waiting else { return }
        Task { await confirmTotp() }
    }

    func confirmTotp() async {
        guard !busy, confirmed == nil else { return }
        let code = String(totpCode.filter(\.isNumber).prefix(6))
        guard code.count == 6 else { return }
        busy = true
        defer { busy = false }
        line = nil
        guard await ensurePassword() else { return }
        do {
            await confirmedWith(try await ports.confirmTotp(code))
        } catch {
            // The digits clear and keep focus.
            totpCode = ""
            refuse(error, on: .authenticator)
        }
    }

    // MARK: Code by email

    var addressValid: Bool { (try? ports.normalize(address)) != nil }

    /// `Send code` / `Send again`.
    func sendEmailCode() async {
        guard !busy, confirmed == nil, !emailWait.waiting else { return }
        guard let normalized = try? ports.normalize(address) else {
            line = .emailAddressInvalid
            return
        }
        busy = true
        defer { busy = false }
        line = nil
        guard await ensurePassword() else { return }
        do {
            let started = try await ports.startEmail(normalized, usernameKnown ? nil : username)
            challenge = started
            emailCode = ""
            emailSent = true
            emailResend.start(forSecs: max(0, started.resendAfterSecs))
        } catch {
            if error is FreshAuthUsernameRequired {
                passwordLine = .usernameNeeded
            } else {
                refuse(error, on: .email)
            }
        }
    }

    func emailCodeChanged() {
        guard emailCode.filter(\.isNumber).count >= 6, !busy else { return }
        Task { await confirmEmailCode() }
    }

    func confirmEmailCode() async {
        guard !busy, confirmed == nil, let challenge else { return }
        let code = String(emailCode.filter(\.isNumber).prefix(6))
        guard code.count == 6 else { return }
        busy = true
        defer { busy = false }
        line = nil
        guard await ensurePassword() else { return }
        do {
            await confirmedWith(try await ports.confirmEmail(challenge.challengeId, code))
        } catch {
            emailCode = ""
            refuse(error, on: .email)
        }
    }

    /// `Use a different address`.
    func useOtherAddress() {
        guard !busy else { return }
        challenge = nil
        emailSent = false
        emailCode = ""
        address = ""
        line = nil
    }

    // MARK: Passkey

    /// `Use passkey`: the platform's own passkey sheet. Also the action of
    /// "Use a passkey that keeps your chats to bring them back." under an
    /// unsealed line, where a confirmation already stands: a seal from the
    /// passkey then makes the line the sealed one.
    func usePasskey() async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        line = nil
        guard await ensurePassword() else { return }
        passkeySheet = true
        defer { passkeySheet = false }
        do {
            let used = try await ports.confirmPasskey()
            await confirmedWith(used.confirmation, passkey: used)
        } catch {
            if AuthErrorCopy.isPasskeyCancelled(error) {
                line = .passkeyCancelled
            } else if error is FreshAuthPasskeyOtherAccount {
                line = .passkeyOtherAccount
            } else if error is FreshAuthRequired || FreshAuthRefusal.isRequired(error) {
                passwordProofLive = false
            } else {
                refuse(error, on: .passkey)
            }
        }
    }
}

// MARK: - The view

struct ResetFactorSectionView: View {
    @ObservedObject var model: ResetFactorModel
    /// The account's username, for the email row's "a code is on its way".
    let username: String
    @ObservedObject private var l10n = Localization.shared
    @FocusState private var totpFocused: Bool
    @FocusState private var emailCodeFocused: Bool

    var body: some View {
        switch model.form {
        case .loading:
            HStack(spacing: 8) {
                ProgressView()
                Text(l10n.t("common.checking"))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
        case .cannotReset(let bodyKey, let showsBack):
            cannotReset(bodyKey: bodyKey, showsBack: showsBack)
        case .section(let rows, let showsPassword, let titleKey):
            section(rows: rows, showsPassword: showsPassword, titleKey: titleKey)
        }
    }

    // MARK: The section

    @ViewBuilder
    private func section(rows: [ResetFactorKind], showsPassword: Bool, titleKey: String) -> some View {
        Text(l10n.t("proof.reset.factor.heading"))
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(Color.lakkoText)
            .accessibilityAddTraits(.isHeader)
        if let confirmed = model.confirmed {
            // One line with a check mark, then what this reset brings back.
            Label(l10n.t(confirmed.kind.doneKey), icon: .checkmarkCircleFill, size: 16, relativeTo: .subheadline)
                .font(.subheadline)
                .foregroundStyle(Color.lakkoSuccess)
            sealLine
        } else {
            if showsPassword {
                if !model.usernameKnown {
                    AuthTextField(
                        label: l10n.t("auth.username.label"), text: $model.username,
                        contentType: .username, disabled: model.busy)
                }
                AuthSecureField(
                    label: l10n.t("restore_identity.reset.password_label"),
                    text: $model.password, disabled: model.busy)
                if let passwordLine = model.passwordLine {
                    AuthHint(text: l10n.t(passwordLine.key), isError: true)
                } else {
                    AuthHint(text: l10n.t("restore_identity.reset.password_help"))
                }
            }
            Text(l10n.t(titleKey))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
            VStack(spacing: 0) {
                ForEach(Array(rows.enumerated()), id: \.element) { index, kind in
                    row(kind)
                    if index < rows.count - 1 { Divider().padding(.leading, 44) }
                }
            }
            .background(Color.lakkoSurfaceSunken)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
            Button(l10n.t("proof.reset.factor.none")) { model.chooseNone() }
                .buttonStyle(.plain)
                .font(.footnote.weight(.medium))
                .foregroundStyle(Color.lakkoAccent)
                .frame(minHeight: 44, alignment: .leading)
                .disabled(model.busy)
        }
    }

    @ViewBuilder private var sealLine: some View {
        switch model.sealLine {
        case .sealed?:
            Text(l10n.t(ResetSealLine.sealedKey))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
        case .unsealed(let offersPasskey)?:
            ProtectWarningNote(text: l10n.t(ResetSealLine.unsealedKey))
            if offersPasskey {
                Text(l10n.t(ResetSealLine.unsealedUsePasskeyKey))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                Button(l10n.t("proof.reset.factor.passkey.use")) { Task { await model.usePasskey() } }
                    .buttonStyle(.plain)
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Color.lakkoAccent)
                    .frame(minHeight: 44, alignment: .leading)
                    .disabled(model.busy)
                if let line = model.line {
                    AuthHint(text: l10n.t(line.key), isError: line.isError)
                }
            }
        case nil:
            EmptyView()
        }
    }

    private func icon(_ kind: ResetFactorKind) -> LkIcon {
        switch kind {
        case .authenticator: return .smartphone
        case .email: return .envelope
        case .passkey: return .passkey
        }
    }

    private func title(_ kind: ResetFactorKind) -> String {
        switch kind {
        case .authenticator: return l10n.t("proof.reset.factor.totp")
        case .email: return l10n.t("proof.reset.factor.email")
        case .passkey: return l10n.t("proof.reset.factor.passkey")
        }
    }

    @ViewBuilder
    private func row(_ kind: ResetFactorKind) -> some View {
        let open = model.expanded == kind
        Button {
            model.choose(kind)
        } label: {
            HStack(spacing: 12) {
                LkIconView(icon(kind), size: 18)
                    .frame(width: 24)
                    .foregroundStyle(Color.lakkoAccent)
                Text(title(kind))
                    .font(.body)
                    .foregroundStyle(Color.lakkoText)
                Spacer()
                LkIconView(open ? .chevronUp : .chevronForward, size: 14, relativeTo: .footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 48)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(model.busy)
        .accessibilityAddTraits(open ? .isSelected : [])
        if open {
            VStack(alignment: .leading, spacing: 10) {
                switch kind {
                case .authenticator: totp
                case .email: email
                case .passkey: passkey
                }
                if let line = model.line {
                    AuthHint(text: l10n.t(line.key), isError: line.isError)
                }
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 12)
        }
    }

    @ViewBuilder private var totp: some View {
        AuthTextField(
            label: l10n.t("proof.reset.factor.totp.prompt"),
            placeholder: "123456",
            text: $model.totpCode,
            keyboard: .numberPad,
            contentType: .oneTimeCode,
            disabled: model.busy || model.totpWait.waiting,
            isFocused: $totpFocused
        )
        .onChange(of: model.totpCode) { _, _ in model.totpCodeChanged() }
        .onAppear { totpFocused = true }
        WaitNotice(countdown: model.totpWait)
    }

    @ViewBuilder private var email: some View {
        if model.emailSent {
            Text(l10n.t("proof.reset.factor.email.sent", texts: ["username": "@\(username)"]))
                .font(.footnote)
                .foregroundStyle(Color.lakkoTextMuted)
            AuthTextField(
                label: l10n.t("recovery_email.code.title"),
                placeholder: "123456",
                text: $model.emailCode,
                keyboard: .numberPad,
                contentType: .oneTimeCode,
                disabled: model.busy,
                isFocused: $emailCodeFocused
            )
            .onChange(of: model.emailCode) { _, _ in model.emailCodeChanged() }
            .onAppear { emailCodeFocused = true }
            ResendRow(countdown: model.emailResend, disabled: model.busy || model.emailWait.waiting) {
                Task { await model.sendEmailCode() }
            }
            AuthLinkRow(title: l10n.t("recovery_email.code.other_address"), disabled: model.busy) {
                model.useOtherAddress()
            }
        } else {
            AuthTextField(
                label: l10n.t("recovery_email.field"),
                placeholder: "name@example.com",
                text: $model.address,
                keyboard: .emailAddress,
                contentType: .emailAddress,
                disabled: model.busy,
                submitLabel: .send,
                onSubmit: { Task { await model.sendEmailCode() } }
            )
            AuthPrimaryButton(
                title: l10n.t("recovery_email.send"),
                busy: model.busy,
                disabled: !model.addressValid || model.emailWait.waiting
            ) { Task { await model.sendEmailCode() } }
        }
        WaitNotice(countdown: model.emailWait)
    }

    @ViewBuilder private var passkey: some View {
        AuthPrimaryButton(
            title: l10n.t("proof.reset.factor.passkey.use"),
            busy: model.passkeySheet,
            busyTitle: l10n.t("recovery.passkey.waiting"),
            disabled: model.busy
        ) { Task { await model.usePasskey() } }
    }

    // MARK: The "cannot reset" panel

    /// Text only: no disabled reset button, no control to add a factor (it
    /// would need only the password), no sentence suggesting a later way.
    private func cannotReset(bodyKey: String, showsBack: Bool) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(l10n.t(ResetFactorSection.cannotTitleKey))
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Color.lakkoText)
                .accessibilityAddTraits(.isHeader)
            Text(l10n.t(bodyKey))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
            Text(l10n.t("proof.reset.cannot.what_you_can_do"))
                .font(.subheadline.weight(.medium))
                .foregroundStyle(Color.lakkoText)
            VStack(alignment: .leading, spacing: 6) {
                ForEach([
                    "proof.reset.cannot.option_device",
                    "proof.reset.cannot.option_phrase",
                    "proof.reset.cannot.option_passkey",
                ], id: \.self) { key in
                    HStack(alignment: .top, spacing: 8) {
                        Text("\u{2022}")
                        Text(l10n.t(key))
                    }
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
                }
            }
            Text(l10n.t("proof.reset.cannot.otherwise"))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
            if showsBack {
                Button(l10n.t("common.back")) { model.backToSection() }
                    .buttonStyle(.plain)
                    .font(.body.weight(.medium))
                    .foregroundStyle(Color.lakkoAccent)
                    .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
            }
        }
    }
}
