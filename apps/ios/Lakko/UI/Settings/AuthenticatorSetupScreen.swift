// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import SwiftUI
import UIKit

// UX `identity-recovery.md` 19.1b — setting up an authenticator app: the
// same screen from `Use an authenticator app` on the sign-up step (19.1a)
// and from Settings > Security > Recovery > Authenticator app. The pure
// rules are in `Auth/AuthenticatorSetup.swift`.

/// The state machine over the two enrolment routes.
@MainActor
final class AuthenticatorSetupModel: ObservableObject {

    struct Ports {
        /// The live fresh-auth grant, or `nil` (the 17.1 block is drawn).
        var liveGrant: @MainActor () -> String?
        var start: @MainActor (_ freshGrant: String) async throws -> (otpauthUri: String, secret: String)
        var finish: @MainActor (_ code: String, _ freshGrant: String) async throws -> Void
        var nowMs: () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }

        static func live(_ repository: AuthRepository) -> Ports {
            Ports(
                liveGrant: { [weak repository] in repository?.liveFreshGrant() },
                start: { [weak repository] grant in
                    guard let repository, let token = repository.identitySecrets?.sessionToken else {
                        throw FreshAuthNoAccount()
                    }
                    let answer = try await AuthClient().totpEnrollStart(sessionToken: token, freshGrant: grant)
                    return (answer.otpauthUri, answer.secretB32)
                },
                finish: { [weak repository] code, grant in
                    guard let repository, let token = repository.identitySecrets?.sessionToken else {
                        throw FreshAuthNoAccount()
                    }
                    try await AuthClient().totpEnrollFinish(sessionToken: token, code: code, freshGrant: grant)
                }
            )
        }
    }

    @Published private(set) var phase: AuthenticatorSetup.Phase
    @Published private(set) var otpauthUri = ""
    @Published private(set) var secret = ""
    @Published var code = ""
    /// The one line under the code field or in the failed state.
    @Published private(set) var lineKey: String?
    /// The wait a `429` named: the field stays disabled until zero.
    let wait: RetryCountdown

    private let ports: Ports
    private var waitSink: AnyCancellable?
    private var asking = false

    init(ports: Ports) {
        self.ports = ports
        self.wait = RetryCountdown(nowMs: ports.nowMs)
        self.phase = ports.liveGrant() == nil ? .needsProof : .starting
        waitSink = wait.objectWillChange.sink { [weak self] _ in self?.objectWillChange.send() }
    }

    convenience init(repository: AuthRepository) {
        self.init(ports: .live(repository))
    }

    /// The field takes digits while the key is on screen and no wait runs.
    var fieldEnabled: Bool { phase == .ready && !wait.waiting }

    /// The screen opened, the proof block was satisfied, or `Try again`:
    /// ask for a NEW key (a key shown before is never used).
    func begin() async {
        guard !asking, phase != .checking else { return }
        guard let grant = ports.liveGrant() else {
            phase = .needsProof
            return
        }
        asking = true
        defer { asking = false }
        code = ""
        lineKey = nil
        otpauthUri = ""
        secret = ""
        phase = .starting
        do {
            let key = try await ports.start(grant)
            otpauthUri = key.otpauthUri
            secret = key.secret
            phase = .ready
        } catch {
            fail(AuthenticatorSetup.failure(error), checking: false)
        }
    }

    /// The first appearance: start when a proof is live.
    func opened() async {
        guard otpauthUri.isEmpty, phase == .starting || phase == .needsProof else { return }
        await begin()
    }

    /// The code field changed: digits only, and the sixth digit checks.
    func codeChanged() {
        let digits = AuthenticatorSetup.digits(code)
        if digits != code { code = digits }
        guard fieldEnabled, let complete = AuthenticatorSetup.completeCode(code) else { return }
        Task { await check(complete) }
    }

    func check(_ complete: String) async {
        guard phase == .ready else { return }
        guard let grant = ports.liveGrant() else {
            // The proof ran out while the key was on screen: the block
            // appears; the setup starts over after it.
            code = ""
            phase = .needsProof
            return
        }
        phase = .checking
        lineKey = nil
        do {
            try await ports.finish(complete, grant)
            code = ""
            phase = .done
        } catch {
            fail(AuthenticatorSetup.failure(error), checking: true)
        }
    }

    private func fail(_ failure: AuthenticatorSetup.Failure, checking: Bool) {
        code = ""
        switch failure {
        case .wrongCode where checking:
            lineKey = AuthenticatorSetup.wrongCodeKey
            phase = .ready
        case .limited(let secs) where checking:
            if let secs, secs > 0 { wait.start(forSecs: secs) } else { lineKey = "common.error.too_many_attempts" }
            phase = .ready
        case .offline where checking:
            // The QR code and the key stay.
            lineKey = AuthenticatorSetup.offlineKey
            phase = .ready
        case .alreadyEnabled:
            phase = .alreadyEnabled
        case .proofNeeded:
            phase = .needsProof
        case .offline:
            lineKey = AuthenticatorSetup.offlineKey
            phase = .failed
        case .limited(let secs):
            if let secs, secs > 0 { wait.start(forSecs: secs) }
            lineKey = (secs ?? 0) > 0 ? nil : "common.error.too_many_attempts"
            phase = .failed
        case .wrongCode, .failed:
            lineKey = AuthenticatorSetup.genericKey
            phase = .failed
        }
    }
}

/// The screen of UX 19.1b.
struct AuthenticatorSetupScreen: View {
    @ObservedObject var authRepository: AuthRepository
    /// `true` on the sign-up step: `Done` goes straight back to the step.
    let atSignUp: Bool
    /// The account has an authenticator now (it was just set up, or the
    /// server said it already had one).
    let onSetUp: () -> Void
    /// Leave the screen (`Back`, or `Continue` after the result line).
    let onClose: () -> Void

    @StateObject private var model: AuthenticatorSetupModel
    @StateObject private var proof: FreshAuthProofModel
    @ObservedObject private var l10n = Localization.shared
    @FocusState private var codeFocused: Bool
    @State private var copied = false

    init(
        authRepository: AuthRepository, atSignUp: Bool,
        onSetUp: @escaping () -> Void, onClose: @escaping () -> Void
    ) {
        self.authRepository = authRepository
        self.atSignUp = atSignUp
        self.onSetUp = onSetUp
        self.onClose = onClose
        self._model = StateObject(wrappedValue: AuthenticatorSetupModel(repository: authRepository))
        self._proof = StateObject(
            wrappedValue: FreshAuthProofModel(surface: .addAuthenticator, repository: authRepository))
    }

    var body: some View {
        AuthScreen {
            AuthTitle(
                title: l10n.t("protect.totp.title"),
                subtitle: model.phase == .done || model.phase == .alreadyEnabled
                    ? nil : l10n.t("settings.recovery.totp.scan"))
            content
            if model.phase != .done {
                AuthLinkRow(
                    title: l10n.t("common.back"), disabled: model.phase == .checking, action: onClose)
            }
        }
        .task {
            await proof.opened()
            await model.opened()
        }
        .onDisappear { proof.closed() }
        .onChange(of: model.phase) { _, phase in
            switch phase {
            case .done, .alreadyEnabled:
                onSetUp()
                // At sign-up the step itself says `Done` on the row.
                if atSignUp { onClose() }
            case .ready: codeFocused = true
            case .needsProof: proof.proofExpired()
            default: break
            }
        }
        .onChange(of: model.code) { _, _ in model.codeChanged() }
    }

    @ViewBuilder
    private var content: some View {
        switch model.phase {
        case .needsProof:
            FreshAuthBlock(model: proof, onSubmit: prove)
            AuthPrimaryButton(
                title: l10n.t("settings.recovery.totp.set_up"),
                busy: proof.layout.working,
                busyTitle: l10n.t("common.checking"),
                disabled: !proof.layout.ready,
                action: prove
            )
        case .starting:
            ProgressView()
                .frame(maxWidth: .infinity, minHeight: 240)
        case .ready, .checking:
            QrTextImageView(text: model.otpauthUri)
                .frame(maxWidth: .infinity)
                .accessibilityHidden(true)
            keyRow
            AuthSecondaryButton(title: l10n.t("settings.recovery.totp.open_app"), action: openInApp)
            HStack(alignment: .bottom, spacing: 12) {
                AuthTextField(
                    label: l10n.t("recovery.totp.code_label"),
                    text: $model.code,
                    keyboard: .numberPad,
                    contentType: .oneTimeCode,
                    disabled: !model.fieldEnabled,
                    isFocused: $codeFocused
                )
                if model.phase == .checking {
                    ProgressView().padding(.bottom, 14)
                }
            }
            if model.wait.waiting {
                WaitNotice(countdown: model.wait)
            } else if let key = model.lineKey {
                AuthHint(text: l10n.t(key), isError: true)
            }
        case .done:
            AuthHint(text: l10n.t(AuthenticatorSetup.doneKey))
            AuthPrimaryButton(title: l10n.t("common.continue"), action: onClose)
        case .alreadyEnabled:
            AuthHint(text: l10n.t(AuthenticatorSetup.alreadyEnabledKey))
        case .failed:
            if model.wait.waiting {
                WaitNotice(countdown: model.wait)
            } else if let key = model.lineKey {
                AuthHint(text: l10n.t(key), isError: true)
            }
            AuthPrimaryButton(
                title: l10n.t("common.try_again"), disabled: model.wait.waiting,
                action: { Task { await model.begin() } })
        }
    }

    /// `Key: ABCD EFGH ...` in the code font, with `Copy`.
    private var keyRow: some View {
        let parts = l10n.parts("settings.recovery.totp.key", around: "key")
        let grouped = AuthenticatorSetup.groupedKey(model.secret)
        return HStack(alignment: .firstTextBaseline, spacing: 12) {
            (Text(parts.lead)
                + Text(grouped).font(.footnote.monospaced().weight(.semibold))
                + Text(parts.trail))
                .font(.footnote)
                .foregroundStyle(Color.lakkoText)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button(l10n.t(copied ? "common.copied" : "common.copy")) {
                // The key without the spaces: it pastes into any app.
                UIPasteboard.general.string = model.secret
                copied = true
            }
            .font(.footnote.weight(.semibold))
            .foregroundStyle(Color.lakkoAccent)
            .frame(minHeight: 44)
        }
    }

    private func prove() {
        Task {
            guard await proof.ensure() else { return }
            await model.begin()
        }
    }

    /// Hand the setup link to whichever authenticator app the phone has (a
    /// phone cannot scan its own screen). A link to another app, never a
    /// share sheet.
    private func openInApp() {
        guard let url = URL(string: model.otpauthUri) else { return }
        UIApplication.shared.open(url, options: [:]) { opened in
            if !opened { NSLog("[recovery] WARN no app opened the authenticator setup link") }
        }
    }
}
