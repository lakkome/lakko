// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// What the change-username screen shows, pure (ADR-0068).
enum ChangeUsernameLayout: Equatable {
    /// The last change is under 30 days ago: `username.change.too_soon`
    /// with the date, no form.
    case tooSoon(untilMs: Int64)
    /// The warnings, the proof block, the field.
    case form(emailTurnedOff: Bool)
    /// `username.claim.done` with the new name and
    /// `username.change.other_devices`.
    case done(newName: String)

    static func decide(
        tooSoonUntilMs: Int64?, renamedTo: String?, recoveryEmailEnabled: Bool, addressHeld: Bool
    ) -> ChangeUsernameLayout {
        if let renamedTo { return .done(newName: renamedTo) }
        if let tooSoonUntilMs { return .tooSoon(untilMs: tooSoonUntilMs) }
        return .form(
            emailTurnedOff: UsernameRename.warnings(
                recoveryEmailEnabled: recoveryEmailEnabled, addressHeld: addressHeld
            ).recoveryEmailTurnedOff)
    }

    /// Whether `Change` is enabled.
    static func canSubmit(typed: String, current: String, ruleMessage: String?, busy: Bool, proofReady: Bool) -> Bool {
        let trimmed = typed.trimmingCharacters(in: .whitespacesAndNewlines)
        return !busy && proofReady && !trimmed.isEmpty && ruleMessage == nil
            && trimmed.lowercased() != current.lowercased()
    }
}

/// "Change username" (ADR-0068): the warnings, the 17.1 proof block when no
/// fresh grant is live, the username field with the live rule, `Change`.
struct ChangeUsernameScreen: View {
    @ObservedObject var authRepository: AuthRepository
    @ObservedObject var center: AccountProtectionCenter
    let currentUsername: String
    let onDone: () -> Void

    @StateObject private var proof: FreshAuthProofModel
    @State private var username = ""
    @State private var busy = false
    @State private var failure: UsernameRename.Failure?
    @State private var renamedTo: String?
    @ObservedObject private var l10n = Localization.shared

    init(
        authRepository: AuthRepository, center: AccountProtectionCenter, currentUsername: String,
        onDone: @escaping () -> Void
    ) {
        self.authRepository = authRepository
        self.center = center
        self.currentUsername = currentUsername
        self.onDone = onDone
        self._proof = StateObject(
            wrappedValue: FreshAuthProofModel(surface: .changeUsername, repository: authRepository))
    }

    private var working: Bool { busy || proof.layout.working }
    private var ruleMessage: String? { checkUsername(username).inlineMessage }

    private var layout: ChangeUsernameLayout {
        ChangeUsernameLayout.decide(
            tooSoonUntilMs: tooSoonUntilMs,
            renamedTo: renamedTo,
            recoveryEmailEnabled: center.status?.enabled == true,
            addressHeld: center.addressHeld != nil)
    }

    /// The device's own stamp, or the date the server named.
    private var tooSoonUntilMs: Int64? {
        if case .tooSoon(let until)? = failure, let until { return until }
        return authRepository.renameTooSoonUntilMs
    }

    var body: some View {
        AuthScreen {
            AuthTitle(title: l10n.t("username.change.title"))
            switch layout {
            case .done(let newName):
                AuthHint(text: l10n.t("username.claim.done", texts: ["username": "@\(newName)"]))
                AuthHint(text: l10n.t("username.change.other_devices"))
                AuthPrimaryButton(title: l10n.t("common.continue"), action: onDone)
            case .tooSoon(let untilMs):
                AuthHint(text: l10n.t(
                    "username.change.too_soon",
                    texts: ["date": RecallCopy.date(ms: untilMs, locale: l10n.locale)]))
            case .form(let emailTurnedOff):
                AuthHint(text: l10n.t("username.change.body", texts: ["old": "@\(currentUsername)"]))
                AuthHint(text: l10n.t("username.change.next_change"))
                if emailTurnedOff {
                    AuthHint(text: l10n.t("recovery_email.username_change"), isError: true)
                }
                FreshAuthBlock(model: proof, onSubmit: submit)
                VStack(alignment: .leading, spacing: 6) {
                    AuthTextField(
                        label: l10n.t("auth.username.label"),
                        text: $username,
                        contentType: .username,
                        disabled: working,
                        submitLabel: .go,
                        onSubmit: submit
                    )
                    if let ruleMessage {
                        AuthHint(text: ruleMessage, isError: true)
                    } else {
                        AuthHint(text: l10n.t("auth.signup.username.hint"))
                    }
                }
                if let failure {
                    AuthHint(text: text(for: failure), isError: true)
                }
                AuthPrimaryButton(
                    title: l10n.t("username.change.save"),
                    busy: working,
                    busyTitle: l10n.t("common.saving"),
                    disabled: !ChangeUsernameLayout.canSubmit(
                        typed: username, current: currentUsername, ruleMessage: ruleMessage,
                        busy: working, proofReady: proof.layout.ready),
                    action: submit
                )
            }
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await proof.opened()
            await center.refresh()
        }
        .onDisappear { proof.closed() }
    }

    private func text(for failure: UsernameRename.Failure) -> String {
        switch failure {
        case .taken: return l10n.t("auth.signup.error.username_taken")
        case .tooSoon(let untilMs):
            return untilMs.map {
                l10n.t("username.change.too_soon", texts: ["date": RecallCopy.date(ms: $0, locale: l10n.locale)])
            } ?? l10n.t("common.error.too_many_attempts")
        case .bindLimited(let secs): return AuthRateLimit.handleBindLimitedCopy(retryAfterSecs: secs)
        case .rule, .sameName: return ruleMessage ?? l10n.t("username.error.unknown")
        case .offline: return l10n.t("common.error.offline")
        case .proofNeeded, .failed: return l10n.t("common.error.unavailable")
        }
    }

    private func submit() {
        guard ChangeUsernameLayout.canSubmit(
            typed: username, current: currentUsername, ruleMessage: ruleMessage,
            busy: working, proofReady: proof.layout.ready) else { return }
        let typed = username
        Task {
            // The proof first; a failure stays in the block.
            guard await proof.ensure() else { return }
            busy = true
            defer { busy = false }
            failure = nil
            do {
                renamedTo = try await authRepository.renameUsername(to: typed)
            } catch is FreshAuthRequired {
                // Not an error: the proof ran out. The block asks again.
                proof.proofExpired()
            } catch let refused as UsernameRenameFailed {
                failure = refused.failure
            } catch {
                failure = .failed
            }
        }
    }
}
