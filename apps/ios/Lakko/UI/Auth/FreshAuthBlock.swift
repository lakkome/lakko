// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import SwiftUI

// UX `docs/ux/identity-recovery.md` section 17.1 — the shared proof block.
//
// One component, drawn inside the surface that already exists (the reset
// panel, the delete dialog, the "Is this you?" sheet, the scan confirm step,
// the passkey and password settings). It is absent while a fresh-auth proof
// is live. It never is a second screen, a second dialog or a system prompt;
// the only system surface it opens is the platform's passkey sheet.
//
//   * ``FreshAuthSurface``      which surface, and the lines that are its own;
//   * ``FreshAuthBlockState``   the shared states of 17.1;
//   * ``FreshAuthBlockLayout``  the pure decision: what is drawn, what is
//                               enabled;
//   * ``FreshAuthProofModel``   the state machine over injected ports;
//   * ``FreshAuthBlock``        the view.

/// The surface a proof block is drawn in.
enum FreshAuthSurface: Equatable {
    case reset
    case deleteAccount
    case addDeviceRequest
    case addDeviceScan
    case addPasskey
    case changePassword
    /// Settings > Devices > Remove (ADR-0066 section 2.3, UX 18.1).
    case removeDevice
    /// ADR-0067 / UX 19.2 — adding, replacing or removing the recovery email.
    case recoveryEmail
    /// ADR-0067 / UX 19.5 — `Save a recovery phrase`.
    case savePhrase
    /// ADR-0068 — changing the username.
    case changeUsername

    /// The helper under the field, naming the action.
    var helpKey: String? {
        switch self {
        case .recoveryEmail: return "proof.recovery_email.help"
        case .savePhrase: return "proof.save_phrase.help"
        case .changeUsername: return "proof.username.help"
        case .removeDevice: return "proof.remove_device.help"
        case .reset: return "restore_identity.reset.password_help"
        case .deleteAccount: return "proof.delete.help"
        case .addDeviceRequest, .addDeviceScan: return "proof.add_device.help"
        case .addPasskey: return "proof.add_passkey.help"
        case .changePassword: return "proof.change_password.help"
        }
    }

    /// The line under the field after a wrong password: it says what did
    /// NOT happen.
    var wrongPasswordKey: String {
        switch self {
        case .reset, .addPasskey, .changePassword:
            return "restore_identity.reset.error.wrong_password"
        case .recoveryEmail, .savePhrase, .changeUsername: return "settings.recovery.phrase.wrong_password"
        case .removeDevice: return "proof.remove_device.wrong_password"
        case .deleteAccount: return "proof.delete.wrong_password"
        case .addDeviceRequest: return "proof.add_device.request.wrong_password"
        case .addDeviceScan: return "proof.add_device.scan.wrong_password"
        }
    }

    /// The line after a dismissed passkey sheet: it says what did NOT
    /// happen on the surface. `nil` where the catalog has no such line: the
    /// surface is simply as it was.
    var cancelledKey: String? {
        switch self {
        case .recoveryEmail, .savePhrase, .changeUsername: return nil
        case .reset: return "proof.reset.cancelled"
        case .deleteAccount: return "settings.account.delete.error.cancelled"
        case .addDeviceRequest: return "proof.add_device.request.cancelled"
        case .addDeviceScan: return "proof.add_device.scan.cancelled"
        case .addPasskey: return "proof.add_passkey.cancelled"
        case .changePassword: return "proof.change_password.cancelled"
        case .removeDevice: return "proof.remove_device.cancelled"
        }
    }

    /// UX 17.4 — on the add-device surfaces an account with a usable passkey
    /// is asked for the gesture first (no field; `Use your password instead`
    /// swaps the notice for the field).
    var passkeyFirst: Bool {
        self == .addDeviceRequest || self == .addDeviceScan
    }
}

/// The states the block shares, wherever it is drawn (UX 17.1).
enum FreshAuthBlockState: Equatable {
    case idle
    /// The password sign-in is running.
    case checking
    case wrongPassword
    /// The typed username derives no login handle.
    case usernameRefused
    /// `429`. `timed` when the server named a wait (the live countdown).
    case rateLimited(timed: Bool)
    /// The platform's passkey sheet is up.
    case passkeySheet
    /// The sheet was dismissed. Not an error.
    case cancelled
    case passkeyOtherAccount
    case passkeyFailed
    case offline
    /// Any other failure of the password sign-in.
    case failed
}

/// What the block draws and enables, decided purely.
struct FreshAuthBlockLayout: Equatable {

    /// The one line under the field.
    enum Line: Equatable {
        case text(key: String, isError: Bool)
        /// `recovery.error.rate_limited` with the live countdown.
        case countdown
        /// The username rule's own reason for the typed name.
        case usernameRule
    }

    /// `false` while a proof is live: nothing of the block is drawn.
    var showsBlock = false
    var showsUsernameField = false
    var showsPasswordField = false
    var helpKey: String?
    /// `Use a passkey instead` (`proof.passkey.use`).
    var showsPasskeyButton = false
    /// `You will be asked for your passkey.` (`proof.passkey.notice`).
    var showsPasskeyNotice = false
    /// `Use your password instead` (`proof.password.use`).
    var showsUsePassword = false
    var fieldsDisabled = false
    var passkeyButtonEnabled = false
    /// Whether the surface's main button may proceed, as far as the proof
    /// is concerned (the surface adds its own conditions).
    var ready = false
    /// The main button opens the passkey sheet (passkey-first, UX 17.4).
    var mainOpensPasskey = false
    /// The main button shows its working word.
    var working = false
    var line: Line?

    /// - Parameters:
    ///   - proofLive: a fresh-auth proof is held for the account.
    ///   - hasUsablePasskey: the account has a passkey and this device can
    ///     use passkeys.
    ///   - usernameKnown: the device knows the account's username.
    ///   - usePassword: the person chose `Use your password instead`.
    ///   - waiting: the rate-limit countdown is still running.
    static func decide(
        surface: FreshAuthSurface,
        proofLive: Bool,
        hasUsablePasskey: Bool,
        usernameKnown: Bool,
        usePassword: Bool,
        state: FreshAuthBlockState,
        waiting: Bool,
        password: String,
        username: String
    ) -> FreshAuthBlockLayout {
        var layout = FreshAuthBlockLayout()
        if proofLive {
            layout.ready = true
            return layout
        }
        layout.showsBlock = true
        // A timed rate limit ends with its countdown.
        var state = state
        if case .rateLimited(timed: true) = state, !waiting { state = .idle }
        let busy = state == .checking || state == .passkeySheet
        let limited = state == .rateLimited(timed: true)
        layout.working = busy
        layout.line = line(surface: surface, state: state)

        if surface.passkeyFirst, hasUsablePasskey, !usePassword {
            layout.showsPasskeyNotice = true
            layout.showsUsePassword = true
            layout.mainOpensPasskey = true
            layout.ready = !busy
            layout.passkeyButtonEnabled = !busy
            return layout
        }
        layout.showsPasswordField = true
        layout.showsUsernameField = !usernameKnown
        layout.helpKey = surface.helpKey
        layout.showsPasskeyButton = hasUsablePasskey
        layout.fieldsDisabled = busy || limited
        // The passkey route stays usable while the password is rate limited.
        layout.passkeyButtonEnabled = !busy
        let usernameReady = usernameKnown
            || !username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        layout.ready = !busy && !limited && !password.isEmpty && usernameReady
        return layout
    }

    private static func line(surface: FreshAuthSurface, state: FreshAuthBlockState) -> Line? {
        switch state {
        case .idle, .checking: return nil
        case .wrongPassword: return .text(key: surface.wrongPasswordKey, isError: true)
        case .usernameRefused: return .usernameRule
        case .rateLimited(let timed):
            return timed ? .countdown : .text(key: "common.error.too_many_attempts", isError: true)
        case .passkeySheet: return .text(key: "recovery.passkey.waiting", isError: false)
        case .cancelled: return surface.cancelledKey.map { .text(key: $0, isError: false) }
        case .passkeyOtherAccount: return .text(key: "proof.passkey.other_account", isError: true)
        case .passkeyFailed: return .text(key: "proof.passkey.failed", isError: true)
        case .offline: return .text(key: "common.error.offline", isError: true)
        case .failed: return .text(key: "common.error.unavailable", isError: true)
        }
    }
}

/// The block's state machine. One per surface instance; the typed password
/// never leaves it except into the sign-in, and is cleared when the surface
/// closes.
@MainActor
final class FreshAuthProofModel: ObservableObject {

    /// What the block needs from the repository, injectable for tests.
    struct Ports {
        var holdsLive: @MainActor () -> Bool
        var usernameKnown: @MainActor () -> Bool
        /// Whether the account has a passkey; `nil` when unknown.
        var hasPasskey: @MainActor () async -> Bool?
        var proveWithPassword: @MainActor (_ password: Data, _ username: String?) async throws -> Void
        var proveWithPasskey: @MainActor () async throws -> Void

        static func live(_ repository: AuthRepository) -> Ports {
            Ports(
                holdsLive: { [weak repository] in repository?.holdsLiveFreshAuth() ?? false },
                usernameKnown: { [weak repository] in repository?.proofUsernameKnown ?? false },
                hasPasskey: { [weak repository] in await repository?.accountHasPasskey() },
                proveWithPassword: { [weak repository] password, username in
                    guard let repository else { throw FreshAuthNoAccount() }
                    try await repository.proveWithPassword(password: password, username: username)
                },
                proveWithPasskey: { [weak repository] in
                    guard let repository else { throw FreshAuthNoAccount() }
                    try await repository.proveWithPasskey()
                }
            )
        }
    }

    let surface: FreshAuthSurface
    @Published var password = ""
    @Published var username = ""
    @Published private(set) var state: FreshAuthBlockState = .idle
    @Published private(set) var proofLive = false
    /// `nil` until the passkey list was read; read as "none" by the layout.
    @Published private(set) var hasPasskey: Bool?
    @Published private(set) var usePassword = false
    @Published private(set) var usernameKnown = true

    let countdown: RetryCountdown
    private let ports: Ports
    private var countdownSink: AnyCancellable?

    init(surface: FreshAuthSurface, ports: Ports, countdown: RetryCountdown? = nil) {
        self.surface = surface
        self.ports = ports
        let countdown = countdown ?? RetryCountdown()
        self.countdown = countdown
        // The layout depends on whether the wait is still running.
        countdownSink = countdown.objectWillChange.sink { [weak self] _ in
            self?.objectWillChange.send()
        }
        // Known before the first frame, so a live proof never flashes a field.
        proofLive = ports.holdsLive()
        usernameKnown = ports.usernameKnown()
    }

    convenience init(surface: FreshAuthSurface, repository: AuthRepository) {
        self.init(surface: surface, ports: .live(repository))
    }

    /// The account has a passkey and this device can use passkeys (every
    /// iOS version this app runs on can).
    var hasUsablePasskey: Bool { hasPasskey == true }

    var layout: FreshAuthBlockLayout {
        FreshAuthBlockLayout.decide(
            surface: surface,
            proofLive: proofLive,
            hasUsablePasskey: hasUsablePasskey,
            usernameKnown: usernameKnown,
            usePassword: usePassword,
            state: state,
            waiting: countdown.waiting,
            password: password,
            username: username
        )
    }

    /// Re-read whether a proof is live (it may have run out, or been
    /// obtained on another surface).
    func refresh() {
        proofLive = ports.holdsLive()
        usernameKnown = ports.usernameKnown()
    }

    /// The surface opened: read the proof, then whether a passkey can stand
    /// in for the password.
    func opened() async {
        refresh()
        if hasPasskey == nil { hasPasskey = await ports.hasPasskey() }
    }

    /// The surface closed: the typed password is discarded.
    func closed() {
        password = ""
        username = ""
        usePassword = false
        state = .idle
        countdown.stop()
    }

    /// A gated route answered `403 fresh-auth-required`: the proof ran out
    /// between steps. The block appears with no error styling (nothing went
    /// wrong).
    func proofExpired() {
        proofLive = false
        state = .idle
    }

    /// `Use your password instead` / back to the passkey notice.
    func setUsePassword(_ on: Bool) {
        usePassword = on
        state = .idle
    }

    /// The surface's main button: `true` when a proof is live afterwards and
    /// the surface may continue. With a live proof nothing is asked; in
    /// passkey-first form the sheet opens; otherwise the typed password is
    /// checked. On `false` the block shows why and the surface stays.
    func ensure() async -> Bool {
        refresh()
        if proofLive { return true }
        let layout = self.layout
        if layout.mainOpensPasskey { return await usePasskey() }
        guard layout.ready else { return false }
        state = .checking
        let typedName = usernameKnown ? nil : username
        do {
            try await ports.proveWithPassword(Data(password.utf8), typedName)
            password = ""
            username = ""
            state = .idle
            proofLive = true
            return true
        } catch {
            state = Self.passwordFailure(error, countdown: countdown)
            if state == .wrongPassword { password = "" }
            return false
        }
    }

    /// `Use a passkey instead`, or the main button in passkey-first form.
    func usePasskey() async -> Bool {
        guard state != .checking, state != .passkeySheet else { return false }
        state = .passkeySheet
        do {
            try await ports.proveWithPasskey()
            password = ""
            state = .idle
            proofLive = true
            return true
        } catch {
            state = Self.passkeyFailure(error)
            return false
        }
    }

    /// The state a failed password proof leaves (the 17.1 table).
    static func passwordFailure(_ error: Error, countdown: RetryCountdown?) -> FreshAuthBlockState {
        if error is FreshAuthWrongPassword { return .wrongPassword }
        if error is FreshAuthUsernameRequired { return .usernameRefused }
        if let secs = RetryCountdown.attemptsRetryAfterSecs(error) {
            countdown?.start(forSecs: secs)
            return .rateLimited(timed: true)
        }
        if let err = error as? AuthClientError, AuthRateLimit.refusal(in: err) != nil {
            return .rateLimited(timed: false)
        }
        if FreshAuthRefusal.isOffline(error) { return .offline }
        return .failed
    }

    /// The state a failed passkey proof leaves.
    static func passkeyFailure(_ error: Error) -> FreshAuthBlockState {
        if AuthErrorCopy.isPasskeyCancelled(error) { return .cancelled }
        if error is FreshAuthPasskeyOtherAccount { return .passkeyOtherAccount }
        if FreshAuthRefusal.isOffline(error) { return .offline }
        return .passkeyFailed
    }
}

/// The proof block (UX 17.1). Draws nothing while a proof is live.
struct FreshAuthBlock: View {
    @ObservedObject var model: FreshAuthProofModel
    /// The surface's own submit, run by the field's return key.
    var onSubmit: () -> Void = {}

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        let layout = model.layout
        if layout.showsBlock {
            VStack(alignment: .leading, spacing: 10) {
                if layout.showsPasskeyNotice {
                    AuthHint(text: l10n.t("proof.passkey.notice"))
                }
                if layout.showsUsernameField {
                    AuthTextField(
                        label: l10n.t("auth.username.label"),
                        text: $model.username,
                        contentType: .username,
                        disabled: layout.fieldsDisabled
                    )
                }
                if layout.showsPasswordField {
                    AuthSecureField(
                        label: l10n.t("auth.password.label"),
                        text: $model.password,
                        disabled: layout.fieldsDisabled,
                        submitLabel: .go,
                        onSubmit: onSubmit
                    )
                    .accessibilityHint(layout.helpKey.map { l10n.t($0) } ?? "")
                    if let helpKey = layout.helpKey {
                        AuthHint(text: l10n.t(helpKey))
                    }
                }
                FreshAuthLine(model: model)
                if layout.showsPasskeyButton {
                    ghost("proof.passkey.use", enabled: layout.passkeyButtonEnabled) {
                        // The gesture satisfies the block; the surface's own
                        // button still decides (UX 17.2).
                        Task { _ = await model.usePasskey() }
                    }
                }
                if layout.showsUsePassword {
                    ghost("proof.password.use", enabled: layout.passkeyButtonEnabled) {
                        model.setUsePassword(true)
                    }
                }
            }
            .environment(\.layoutDirection, l10n.layoutDirection)
        }
    }

    /// A ghost button: accent text, no fill, a 44 pt target.
    private func ghost(_ key: String, enabled: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(l10n.t(key))
                .font(.body.weight(.medium))
                .foregroundStyle(Color.lakkoAccent)
                .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.5)
    }
}

/// The block's one line (wrong password, the countdown, the passkey sheet's
/// waiting line, ...).
struct FreshAuthLine: View {
    @ObservedObject var model: FreshAuthProofModel
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        switch model.layout.line {
        case .text(let key, let isError):
            AuthHint(text: l10n.t(key), isError: isError)
        case .countdown:
            WaitNotice(countdown: model.countdown)
        case .usernameRule:
            AuthHint(
                text: checkUsername(model.username).inlineMessage ?? l10n.t("recovery.username_required"),
                isError: true)
        case nil:
            EmptyView()
        }
    }
}
